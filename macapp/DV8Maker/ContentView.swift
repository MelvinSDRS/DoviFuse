import SwiftUI
import UniformTypeIdentifiers

struct ContentView: View {
    @ObservedObject var model: AppModel
    @State private var presentedSheet: PresentedSheet?
    @State private var showPaddingReview = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        VStack(spacing: 0) {
            modePicker
                .padding(.top, 22)

            ZStack {
                if model.isRunning || model.outputURL != nil || model.errorMessage != nil {
                    ProcessingExperience(model: model)
                        .transition(.opacity.combined(with: .scale(scale: 0.98)))
                } else {
                    InputCanvas(model: model)
                        .transition(.opacity.combined(with: .scale(scale: 1.02)))
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)

            bottomBar
                .padding(.horizontal, 26)
                .padding(.bottom, 22)
        }
        .background {
            if !showingProcessing {
                LinearGradient(
                    colors: [Color(nsColor: .windowBackgroundColor), Color.accentColor.opacity(0.035)],
                    startPoint: .top,
                    endPoint: .bottomTrailing
                )
                .ignoresSafeArea()
            }
        }
        .animation(.spring(response: 0.55, dampingFraction: 0.86), value: model.isRunning)
        .animation(.spring(response: 0.55, dampingFraction: 0.86), value: model.outputURL)
        .animation(.spring(response: 0.55, dampingFraction: 0.86), value: model.errorMessage)
        .transaction { if reduceMotion { $0.animation = nil } }
        .sheet(item: $presentedSheet) { sheet in
            switch sheet {
            case .logs:
                LogView(log: model.log)
            }
        }
        .task {
            if model.scratchURL == nil && model.mode.needsScratch {
                model.chooseScratchFolder()
            }
        }
        .onChange(of: model.mode) {
            if !model.isRunning { model.resetStatus() }
        }
    }

    private var showingProcessing: Bool {
        model.isRunning || model.outputURL != nil || model.errorMessage != nil
    }

    private var modePicker: some View {
        Picker("Mode", selection: $model.mode) {
            ForEach(ConversionMode.allCases) { mode in
                Text(mode.rawValue).tag(mode)
            }
        }
        .labelsHidden()
        .pickerStyle(.segmented)
        .frame(width: 460)
        .disabled(model.isRunning)
        .accessibilityLabel("Conversion mode")
    }

    private var bottomBar: some View {
        HStack(spacing: 14) {
            Button {
                presentedSheet = .logs
            } label: {
                Label("Logs", systemImage: "doc.text")
            }
            .buttonStyle(.plain)
            .foregroundStyle(.secondary)
            .opacity(model.log.isEmpty ? 0.45 : 1)
            .accessibilityHint("Shows detailed conversion output")

            Spacer()

            if model.isRunning {
                Button("Cancel", role: .destructive) {
                    model.cancel()
                }
                .buttonStyle(.borderless)
            } else if model.outputURL != nil || model.errorMessage != nil {
                if model.canFix {
                    Button {
                        showPaddingReview = true
                    } label: {
                        Label("Fix", systemImage: "wrench.and.screwdriver")
                    }
                    .buttonStyle(PrimaryActionButtonStyle())
                    .accessibilityHint("Review edge padding before creating a repaired copy")
                    .alert("Repair with repeated edge metadata?", isPresented: $showPaddingReview) {
                        Button("Repair with Padding") { model.fix() }
                        Button("Cancel", role: .cancel) { }
                    } message: {
                        Text(model.repairPaddingDetail)
                    }

                    Button("New Check") {
                        model.resetStatus()
                    }
                    .buttonStyle(.bordered)
                } else {
                    Button(model.mode == .checker ? "New Check" : "New Conversion") {
                        model.resetStatus()
                    }
                    .buttonStyle(PrimaryActionButtonStyle())
                }
            } else {
                Button(model.mode == .checker ? "Check File" : "Start") {
                    model.run()
                }
                .buttonStyle(PrimaryActionButtonStyle())
                .disabled(!model.canRun)
                .opacity(model.canRun ? 1 : 0.34)
                .keyboardShortcut(.defaultAction)
            }
        }
        .frame(minHeight: 42)
    }
}

private enum PresentedSheet: String, Identifiable {
    case logs
    var id: String { rawValue }
}

private struct InputCanvas: View {
    @ObservedObject var model: AppModel

    var body: some View {
        VStack(spacing: 18) {
            Spacer(minLength: 18)

            if model.mode.usesPair {
                HStack(spacing: 18) {
                    DropZone(
                        title: "Dolby Vision Profile 7 / 8",
                        subtitle: "Metadata source — Profile 5 unsupported",
                        url: model.dvSource
                    ) { model.accept($0, role: "dv") }

                    DropZone(
                        title: "HDR10 REMUX",
                        subtitle: "Video and audio target",
                        url: model.hdrTarget
                    ) { model.accept($0, role: "hdr") }
                }
            } else if model.mode == .standard {
                DropZone(
                    title: "Dolby Vision Profile 7",
                    subtitle: "Drop the MKV to convert",
                    url: model.standardSource
                ) { model.accept($0, role: "standard") }
                .frame(maxWidth: 430)
                Text("MEL/FEL is checked before conversion. FEL picture detail is discarded; the result uses the HDR10 base. Archive settings control whether the original enhancement layer is saved separately.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: 520)
            } else {
                DropZone(
                    title: "DV8 Movie",
                    subtitle: "Drop any Profile 8 MKV to verify",
                    url: model.checkerSource
                ) { model.accept($0, role: "checker") }
                .frame(maxWidth: 520)
            }

            statusHint
                .frame(height: 24)

            Spacer(minLength: 20)
        }
        .padding(.horizontal, 34)
    }

    @ViewBuilder
    private var statusHint: some View {
        if let error = model.validationError {
            Text(error)
                .font(.callout)
                .foregroundStyle(.secondary)
                .contentTransition(.opacity)
        } else if model.mode == .hybrid {
            Label("Ready — both source files will be kept", systemImage: "checkmark.circle.fill")
                .font(.callout)
                .foregroundStyle(.green)
                .transition(.opacity.combined(with: .scale(scale: 0.96)))
        } else if model.mode == .standard {
            Label("Ready — the validated DV8 replaces the original", systemImage: "exclamationmark.triangle.fill")
                .font(.callout)
                .foregroundStyle(.orange)
                .transition(.opacity.combined(with: .scale(scale: 0.96)))
        } else {
            Label("Ready — the file will not be modified", systemImage: "checkmark.shield.fill")
                .font(.callout)
                .foregroundStyle(.green)
                .transition(.opacity.combined(with: .scale(scale: 0.96)))
        }
    }
}

private struct DropZone: View {
    let title: String
    let subtitle: String
    let url: URL?
    let onDropURL: (URL) -> Void
    @State private var targeted = false

    var body: some View {
        VStack(spacing: 18) {
            ZStack {
                Circle()
                    .fill(iconTint.opacity(url == nil ? 0.08 : 0.13))
                    .frame(width: 62, height: 62)

                Image(systemName: url == nil ? "arrow.down.doc" : "checkmark")
                    .font(.system(size: 25, weight: .medium))
                    .foregroundStyle(iconTint)
                    .rotationEffect(.degrees(targeted ? -4 : 0))
            }

            VStack(spacing: 6) {
                Text(title)
                    .font(.title3.weight(.semibold))

                Text(url?.lastPathComponent ?? subtitle)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
                    .multilineTextAlignment(.center)
                    .contentTransition(.opacity)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .frame(minHeight: 245)
        .padding(24)
        .background {
            RoundedRectangle(cornerRadius: 22, style: .continuous)
                .fill(Color(nsColor: .controlBackgroundColor).opacity(targeted ? 0.88 : 0.58))
                .shadow(color: .black.opacity(targeted ? 0.09 : 0.035), radius: targeted ? 18 : 10, y: 5)
        }
        .overlay {
            RoundedRectangle(cornerRadius: 22, style: .continuous)
                .strokeBorder(
                    targeted ? Color.accentColor : Color.primary.opacity(0.10),
                    style: StrokeStyle(lineWidth: targeted ? 2 : 1, dash: url == nil ? [8, 7] : [])
                )
        }
        .scaleEffect(targeted ? 1.018 : 1)
        .animation(.spring(response: 0.32, dampingFraction: 0.72), value: targeted)
        .animation(.spring(response: 0.45, dampingFraction: 0.78), value: url)
        .onDrop(of: [UTType.fileURL], isTargeted: $targeted) { providers in
            guard let provider = providers.first else { return false }
            provider.loadDataRepresentation(forTypeIdentifier: UTType.fileURL.identifier) { data, _ in
                guard let data, let url = URL(dataRepresentation: data, relativeTo: nil) else { return }
                Task { @MainActor in onDropURL(url) }
            }
            return true
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(title) drop zone")
    }

    private var iconTint: Color {
        if targeted { return .accentColor }
        return url == nil ? Color(nsColor: .secondaryLabelColor) : .green
    }
}

struct CheckResultsView: View {
    let results: [CheckResult]

    var body: some View {
        ScrollView {
            VStack(spacing: 8) {
                ForEach(results) { result in
                    HStack(spacing: 11) {
                        Image(systemName: icon(for: result.status))
                            .foregroundStyle(color(for: result.status))
                            .font(.system(size: 15, weight: .semibold))
                            .contentTransition(.symbolEffect(.replace))

                        VStack(alignment: .leading, spacing: 2) {
                            Text(result.label)
                                .font(.callout.weight(.medium))
                            Text(result.detail)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .lineLimit(2)
                        }

                        Spacer(minLength: 8)
                    }
                    .padding(.horizontal, 14)
                    .padding(.vertical, 9)
                    .background(Color.primary.opacity(0.035), in: RoundedRectangle(cornerRadius: 11))
                    .transition(.move(edge: .bottom).combined(with: .opacity))
                }
            }
            .animation(.spring(response: 0.4, dampingFraction: 0.82), value: results.count)
        }
    }

    private func icon(for status: String) -> String {
        switch status {
        case "pass": "checkmark.circle.fill"
        case "warn": "exclamationmark.triangle.fill"
        case "skipped": "minus.circle.fill"
        case "fail": "xmark.circle.fill"
        default: "questionmark.circle.fill"
        }
    }

    private func color(for status: String) -> Color {
        switch status {
        case "pass": .green
        case "warn": .orange
        case "skipped": .secondary
        case "fail": .red
        default: .orange
        }
    }
}

private struct LogView: View {
    let log: String
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 16) {
            HStack {
                Text("Conversion Log")
                    .font(.title2.weight(.semibold))
                Spacer()
                Button("Done") { dismiss() }
                    .keyboardShortcut(.defaultAction)
            }

            ScrollView {
                Text(log.isEmpty ? "No log output yet." : log)
                    .font(.system(.caption, design: .monospaced))
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .padding(12)
            .background(Color(nsColor: .textBackgroundColor), in: RoundedRectangle(cornerRadius: 10))
        }
        .padding(22)
        .frame(minWidth: 680, minHeight: 420)
    }
}

private struct PrimaryActionButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.headline)
            .foregroundStyle(.white)
            .padding(.horizontal, 34)
            .frame(height: 42)
            .background(Color.accentColor, in: Capsule())
            .shadow(color: Color.accentColor.opacity(configuration.isPressed ? 0.12 : 0.25), radius: 10, y: 4)
            .scaleEffect(configuration.isPressed ? 0.97 : 1)
            .opacity(configuration.isPressed ? 0.86 : 1)
            .animation(.spring(response: 0.24, dampingFraction: 0.72), value: configuration.isPressed)
    }
}
