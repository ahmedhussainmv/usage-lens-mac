//! Native AppKit presentation. Collection and SQL queries run off the main thread.
//!
//! Visual language: a full-height translucent sidebar, a quiet page header with the
//! filters on the right, and rounded "cards" on the window background. Numbers use
//! tabular digits; colour is reserved for data (token categories and sources).
use crate::{
    charts::{category_color, ChartData, ChartView},
    collector::{self, Roots, Scan},
    update::{self, Release},
    store::{self, Filter, Group, Rate, Summary},
};
use chrono::Local;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{define_class, msg_send, sel, DefinedClass, MainThreadOnly};
use objc2_app_kit::{
    NSAccessibility, NSAlert, NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate,
    NSAutoresizingMaskOptions as Mask, NSBackingStoreType, NSBox, NSBoxType, NSButton,
    NSCellImagePosition, NSColor, NSFont, NSImage, NSMenu, NSMenuItem, NSPopUpButton, NSSavePanel, NSScrollView, NSSegmentedControl, NSSegmentSwitchTracking,
    NSTextAlignment, NSTextField, NSTitlePosition, NSView, NSVisualEffectBlendingMode,
    NSVisualEffectMaterial, NSVisualEffectView, NSWindow, NSWindowDelegate, NSWindowStyleMask,
    NSWindowTitleVisibility,
};
use objc2_foundation::{
    ns_string, MainThreadMarker, NSArray, NSNotification, NSObject, NSObjectProtocol, NSPoint, NSRect,
    NSSize, NSString, NSTimer,
};
use std::{
    cell::{OnceCell, RefCell},
    collections::HashMap,
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};

// ---------------------------------------------------------------------------------------
// Layout constants
// ---------------------------------------------------------------------------------------
const WINDOW_W: f64 = 1120.;
const WINDOW_H: f64 = 780.;
const SIDEBAR_W: f64 = 224.;
const PAGE_PAD: f64 = 28.;
const FOOTER_H: f64 = 44.;
const HEADER_H: f64 = 108.;
const GAP: f64 = 14.;
const CARD_PAD: f64 = 20.;

const W_REGULAR: f64 = 0.0;
const W_MEDIUM: f64 = 0.23;
const W_SEMIBOLD: f64 = 0.3;

const PAGES: [(&str, &str); 6] = [
    ("Overview", "square.grid.2x2"),
    ("Insights", "chart.xyaxis.line"),
    ("Models & effort", "cpu"),
    ("Projects", "folder"),
    ("Rates", "dollarsign.circle"),
    ("About & updates", "info.circle"),
];

fn rect(x: f64, y: f64, w: f64, h: f64) -> NSRect {
    NSRect::new(NSPoint::new(x, y), NSSize::new(w, h))
}
fn text(s: &str) -> Retained<NSString> {
    NSString::from_str(s)
}
fn number(v: i64) -> String {
    let s = v.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 && c != '-' && !s[..i].ends_with('-') {
            out.push(',');
        }
        out.push(c);
    }
    out
}

// ---------------------------------------------------------------------------------------
// View helpers
// ---------------------------------------------------------------------------------------
struct Sty {
    size: f64,
    weight: f64,
    color: Retained<NSColor>,
    align: NSTextAlignment,
    mono: bool,
}
impl Sty {
    fn new(size: f64, weight: f64) -> Self {
        Sty {
            size,
            weight,
            color: NSColor::labelColor(),
            align: NSTextAlignment::Left,
            mono: false,
        }
    }
    fn color(mut self, c: Retained<NSColor>) -> Self {
        self.color = c;
        self
    }
    fn secondary(self) -> Self {
        self.color(NSColor::secondaryLabelColor())
    }
    fn tertiary(self) -> Self {
        self.color(NSColor::tertiaryLabelColor())
    }
    fn right(mut self) -> Self {
        self.align = NSTextAlignment::Right;
        self
    }
    fn center(mut self) -> Self {
        self.align = NSTextAlignment::Center;
        self
    }
    fn mono(mut self) -> Self {
        self.mono = true;
        self
    }
}

fn put(
    parent: &NSView,
    mtm: MainThreadMarker,
    value: &str,
    frame: NSRect,
    style: Sty,
) -> Retained<NSTextField> {
    let v = NSTextField::labelWithString(&text(value), mtm);
    v.setFrame(frame);
    let font = if style.mono {
        NSFont::monospacedDigitSystemFontOfSize_weight(style.size, style.weight)
    } else {
        NSFont::systemFontOfSize_weight(style.size, style.weight)
    };
    v.setFont(Some(&font));
    v.setTextColor(Some(&style.color));
    v.setAlignment(style.align);
    v.setAutoresizingMask(Mask::ViewMinYMargin);
    parent.addSubview(&v);
    v
}

/// A small uppercase caption used above values and as table headers.
fn caption(parent: &NSView, mtm: MainThreadMarker, value: &str, frame: NSRect, right: bool) {
    let mut style = Sty::new(10., W_SEMIBOLD).secondary();
    if right {
        style = style.right();
    }
    put(parent, mtm, &value.to_uppercase(), frame, style);
}

fn rounded(
    parent: &NSView,
    mtm: MainThreadMarker,
    frame: NSRect,
    radius: f64,
    fill: &NSColor,
) -> Retained<NSBox> {
    let b = NSBox::initWithFrame(NSBox::alloc(mtm), frame);
    b.setBoxType(NSBoxType::Custom);
    b.setTitlePosition(NSTitlePosition::NoTitle);
    b.setBorderWidth(0.);
    b.setCornerRadius(radius);
    b.setFillColor(fill);
    b.setAutoresizingMask(Mask::ViewMinYMargin);
    parent.addSubview(&b);
    b
}
fn card(parent: &NSView, mtm: MainThreadMarker, frame: NSRect) -> Retained<NSBox> {
    let b = rounded(parent, mtm, frame, 16., &NSColor::controlBackgroundColor());
    b.setBorderWidth(0.5);
    b.setBorderColor(&NSColor::separatorColor().colorWithAlphaComponent(0.35));
    b
}
fn dot(parent: &NSView, mtm: MainThreadMarker, x: f64, y: f64, d: f64, color: &NSColor) {
    rounded(parent, mtm, rect(x, y, d, d), d / 2., color);
}
fn hline(parent: &NSView, mtm: MainThreadMarker, x: f64, y: f64, w: f64) {
    rounded(
        parent,
        mtm,
        rect(x, y, w, 1.),
        0.,
        &NSColor::separatorColor().colorWithAlphaComponent(0.4),
    );
}
fn meter(
    parent: &NSView,
    mtm: MainThreadMarker,
    x: f64,
    y: f64,
    w: f64,
    fraction: f64,
    color: &NSColor,
) {
    rounded(
        parent,
        mtm,
        rect(x, y, w, 6.),
        3.,
        &NSColor::quaternaryLabelColor(),
    );
    if fraction > 0. {
        rounded(
            parent,
            mtm,
            rect(x, y, (w * fraction).max(6.).min(w), 6.),
            3.,
            color,
        );
    }
}
fn source_color(source: &str) -> Retained<NSColor> {
    if source.eq_ignore_ascii_case("claude") {
        NSColor::systemOrangeColor()
    } else {
        NSColor::systemBlueColor()
    }
}
fn group_color(category: &str) -> Retained<NSColor> {
    if category == "personal" {
        NSColor::systemPinkColor()
    } else {
        NSColor::systemIndigoColor()
    }
}
fn category_label(category: &str) -> &'static str {
    if category == "personal" {
        "Personal"
    } else {
        "Work"
    }
}
fn estimate_text(sp: &store::Split) -> String {
    if sp.events == 0 || sp.unpriced == sp.events {
        "no estimate".into()
    } else if sp.unpriced > 0 {
        format!("${} + unpriced", with_cents(sp.estimate))
    } else {
        format!("${}", with_cents(sp.estimate))
    }
}
fn csv_field(v: &str) -> String {
    // Quote every field and neutralise spreadsheet formulas.
    let guard = if v.starts_with(['=', '+', '-', '@']) { "'" } else { "" };
    format!("\"{guard}{}\"", v.replace('"', "\"\""))
}
fn source_name(source: &str) -> &'static str {
    if source.eq_ignore_ascii_case("claude") {
        "Claude"
    } else {
        "Codex"
    }
}
/// A tinted capsule naming the source of a row.
fn source_pill(parent: &NSView, mtm: MainThreadMarker, x: f64, y: f64, source: &str) {
    let color = source_color(source);
    rounded(
        parent,
        mtm,
        rect(x, y, 58., 20.),
        10.,
        &color.colorWithAlphaComponent(0.15),
    );
    put(
        parent,
        mtm,
        source_name(source),
        rect(x, y + 2., 58., 16.),
        Sty::new(11., W_SEMIBOLD).color(color).center(),
    );
}
fn button(
    mtm: MainThreadMarker,
    title: &str,
    frame: NSRect,
    target: &AnyObject,
    action: objc2::runtime::Sel,
) -> Retained<NSButton> {
    let b = unsafe {
        NSButton::buttonWithTitle_target_action(&text(title), Some(target), Some(action), mtm)
    };
    b.setFrame(frame);
    b.setAutoresizingMask(Mask::ViewMinYMargin);
    b
}
fn symbol(name: &str, description: &str) -> Option<Retained<NSImage>> {
    NSImage::imageWithSystemSymbolName_accessibilityDescription(&text(name), Some(&text(description)))
}
fn popup(
    mtm: MainThreadMarker,
    frame: NSRect,
    values: &[&str],
    target: &AnyObject,
    action: objc2::runtime::Sel,
) -> Retained<NSPopUpButton> {
    let b = NSPopUpButton::initWithFrame_pullsDown(NSPopUpButton::alloc(mtm), frame, false);
    for value in values {
        b.addItemWithTitle(&text(value));
    }
    unsafe {
        b.setTarget(Some(target));
        b.setAction(Some(action));
    }
    b.setAutoresizingMask(Mask::ViewMinYMargin);
    b
}
/// A vertically scrolling list whose rows are laid out from the top.
/// Returns the document view and its height; rows are placed with `row_y`.
fn scroller(
    parent: &NSView,
    mtm: MainThreadMarker,
    frame: NSRect,
    content_height: f64,
    from_top: f64,
) -> (Retained<NSView>, f64) {
    let scroll = NSScrollView::initWithFrame(NSScrollView::alloc(mtm), frame);
    scroll.setHasVerticalScroller(true);
    scroll.setHasHorizontalScroller(false);
    scroll.setAutohidesScrollers(true);
    scroll.setDrawsBackground(false);
    scroll.setAutoresizingMask(Mask::ViewWidthSizable | Mask::ViewHeightSizable);
    let doc_h = content_height.max(frame.size.height);
    let doc = NSView::initWithFrame(NSView::alloc(mtm), rect(0., 0., frame.size.width, doc_h));
    scroll.setDocumentView(Some(&doc));
    parent.addSubview(&scroll);
    // Non-flipped documents open at the bottom; scroll to the first row.
    doc.scrollPoint(NSPoint::new(
        0.,
        (doc_h - frame.size.height - from_top).clamp(0., doc_h),
    ));
    (doc, doc_h)
}

fn truncate(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        s.into()
    } else {
        format!("{}…", s.chars().take(max.saturating_sub(1)).collect::<String>())
    }
}
/// Truncate to what fits in `width` points at roughly `size`-point type.
fn fit(s: &str, width: f64, size: f64) -> String {
    truncate(s, ((width / (size * 0.56)).floor() as usize).max(4))
}
fn compact(n: i64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.1}B", n as f64 / 1_000_000_000.)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.)
    } else {
        number(n)
    }
}
fn compact_precise(n: i64) -> String {
    if n >= 1_000_000_000 {
        format!("{:.2}B", n as f64 / 1_000_000_000.)
    } else if n >= 1_000_000 {
        format!("{:.2}M", n as f64 / 1_000_000.)
    } else if n >= 100_000 {
        format!("{:.1}K", n as f64 / 1_000.)
    } else {
        number(n)
    }
}
fn percent(value: i64, total: i64) -> String {
    if total == 0 {
        "0%".into()
    } else if value > 0 && (value as f64) / (total as f64) < 0.001 {
        "<0.1%".into()
    } else {
        format!("{:.1}%", 100.0 * (value as f64) / (total as f64))
    }
}
fn short_date(s: &str) -> String {
    chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map(|d| d.format("%b %-d").to_string())
        .unwrap_or_else(|_| s.to_owned())
}
fn group_total(g: &Group) -> i64 {
    g.uncached + g.cache_read + g.cache_write + g.output
}
fn page_subtitle(page: Page) -> &'static str {
    match page {
        Page::Overview => "A first look at your local Codex and Claude Code usage",
        Page::Insights => "Daily activity, token mix and leading models for the selected filters",
        Page::Models => "Token totals grouped by recorded model and effort",
        Page::Projects => "Grouped by last recorded folder name; no work labels are inferred",
        Page::Rates => "API-equivalent estimates. These are not subscription charges",
        Page::About => "Version, updates, feedback and how your data is handled",
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Overview,
    Insights,
    Models,
    Projects,
    Rates,
    About,
}
impl Page {
    fn index(self) -> usize {
        match self {
            Page::Overview => 0,
            Page::Insights => 1,
            Page::Models => 2,
            Page::Projects => 3,
            Page::Rates => 4,
            Page::About => 5,
        }
    }
}
type ScanMessage = Result<Scan, String>;
type UpdateMessage = (bool, Result<Option<Release>, String>);
type QueryMessage = (u64, Result<Summary, String>);
struct State {
    window: Retained<NSWindow>,
    body: Retained<NSView>,
    title: Retained<NSTextField>,
    subtitle: Retained<NSTextField>,
    status: Retained<NSTextField>,
    status_dot: Retained<NSBox>,
    source: Retained<NSPopUpButton>,
    period: Retained<NSPopUpButton>,
    refresh: Retained<NSButton>,
    category: Retained<NSPopUpButton>,
    update_tx: Sender<UpdateMessage>,
    update_rx: Receiver<UpdateMessage>,
    update_available: Option<Release>,
    update_note: String,
    checking: bool,
    auto_update: bool,
    demo: bool,
    project_rows: Vec<(String, String)>,
    list_doc: Option<Retained<NSView>>,
    rendered_page: Option<Page>,
    nav: Vec<Retained<NSButton>>,
    nav_bg: Vec<Retained<NSBox>>,
    page: Page,
    db: PathBuf,
    roots: Roots,
    summary: Option<Summary>,
    scan_tx: Sender<ScanMessage>,
    scan_rx: Receiver<ScanMessage>,
    query_tx: Sender<QueryMessage>,
    query_rx: Receiver<QueryMessage>,
    scanning: bool,
    last_started: Option<Instant>,
    last_updated: Option<String>,
    last_scan: Option<Scan>,
    error: Option<String>,
    generation: u64,
    rate_fields: Vec<Retained<NSTextField>>,
    rate_source: Option<Retained<NSPopUpButton>>,
}

struct DelegateIvars {
    state: OnceCell<RefCell<State>>,
}

define_class!(
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = DelegateIvars]
    struct Delegate;
    unsafe impl NSObjectProtocol for Delegate {}
    unsafe impl NSApplicationDelegate for Delegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, notification:&NSNotification) {
            let app=notification.object().unwrap().downcast::<NSApplication>().unwrap();
            self.build(&app);
        }
    }
    unsafe impl NSWindowDelegate for Delegate {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self,_notification:&NSNotification) {
            if let Some(c)=self.ivars().state.get() {
                if let Ok(s)=c.try_borrow() {if s.demo {if let Some(d)=s.db.parent() {let _=std::fs::remove_dir_all(d);}}}
            }
            NSApplication::sharedApplication(self.mtm()).terminate(None);
        }
        #[unsafe(method(windowDidResize:))]
        fn window_did_resize(&self,_notification:&NSNotification) {
            // During a live drag the cards stay anchored; the layout is rebuilt once it ends.
            let live = self.ivars().state.get()
                .and_then(|c| c.try_borrow().ok().map(|s| s.window.inLiveResize()))
                .unwrap_or(true);
            if !live {self.render_keeping_input();}
        }
        #[unsafe(method(windowDidEndLiveResize:))]
        fn window_did_end_live_resize(&self,_notification:&NSNotification) {self.render_keeping_input();}
    }
    impl Delegate {
        #[unsafe(method(navigate:))]
        fn navigate(&self,sender:&NSButton) {
            if let Some(cell)=self.ivars().state.get() {
                let mut state=cell.borrow_mut();
                state.page=match sender.tag() {1=>Page::Insights,2=>Page::Models,3=>Page::Projects,4=>Page::Rates,5=>Page::About,_=>Page::Overview};
            }
            self.render();
        }
        #[unsafe(method(filterChanged:))]
        fn filter_changed(&self,_sender:&AnyObject) {self.request_query();}
        #[unsafe(method(refreshNow:))]
        fn refresh_now(&self,_sender:&AnyObject) {self.start_scan();}
        #[unsafe(method(categoryChanged:))]
        fn category_changed(&self,sender:&NSSegmentedControl) {self.assign_project(sender.tag() as usize, sender.selectedSegment());}
        #[unsafe(method(defaultCategoryChanged:))]
        fn default_category_changed(&self,sender:&NSPopUpButton) {self.set_default(sender.indexOfSelectedItem());}
        #[unsafe(method(appearanceChanged:))]
        fn appearance_changed(&self,sender:&NSSegmentedControl) {
            let mode=match sender.selectedSegment() {1=>"light",2=>"dark",_=>"system"};
            self.apply_appearance(mode);
            if let Some(cell)=self.ivars().state.get() {
                if let Ok(s)=cell.try_borrow() {let _=store::open(&s.db).and_then(|c| store::set_setting(&c,"appearance",mode));}
            }
        }
        #[unsafe(method(checkUpdates:))]
        fn check_updates_action(&self,_sender:&AnyObject) {self.start_update_check(true);}
        #[unsafe(method(sendFeedback:))]
        fn send_feedback_action(&self,_sender:&AnyObject) {open_url(&update::feedback_url(&update::macos_version()));}
        #[unsafe(method(openRepo:))]
        fn open_repo_action(&self,_sender:&AnyObject) {open_url(update::REPO_URL);}
        #[unsafe(method(openRelease:))]
        fn open_release_action(&self,_sender:&AnyObject) {
            let url=self.ivars().state.get().and_then(|c| c.try_borrow().ok().and_then(|s| s.update_available.as_ref().map(|r| r.url.clone())));
            open_url(&url.unwrap_or_else(|| format!("{}/releases/latest", update::REPO_URL)));
        }
        #[unsafe(method(autoUpdateToggled:))]
        fn auto_update_toggled(&self,sender:&NSButton) {
            let on = sender.state() == 1;
            if let Some(cell)=self.ivars().state.get() {
                if let Ok(mut s)=cell.try_borrow_mut() {
                    s.auto_update=on;
                    let _=store::open(&s.db).and_then(|c| store::set_setting(&c,"auto_update",if on {"1"} else {"0"}));
                }
            }
        }
        #[unsafe(method(exportCsv:))]
        fn export_csv_action(&self,_sender:&AnyObject) {self.export_csv();}
        #[unsafe(method(saveRate:))]
        fn save_rate_action(&self,_sender:&AnyObject) {self.save_rate();}
        #[unsafe(method(tick:))]
        fn tick(&self,_timer:&NSTimer) {self.on_tick();}
    }
);

impl Delegate {
    fn new(mtm: MainThreadMarker) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars(DelegateIvars {
            state: OnceCell::new(),
        });
        unsafe { msg_send![super(this), init] }
    }
    fn build_menu(&self, app: &NSApplication) {
        let mtm = self.mtm();
        let menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!(""));
        let root = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                ns_string!(""),
                None,
                ns_string!(""),
            )
        };
        let app_menu = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("Usage Lens"));
        unsafe {
            app_menu.addItemWithTitle_action_keyEquivalent(
                ns_string!("Quit Usage Lens"),
                Some(sel!(terminate:)),
                ns_string!("q"),
            );
        }
        root.setSubmenu(Some(&app_menu));
        menu.addItem(&root);
        let edit_root = unsafe {
            NSMenuItem::initWithTitle_action_keyEquivalent(
                NSMenuItem::alloc(mtm),
                ns_string!("Edit"),
                None,
                ns_string!(""),
            )
        };
        let edit = NSMenu::initWithTitle(NSMenu::alloc(mtm), ns_string!("Edit"));
        unsafe {
            edit.addItemWithTitle_action_keyEquivalent(
                ns_string!("Cut"),
                Some(sel!(cut:)),
                ns_string!("x"),
            );
            edit.addItemWithTitle_action_keyEquivalent(
                ns_string!("Copy"),
                Some(sel!(copy:)),
                ns_string!("c"),
            );
            edit.addItemWithTitle_action_keyEquivalent(
                ns_string!("Paste"),
                Some(sel!(paste:)),
                ns_string!("v"),
            );
            edit.addItemWithTitle_action_keyEquivalent(
                ns_string!("Select All"),
                Some(sel!(selectAll:)),
                ns_string!("a"),
            );
        }
        edit_root.setSubmenu(Some(&edit));
        menu.addItem(&edit_root);
        app.setMainMenu(Some(&menu));
    }
    fn build(&self, app: &NSApplication) {
        let mtm = self.mtm();
        self.build_menu(app);
        let demo = std::env::var_os("USAGE_LENS_DEMO").is_some();
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        let data = if demo {
            // Synthetic data in a throwaway folder; nothing on this Mac is read.
            let dir = std::env::temp_dir().join(format!("usage-lens-demo-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            let _ = crate::demo::seed(&dir.join("usage.sqlite"));
            dir
        } else {
            std::env::var_os("USAGE_LENS_DATA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join("Library/Application Support/Usage Lens Mac"))
        };
        let saved_settings = store::open(&data.join("usage.sqlite")).ok();
        let auto_update = saved_settings
            .as_ref()
            .and_then(|c| store::setting(c, "auto_update").ok().flatten())
            .is_some_and(|v| v == "1");
        // AppKit windows are released explicitly by our retained State.
        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                rect(0., 0., WINDOW_W, WINDOW_H),
                NSWindowStyleMask::Titled
                    | NSWindowStyleMask::Closable
                    | NSWindowStyleMask::Miniaturizable
                    | NSWindowStyleMask::Resizable
                    | NSWindowStyleMask::FullSizeContentView,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        unsafe {
            window.setReleasedWhenClosed(false);
            window.setDelegate(Some(ProtocolObject::from_ref(self)));
        }
        window.setTitle(&text(if std::env::var_os("USAGE_LENS_DEMO").is_some() {
            "Usage Lens (demo data)"
        } else {
            "Usage Lens"
        }));
        window.setTitlebarAppearsTransparent(true);
        window.setTitleVisibility(NSWindowTitleVisibility::Hidden);
        window.setMovableByWindowBackground(true);
        window.setContentMinSize(NSSize::new(980., 720.));
        let content = window.contentView().unwrap();

        // ---- Sidebar -------------------------------------------------------------------
        let sidebar = NSVisualEffectView::initWithFrame(
            NSVisualEffectView::alloc(mtm),
            rect(0., 0., SIDEBAR_W, WINDOW_H),
        );
        sidebar.setMaterial(NSVisualEffectMaterial::Sidebar);
        sidebar.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        sidebar.setAutoresizingMask(Mask::ViewHeightSizable);
        content.addSubview(&sidebar);

        // Brand: accent tile with a glyph, name and a one-line description.
        let brand_y = WINDOW_H - 84.;
        rounded(
            &sidebar,
            mtm,
            rect(18., brand_y, 34., 34.),
            9.,
            &NSColor::systemBlueColor(),
        );
        let glyph = unsafe {
            NSButton::buttonWithImage_target_action(
                &symbol("chart.bar.xaxis", "Usage Lens").unwrap(),
                None,
                None,
                mtm,
            )
        };
        glyph.setBordered(false);
        glyph.setFrame(rect(18., brand_y, 34., 34.));
        glyph.setContentTintColor(Some(&NSColor::whiteColor()));
        glyph.setAutoresizingMask(Mask::ViewMinYMargin);
        sidebar.addSubview(&glyph);
        put(
            &sidebar,
            mtm,
            "Usage Lens",
            rect(62., brand_y + 16., 150., 18.),
            Sty::new(15., W_SEMIBOLD),
        );
        put(
            &sidebar,
            mtm,
            "Local usage",
            rect(62., brand_y + 1., 150., 14.),
            Sty::new(11., W_REGULAR).secondary(),
        );
        put(
            &sidebar,
            mtm,
            "BROWSE",
            rect(24., WINDOW_H - 142., 160., 14.),
            Sty::new(10., W_SEMIBOLD).tertiary(),
        );
        let mut nav = Vec::new();
        let mut nav_bg = Vec::new();
        for (i, (name, glyph_name)) in PAGES.iter().enumerate() {
            let y = WINDOW_H - 184. - (i as f64) * 36.;
            let bg = rounded(
                &sidebar,
                mtm,
                rect(12., y, SIDEBAR_W - 24., 32.),
                8.,
                &NSColor::clearColor(),
            );
            let b = button(
                mtm,
                name,
                rect(20., y, SIDEBAR_W - 40., 32.),
                self.as_ref(),
                sel!(navigate:),
            );
            b.setBordered(false);
            b.setAlignment(NSTextAlignment::Left);
            b.setImage(symbol(glyph_name, name).as_deref());
            b.setImagePosition(NSCellImagePosition::ImageLeading);
            b.setTag(i as isize);
            sidebar.addSubview(&b);
            nav.push(b);
            nav_bg.push(bg);
        }
        put(
            &sidebar,
            mtm,
            "APPEARANCE",
            rect(24., 92., 160., 14.),
            Sty::new(10., W_SEMIBOLD).tertiary(),
        )
        .setAutoresizingMask(Mask::empty());
        let saved_mode = std::env::var("USAGE_LENS_APPEARANCE")
            .ok()
            .or_else(|| {
                saved_settings
                    .as_ref()
                    .and_then(|c| store::setting(c, "appearance").ok().flatten())
            })
            .unwrap_or_else(|| "system".into());
        let modes = NSArray::from_retained_slice(&[text("Auto"), text("Light"), text("Dark")]);
        let appearance_control = unsafe {
            NSSegmentedControl::segmentedControlWithLabels_trackingMode_target_action(
                &modes,
                NSSegmentSwitchTracking::SelectOne,
                Some(self.as_ref()),
                Some(sel!(appearanceChanged:)),
                mtm,
            )
        };
        appearance_control.setFrame(rect(18., 62., SIDEBAR_W - 36., 24.));
        appearance_control.setSelectedSegment(match saved_mode.as_str() {
            "light" => 1,
            "dark" => 2,
            _ => 0,
        });
        appearance_control.setAutoresizingMask(Mask::empty());
        sidebar.addSubview(&appearance_control);
        self.apply_appearance(&saved_mode);
        dot(&sidebar, mtm, 24., 31., 7., &NSColor::systemGreenColor());
        let foot = put(
            &sidebar,
            mtm,
            "Local to this Mac",
            rect(38., 25., SIDEBAR_W - 50., 16.),
            Sty::new(11., W_REGULAR).secondary(),
        );
        foot.setAutoresizingMask(Mask::empty());

        // ---- Header --------------------------------------------------------------------
        let left = SIDEBAR_W + PAGE_PAD;
        let title = put(
            &content,
            mtm,
            "Overview",
            rect(left, WINDOW_H - 74., 420., 32.),
            Sty::new(26., W_SEMIBOLD),
        );
        let subtitle = put(
            &content,
            mtm,
            page_subtitle(Page::Overview),
            rect(left, WINDOW_H - 96., 520., 18.),
            Sty::new(12., W_REGULAR).secondary(),
        );
        let right_edge = WINDOW_W - PAGE_PAD;
        let refresh = button(
            mtm,
            "Refresh",
            rect(right_edge - 104., WINDOW_H - 72., 104., 28.),
            self.as_ref(),
            sel!(refreshNow:),
        );
        refresh.setImage(symbol("arrow.clockwise", "Refresh").as_deref());
        refresh.setImagePosition(NSCellImagePosition::ImageLeading);
        refresh.setAutoresizingMask(Mask::ViewMinXMargin | Mask::ViewMinYMargin);
        content.addSubview(&refresh);
        let period = popup(
            mtm,
            rect(right_edge - 104. - 8. - 140., WINDOW_H - 72., 140., 28.),
            &["All time", "Last 24 hours", "Last 7 days", "Last 30 days"],
            self.as_ref(),
            sel!(filterChanged:),
        );
        period.setAutoresizingMask(Mask::ViewMinXMargin | Mask::ViewMinYMargin);
        content.addSubview(&period);
        let category = popup(
            mtm,
            rect(right_edge - 104. - 8. - 140. - 8. - 128. - 8. - 140., WINDOW_H - 72., 140., 28.),
            &["All categories", "Work", "Personal"],
            self.as_ref(),
            sel!(filterChanged:),
        );
        category.setAutoresizingMask(Mask::ViewMinXMargin | Mask::ViewMinYMargin);
        content.addSubview(&category);
        let source = popup(
            mtm,
            rect(right_edge - 104. - 8. - 140. - 8. - 128., WINDOW_H - 72., 128., 28.),
            &["Both sources", "Codex", "Claude"],
            self.as_ref(),
            sel!(filterChanged:),
        );
        source.setAutoresizingMask(Mask::ViewMinXMargin | Mask::ViewMinYMargin);
        content.addSubview(&source);

        // ---- Body and footer -----------------------------------------------------------
        let body = NSView::initWithFrame(
            NSView::alloc(mtm),
            rect(
                left,
                FOOTER_H,
                WINDOW_W - SIDEBAR_W - 2. * PAGE_PAD,
                WINDOW_H - FOOTER_H - HEADER_H,
            ),
        );
        body.setAutoresizingMask(Mask::ViewWidthSizable | Mask::ViewHeightSizable);
        content.addSubview(&body);
        let status_dot = rounded(
            &content,
            mtm,
            rect(left, 21., 8., 8.),
            4.,
            &NSColor::tertiaryLabelColor(),
        );
        status_dot.setAutoresizingMask(Mask::empty());
        let status = put(
            &content,
            mtm,
            "Preparing local history…",
            rect(left + 16., 15., 800., 20.),
            Sty::new(12., W_REGULAR).secondary(),
        );
        status.setAutoresizingMask(Mask::ViewWidthSizable);

        let (update_tx, update_rx) = mpsc::channel();
        let (scan_tx, scan_rx) = mpsc::channel();
        let (query_tx, query_rx) = mpsc::channel();
        let state = State {
            window,
            body,
            title,
            subtitle,
            status,
            status_dot,
            source,
            period,
            refresh,
            category,
            update_tx,
            update_rx,
            update_available: None,
            update_note: String::new(),
            checking: false,
            auto_update,
            demo,
            project_rows: Vec::new(),
            list_doc: None,
            rendered_page: None,
            nav,
            nav_bg,
            page: Page::Overview,
            db: data.join("usage.sqlite"),
            roots: if demo {
                Roots::for_home(std::path::Path::new("/nonexistent-usage-lens-demo"))
            } else {
                Roots::for_home(&home)
            },
            summary: None,
            scan_tx,
            scan_rx,
            query_tx,
            query_rx,
            scanning: false,
            last_started: None,
            last_updated: None,
            last_scan: None,
            error: None,
            generation: 0,
            rate_fields: Vec::new(),
            rate_source: None,
        };
        let _ = self.ivars().state.set(RefCell::new(state));
        self.render();
        self.request_query();
        self.start_scan();
        unsafe {
            let _timer = NSTimer::scheduledTimerWithTimeInterval_target_selector_userInfo_repeats(
                1.0,
                self.as_ref(),
                sel!(tick:),
                None,
                true,
            );
        }
        let state = self.ivars().state.get().unwrap().borrow();
        state.window.center();
        state.window.makeKeyAndOrderFront(None);
        app.setActivationPolicy(NSApplicationActivationPolicy::Regular);
        #[allow(deprecated)]
        app.activateIgnoringOtherApps(true);
        let auto = state.auto_update;
        drop(state);
        self.onboard();
        if auto {
            self.start_update_check(false);
        }
    }
    /// "system" follows macOS; "light" and "dark" force this app's appearance.
    fn apply_appearance(&self, mode: &str) {
        let app = NSApplication::sharedApplication(self.mtm());
        let appearance = match mode {
            "light" => NSAppearance::appearanceNamed(unsafe { NSAppearanceNameAqua }),
            "dark" => NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }),
            _ => None,
        };
        app.setAppearance(appearance.as_deref());
    }
    fn assign_project(&self, row: usize, segment: isize) {
        let category = if segment == 1 { "personal" } else { "work" };
        let failure = {
            let Some(cell) = self.ivars().state.get() else {
                return;
            };
            let Ok(mut s) = cell.try_borrow_mut() else {
                return;
            };
            let Some((source, project)) = s.project_rows.get(row).cloned() else {
                return;
            };
            let result = store::open(&s.db)
                .and_then(|c| store::set_category(&c, "project", &source, &project, category));
            match result {
                Ok(()) => {
                    if let Some(summary) = s.summary.as_mut() {
                        summary.project_cats.insert((source, project), category.into());
                    }
                    None
                }
                Err(e) => Some(format!("Could not save category: {e}")),
            }
        };
        if let (Some(message), Some(cell)) = (failure, self.ivars().state.get()) {
            if let Ok(s) = cell.try_borrow() {
                s.status.setStringValue(&text(&message));
                s.status_dot.setFillColor(&NSColor::systemRedColor());
            }
        }
        self.request_query();
    }
    fn set_default(&self, index: isize) {
        let category = if index == 1 { "personal" } else { "work" };
        if let Some(cell) = self.ivars().state.get() {
            if let Ok(s) = cell.try_borrow() {
                let _ = store::open(&s.db).and_then(|c| store::set_default_category(&c, category));
            }
        }
        self.request_query();
    }
    /// One-time question on first launch: which category unassigned usage belongs to.
    fn onboard(&self) {
        let Some(cell) = self.ivars().state.get() else {
            return;
        };
        let db = cell.borrow().db.clone();
        let done = store::open(&db)
            .and_then(|c| store::setting(&c, "onboarded"))
            .map(|v| v.is_some())
            .unwrap_or(true);
        if done {
            return;
        }
        let alert = NSAlert::new(self.mtm());
        alert.setMessageText(ns_string!("How will you mostly use Usage Lens?"));
        alert.setInformativeText(ns_string!(
            "Usage Lens sorts your usage into Work and Personal so you can report on them separately.\n\nEverything it finds starts in the category you pick here. After the first scan, open Projects to move individual projects to Personal. You can change the default there at any time.\n\nThis choice stays on this Mac."
        ));
        alert.addButtonWithTitle(ns_string!("Mostly work"));
        alert.addButtonWithTitle(ns_string!("Mostly personal"));
        let personal = alert.runModal() == 1001;
        let _ = store::open(&db).and_then(|c| {
            store::set_default_category(&c, if personal { "personal" } else { "work" })?;
            store::set_setting(&c, "onboarded", "1")
        });
        self.request_query();
    }
    fn export_csv(&self) {
        let Some(cell) = self.ivars().state.get() else {
            return;
        };
        let (db, filter) = {
            let s = cell.borrow();
            (s.db.clone(), current_filter(&s))
        };
        let panel = NSSavePanel::savePanel(self.mtm());
        panel.setNameFieldStringValue(ns_string!("usage-lens-report.csv"));
        panel.setMessage(Some(ns_string!(
            "The report lists token counts per category, source, project folder and model."
        )));
        if panel.runModal() != 1 {
            return;
        }
        let Some(path) = panel.URL().and_then(|u| u.path()).map(|p| p.to_string()) else {
            return;
        };
        let result = store::open(&db)
            .and_then(|c| store::report_rows(&c, &filter))
            .and_then(|rows| {
                let mut out = String::from(
                    "category,source,project,model,records,uncached_input,cache_read,cache_write,output,total_tokens,estimate_usd\n",
                );
                for r in rows {
                    out.push_str(&format!(
                        "{},{},{},{},{},{},{},{},{},{},{:.4}\n",
                        csv_field(&r.category),
                        csv_field(&r.source),
                        csv_field(&r.project),
                        csv_field(&r.model),
                        r.events,
                        r.uncached,
                        r.cache_read,
                        r.cache_write,
                        r.output,
                        r.uncached + r.cache_read + r.cache_write + r.output,
                        r.estimate
                    ));
                }
                std::fs::write(&path, out).map_err(Into::into)
            });
        if let Ok(s) = cell.try_borrow() {
            let (message, color) = match result {
                Ok(()) => ("Report exported".to_owned(), NSColor::systemGreenColor()),
                Err(e) => (format!("Could not export: {e}"), NSColor::systemRedColor()),
            };
            s.status.setStringValue(&text(&message));
            s.status_dot.setFillColor(&color);
        }
    }
    fn start_update_check(&self, manual: bool) {
        let Some(cell) = self.ivars().state.get() else {
            return;
        };
        {
            let Ok(mut s) = cell.try_borrow_mut() else {
                return;
            };
            if s.checking {
                return;
            }
            s.checking = true;
            s.update_note = "Checking for updates…".into();
            let tx = s.update_tx.clone();
            thread::spawn(move || {
                let _ = tx.send((manual, update::check().map_err(|e| format!("{e:#}"))));
            });
        }
        if cell.try_borrow().is_ok_and(|s| s.page == Page::About) {
            self.render();
        }
    }
    fn finish_update_check(&self, manual: bool, result: Result<Option<Release>, String>) {
        let alert = |title: &str, body: &str, open: Option<&Release>| {
            let alert = NSAlert::new(self.mtm());
            alert.setMessageText(&text(title));
            alert.setInformativeText(&text(body));
            if open.is_some() {
                alert.addButtonWithTitle(ns_string!("Open download page"));
                alert.addButtonWithTitle(ns_string!("Later"));
            }
            if alert.runModal() == 1000 {
                if let Some(r) = open {
                    open_url(&r.url);
                }
            }
        };
        if let Some(cell) = self.ivars().state.get() {
            if let Ok(mut s) = cell.try_borrow_mut() {
                s.checking = false;
                match &result {
                    Ok(Some(r)) => {
                        s.update_available = Some(r.clone());
                        s.update_note = format!("Version {} is available.", r.version);
                    }
                    Ok(None) => {
                        s.update_note = format!("Usage Lens {} is up to date.", update::VERSION);
                    }
                    Err(e) => s.update_note = format!("Could not check: {e}"),
                }
            }
        }
        if manual {
            match &result {
                Ok(Some(r)) => alert(
                    &format!("Usage Lens {} is available", r.version),
                    &if r.notes.is_empty() {
                        format!("You have {}.", update::VERSION)
                    } else {
                        format!("You have {}.\n\n{}", update::VERSION, r.notes)
                    },
                    Some(r),
                ),
                Ok(None) => alert(
                    "You're up to date",
                    &format!("Usage Lens {} is the latest version.", update::VERSION),
                    None,
                ),
                Err(e) => alert("Couldn't check for updates", e, None),
            }
        }
        if self
            .ivars()
            .state
            .get()
            .is_some_and(|c| c.try_borrow().is_ok_and(|s| s.page == Page::About))
        {
            self.render();
        }
    }
    fn start_scan(&self) {
        let Some(cell) = self.ivars().state.get() else {
            return;
        };
        let mut s = cell.borrow_mut();
        if s.scanning {
            return;
        }
        s.scanning = true;
        s.error = None;
        s.last_started = Some(Instant::now());
        s.status
            .setStringValue(ns_string!("Scanning local histories in the background…"));
        s.status_dot.setFillColor(&NSColor::systemBlueColor());
        s.refresh.setEnabled(false);
        s.refresh.setTitle(ns_string!("Refreshing…"));
        let (roots, db, tx) = (s.roots.clone(), s.db.clone(), s.scan_tx.clone());
        thread::spawn(move || {
            let result = collector::collect(&roots, &db).map_err(|e| format!("{e:#}"));
            let _ = tx.send(result);
        });
    }
    fn request_query(&self) {
        let Some(cell) = self.ivars().state.get() else {
            return;
        };
        let mut s = cell.borrow_mut();
        s.generation += 1;
        let source = match s.source.indexOfSelectedItem() {
            1 => "codex",
            2 => "claude",
            _ => "both",
        }
        .to_owned();
        let category = match s.category.indexOfSelectedItem() {
            1 => "work",
            2 => "personal",
            _ => "all",
        }
        .to_owned();
        let seconds = match s.period.indexOfSelectedItem() {
            1 => Some(86_400),
            2 => Some(7 * 86_400),
            3 => Some(30 * 86_400),
            _ => None,
        };
        let filter = Filter {
            source,
            since: seconds.map(|n| chrono::Utc::now().timestamp() - n),
            category,
        };
        let (db, tx, generation) = (s.db.clone(), s.query_tx.clone(), s.generation);
        thread::spawn(move || {
            collector::background_priority();
            let result = store::open(&db)
                .and_then(|c| store::query(&c, &filter))
                .map_err(|e| format!("{e:#}"));
            let _ = tx.send((generation, result));
        });
    }
    fn on_tick(&self) {
        let Some(cell) = self.ivars().state.get() else {
            return;
        };
        let mut query = false;
        let mut render = false;
        let mut refresh = false;
        let mut updates = Vec::new();
        {
            let mut s = cell.borrow_mut();
            while let Ok(message) = s.update_rx.try_recv() {
                updates.push(message);
            }
            while let Ok(result) = s.scan_rx.try_recv() {
                s.scanning = false;
                match result {
                    Ok(scan) => {
                        s.last_updated = Some(Local::now().format("%Y-%m-%d %H:%M:%S").to_string());
                        s.last_scan = Some(scan);
                        s.error = None;
                    }
                    Err(err) => s.error = Some(err),
                }
                query = true;
                render = true;
            }
            while let Ok((generation, result)) = s.query_rx.try_recv() {
                if generation == s.generation {
                    match result {
                        Ok(summary) => {
                            s.summary = Some(summary);
                        }
                        Err(err) => s.error = Some(err),
                    };
                    render = true;
                }
            }
            if !s.scanning
                && s.last_started
                    .is_some_and(|t| t.elapsed() >= Duration::from_secs(60))
            {
                refresh = true;
            }
            if !s.scanning {
                s.refresh.setEnabled(true);
                s.refresh.setTitle(ns_string!("Refresh"));
                let mut healthy = true;
                let status = if let Some(error) = &s.error {
                    healthy = false;
                    format!("Could not refresh: {error}")
                } else if let Some(scan) = &s.last_scan {
                    let mut issues = Vec::new();
                    if !scan.codex_present && !scan.archive_present {
                        issues.push("Codex history unavailable".to_string());
                    }
                    if !scan.claude_present {
                        issues.push("Claude history unavailable".to_string());
                    }
                    if scan.errors > 0 {
                        issues.push(format!("{} files or folders unreadable", scan.errors));
                    }
                    if scan.malformed > 0 {
                        issues.push(format!("{} malformed lines skipped", scan.malformed));
                    }
                    healthy = issues.is_empty();
                    let end = if issues.is_empty() {
                        "Local only".to_string()
                    } else {
                        issues.join(" · ")
                    };
                    format!(
                        "Updated {}  ·  {end}",
                        s.last_updated.as_deref().unwrap_or("just now")
                    )
                } else {
                    "Waiting for first refresh".into()
                };
                let mut status = status;
                if let Some(r) = &s.update_available {
                    status.push_str(&format!("  ·  Update {} available", r.version));
                }
                if s.demo {
                    status = "Demo data  ·  nothing is read from this Mac".into();
                    healthy = true;
                }
                s.status.setStringValue(&text(&status));
                let dot = if s.error.is_some() {
                    NSColor::systemRedColor()
                } else if healthy {
                    NSColor::systemGreenColor()
                } else {
                    NSColor::systemOrangeColor()
                };
                s.status_dot.setFillColor(&dot);
            }
        }
        for (manual, result) in updates {
            self.finish_update_check(manual, result);
        }
        if query {
            self.request_query();
        }
        let may_render = cell.borrow().page != Page::Rates;
        if render && may_render {
            self.render();
        }
        if refresh {
            self.start_scan();
        }
    }
    fn render(&self) {
        self.render_page(false);
    }
    /// Re-layout after a resize without discarding what was typed on the Rates page.
    fn render_keeping_input(&self) {
        self.render_page(true);
    }
    fn render_page(&self, keep_input: bool) {
        let Some(cell) = self.ivars().state.get() else {
            return;
        };
        let Ok(mut s) = cell.try_borrow_mut() else {
            return;
        };
        let mtm = self.mtm();
        s.title.setStringValue(&text(PAGES[s.page.index()].0));
        let filters_visible = !matches!(s.page, Page::Rates | Page::About);
        s.source.setHidden(!filters_visible);
        s.period.setHidden(!filters_visible);
        s.category.setHidden(!filters_visible);
        let mut subtitle = page_subtitle(s.page).to_owned();
        if let Some(data) = s.summary.as_ref() {
            let shown = match s.page {
                Page::Models => data.models.len(),
                Page::Projects => data.projects.len(),
                _ => 0,
            };
            if shown == 500 {
                subtitle.push_str(" · showing the top 500");
            }
        }
        s.subtitle.setStringValue(&text(&subtitle));
        let scroll_top = match (s.rendered_page == Some(s.page), s.list_doc.as_ref()) {
            (true, Some(doc)) => {
                let visible = doc.visibleRect();
                (doc.bounds().size.height - visible.origin.y - visible.size.height).max(0.)
            }
            _ => 0.,
        };
        s.list_doc = None;
        s.rendered_page = Some(s.page);
        let kept: Vec<String> = if keep_input {
            s.rate_fields
                .iter()
                .map(|f| f.stringValue().to_string())
                .collect()
        } else {
            Vec::new()
        };
        let kept_source = if keep_input {
            s.rate_source.as_ref().map(|p| p.indexOfSelectedItem())
        } else {
            None
        };
        while let Some(view) = s.body.subviews().firstObject() {
            view.removeFromSuperview();
        }
        s.rate_fields.clear();
        s.rate_source = None;
        let body = s.body.clone();
        let h = body.bounds().size.height;
        let w = body.bounds().size.width;
        let selected = s.page.index();
        for (i, (b, bg)) in s.nav.iter().zip(s.nav_bg.iter()).enumerate() {
            let on = i == selected;
            let fill = if on {
                NSColor::controlAccentColor().colorWithAlphaComponent(0.16)
            } else {
                NSColor::clearColor()
            };
            bg.setFillColor(&fill);
            b.setFont(Some(&NSFont::systemFontOfSize_weight(
                13.,
                if on { W_SEMIBOLD } else { W_MEDIUM },
            )));
            let tint = if on {
                NSColor::controlAccentColor()
            } else {
                NSColor::secondaryLabelColor()
            };
            b.setContentTintColor(Some(&tint));
        }
        if s.page == Page::Rates {
            self.render_rates(&mut s, &body, h, w, &kept, kept_source);
            return;
        }
        if s.page == Page::About {
            s.project_rows.clear();
            let (note, release, checking, auto) = (
                s.update_note.clone(),
                s.update_available.clone(),
                s.checking,
                s.auto_update,
            );
            self.render_about(&body, h, w, &note, release.as_ref(), checking, auto);
            return;
        }
        let Some(data) = s.summary.as_ref() else {
            s.project_rows.clear();
            put(
                &body,
                mtm,
                "Reading local usage…",
                rect(0., h - 30., w, 20.),
                Sty::new(14., W_REGULAR).secondary(),
            );
            return;
        };
        let mut rows = Vec::new();
        let mut doc = None;
        match s.page {
            Page::Overview => {
                self.render_overview(&body, h, w, data, s.period.indexOfSelectedItem() != 0)
            }
            Page::Insights => self.render_insights(&body, h, w, data),
            Page::Models => {
                doc = self.render_groups(&body, h, w, data, true, scroll_top).1;
            }
            _ => {
                let (r, d) = self.render_groups(&body, h, w, data, false, scroll_top);
                rows = r;
                doc = d;
            }
        }
        s.project_rows = rows;
        s.list_doc = doc;
    }

    // ---- Overview ------------------------------------------------------------------------
    fn render_overview(&self, body: &NSView, h: f64, w: f64, s: &Summary, filtered: bool) {
        let mtm = self.mtm();
        let total = s.uncached + s.cache_read + s.cache_write + s.output;

        // KPI row.
        let kpi_h = 108.;
        let kw = (w - 3. * GAP) / 4.;
        let (cost, cost_note) = if s.events == 0 {
            ("—".to_owned(), "No usage in this period".to_owned())
        } else if s.unpriced == s.events {
            ("Not priced".to_owned(), "Add rates under Rates".to_owned())
        } else if s.unpriced > 0 {
            (
                format!("${}", with_cents(s.estimate)),
                format!("+ {} records unpriced", number(s.unpriced)),
            )
        } else {
            (
                format!("${}", with_cents(s.estimate)),
                "All records priced".to_owned(),
            )
        };
        let kpis = [
            (
                "Total tokens",
                compact_precise(total),
                format!("{} tokens", number(total)),
                NSColor::systemBlueColor(),
            ),
            (
                "Threads / sessions",
                number(s.threads),
                "With recorded usage".to_owned(),
                NSColor::systemPurpleColor(),
            ),
            (
                "Usage records",
                number(s.events),
                "Responses, not user turns".to_owned(),
                NSColor::systemTealColor(),
            ),
            (
                "API estimate",
                cost,
                cost_note,
                NSColor::systemOrangeColor(),
            ),
        ];
        for (i, (name, value, note, color)) in kpis.iter().enumerate() {
            let x = (i as f64) * (kw + GAP);
            let y = h - kpi_h;
            card(body, mtm, rect(x, y, kw, kpi_h));
            dot(body, mtm, x + CARD_PAD - 2., y + kpi_h - 28., 8., color);
            caption(
                body,
                mtm,
                name,
                rect(x + CARD_PAD + 12., y + kpi_h - 31., kw - 2. * CARD_PAD - 12., 14.),
                false,
            );
            put(
                body,
                mtm,
                value,
                rect(x + CARD_PAD, y + 38., kw - 2. * CARD_PAD, 34.),
                Sty::new(30., W_SEMIBOLD).mono(),
            );
            put(
                body,
                mtm,
                &fit(note, kw - 2. * CARD_PAD, 11.),
                rect(x + CARD_PAD, y + 16., kw - 2. * CARD_PAD, 16.),
                Sty::new(11., W_REGULAR).secondary(),
            );
        }

        // Work vs Personal split, independent of the category filter.
        let sh = 118.;
        let sy = h - kpi_h - GAP - sh;
        card(body, mtm, rect(0., sy, w, sh));
        put(
            body,
            mtm,
            "Work vs Personal",
            rect(CARD_PAD, sy + sh - 36., 220., 18.),
            Sty::new(14., W_SEMIBOLD),
        );
        let export = button(
            mtm,
            "Export CSV…",
            rect(w - CARD_PAD - 112., sy + sh - 40., 112., 26.),
            self.as_ref(),
            sel!(exportCsv:),
        );
        export.setImage(symbol("square.and.arrow.up", "Export").as_deref());
        export.setImagePosition(NSCellImagePosition::ImageLeading);
        body.addSubview(&export);
        let split_total: i64 = s.split.iter().map(|x| x.tokens).sum();
        let bar_w = w - 2. * CARD_PAD;
        rounded(
            body,
            mtm,
            rect(CARD_PAD, sy + sh - 62., bar_w, 10.),
            5.,
            &NSColor::quaternaryLabelColor(),
        );
        if split_total > 0 {
            let mut x = CARD_PAD;
            let nonzero = s.split.iter().filter(|x| x.tokens > 0).count() as f64;
            let usable = bar_w - 2. * (nonzero - 1.).max(0.);
            for sp in s.split.iter().filter(|x| x.tokens > 0) {
                let seg = (usable * (sp.tokens as f64) / (split_total as f64)).max(4.);
                rounded(
                    body,
                    mtm,
                    rect(x, sy + sh - 62., seg, 10.),
                    5.,
                    &group_color(&sp.category),
                );
                x += seg + 2.;
            }
        }
        let half = bar_w / 2.;
        for (i, sp) in s.split.iter().enumerate() {
            let x = CARD_PAD + (i as f64) * half;
            dot(body, mtm, x, sy + sh - 85. + 4., 8., &group_color(&sp.category));
            put(
                body,
                mtm,
                &format!(
                    "{}  ·  {}",
                    category_label(&sp.category),
                    percent(sp.tokens, split_total)
                ),
                rect(x + 14., sy + sh - 86., half - 20., 16.),
                Sty::new(12., W_MEDIUM),
            );
            put(
                body,
                mtm,
                &fit(
                    &format!(
                        "{} tokens  ·  {} threads  ·  {}",
                        compact_precise(sp.tokens),
                        number(sp.threads),
                        estimate_text(sp)
                    ),
                    half - 20.,
                    11.,
                ),
                rect(x + 14., sy + sh - 105., half - 20., 15.),
                Sty::new(11., W_REGULAR).secondary().mono(),
            );
        }

        // Token breakdown: a single stacked bar plus a four-column legend.
        let bh = 176.;
        let by = sy - GAP - bh;
        card(body, mtm, rect(0., by, w, bh));
        put(
            body,
            mtm,
            "Token breakdown",
            rect(CARD_PAD, by + bh - 36., 300., 18.),
            Sty::new(14., W_SEMIBOLD),
        );
        put(
            body,
            mtm,
            "Input and output from recorded usage",
            rect(w - CARD_PAD - 320., by + bh - 35., 320., 16.),
            Sty::new(11., W_REGULAR).tertiary().right(),
        );
        let values = [s.uncached, s.cache_read, s.cache_write, s.output];
        let bar_w = w - 2. * CARD_PAD;
        let bar_y = by + bh - 68.;
        rounded(
            body,
            mtm,
            rect(CARD_PAD, bar_y, bar_w, 14.),
            7.,
            &NSColor::quaternaryLabelColor(),
        );
        if total > 0 {
            let mut x = CARD_PAD;
            let nonzero = values.iter().filter(|v| **v > 0).count() as f64;
            let usable = bar_w - 2. * (nonzero - 1.).max(0.);
            for (i, v) in values.iter().enumerate() {
                if *v <= 0 {
                    continue;
                }
                let seg = (usable * (*v as f64) / (total as f64)).max(4.);
                rounded(body, mtm, rect(x, bar_y, seg, 14.), 4., &category_color(i));
                x += seg + 2.;
            }
        }
        let names = ["Uncached input", "Cache reads", "Cache writes", "Output"];
        let cw = bar_w / 4.;
        for (i, name) in names.iter().enumerate() {
            let x = CARD_PAD + (i as f64) * cw;
            dot(body, mtm, x, by + bh - 100. + 4., 8., &category_color(i));
            put(
                body,
                mtm,
                name,
                rect(x + 14., by + bh - 101., cw - 20., 16.),
                Sty::new(12., W_MEDIUM).secondary(),
            );
            put(
                body,
                mtm,
                &number(values[i]),
                rect(x, by + bh - 132., cw - 12., 24.),
                Sty::new(18., W_SEMIBOLD).mono(),
            );
            put(
                body,
                mtm,
                &percent(values[i], total),
                rect(x, by + bh - 152., cw - 12., 16.),
                Sty::new(11., W_REGULAR).secondary().mono(),
            );
        }

        // Notes.
        let mut notes = Vec::new();
        if s.excluded > 0 {
            notes.push(format!(
                "{} records could not be counted.",
                number(s.excluded)
            ));
        }
        if s.incomplete > 0 {
            notes.push(format!(
                "{} records have incomplete fields.",
                number(s.incomplete)
            ));
        }
        if s.conflicting_turns > 0 {
            notes.push(format!(
                "{} Codex turns have conflicting counters; structured usage is shown.",
                number(s.conflicting_turns)
            ));
        }
        if filtered && s.undated > 0 {
            notes.push(format!(
                "{} undated records are excluded from this period.",
                number(s.undated)
            ));
        }
        if s.reasoning > 0 {
            notes.push(format!(
                "Reasoning: {} tokens, included in output where reported.",
                number(s.reasoning)
            ));
        }
        notes.push("Only history collected by this app on this Mac is included. Rates produce API-equivalent estimates, not subscription bills.".into());
        let ny = by - GAP - 112.;
        if ny >= 0. {
            card(body, mtm, rect(0., ny, w, 112.));
            put(
                body,
                mtm,
                "About these numbers",
                rect(CARD_PAD, ny + 112. - 36., 300., 18.),
                Sty::new(14., W_SEMIBOLD),
            );
            let note = NSTextField::wrappingLabelWithString(&text(&notes.join("\n")), mtm);
            note.setFrame(rect(CARD_PAD, ny + 12., w - 2. * CARD_PAD, 112. - 52.));
            note.setFont(Some(&NSFont::systemFontOfSize_weight(12., W_REGULAR)));
            note.setTextColor(Some(&NSColor::secondaryLabelColor()));
            note.setAutoresizingMask(Mask::ViewMinYMargin);
            body.addSubview(&note);
        }
    }

    // ---- Insights ------------------------------------------------------------------------
    fn render_insights(&self, body: &NSView, h: f64, w: f64, s: &Summary) {
        let mtm = self.mtm();
        let daily: Vec<i64> = s.days.iter().map(|d| d.tokens).collect();
        let peak = daily.iter().copied().max().unwrap_or(0);
        let today = daily.last().copied().unwrap_or(0);
        let average = if daily.is_empty() {
            0
        } else {
            daily.iter().sum::<i64>() / daily.len() as i64
        };

        // Daily tokens card.
        let ah = 252.;
        let ay = h - ah;
        card(body, mtm, rect(0., ay, w, ah));
        put(
            body,
            mtm,
            "Daily tokens",
            rect(CARD_PAD, ay + ah - 36., 260., 18.),
            Sty::new(14., W_SEMIBOLD),
        );
        put(
            body,
            mtm,
            "Up to 14 local days within the selected period",
            rect(CARD_PAD, ay + ah - 55., 340., 16.),
            Sty::new(11., W_REGULAR).tertiary(),
        );
        for (i, (name, value)) in [("Peak", peak), ("Average", average), ("Today", today)]
            .iter()
            .enumerate()
        {
            let x = w - CARD_PAD - 3. * 96. + (i as f64) * 96.;
            caption(body, mtm, name, rect(x, ay + ah - 32., 90., 14.), true);
            put(
                body,
                mtm,
                &compact(*value),
                rect(x, ay + ah - 54., 90., 20.),
                Sty::new(16., W_SEMIBOLD).mono().right(),
            );
        }
        let axis_w = 46.;
        let chart_x = CARD_PAD + axis_w;
        let chart_w = w - 2. * CARD_PAD - axis_w;
        let chart_y = ay + 38.;
        let chart_h = ah - 38. - 72.;
        put(
            body,
            mtm,
            &compact(peak),
            rect(CARD_PAD, chart_y + chart_h - 16., axis_w - 8., 14.),
            Sty::new(10., W_REGULAR).tertiary().mono().right(),
        );
        put(
            body,
            mtm,
            &compact(peak / 2),
            rect(CARD_PAD, chart_y + chart_h / 2. - 12., axis_w - 8., 14.),
            Sty::new(10., W_REGULAR).tertiary().mono().right(),
        );
        put(
            body,
            mtm,
            "0",
            rect(CARD_PAD, chart_y - 2., axis_w - 8., 14.),
            Sty::new(10., W_REGULAR).tertiary().mono().right(),
        );
        let trend = ChartView::new(
            mtm,
            rect(chart_x, chart_y, chart_w, chart_h),
            ChartData::Trend(daily.clone()),
        );
        let daily_description = s
            .days
            .iter()
            .map(|day| format!("{}: {} tokens", day.date, number(day.tokens)))
            .collect::<Vec<_>>()
            .join(", ");
        trend.setAccessibilityElement(true);
        trend.setAccessibilityLabel(Some(&text(&format!("Daily usage. {daily_description}"))));
        trend.setAutoresizingMask(Mask::ViewMinYMargin);
        body.addSubview(&trend);
        if let Some((peak_i, _)) = daily.iter().enumerate().max_by_key(|(i, v)| (**v, usize::MAX - *i)) {
            if peak > 0 && daily.len() > 1 {
                let px = chart_x + 10. + (chart_w - 20.) * (peak_i as f64) / ((daily.len() - 1) as f64);
                let py = chart_y + 4. + (chart_h - 12.) * (peak as f64) / (peak.max(1) as f64);
                let (bw, bh) = (124., 24.);
                let bx = (px - bw / 2.).clamp(chart_x, chart_x + chart_w - bw);
                let by = (py - bh - 14.).max(chart_y + 4.);
                rounded(body, mtm, rect(bx, by, bw, bh), bh / 2., &NSColor::systemBlueColor());
                put(
                    body,
                    mtm,
                    &format!("{} · {}", compact(peak), short_date(&s.days[peak_i].date)),
                    rect(bx, by + 4., bw, 16.),
                    Sty::new(11., W_SEMIBOLD).color(NSColor::whiteColor()).center().mono(),
                );
            }
        }
        let start = s
            .days
            .first()
            .map(|d| short_date(&d.date))
            .unwrap_or_default();
        put(
            body,
            mtm,
            &start,
            rect(chart_x, ay + 14., 120., 14.),
            Sty::new(10., W_REGULAR).tertiary(),
        );
        put(
            body,
            mtm,
            "Today",
            rect(chart_x + chart_w - 120., ay + 14., 120., 14.),
            Sty::new(10., W_REGULAR).tertiary().right(),
        );

        // Lower row: token mix + top models.
        let rh = (h - ah - GAP).min(264.);
        let ry = h - ah - GAP - rh;
        let lw = ((w - GAP) * 0.47).floor();
        let rw = w - GAP - lw;
        let values = [s.uncached, s.cache_read, s.cache_write, s.output];
        let total: i64 = values.iter().sum();
        card(body, mtm, rect(0., ry, lw, rh));
        put(
            body,
            mtm,
            "Token mix",
            rect(CARD_PAD, ry + rh - 36., 200., 18.),
            Sty::new(14., W_SEMIBOLD),
        );
        let donut_d = 156.;
        let donut_y = ry + (rh - 44. - donut_d) / 2.;
        let donut = ChartView::new(
            mtm,
            rect(CARD_PAD - 4., donut_y, donut_d, donut_d),
            ChartData::Donut(values),
        );
        donut.setAccessibilityElement(true);
        donut.setAccessibilityLabel(Some(&text(&format!(
            "Token mix: {} uncached input, {} cache reads, {} cache writes, {} output",
            number(values[0]),
            number(values[1]),
            number(values[2]),
            number(values[3])
        ))));
        donut.setAutoresizingMask(Mask::ViewMinYMargin);
        body.addSubview(&donut);
        let cx = CARD_PAD - 4.;
        put(
            body,
            mtm,
            &if total == 0 {
                "No tokens".to_owned()
            } else {
                compact_precise(total)
            },
            rect(cx + 20., donut_y + donut_d / 2. - 4., donut_d - 40., 22.),
            Sty::new(17., W_SEMIBOLD).mono().center(),
        );
        if total > 0 {
            put(
                body,
                mtm,
                "total tokens",
                rect(cx + 20., donut_y + donut_d / 2. - 22., donut_d - 40., 14.),
                Sty::new(10., W_REGULAR).secondary().center(),
            );
        }
        let legend_x = CARD_PAD + donut_d + 14.;
        let legend_w = (lw - legend_x - CARD_PAD).max(80.);
        for (i, name) in ["Uncached input", "Cache reads", "Cache writes", "Output"]
            .iter()
            .enumerate()
        {
            let y = ry + rh - 84. - (i as f64) * 42.;
            dot(body, mtm, legend_x, y + 4., 8., &category_color(i));
            put(
                body,
                mtm,
                name,
                rect(legend_x + 14., y, legend_w - 14., 16.),
                Sty::new(12., W_MEDIUM),
            );
            put(
                body,
                mtm,
                &format!("{}  ·  {}", compact(values[i]), percent(values[i], total)),
                rect(legend_x + 14., y - 17., legend_w - 14., 15.),
                Sty::new(11., W_REGULAR).secondary().mono(),
            );
        }

        let mx = lw + GAP;
        card(body, mtm, rect(mx, ry, rw, rh));
        put(
            body,
            mtm,
            "Top models",
            rect(mx + CARD_PAD, ry + rh - 36., 200., 18.),
            Sty::new(14., W_SEMIBOLD),
        );
        let mut by_model: HashMap<(String, String), i64> = HashMap::new();
        for g in &s.models {
            *by_model
                .entry((g.label.clone(), g.source.clone()))
                .or_default() += group_total(g);
        }
        let mut models: Vec<_> = by_model.into_iter().collect();
        models.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let max = models.first().map(|m| m.1).unwrap_or(0).max(1) as f64;
        if models.is_empty() {
            put(
                body,
                mtm,
                "No usage for this selection.",
                rect(mx + CARD_PAD, ry + rh - 70., rw - 2. * CARD_PAD, 18.),
                Sty::new(12., W_REGULAR).secondary(),
            );
        }
        let inner = rw - 2. * CARD_PAD;
        for (i, ((model, source), tokens)) in models.iter().take(5).enumerate() {
            let y = ry + rh - 78. - (i as f64) * 42.;
            put(
                body,
                mtm,
                &fit(model, inner - 130., 12.),
                rect(mx + CARD_PAD, y, inner - 120., 16.),
                Sty::new(12., W_MEDIUM),
            );
            put(
                body,
                mtm,
                &format!("{}  ·  {}", source_name(source), compact(*tokens)),
                rect(mx + CARD_PAD + inner - 130., y, 130., 16.),
                Sty::new(11., W_REGULAR).secondary().mono().right(),
            );
            meter(
                body,
                mtm,
                mx + CARD_PAD,
                y - 12.,
                inner,
                (*tokens as f64) / max,
                &source_color(source),
            );
        }
    }

    // ---- Models / Projects table ---------------------------------------------------------
    /// Returns the Projects row keys (for the category toggles) and the scrolling list view.
    fn render_groups(
        &self,
        body: &NSView,
        h: f64,
        w: f64,
        data: &Summary,
        models: bool,
        scroll_top: f64,
    ) -> (Vec<(String, String)>, Option<Retained<NSView>>) {
        let mtm = self.mtm();
        let groups = if models { &data.models } else { &data.projects };
        card(body, mtm, rect(0., 0., w, h));
        let cat_w = if models { 0. } else { 150. };
        let cat_gap = if models { 0. } else { 16. };
        let (rec_w, tok_w, src_w) = (84., 120., 70.);
        let bar_w = if w >= 820. {
            150.
        } else {
            0.
        };
        let cat_x = w - CARD_PAD - cat_w;
        let rec_x = cat_x - cat_gap - rec_w;
        let tok_x = rec_x - 16. - tok_w;
        let bar_x = tok_x - 16. - bar_w;
        let src_x = if bar_w > 0. { bar_x } else { tok_x } - 16. - src_w;
        let name_w = src_x - 16. - CARD_PAD;
        // Projects get a banner row: how many follow the default, and the default itself.
        let banner = if models { 0. } else { 52. };
        if !models {
            let explicit = groups
                .iter()
                .filter(|g| {
                    data.project_cats
                        .contains_key(&(g.source.clone(), g.label.clone()))
                })
                .count();
            let following = groups.len() - explicit;
            put(
                body,
                mtm,
                &if following == 0 {
                    "Every project has a category. Use the toggle to move one.".to_owned()
                } else {
                    format!(
                        "{} of {} projects follow your default. Use the toggle to set one explicitly.",
                        number(following as i64),
                        number(groups.len() as i64)
                    )
                },
                rect(CARD_PAD, h - 33., (w - 2. * CARD_PAD - 230.).max(120.), 16.),
                Sty::new(12., W_REGULAR).secondary(),
            );
            put(
                body,
                mtm,
                "New projects:",
                rect(w - CARD_PAD - 230., h - 33., 92., 16.),
                Sty::new(11., W_REGULAR).tertiary().right(),
            );
            let default = popup(
                mtm,
                rect(w - CARD_PAD - 130., h - 38., 130., 26.),
                &["Work", "Personal"],
                self.as_ref(),
                sel!(defaultCategoryChanged:),
            );
            default.selectItemAtIndex(if data.default_category == "personal" { 1 } else { 0 });
            body.addSubview(&default);
            hline(body, mtm, 0., h - banner, w);
        }
        let head_y = h - banner - 32.;
        caption(
            body,
            mtm,
            if models { "Model" } else { "Project" },
            rect(CARD_PAD, head_y, name_w, 14.),
            false,
        );
        caption(body, mtm, "Source", rect(src_x, head_y, src_w, 14.), false);
        if bar_w > 0. {
            caption(body, mtm, "Share", rect(bar_x, head_y, bar_w, 14.), false);
        }
        caption(body, mtm, "Tokens", rect(tok_x, head_y, tok_w, 14.), true);
        caption(body, mtm, "Records", rect(rec_x, head_y, rec_w, 14.), true);
        if !models {
            caption(body, mtm, "Category", rect(cat_x, head_y, cat_w, 14.), false);
        }
        let list_top = h - banner - 44.;
        hline(body, mtm, 0., list_top, w);
        if groups.is_empty() {
            put(
                body,
                mtm,
                "No usage found for this selection.",
                rect(0., list_top / 2., w, 20.),
                Sty::new(13., W_REGULAR).secondary().center(),
            );
            return (Vec::new(), None);
        }
        let row_h = 48.;
        let (doc, doc_h) = scroller(
            body,
            mtm,
            rect(1., 1., w - 2., list_top - 1.),
            (groups.len() as f64) * row_h,
            scroll_top,
        );
        let max = groups.iter().map(group_total).max().unwrap_or(0).max(1) as f64;
        let mut rows = Vec::new();
        for (i, g) in groups.iter().enumerate() {
            let y = doc_h - ((i + 1) as f64) * row_h;
            let tokens = group_total(g);
            if models {
                put(
                    &doc,
                    mtm,
                    &fit(&g.label, name_w, 13.),
                    rect(CARD_PAD - 1., y + 25., name_w, 17.),
                    Sty::new(13., W_MEDIUM),
                );
                let effort = if g.effort.is_empty() || g.effort.eq_ignore_ascii_case("unknown") {
                    "Effort not recorded".to_owned()
                } else {
                    format!("{} effort", g.effort)
                };
                put(
                    &doc,
                    mtm,
                    &effort,
                    rect(CARD_PAD - 1., y + 9., name_w, 14.),
                    Sty::new(11., W_REGULAR).secondary(),
                );
            } else {
                put(
                    &doc,
                    mtm,
                    &fit(&g.label, name_w, 13.),
                    rect(CARD_PAD - 1., y + 15., name_w, 17.),
                    Sty::new(13., W_MEDIUM),
                );
                let key = (g.source.clone(), g.label.clone());
                let current = data
                    .project_cats
                    .get(&key)
                    .unwrap_or(&data.default_category);
                let labels = NSArray::from_retained_slice(&[text("Work"), text("Personal")]);
                let toggle = unsafe {
                    NSSegmentedControl::segmentedControlWithLabels_trackingMode_target_action(
                        &labels,
                        NSSegmentSwitchTracking::SelectOne,
                        Some(self.as_ref()),
                        Some(sel!(categoryChanged:)),
                        mtm,
                    )
                };
                toggle.setFrame(rect(cat_x - 1., y + 12., cat_w, 24.));
                toggle.setSelectedSegment(if current == "personal" { 1 } else { 0 });
                toggle.setTag(rows.len() as isize);
                toggle.setAutoresizingMask(Mask::ViewMinYMargin);
                doc.addSubview(&toggle);
                rows.push(key);
            }
            source_pill(&doc, mtm, src_x - 1., y + 14., &g.source);
            if bar_w > 0. {
                meter(
                    &doc,
                    mtm,
                    bar_x - 1.,
                    y + 21.,
                    bar_w,
                    (tokens as f64) / max,
                    &source_color(&g.source),
                );
            }
            put(
                &doc,
                mtm,
                &number(tokens),
                rect(tok_x - 1., y + 15., tok_w, 17.),
                Sty::new(13., W_MEDIUM).mono().right(),
            );
            put(
                &doc,
                mtm,
                &number(g.events),
                rect(rec_x - 1., y + 15., rec_w, 17.),
                Sty::new(13., W_REGULAR).secondary().mono().right(),
            );
            if i + 1 < groups.len() {
                hline(&doc, mtm, CARD_PAD - 1., y, w - 2. * CARD_PAD);
            }
        }
        (rows, Some(doc))
    }

    // ---- About & updates -----------------------------------------------------------------
    #[allow(clippy::too_many_arguments)]
    fn render_about(
        &self,
        body: &NSView,
        h: f64,
        w: f64,
        note: &str,
        release: Option<&Release>,
        checking: bool,
        auto: bool,
    ) {
        let mtm = self.mtm();
        let ah = 156.;
        let ay = h - ah;
        card(body, mtm, rect(0., ay, w, ah));
        put(
            body,
            mtm,
            "Usage Lens",
            rect(CARD_PAD, ay + ah - 46., 300., 28.),
            Sty::new(22., W_SEMIBOLD),
        );
        put(
            body,
            mtm,
            &format!("Version {}  ·  open source (MIT)", update::VERSION),
            rect(CARD_PAD, ay + ah - 68., 400., 16.),
            Sty::new(12., W_REGULAR).secondary(),
        );
        put(
            body,
            mtm,
            "A private, local view of your Codex and Claude Code usage.",
            rect(CARD_PAD, ay + ah - 92., w - 2. * CARD_PAD, 16.),
            Sty::new(12., W_REGULAR).secondary(),
        );
        let status = if let Some(r) = release {
            format!("Version {} is available", r.version)
        } else {
            note.to_owned()
        };
        put(
            body,
            mtm,
            &fit(&status, w / 2., 12.),
            rect(w / 2., ay + ah - 46., w / 2. - CARD_PAD, 16.),
            Sty::new(12., W_MEDIUM)
                .color(if release.is_some() {
                    NSColor::systemGreenColor()
                } else {
                    NSColor::secondaryLabelColor()
                })
                .right(),
        );
        let mut x = CARD_PAD;
        let check = button(
            mtm,
            if checking { "Checking…" } else { "Check for updates" },
            rect(x, ay + 24., 156., 30.),
            self.as_ref(),
            sel!(checkUpdates:),
        );
        check.setEnabled(!checking);
        body.addSubview(&check);
        x += 156. + 10.;
        if release.is_some() {
            let get = button(
                mtm,
                "Download update",
                rect(x, ay + 24., 150., 30.),
                self.as_ref(),
                sel!(openRelease:),
            );
            get.setKeyEquivalent(ns_string!("\r"));
            body.addSubview(&get);
            x += 150. + 10.;
        }
        let feedback = button(
            mtm,
            "Send feedback…",
            rect(x, ay + 24., 144., 30.),
            self.as_ref(),
            sel!(sendFeedback:),
        );
        feedback.setImage(symbol("bubble.left", "Feedback").as_deref());
        feedback.setImagePosition(NSCellImagePosition::ImageLeading);
        body.addSubview(&feedback);
        x += 144. + 10.;
        let repo = button(
            mtm,
            "View on GitHub",
            rect(x, ay + 24., 140., 30.),
            self.as_ref(),
            sel!(openRepo:),
        );
        body.addSubview(&repo);

        let uh = 104.;
        let uy = ay - GAP - uh;
        card(body, mtm, rect(0., uy, w, uh));
        put(
            body,
            mtm,
            "Updates",
            rect(CARD_PAD, uy + uh - 36., 200., 18.),
            Sty::new(14., W_SEMIBOLD),
        );
        let toggle = unsafe {
            NSButton::checkboxWithTitle_target_action(
                &text("Check for updates automatically when Usage Lens opens"),
                Some(self.as_ref()),
                Some(sel!(autoUpdateToggled:)),
                mtm,
            )
        };
        toggle.setFrame(rect(CARD_PAD - 2., uy + 36., w - 2. * CARD_PAD, 22.));
        toggle.setState(if auto { 1 } else { 0 });
        toggle.setAutoresizingMask(Mask::ViewMinYMargin);
        body.addSubview(&toggle);
        put(
            body,
            mtm,
            "Off by default. A check makes one request to the project's download page and sends only the app version.",
            rect(CARD_PAD + 20., uy + 14., w - 2. * CARD_PAD - 20., 16.),
            Sty::new(11., W_REGULAR).tertiary(),
        );

        let ph = (uy - GAP).min(188.);
        if ph < 120. {
            return;
        }
        let py = uy - GAP - ph;
        card(body, mtm, rect(0., py, w, ph));
        put(
            body,
            mtm,
            "Privacy",
            rect(CARD_PAD, py + ph - 36., 200., 18.),
            Sty::new(14., W_SEMIBOLD),
        );
        let lines = [
            "Usage history is read from files on this Mac and summarised in a local database. Prompts and responses are not stored.",
            "There is no analytics or tracking, and no account.",
            "The only network access is the update check above, and only when you ask for it or turn on automatic checks.",
            "Send feedback opens a pre-filled GitHub issue in your browser with this app's and macOS's version. Nothing is sent until you submit it.",
        ];
        let text_view = NSTextField::wrappingLabelWithString(&text(&lines.join("\n")), mtm);
        text_view.setFrame(rect(CARD_PAD, py + 14., w - 2. * CARD_PAD, ph - 56.));
        text_view.setFont(Some(&NSFont::systemFontOfSize_weight(12., W_REGULAR)));
        text_view.setTextColor(Some(&NSColor::secondaryLabelColor()));
        text_view.setAutoresizingMask(Mask::ViewMinYMargin);
        body.addSubview(&text_view);
    }

    // ---- Rates ---------------------------------------------------------------------------
    fn render_rates(
        &self,
        s: &mut State,
        body: &NSView,
        h: f64,
        w: f64,
        kept: &[String],
        kept_source: Option<isize>,
    ) {
        let mtm = self.mtm();
        // Form card.
        let fh = 206.;
        let fy = h - fh;
        card(body, mtm, rect(0., fy, w, fh));
        put(
            body,
            mtm,
            "Add or update a rate",
            rect(CARD_PAD, fy + fh - 36., 300., 18.),
            Sty::new(14., W_SEMIBOLD),
        );
        put(
            body,
            mtm,
            "USD per million tokens, for an exact recorded model name. Saving the same source and model again updates it.",
            rect(CARD_PAD, fy + fh - 55., w - 2. * CARD_PAD, 16.),
            Sty::new(11., W_REGULAR).tertiary(),
        );
        let row1_label = fy + fh - 80.;
        let row1 = fy + fh - 110.;
        caption(body, mtm, "Source", rect(CARD_PAD, row1_label, 120., 14.), false);
        let rate_source = popup(
            mtm,
            rect(CARD_PAD, row1, 124., 26.),
            &["Codex", "Claude"],
            self.as_ref(),
            sel!(rateSourceChanged:),
        );
        unsafe {
            rate_source.setAction(None);
        }
        if let Some(i) = kept_source {
            rate_source.selectItemAtIndex(i);
        }
        body.addSubview(&rate_source);
        s.rate_source = Some(rate_source);

        let model_x = CARD_PAD + 124. + 14.;
        let inner = w - 2. * CARD_PAD;
        let save_w = 110.;
        let n = 4.;
        let fw = (inner - save_w - n * 14.) / n;
        let mut fields: Vec<(&str, f64, f64, f64, &str)> = vec![(
            "Model name",
            model_x,
            w - CARD_PAD - model_x,
            row1,
            "Exact recorded model name",
        )];
        let row2_label = fy + fh - 136.;
        let row2 = fy + fh - 166.;
        for (i, name) in ["Input", "Cache read", "Cache write", "Output"]
            .iter()
            .enumerate()
        {
            fields.push((name, CARD_PAD + (i as f64) * (fw + 14.), fw, row2, "0.00"));
        }
        for (i, (name, x, width, y, placeholder)) in fields.iter().enumerate() {
            let label_y = if i == 0 { row1_label } else { row2_label };
            caption(body, mtm, name, rect(*x, label_y, *width, 14.), false);
            let field = NSTextField::textFieldWithString(ns_string!(""), mtm);
            field.setFrame(rect(*x, *y, *width, 26.));
            field.setPlaceholderString(Some(&text(placeholder)));
            if let Some(v) = kept.get(i) {
                field.setStringValue(&text(v));
            }
            field.setAutoresizingMask(Mask::ViewMinYMargin);
            body.addSubview(&field);
            s.rate_fields.push(field);
        }
        let save = button(
            mtm,
            "Save rate",
            rect(w - CARD_PAD - save_w, row2 - 1., save_w, 28.),
            self.as_ref(),
            sel!(saveRate:),
        );
        save.setKeyEquivalent(ns_string!("\r"));
        body.addSubview(&save);

        // Saved rates card.
        let ly_h = h - fh - GAP;
        if ly_h < 110. {
            return;
        }
        card(body, mtm, rect(0., 0., w, ly_h));
        put(
            body,
            mtm,
            "Saved rates",
            rect(CARD_PAD, ly_h - 36., 200., 18.),
            Sty::new(14., W_SEMIBOLD),
        );
        put(
            body,
            mtm,
            "USD / 1M tokens",
            rect(w - CARD_PAD - 200., ly_h - 35., 200., 16.),
            Sty::new(11., W_REGULAR).tertiary().right(),
        );
        let col = 88.;
        let x_out = w - CARD_PAD - col;
        let x_write = x_out - col;
        let x_read = x_write - col;
        let x_in = x_read - col;
        let model_w = x_in - 16. - (CARD_PAD + 74.);
        let head_y = ly_h - 70.;
        caption(body, mtm, "Source", rect(CARD_PAD, head_y, 70., 14.), false);
        caption(body, mtm, "Model", rect(CARD_PAD + 74., head_y, model_w, 14.), false);
        for (name, x) in [
            ("Input", x_in),
            ("Cache read", x_read),
            ("Cache write", x_write),
            ("Output", x_out),
        ] {
            caption(body, mtm, name, rect(x, head_y, col, 14.), true);
        }
        hline(body, mtm, 0., head_y - 10., w);
        let list_h = head_y - 11.;
        match store::open(&s.db).and_then(|c| store::rates(&c)) {
            Ok(rates) if rates.is_empty() => {
                put(
                    body,
                    mtm,
                    "No rates saved. All usage is currently unpriced.",
                    rect(0., list_h / 2. - 10., w, 20.),
                    Sty::new(13., W_REGULAR).secondary().center(),
                );
            }
            Ok(rates) => {
                let row_h = 38.;
                let (doc, doc_h) = scroller(
                    body,
                    mtm,
                    rect(1., 1., w - 2., list_h - 1.),
                    (rates.len() as f64) * row_h,
                    0.,
                );
                for (i, r) in rates.iter().enumerate() {
                    let y = doc_h - ((i + 1) as f64) * row_h;
                    source_pill(&doc, mtm, CARD_PAD - 1., y + 9., &r.source);
                    put(
                        &doc,
                        mtm,
                        &fit(&r.model, model_w, 13.),
                        rect(CARD_PAD + 73., y + 10., model_w, 17.),
                        Sty::new(13., W_MEDIUM),
                    );
                    for (v, x) in [
                        (r.input, x_in),
                        (r.cache_read, x_read),
                        (r.cache_write, x_write),
                        (r.output, x_out),
                    ] {
                        put(
                            &doc,
                            mtm,
                            &format!("{v:.2}"),
                            rect(x - 1., y + 10., col, 17.),
                            Sty::new(13., W_REGULAR).mono().right(),
                        );
                    }
                    if i + 1 < rates.len() {
                        hline(&doc, mtm, CARD_PAD - 1., y, w - 2. * CARD_PAD);
                    }
                }
            }
            Err(_) => {
                put(
                    body,
                    mtm,
                    "Rates are unavailable until the local database can be opened.",
                    rect(0., list_h / 2. - 10., w, 20.),
                    Sty::new(13., W_REGULAR).secondary().center(),
                );
            }
        }
    }
    fn save_rate(&self) {
        let Some(cell) = self.ivars().state.get() else {
            return;
        };
        let mut s = cell.borrow_mut();
        if s.rate_fields.len() != 5 {
            return;
        }
        let values: Vec<String> = s
            .rate_fields
            .iter()
            .map(|x| x.stringValue().to_string())
            .collect();
        let parsed: Option<Vec<f64>> = values[1..]
            .iter()
            .map(|v| v.trim().parse::<f64>().ok())
            .collect();
        let result = if let Some(v) = parsed {
            let source = if s
                .rate_source
                .as_ref()
                .is_some_and(|p| p.indexOfSelectedItem() == 1)
            {
                "claude"
            } else {
                "codex"
            };
            store::open(&s.db).and_then(|c| {
                store::save_rate(
                    &c,
                    &Rate {
                        source: source.into(),
                        model: values[0].clone(),
                        input: v[0],
                        cache_read: v[1],
                        cache_write: v[2],
                        output: v[3],
                    },
                )
            })
        } else {
            Err(anyhow::anyhow!("Enter all four rates as numbers."))
        };
        match result {
            Ok(()) => {
                s.error = None;
                drop(s);
                self.render();
                self.request_query();
            }
            Err(e) => {
                s.status
                    .setStringValue(&text(&format!("Could not save rate: {e}")));
                s.status_dot.setFillColor(&NSColor::systemRedColor());
                s.error = Some(e.to_string());
            }
        }
    }
}
fn open_url(url: &str) {
    // Only ever called with links built by this app.
    let _ = std::process::Command::new("/usr/bin/open").arg(url).spawn();
}
fn current_filter(s: &State) -> Filter {
    Filter {
        source: match s.source.indexOfSelectedItem() {
            1 => "codex",
            2 => "claude",
            _ => "both",
        }
        .into(),
        since: match s.period.indexOfSelectedItem() {
            1 => Some(86_400),
            2 => Some(7 * 86_400),
            3 => Some(30 * 86_400),
            _ => None,
        }
        .map(|n| chrono::Utc::now().timestamp() - n),
        category: match s.category.indexOfSelectedItem() {
            1 => "work",
            2 => "personal",
            _ => "all",
        }
        .into(),
    }
}
fn with_cents(v: f64) -> String {
    let whole = v.trunc() as i64;
    format!("{}.{:02}", number(whole), ((v.fract() * 100.).round() as i64).clamp(0, 99))
}

pub fn run() -> anyhow::Result<()> {
    let mtm = MainThreadMarker::new()
        .ok_or_else(|| anyhow::anyhow!("AppKit must run on the main thread"))?;
    let app = NSApplication::sharedApplication(mtm);
    let delegate = Delegate::new(mtm);
    app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    app.run();
    Ok(())
}
