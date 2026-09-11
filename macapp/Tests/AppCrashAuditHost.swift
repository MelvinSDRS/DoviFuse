import AppKit

// An isolated native app host for the production AppModel. The audit supervisor
// kills this host, never the user's installed application or a library job.
@main
struct AppCrashAuditHost {
    @MainActor
    static func main() throws {
        let root = URL(fileURLWithPath: ProcessInfo.processInfo.environment["DV8_NATIVE_AUDIT_ROOT"]!)
        let preferences = UserDefaults(suiteName: "DV8CrashAudit-" + root.lastPathComponent)!
        let model = AppModel(preferences: preferences)
        model.mode = .standard
        model.hardware = .off
        model.saveEnhancementLayer = false
        model.standardSource = root.appendingPathComponent("source.mkv")
        model.scratchURL = root.appendingPathComponent("scratch")
        let app = NSApplication.shared
        app.setActivationPolicy(.accessory)
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 320, height: 100),
                              styleMask: [.titled], backing: .buffered, defer: false)
        window.title = "DV8 isolated interruption audit"
        window.orderFront(nil)
        if CommandLine.arguments.contains("--run") {
            model.run()
            guard model.isRunning else { throw NSError(domain: model.errorMessage ?? "Job did not start", code: 1) }
        } else {
            let state: [String: Any] = ["isRunning": model.isRunning, "phase": model.phase,
                                       "hasOutput": model.outputURL != nil, "hasReport": model.reportURL != nil]
            try JSONSerialization.data(withJSONObject: state).write(to: root.appendingPathComponent("restart.json"))
        }
        withExtendedLifetime(model) { app.run() }
    }
}
