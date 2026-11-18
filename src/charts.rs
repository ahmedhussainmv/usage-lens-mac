//! Small AppKit chart views. The figures are derived only from stored usage counts.
use objc2::rc::Retained;
use objc2::AllocAnyThread;
use objc2::{define_class, msg_send, DefinedClass, MainThreadOnly};
use objc2_app_kit::{NSBezierPath, NSColor, NSGradient, NSView};
use objc2_foundation::{MainThreadMarker, NSObjectProtocol, NSPoint, NSRect, NSSize};

pub enum ChartData {
    Trend(Vec<i64>),
    Donut([i64; 4]),
}
pub struct ChartIvars {
    data: ChartData,
}

/// Colour for token category `i`: uncached input, cache reads, cache writes, output.
pub fn category_color(i: usize) -> Retained<NSColor> {
    match i {
        0 => NSColor::systemBlueColor(),
        1 => NSColor::systemTealColor(),
        2 => NSColor::systemOrangeColor(),
        _ => NSColor::systemPurpleColor(),
    }
}

define_class!(
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = ChartIvars]
    pub struct ChartView;
    unsafe impl NSObjectProtocol for ChartView {}
    impl ChartView {
        #[unsafe(method(drawRect:))]
        fn draw_rect(&self,_dirty:NSRect) {
            let bounds=self.bounds();let w=bounds.size.width;let h=bounds.size.height;
            match &self.ivars().data {
                ChartData::Trend(values)=>draw_trend(values,w,h),
                ChartData::Donut(values)=>draw_donut(values,w,h),
            }
        }
    }
);

impl ChartView {
    pub fn new(mtm: MainThreadMarker, frame: NSRect, data: ChartData) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(ChartIvars { data });
        // NSView's designated initializer retains the frame and initializes the subclass.
        unsafe { msg_send![super(this),initWithFrame:frame] }
    }
}

fn dot(center: NSPoint, r: f64) -> Retained<NSBezierPath> {
    NSBezierPath::bezierPathWithOvalInRect(NSRect::new(
        NSPoint::new(center.x - r, center.y - r),
        NSSize::new(r * 2., r * 2.),
    ))
}

fn draw_trend(values: &[i64], w: f64, h: f64) {
    if values.is_empty() {
        return;
    }
    let (left, right, bottom, top) = (10.0, (w - 10.0).max(11.0), 4.0, (h - 8.0).max(9.0));
    // Gridlines at 0, 50 % and 100 % of the peak.
    for n in 0..=2 {
        let y = bottom + (top - bottom) * (n as f64) / 2.0;
        let line = NSBezierPath::bezierPath();
        line.moveToPoint(NSPoint::new(0., y));
        line.lineToPoint(NSPoint::new(w, y));
        line.setLineWidth(if n == 0 { 1.0 } else { 0.5 });
        NSColor::separatorColor().setStroke();
        line.stroke();
    }
    let max = (*values.iter().max().unwrap_or(&0)).max(1) as f64;
    let count = values.len();
    let points: Vec<NSPoint> = values
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let x = if count == 1 {
                (left + right) / 2.
            } else {
                left + (right - left) * (i as f64) / ((count - 1) as f64)
            };
            NSPoint::new(x, bottom + (top - bottom) * (*v as f64) / max)
        })
        .collect();
    // Catmull-Rom spline converted to cubic Beziers, clamped so it never dips below zero.
    let clamp = |y: f64| y.clamp(bottom, top);
    let build = |path: &NSBezierPath| {
        path.moveToPoint(points[0]);
        for i in 0..points.len().saturating_sub(1) {
            let p0 = points[i.saturating_sub(1)];
            let p1 = points[i];
            let p2 = points[i + 1];
            let p3 = points[(i + 2).min(points.len() - 1)];
            let c1 = NSPoint::new(p1.x + (p2.x - p0.x) / 6., clamp(p1.y + (p2.y - p0.y) / 6.));
            let c2 = NSPoint::new(p2.x - (p3.x - p1.x) / 6., clamp(p2.y - (p3.y - p1.y) / 6.));
            path.curveToPoint_controlPoint1_controlPoint2(p2, c1, c2);
        }
    };
    let blue = NSColor::systemBlueColor();
    if points.len() > 1 {
        let area = NSBezierPath::bezierPath();
        build(&area);
        area.lineToPoint(NSPoint::new(points[points.len() - 1].x, bottom));
        area.lineToPoint(NSPoint::new(points[0].x, bottom));
        area.closePath();
        // Fade from a soft tint at the line down to transparent at the baseline.
        match NSGradient::initWithStartingColor_endingColor(
            NSGradient::alloc(),
            &blue.colorWithAlphaComponent(0.0),
            &blue.colorWithAlphaComponent(0.32),
        ) {
            Some(gradient) => gradient.drawInBezierPath_angle(&area, 90.),
            None => {
                blue.colorWithAlphaComponent(0.14).setFill();
                area.fill();
            }
        }
        let line = NSBezierPath::bezierPath();
        build(&line);
        line.setLineWidth(2.25);
        blue.setStroke();
        line.stroke();
    }
    blue.setFill();
    for p in &points[..points.len() - 1] {
        dot(*p, 2.75).fill();
    }
    let last = points[points.len() - 1];
    blue.colorWithAlphaComponent(0.2).setFill();
    dot(last, 8.).fill();
    blue.setFill();
    dot(last, 4.5).fill();
}

fn draw_donut(values: &[i64; 4], w: f64, h: f64) {
    let thickness = 18.;
    let center = NSPoint::new(w / 2., h / 2.);
    let radius = (w.min(h) / 2. - thickness / 2. - 4.).max(1.);
    let sum: i64 = values.iter().sum();
    let track = |color: &NSColor| {
        let ring = dot(center, radius);
        ring.setLineWidth(thickness);
        color.setStroke();
        ring.stroke();
    };
    if sum == 0 {
        track(&NSColor::separatorColor());
        return;
    }
    if let Some((index, _)) = values.iter().enumerate().find(|(_, value)| **value == sum) {
        track(&category_color(index));
        return;
    }
    let mut angle = 90.0;
    for (i, value) in values.iter().enumerate() {
        if *value <= 0 {
            continue;
        }
        let sweep = 360.0 * (*value as f64) / (sum as f64);
        let end = angle - sweep;
        // A hairline gap between segments keeps neighbouring colours distinct.
        let gap = if sweep > 4. { 0.8 } else { 0. };
        let segment = NSBezierPath::bezierPath();
        segment.appendBezierPathWithArcWithCenter_radius_startAngle_endAngle_clockwise(
            center,
            radius,
            angle - gap,
            end + gap,
            true,
        );
        segment.setLineWidth(thickness);
        category_color(i).setStroke();
        segment.stroke();
        angle = end;
    }
}
