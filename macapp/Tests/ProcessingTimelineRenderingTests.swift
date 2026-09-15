import AppKit
import SwiftUI

// Keep this test independent from AppModel and the converter process.
enum ConversionMode: Equatable {
    case hybrid, standard, checker
}

@main
struct ProcessingTimelineRenderingTests {
    @MainActor
    static func main() throws {
        _ = NSApplication.shared

        let rootView = OrganicProcessingField(mode: .hybrid, progress: 0.54, active: true)
            .environment(\.scenePhase, .active)
            .frame(width: 760, height: 360)
        let hosting = NSHostingView(rootView: rootView)
        let window = NSWindow(contentRect: NSRect(x: -10000, y: -10000, width: 760, height: 360),
                              styleMask: [.borderless], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = hosting
        hosting.frame = NSRect(x: 0, y: 0, width: 760, height: 360)
        window.orderFrontRegardless()
        window.displayIfNeeded()
        hosting.layoutSubtreeIfNeeded()

        // Give TimelineView its initial frame, then sample again without
        // hover, pointer, layout, or model-progress events.
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.15))
        let first = try snapshot(of: hosting)
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.30))
        let second = try snapshot(of: hosting)
        window.close()

        precondition(first != second,
                     "TimelineView/Canvas did not render a new frame without hover input")
        print("Passed: processing canvas advances without hover input")
    }

    @MainActor
    private static func snapshot<V: View>(of hosting: NSHostingView<V>) throws -> Data {
        hosting.layoutSubtreeIfNeeded()
        guard let bitmap = hosting.bitmapImageRepForCachingDisplay(in: hosting.bounds) else {
            throw NSError(domain: "ProcessingTimelineRenderingTests", code: 1,
                          userInfo: [NSLocalizedDescriptionKey: "Could not capture processing canvas"])
        }
        hosting.cacheDisplay(in: hosting.bounds, to: bitmap)
        guard let data = bitmap.representation(using: .png, properties: [:]) else {
            throw NSError(domain: "ProcessingTimelineRenderingTests", code: 1,
                          userInfo: [NSLocalizedDescriptionKey: "Could not encode processing canvas"])
        }
        return data
    }
}
