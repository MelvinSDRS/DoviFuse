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
        // The scenePhase environment does not activate an AppKit process. Keep
        // TimelineView's animation clock live on headless CI runners.
        _ = NSApp.setActivationPolicy(.accessory)
        NSApp.finishLaunching()
        NSApp.activate(ignoringOtherApps: true)
        let reduceMotion = NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
        precondition(!reduceMotion,
                     "TimelineView animation test requires Reduce Motion disabled (com.apple.Accessibility ReduceMotionEnabled=false)")

        let rootView = OrganicProcessingField(mode: .hybrid, progress: 0.54, active: true)
            .environment(\.scenePhase, .active)
            .frame(width: 760, height: 360)
        let hosting = NSHostingView(rootView: rootView)
        // Keep the hosting view onscreen so the window server schedules it on CI.
        let window = NSWindow(contentRect: NSRect(x: 100, y: 100, width: 760, height: 360),
                              styleMask: [.borderless], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        // The assertion must remain independent of pointer or hover events.
        window.ignoresMouseEvents = true
        window.contentView = hosting
        hosting.frame = NSRect(x: 0, y: 0, width: 760, height: 360)
        window.makeKeyAndOrderFront(nil)
        window.displayIfNeeded()
        hosting.layoutSubtreeIfNeeded()

        // Let AppKit and TimelineView establish their initial frame, then
        // sample again after a bounded period without hover, pointer, or model events.
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 0.50))
        let first = try snapshot(of: hosting)
        RunLoop.main.run(until: Date(timeIntervalSinceNow: 1.50))
        let second = try snapshot(of: hosting)
        let diagnostics = "appActive=\(NSApp.isActive), windowVisible=\(window.isVisible), "
            + "screenCount=\(NSScreen.screens.count), reduceMotion=\(reduceMotion)"
        window.close()

        precondition(first != second,
                     "TimelineView/Canvas did not render a new frame without hover input (\(diagnostics))")
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
