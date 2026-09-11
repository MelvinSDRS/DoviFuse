import SwiftUI
import AppKit

@main
struct ProcessingRibbonRenderingTests {
    @MainActor static func main() {
        for mode in ConversionMode.allCases {
            for height in [150, 360] {
                let width = 760
                let view = Canvas(opaque: false) { context, size in
                    ProcessingRibbons(size: size, time: 3.2, progress: 0.54, pointer: .zero, mode: mode)
                        .draw(context: &context)
                }.frame(width: Double(width), height: Double(height))
                let renderer = ImageRenderer(content: view)
                renderer.scale = 1
                renderer.isOpaque = false
                guard let image = renderer.cgImage else { fatalError("Could not render ribbons") }
                var pixels = [UInt8](repeating: 0, count: width * height * 4)
                pixels.withUnsafeMutableBytes { data in
                    let bitmap = CGContext(data: data.baseAddress, width: width, height: height,
                                           bitsPerComponent: 8, bytesPerRow: width * 4,
                                           space: CGColorSpaceCreateDeviceRGB(),
                                           bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue | CGBitmapInfo.byteOrder32Big.rawValue)!
                    bitmap.draw(image, in: CGRect(x: 0, y: 0, width: width, height: height))
                }
                for (x, y) in [(0,0),(width/2,0),(width-1,0),(0,height-1),(width/2,height-1),(width-1,height-1)] {
                    precondition(pixels[(y * width + x) * 4 + 3] == 0, "Background must stay transparent: \(mode), \(height)")
                }
                precondition(pixels[((height/2) * width + width/2) * 4 + 3] == 255, "The central object must render")
                print("Passed: \(mode.rawValue), \(width)×\(height), transparent edges and rendered core")
            }
        }
    }
}
