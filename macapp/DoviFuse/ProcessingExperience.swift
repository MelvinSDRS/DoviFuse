import SwiftUI

struct ProcessingExperience: View {
    @ObservedObject var model: AppModel
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    private var fraction: Double? {
        guard let value = model.progress, value.isFinite else { return nil }
        return min(max(value, 0), 1)
    }

    private var finished: Bool { !model.isRunning && model.outputURL != nil }
    private var active: Bool { model.isRunning && model.outputURL == nil && model.errorMessage == nil && model.phase != "Cancelling…" }
    private var palette: ProcessingRibbonPalette { .forMode(model.mode) }

    private var accent: Color {
        if model.errorMessage != nil { return .orange }
        if finished && (model.mode == .checker || !model.checkResults.isEmpty) {
            switch model.checkerOutcome {
            case .warnings, .inconclusive: return .orange
            case .failures: return .red
            case .passed: return palette.primary
            }
        }
        return palette.primary
    }

    var body: some View {
        VStack(spacing: 0) {
            HStack(alignment: .top) {
                VStack(alignment: .leading, spacing: 5) {
                    Text(model.mode.experienceTitle.uppercased())
                        .font(.system(size: 11, weight: .semibold, design: .rounded))
                        .tracking(3)
                        .foregroundStyle(accent)
                    Text(model.selectedInput?.lastPathComponent ?? "DoviFuse")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                        .help(model.selectedInput?.lastPathComponent ?? "DoviFuse")
                }
                Spacer(minLength: 24)
                Label(model.isRunning ? "LIVE" : (finished ? "FINISHED" : "STOPPED"),
                      systemImage: model.isRunning ? "waveform" : (finished ? "checkmark.circle" : "pause.circle"))
                    .font(.system(size: 10, weight: .semibold, design: .monospaced))
                    .tracking(1.5)
                    .foregroundStyle(accent)
            }
            .padding(.top, 24)

            GeometryReader { geometry in
                let side = ProcessingRibbonPalette.coreSize(height: geometry.size.height)
                ZStack {
                    OrganicProcessingField(mode: model.mode, progress: fraction ?? 0, active: active)
                        .padding(.horizontal, -32)

                    Text("DoviFuse")
                        .font(.system(size: side * 0.28, weight: .semibold))
                        .lineLimit(1)
                        .minimumScaleFactor(0.4)
                        .frame(width: side * 0.84)
                        .foregroundStyle(LinearGradient(colors: [palette.text, palette.highlight],
                                                        startPoint: .leading, endPoint: .trailing))
                        .accessibilityHidden(true)
                }
            }
            .frame(minHeight: 150, maxHeight: .infinity)

            VStack(spacing: 12) {
                Text(model.phase)
                    .font(.system(size: 23, weight: .medium, design: .rounded))
                    .multilineTextAlignment(.center)
                    .contentTransition(.opacity)
                    .animation(reduceMotion ? nil : .easeInOut(duration: 0.3), value: model.phase)

                HStack(spacing: 10) {
                    if let fraction {
                        Text(fraction, format: .percent.precision(.fractionLength(0)))
                            .fontWeight(.medium)
                            .foregroundStyle(accent)
                            .monospacedDigit()
                            .contentTransition(.numericText())
                            .animation(reduceMotion ? nil : .smooth(duration: 0.65), value: fraction)
                            .accessibilityLabel("Overall progress")
                            .accessibilityValue("\(Int((fraction * 100).rounded())) percent")
                        Text("·")
                    }
                    Text(model.mode.experienceDetail)
                    Text("·")
                    Text(Duration.seconds(model.elapsed), format: .time(pattern: .hourMinuteSecond))
                        .monospacedDigit()
                }
                .font(.caption)
                .foregroundStyle(.secondary)

                if !model.checkResults.isEmpty {
                    CheckResultsView(results: model.checkResults)
                        .frame(maxWidth: 600)
                        .frame(height: 105)
                }

                if let error = model.errorMessage {
                    ScrollView {
                        Text(error)
                            .font(.callout)
                            .foregroundStyle(.orange)
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                    .frame(maxHeight: 70)
                }

                HStack(spacing: 20) {
                    if model.outputURL != nil {
                        Button("Show in Finder", systemImage: "folder") { model.revealOutput() }
                    }
                    if model.reportURL != nil {
                        Button("Show report", systemImage: "doc.text") { model.revealReport() }
                    }
                }
                .buttonStyle(.borderless)
                .tint(accent)
            }
            .padding(.bottom, 20)
        }
        .padding(.horizontal, 36)
    }
}

extension ConversionMode {
    var experienceTitle: String {
        switch self {
        case .hybrid: "Hybrid synthesis"
        case .standard: "Dolby Vision conversion"
        case .checker: "Dolby Vision inspection"
        }
    }

    var experienceDetail: String {
        switch self {
        case .hybrid: "Two sources. One vision."
        case .standard: "Profile 7 → Profile 8"
        case .checker: "Metadata · Sync · Integrity"
        }
    }
}
