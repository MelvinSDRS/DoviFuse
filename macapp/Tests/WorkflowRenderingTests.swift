import SwiftUI
import AppKit

// Native, off-screen review artifacts; never starts a converter or opens media.
@main
struct WorkflowRenderingTests {
    @MainActor static func main() throws {
        _ = NSApplication.shared
        let output = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
        try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
        let suite = "DV8Render-" + UUID().uuidString
        let preferences = UserDefaults(suiteName: suite)!
        defer { preferences.removePersistentDomain(forName: suite) }
        let model = AppModel(preferences: preferences)
        model.mode = .hybrid
        model.dvSource = URL(fileURLWithPath: "/Example/Film.P8.mkv")
        model.hdrTarget = URL(fileURLWithPath: "/Example/Film.HDR10.mkv")
        try save(ContentView(model: model), width: 900, height: 630, to: output.appendingPathComponent("hybrid.png"))
        try save(ContentView(model: model), width: 760, height: 540, to: output.appendingPathComponent("hybrid-small.png"))
        model.saveEnhancementLayer = true
        try model.rememberArchiveFolder(output)
        try save(SettingsView(model: model), width: 540, height: 600, to: output.appendingPathComponent("archive-settings.png"))
        print("Rendered hybrid and archive settings:", output.path)
    }

    @MainActor static func save<V: View>(_ view: V, width: Double, height: Double, to url: URL) throws {
        // ImageRenderer cannot draw AppKit-backed Form/Picker/drop destinations.
        let hosting = NSHostingView(rootView: view.environment(\.colorScheme, .light))
        let window = NSWindow(contentRect: NSRect(x: -10000, y: -10000, width: width, height: height), styleMask: [.borderless], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = hosting
        hosting.frame = NSRect(x: 0, y: 0, width: width, height: height)
        hosting.layoutSubtreeIfNeeded()
        window.displayIfNeeded()
        RunLoop.current.run(until: Date(timeIntervalSinceNow: 0.2))
        hosting.layoutSubtreeIfNeeded()
        defer { window.close() }
        guard let bitmap = hosting.bitmapImageRepForCachingDisplay(in: hosting.bounds) else {
            fatalError("Could not render workflow")
        }
        hosting.cacheDisplay(in: hosting.bounds, to: bitmap)
        guard let data = bitmap.representation(using: .png, properties: [:]) else { fatalError("Could not encode render") }
        try data.write(to: url, options: .withoutOverwriting)
    }
}
