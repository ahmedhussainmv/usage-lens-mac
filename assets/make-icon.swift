// Regenerate AppIcon.icns on macOS. This is a build-time asset tool, not part of the app.
import AppKit
import Foundation

let destination = CommandLine.arguments.dropFirst().first ?? "AppIcon.iconset"
try FileManager.default.createDirectory(atPath: destination, withIntermediateDirectories: true)
let outputs: [(String, Int)] = [
    ("icon_16x16.png",16), ("icon_16x16@2x.png",32), ("icon_32x32.png",32), ("icon_32x32@2x.png",64),
    ("icon_128x128.png",128), ("icon_128x128@2x.png",256), ("icon_256x256.png",256), ("icon_256x256@2x.png",512),
    ("icon_512x512.png",512), ("icon_512x512@2x.png",1024)
]
for (filename, pixels) in outputs {
    guard let bitmap = NSBitmapImageRep(bitmapDataPlanes:nil, pixelsWide:pixels, pixelsHigh:pixels, bitsPerSample:8,
        samplesPerPixel:4, hasAlpha:true, isPlanar:false, colorSpaceName:.deviceRGB, bytesPerRow:0, bitsPerPixel:0),
        let context = NSGraphicsContext(bitmapImageRep:bitmap) else { fatalError("Bitmap context unavailable") }
    NSGraphicsContext.saveGraphicsState(); NSGraphicsContext.current=context
    let transform=NSAffineTransform(); transform.scale(by:CGFloat(pixels)/1024); transform.concat()
    let background=NSBezierPath(roundedRect:NSRect(x:35,y:35,width:954,height:954),xRadius:220,yRadius:220)
    NSGradient(starting:NSColor(calibratedRed:0.08,green:0.13,blue:0.38,alpha:1),
               ending:NSColor(calibratedRed:0.06,green:0.36,blue:0.70,alpha:1))!.draw(in:background,angle:315)
    let center=NSPoint(x:512,y:528), radius:CGFloat=288
    let arcs:[(CGFloat,CGFloat,NSColor)] = [
        (90,-26,NSColor(calibratedRed:0.10,green:0.85,blue:0.92,alpha:1)),
        (-30,-130,NSColor(calibratedRed:0.69,green:0.30,blue:0.95,alpha:1)),
        (-134,-205,NSColor(calibratedRed:1.00,green:0.61,blue:0.22,alpha:1)),
        (-209,-266,NSColor(calibratedRed:0.35,green:0.66,blue:1.00,alpha:1))
    ]
    for (start,end,color) in arcs {
        let arc=NSBezierPath();arc.lineWidth=84;arc.lineCapStyle = .round
        arc.appendArc(withCenter:center,radius:radius,startAngle:start,endAngle:end,clockwise:true)
        color.setStroke();arc.stroke()
    }
    let bars:[(CGFloat,CGFloat,NSColor)] = [
        (375,152,NSColor(calibratedRed:0.42,green:0.72,blue:1,alpha:1)),
        (466,247,NSColor(calibratedRed:0.23,green:0.90,blue:0.88,alpha:1)),
        (557,198,NSColor(calibratedRed:0.88,green:0.52,blue:1,alpha:1))
    ]
    for (x,height,color) in bars {
        color.setFill();NSBezierPath(roundedRect:NSRect(x:x,y:391,width:62,height:height),xRadius:30,yRadius:30).fill()
    }
    NSGraphicsContext.restoreGraphicsState()
    guard let png=bitmap.representation(using:.png,properties:[:]) else {fatalError("PNG encoding failed")}
    try png.write(to:URL(fileURLWithPath:destination).appendingPathComponent(filename))
}
