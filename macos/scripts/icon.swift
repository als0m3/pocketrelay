import AppKit
let directory = CommandLine.arguments[1]
for size in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let pixels = size * scale
        let image = NSImage(size: NSSize(width: pixels, height: pixels))
        image.lockFocus()
        let rect = NSRect(x: 0, y: 0, width: pixels, height: pixels)
        NSColor(calibratedRed: 0.09, green: 0.10, blue: 0.14, alpha: 1).setFill()
        NSBezierPath(roundedRect: rect.insetBy(dx: CGFloat(pixels) * 0.06, dy: CGFloat(pixels) * 0.06), xRadius: CGFloat(pixels) * 0.21, yRadius: CGFloat(pixels) * 0.21).fill()
        let text = ">_" as NSString
        let attrs: [NSAttributedString.Key: Any] = [.font: NSFont.monospacedSystemFont(ofSize: CGFloat(pixels) * 0.49, weight: .medium), .foregroundColor: NSColor(calibratedRed: 0.72, green: 0.75, blue: 1, alpha: 1)]
        let bounds = text.size(withAttributes: attrs)
        text.draw(at: NSPoint(x: (CGFloat(pixels) - bounds.width) / 2, y: (CGFloat(pixels) - bounds.height) / 2), withAttributes: attrs)
        image.unlockFocus()
        let bitmap = NSBitmapImageRep(data: image.tiffRepresentation!)!
        try bitmap.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: directory + "/icon_\(size)x\(size)\(scale == 2 ? "@2x" : "").png"))
    }
}
