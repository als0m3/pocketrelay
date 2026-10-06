import Foundation
import PDFKit
import AppKit

// Implements only the PDF commands used by content.rs, using macOS system frameworks.
// Docker/native Linux continue to use Poppler without any backend changes.
func fail(_ message: String) -> Never { fputs(message + "\n", stderr); exit(1) }
let args = Array(CommandLine.arguments.dropFirst())
let tool = URL(fileURLWithPath: CommandLine.arguments[0]).lastPathComponent
if args == ["-v"] { print("PocketRelay PDFKit adapter 1.0"); exit(0) }
var path: String
switch tool {
case "pdfinfo":
    guard args.count == 1 else { fail("Expected: pdfinfo FILE") }; path = args[0]
case "pdftotext":
    guard args.count == 3, args[0] == "-layout", args[2] == "-" else { fail("Expected: pdftotext -layout FILE -") }; path = args[1]
case "pdftoppm":
    guard args.count == 10, args[0] == "-f", args[2] == "-l", args[4] == "-scale-to", args[6] == "-singlefile", args[7] == "-png", args[1] == args[3] else { fail("Unsupported PDF render arguments") }; path = args[8]
default: fail("Unknown PDF adapter name")
}
guard let document = PDFDocument(url: URL(fileURLWithPath: path)), !document.isLocked else { fail("Unreadable or password-protected PDF") }
switch tool {
case "pdfinfo": print("Pages: \(document.pageCount)")
case "pdftotext":
    guard document.pageCount <= 500 else { fail("PDF exceeds the page limit") }
    var remaining = 400_000
    for index in 0..<document.pageCount {
        let text = String((document.page(at: index)?.string ?? "").prefix(remaining))
        remaining -= text.count
        FileHandle.standardOutput.write(Data((text + "\u{0c}").utf8))
    }
case "pdftoppm":
    guard let number = Int(args[1]), number > 0, number <= document.pageCount,
          let longest = Int(args[5]), (1...4096).contains(longest), let page = document.page(at: number - 1) else { fail("Invalid page or size") }
    let bounds = page.bounds(for: .mediaBox)
    guard bounds.width > 0, bounds.height > 0, bounds.width.isFinite, bounds.height.isFinite else { fail("Dimensions PDF invalides") }
    // PDFKit handles rotations and the page's crop/coordinate transforms.
    let thumbnail = page.thumbnail(of: NSSize(width: longest, height: longest), for: .mediaBox)
    guard let tiff = thumbnail.tiffRepresentation, let bitmap = NSBitmapImageRep(data: tiff),
          let png = bitmap.representation(using: .png, properties: [:]) else { fail("Cannot render the PDF page") }
    do { try png.write(to: URL(fileURLWithPath: args[9] + ".png")) }
    catch { fail(error.localizedDescription) }
default: break
}
