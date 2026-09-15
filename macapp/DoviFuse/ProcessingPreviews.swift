#if DEBUG
import SwiftUI

@MainActor
enum ProcessingFixtures {
    static func model(mode: ConversionMode = .hybrid, finished: Bool = false, failure: Bool = false) -> AppModel {
        let model = AppModel()
        model.mode = mode
        let source = URL(fileURLWithPath: "/Preview/The.Long.Way.Home.2160p.mkv")
        model.hdrTarget = source
        model.standardSource = source
        model.checkerSource = source
        model.scratchURL = URL(fileURLWithPath: "/Preview/Scratch")
        model.isRunning = !finished && !failure
        model.progress = finished ? 1 : 0.54
        model.elapsed = 183
        model.phase = finished ? "Completed" : "Aligning Dolby Vision metadata"
        if finished { model.outputURL = source.deletingPathExtension().appendingPathExtension("DV8.Hybrid.mkv") }
        if failure {
            model.phase = "Failed"
            model.errorMessage = "The selected scratch volume is no longer available. Reconnect it and try again."
        }
        if mode == .checker {
            model.phase = finished ? "Completed with warnings" : "Checking metadata integrity"
            model.checkResults = [
                CheckResult(id: "profile", label: "Dolby Vision profile", status: "pass", detail: "Profile 8.1, HDR10 compatible", fixAction: nil, fixValue: nil),
                CheckResult(id: "sync", label: "Metadata alignment", status: "warn", detail: "Review the reported alignment before playback.", fixAction: nil, fixValue: nil)
            ]
        }
        return model
    }
}

private struct ProcessingPreview: View {
    @StateObject var model: AppModel

    var body: some View {
        ProcessingExperience(model: model)
            .frame(width: 960, height: 620)
    }
}

#Preview("Hybrid · Processing") { ProcessingPreview(model: ProcessingFixtures.model()) }
#Preview("Profile 8 · Complete") { ProcessingPreview(model: ProcessingFixtures.model(mode: .standard, finished: true)) }
#Preview("Checker · Review results") { ProcessingPreview(model: ProcessingFixtures.model(mode: .checker, finished: true)) }
#Preview("Stopped · Recoverable error") { ProcessingPreview(model: ProcessingFixtures.model(failure: true)) }
#endif
