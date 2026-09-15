import AppKit
import Foundation
import Darwin

enum ConversionMode: String, CaseIterable, Identifiable {
    case hybrid = "Hybrid"
    case standard = "DV7 → DV8"
    case checker = "Checker"
    var id: String { rawValue }
    var usesPair: Bool { self == .hybrid }
    var needsScratch: Bool { self == .hybrid || self == .standard }
}

enum HardwareMode: String, CaseIterable, Identifiable {
    case auto, videotoolbox, off
    var id: String { rawValue }
    var label: String { self == .videotoolbox ? "VideoToolbox" : rawValue.capitalized }
}

struct ProgressEvent: Decodable {
    let version: Int?
    let event: String
    let mode: String?
    let execution: String?
    let report: String?
    let index: Int?
    let total: Int?
    let label: String?
    let fraction: Double?
    let level: String?
    let text: String?
    let output: String?
    let error: String?
    let key: String?
    let status: String?
    let detail: String?
    let fix_action: String?
    let fix_value: Int?
}

struct CheckResult: Identifiable {
    let id: String
    let label: String
    let status: String
    let detail: String
    let fixAction: String?
    let fixValue: Int?
}

private enum RunningOperation {
    case conversion
    case checker
    case repair
}

enum CheckerOutcome {
    case passed
    case inconclusive
    case warnings
    case failures
}

@MainActor
final class AppModel: ObservableObject {
    @Published var mode: ConversionMode = .hybrid
    @Published var dvSource: URL?
    @Published var hdrTarget: URL?
    @Published var standardSource: URL?
    @Published var checkerSource: URL?
    @Published var scratchURL: URL?
    @Published private(set) var archiveURL: URL?
    @Published var saveEnhancementLayer = false {
        didSet { preferences.set(saveEnhancementLayer, forKey: archiveEnabledKey) }
    }
    @Published var hardware: HardwareMode = .auto {
        didSet { preferences.set(hardware.rawValue, forKey: hardwareKey) }
    }
    @Published var isRunning = false
    @Published var phase = "Ready"
    @Published var progress: Double?
    @Published var log = ""
    @Published var outputURL: URL?
    @Published private(set) var reportURL: URL?
    @Published var elapsed: TimeInterval = 0
    @Published var errorMessage: String?
    @Published var checkResults: [CheckResult] = []
    @Published private(set) var lastRunWasRepair = false

    private var process: Process?
    private var runID = UUID()
    private var outputEOF = Set<String>()
    private var exitStatus: Int32?
    private var receivedCompletion = false
    private var reportStatus: String?
    private var expectedReportURL: URL?
    private var checkingIdentity: InputIdentity?
    private var checkedIdentity: InputIdentity?
    private var runningOperation: RunningOperation?
    private var stdoutBuffer = Data()
    private var timerTask: Task<Void, Never>?
    private var phaseIndex = 0
    private var phaseTotal = 1
    private let bookmarkKey = "scratchBookmark"
    private let hardwareKey = "hardwareMode"
    private let archiveEnabledKey = "saveEnhancementLayer"
    private let archiveBookmarkKey = "archiveBookmark"
    private let supportDirectoryName = "DoviFuse"
    private let preferences: UserDefaults

    init(preferences: UserDefaults = .standard) {
        self.preferences = preferences
        // Previous app versions always passed -n; keep that default on upgrade.
        saveEnhancementLayer = preferences.bool(forKey: archiveEnabledKey)
        if let raw = preferences.string(forKey: hardwareKey),
           let saved = HardwareMode(rawValue: raw) {
            hardware = saved
        }
        restoreScratchBookmark()
        if let data = preferences.data(forKey: archiveBookmarkKey) {
            var stale = false
            if let url = try? URL(resolvingBookmarkData: data, options: [.withSecurityScope], relativeTo: nil, bookmarkDataIsStale: &stale), !stale {
                archiveURL = url
                _ = url.startAccessingSecurityScopedResource()
            }
        }
    }

    var selectedInput: URL? {
        switch mode {
        case .hybrid: hdrTarget
        case .standard: standardSource
        case .checker: checkerSource
        }
    }

    var predictedOutput: URL? {
        guard let target = selectedInput else { return nil }
        if mode == .standard || mode == .checker { return target }
        return target.deletingPathExtension()
            .appendingPathExtension("DV8.Hybrid.mkv")
    }

    var validationError: String? {
        if mode.needsScratch, scratchURL == nil { return "Choose a scratch folder on the external SSD." }
        if mode.usesPair {
            guard let dvSource, let hdrTarget else { return "Drop both source files." }
            if dvSource.standardizedFileURL == hdrTarget.standardizedFileURL { return "The two inputs must be different files." }
            if !isMKV(dvSource) || !isMKV(hdrTarget) { return "Both inputs must be MKV files." }
        } else if mode == .standard {
            guard let standardSource else { return "Drop a DV7 MKV file." }
            if !isMKV(standardSource) { return "The input must be an MKV file." }
            if let archiveError { return archiveError }
        } else {
            guard let checkerSource else { return "Drop a DV8 MKV file to check." }
            if !isMKV(checkerSource) { return "The input must be an MKV file." }
        }
        return scratchSpaceError()
    }

    var canRun: Bool { !isRunning && validationError == nil }

    var canFix: Bool {
        !isRunning && mode == .checker && checkerSource != nil && fixableResult != nil && !hasUnresolvedChecks && checkedIdentity != nil && checkedIdentity == checkerSource.flatMap(InputIdentity.init)
    }

    private var hasUnresolvedChecks: Bool {
        checkResults.contains { !["pass", "warn", "skipped", "fail"].contains($0.status) }
    }

    var checkerOutcome: CheckerOutcome {
        if reportStatus == "fail" || checkResults.contains(where: { $0.status == "fail" }) { return .failures }
        if reportStatus == "inconclusive" || checkResults.isEmpty || hasUnresolvedChecks { return .inconclusive }
        if reportStatus == "warn" || checkResults.contains(where: { $0.status == "warn" || $0.status == "skipped" }) {
            return .warnings
        }
        return .passed
    }

    func accept(_ url: URL, role: String) {
        guard !isRunning else { return }
        guard isMKV(url) else { errorMessage = "Please drop an MKV file."; return }
        if role != "dv" && role != "hdr" && role != "standard" {
            resetStatus()
        }
        switch role {
        case "dv": dvSource = url
        case "hdr": hdrTarget = url
        case "standard": standardSource = url
        default: checkerSource = url
        }
        errorMessage = nil
    }

    func chooseScratchFolder() {
        let panel = NSOpenPanel()
        panel.title = "Choose External SSD Scratch Folder"
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.canCreateDirectories = true
        panel.allowsMultipleSelection = false
        guard panel.runModal() == .OK, let url = panel.url else { return }
        do {
            let data = try url.bookmarkData(options: [.withSecurityScope], includingResourceValuesForKeys: nil, relativeTo: nil)
            preferences.set(data, forKey: bookmarkKey)
            scratchURL = url
            _ = url.startAccessingSecurityScopedResource()
        } catch { errorMessage = "Could not remember scratch folder: \(error.localizedDescription)" }
    }

    var archiveError: String? {
        guard saveEnhancementLayer else { return nil }
        guard let archiveURL else { return "Choose an enhancement-layer archive folder in Settings." }
        var directory: ObjCBool = false
        guard FileManager.default.fileExists(atPath: archiveURL.path, isDirectory: &directory), directory.boolValue,
              FileManager.default.isWritableFile(atPath: archiveURL.path) else {
            return "The enhancement-layer archive folder is unavailable or not writable."
        }
        return nil
    }

    var standardArchiveArguments: [String] {
        guard saveEnhancementLayer else { return ["-n"] }
        guard let archiveURL else { return [] } // validationError blocks launch.
        return ["--archive-dir", archiveURL.path]
    }

    func rememberArchiveFolder(_ url: URL) throws {
        let data = try url.bookmarkData(options: [.withSecurityScope], includingResourceValuesForKeys: nil, relativeTo: nil)
        archiveURL?.stopAccessingSecurityScopedResource()
        preferences.set(data, forKey: archiveBookmarkKey)
        archiveURL = url
        _ = url.startAccessingSecurityScopedResource()
    }

    func chooseArchiveFolder() {
        let panel = NSOpenPanel()
        panel.title = "Choose Enhancement-Layer Archive Folder"
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.canCreateDirectories = true
        panel.allowsMultipleSelection = false
        guard panel.runModal() == .OK, let url = panel.url else { return }
        do { try rememberArchiveFolder(url) }
        catch { errorMessage = "Could not remember archive folder: \(error.localizedDescription)" }
    }

    func run() {
        guard canRun else { return }
        launch(args: jobArguments, operation: mode == .checker ? .checker : .conversion)
    }

    var jobArguments: [String] {
        var args = ["--hwaccel", hardware.rawValue, "--progress", "jsonl"]
        if mode.needsScratch, let scratchURL {
            args = ["--tmp-dir", scratchURL.path] + args
        }
        if mode == .hybrid, let dvSource, let hdrTarget {
            args += ["--hybrid", dvSource.path, hdrTarget.path]
        } else if mode == .standard, let standardSource {
            args += standardArchiveArguments + [standardSource.path]
        } else if let checkerSource {
            args += ["--check", checkerSource.path]
        }

        return args
    }

    var repairPaddingDetail: String {
        let frames = fixableResult?.fixValue?.magnitude ?? 0
        return "This repair repeats edge metadata for \(frames) frames. Those pictures remain unverified. The original file is kept."
    }

    func fix() {
        guard canFix, let checkerSource, let fixableResult,
              fixableResult.fixAction == "sync-offset", let offset = fixableResult.fixValue else {
            return
        }
        if scratchURL == nil { chooseScratchFolder() }
        guard let scratchURL else { return }

        let args = [
            "--tmp-dir", scratchURL.path,
            "--hwaccel", hardware.rawValue,
            "--progress", "jsonl",
            "--repair-sync", String(offset), "--allow-padding",
            checkerSource.path
        ]
        launch(args: args, operation: .repair)
    }

    private func launch(
        args: [String],
        operation: RunningOperation
    ) {
        let resources = Bundle.main.resourceURL!
        let executable = resources.appendingPathComponent("tools/dovifuse_converter")
        guard FileManager.default.isExecutableFile(atPath: executable.path) else {
            errorMessage = "The bundled converter is missing. Rebuild the app package."
            return
        }

        let support = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
            .appendingPathComponent(supportDirectoryName, isDirectory: true)
        let logs = support.appendingPathComponent("Logs", isDirectory: true)
        let reports = support.appendingPathComponent("Reports", isDirectory: true)
        do {
            try FileManager.default.createDirectory(at: logs, withIntermediateDirectories: true)
            try FileManager.default.createDirectory(at: reports, withIntermediateDirectories: true)
        } catch {
            errorMessage = "Could not create the report folder: \(error.localizedDescription)"
            return
        }
        let report = reports.appendingPathComponent(UUID().uuidString + ".json")

        let process = Process()
        process.executableURL = executable
        process.arguments = ["--report", report.path] + args
        var environment = ProcessInfo.processInfo.environment
        environment["PATH"] = "/usr/bin:/bin:/usr/sbin:/sbin"
        environment["DOVIFUSE_SCRIPT_DIR"] = resources.path
        environment["DOVIFUSE_PROCESSING_LOG_FILE"] = logs.appendingPathComponent("processing_log.txt").path
        process.environment = environment
        let stdout = Pipe(), stderr = Pipe()
        process.standardOutput = stdout
        process.standardError = stderr
        let id = UUID()
        process.terminationHandler = { [weak self] process in
            let status = process.terminationStatus
            DispatchQueue.main.async { self?.receiveExit(status, id: id) }
        }

        checkingIdentity = operation == .checker ? checkerSource.flatMap(InputIdentity.init) : nil
        do {
            try process.run()
            beginOutputTracking(id: id, report: report)
            checkedIdentity = nil
            self.process = process
            runningOperation = operation
            isRunning = true
            phase = "Starting…"
            progress = nil
            log = ""
            outputURL = nil
            errorMessage = nil
            lastRunWasRepair = false
            stdoutBuffer.removeAll(keepingCapacity: true)
            // A repair's new checks replace the donor report, including failures.
            checkResults = []
            readPipe(stdout.fileHandleForReading, stream: "stdout", id: id)
            readPipe(stderr.fileHandleForReading, stream: "stderr", id: id)
            elapsed = 0
            let started = Date()
            timerTask = Task { [weak self] in
                while !Task.isCancelled {
                    try? await Task.sleep(for: .seconds(1))
                    self?.elapsed = Date().timeIntervalSince(started)
                }
            }
        } catch { errorMessage = "Could not start operation: \(error.localizedDescription)" }
    }

    func cancel() {
        guard let process, process.isRunning else { return }
        phase = "Cancelling…"
        process.terminate()
    }

    func revealReport() {
        guard let reportURL else { return }
        NSWorkspace.shared.activateFileViewerSelecting([reportURL])
    }

    func revealOutput() {
        guard let outputURL else { return }
        NSWorkspace.shared.activateFileViewerSelecting([outputURL])
    }

    func resetStatus() {
        outputURL = nil
        reportURL = nil
        reportStatus = nil
        checkedIdentity = nil
        errorMessage = nil
        phase = "Ready"
        progress = nil
        elapsed = 0
        checkResults = []
        lastRunWasRepair = false
        runningOperation = nil
    }

    private func consume(_ data: Data) {
        stdoutBuffer.append(data)
        while let newline = stdoutBuffer.firstIndex(of: 0x0A) {
            let line = stdoutBuffer.prefix(upTo: newline)
            stdoutBuffer.removeSubrange(...newline)
            guard !line.isEmpty else { continue }
            if let event = try? JSONDecoder().decode(ProgressEvent.self, from: line) { apply(event) }
            else if let text = String(data: line, encoding: .utf8) { appendLog(text + "\n") }
        }
    }

    private func apply(_ event: ProgressEvent) {
        switch event.event {
        case "phase_started":
            phase = event.label ?? "Working…"
            if let index = event.index, let total = event.total, total > 0 {
                phaseIndex = max(0, index - 1)
                phaseTotal = total
                progress = min(Double(phaseIndex) / Double(total), 0.98)
            }
        case "phase_progress":
            if let local = event.fraction, phaseTotal > 0 {
                progress = min((Double(phaseIndex) + local) / Double(phaseTotal), 0.99)
            }
        case "message": if let text = event.text { appendLog(text + "\n") }
        case "check_result":
            if let key = event.key, let label = event.label,
               let status = event.status, let detail = event.detail {
                let result = CheckResult(
                    id: key,
                    label: label,
                    status: status,
                    detail: detail,
                    fixAction: event.fix_action,
                    fixValue: event.fix_value
                )
                if let index = checkResults.firstIndex(where: { $0.id == key }) {
                    checkResults[index] = result
                } else {
                    checkResults.append(result)
                }
            }
        case "completed":
            let repaired = runningOperation == .repair
            if let output = event.output {
                let url = URL(fileURLWithPath: output)
                outputURL = url
                if repaired { checkerSource = url }
            }
            lastRunWasRepair = repaired
            receivedCompletion = true
        case "job_finalized":
            if let path = event.report, URL(fileURLWithPath: path) == expectedReportURL {
                reportURL = expectedReportURL
                reportStatus = event.status
                appendLog("Report: " + path + "\n")
            }
        case "failed":
            lastRunWasRepair = runningOperation == .repair
            errorMessage = event.error
            phase = lastRunWasRepair ? "Fix failed" : "Failed"
        case "cancelled": phase = "Cancelled"
        default: break
        }
    }

    // Both pipe readers enqueue data and EOF on the main queue in stream order.
    // Process exit may arrive first; it cannot finalize the UI until both EOFs.
    private func readPipe(_ handle: FileHandle, stream: String, id: UUID) {
        DispatchQueue.global(qos: .utility).async { [weak self] in
            while true {
                do {
                    guard let data = try handle.read(upToCount: 65_536), !data.isEmpty else { break }
                    DispatchQueue.main.async { [weak self] in self?.receiveOutput(data, stream: stream, id: id) }
                } catch {
                    let message = error.localizedDescription
                    DispatchQueue.main.async { [weak self] in
                        guard let self, self.runID == id else { return }
                        self.errorMessage = "Could not finish reading converter output: \(message)"
                    }
                    break
                }
            }
            try? handle.close()
            DispatchQueue.main.async { [weak self] in self?.receiveEOF(stream, id: id) }
        }
    }

    func beginOutputTracking(id: UUID, report: URL?) {
        runID = id
        outputEOF = []
        exitStatus = nil
        receivedCompletion = false
        reportStatus = nil
        reportURL = nil
        expectedReportURL = report
    }

    func receiveOutput(_ data: Data, stream: String, id: UUID) {
        guard runID == id else { return }
        if stream == "stdout" { consume(data) }
        else { appendLog(String(decoding: data, as: UTF8.self)) }
    }

    func receiveEOF(_ stream: String, id: UUID) {
        guard runID == id else { return }
        if stream == "stdout", !stdoutBuffer.isEmpty { consume(Data([0x0A])) }
        outputEOF.insert(stream)
        finishIfDrained()
    }

    func receiveExit(_ status: Int32, id: UUID) {
        guard runID == id else { return }
        exitStatus = status
        finishIfDrained()
    }

    func bindCheckedInput(_ identity: InputIdentity?) { checkedIdentity = identity }

    private func finishIfDrained() {
        guard let status = exitStatus, outputEOF == ["stdout", "stderr"] else { return }
        timerTask?.cancel(); timerTask = nil
        process = nil; isRunning = false
        let repaired = runningOperation == .repair
        if let expectedReportURL {
            guard reportURL == expectedReportURL,
                  let data = try? Data(contentsOf: expectedReportURL),
                  let saved = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
                  saved["schema_version"] as? Int == 1,
                  saved["validation"] as? String == reportStatus,
                  saved["execution"] as? String == (phase == "Cancelled" ? "cancelled" : status == 0 ? "completed" : "failed") else {
                phase = "Failed"
                errorMessage = "The saved report is missing or disagrees with the converter result."
                checkedIdentity = nil
                return
            }
        }
        if phase == "Cancelled" { checkedIdentity = nil; return }
        if status != 0 || errorMessage != nil {
            lastRunWasRepair = repaired
            phase = repaired ? "Fix failed" : "Failed"
            if errorMessage == nil { errorMessage = "Operation exited with status \(status). See the log for details." }
        } else if !receivedCompletion || (expectedReportURL != nil && reportURL == nil) {
            phase = "Failed"
            errorMessage = "Converter exited without a complete result and saved report."
        } else {
            phase = repaired ? repairCompletionTitle : completionTitle
            progress = 1
        }
        // A checker may fail solely because of a repairable offset. Preserve
        // that action only for the exact unchanged filesystem identity checked.
        if reportURL != nil, let checkingIdentity,
           checkingIdentity == checkerSource.flatMap(InputIdentity.init) {
            checkedIdentity = checkingIdentity
        }
    }

    private func appendLog(_ text: String) {
        log += text
        if log.count > 200_000 { log = String(log.suffix(150_000)) }
    }
    private func isMKV(_ url: URL) -> Bool { url.pathExtension.lowercased() == "mkv" }

    private func scratchSpaceError() -> String? {
        if !mode.needsScratch { return nil }
        guard let scratchURL, let input = selectedInput else { return nil }
        do {
            let size = try input.resourceValues(forKeys: [.fileSizeKey]).fileSize ?? 0
            let values = try scratchURL.resourceValues(forKeys: [.volumeIsReadOnlyKey])
            if values.volumeIsReadOnly == true { return "The scratch volume is read-only." }
            let required = Int64(size) * 2 + 5 * 1024 * 1024 * 1024
            let available = try ScratchCapacity.availableBytes(at: scratchURL)
            if available < required {
                return "Scratch needs about \(ByteCountFormatter.string(fromByteCount: required, countStyle: .file)); only \(ByteCountFormatter.string(fromByteCount: available, countStyle: .file)) is available."
            }
        } catch { return "The scratch folder or input is unavailable." }
        return nil
    }

    var completionTitle: String {
        if mode != .checker && checkResults.isEmpty && reportStatus == nil { return "Completed" }
        switch checkerOutcome {
        case .passed: return mode == .checker ? "All checks passed" : "Completed"
        case .inconclusive: return "Completed — validation incomplete"
        case .warnings: return "Completed with warnings"
        case .failures: return "Completed with failures"
        }
    }

    private var repairCompletionTitle: String {
        switch checkerOutcome {
        case .passed: "Fix verified"
        case .inconclusive: "Fix completed — validation incomplete"
        case .warnings: "Fix completed with warnings"
        case .failures: "Fix completed with failures"
        }
    }

    private var fixableResult: CheckResult? {
        checkResults.first { $0.fixAction != nil && $0.fixValue != nil }
    }

    private func restoreScratchBookmark() {
        guard let data = preferences.data(forKey: bookmarkKey) else { return }
        var stale = false
        if let url = try? URL(resolvingBookmarkData: data, options: [.withSecurityScope], relativeTo: nil, bookmarkDataIsStale: &stale), !stale {
            scratchURL = url
            _ = url.startAccessingSecurityScopedResource()
        }
    }
}

// Deliberately a filesystem identity, not a cryptographic content certificate.
struct InputIdentity: Equatable {
    let path: String
    let bytes: Int64
    let modifiedSeconds: Int
    let modifiedNanoseconds: Int
    let changedSeconds: Int
    let changedNanoseconds: Int
    let inode: UInt64
    let device: Int32

    init?(_ url: URL) {
        let resolved = url.resolvingSymlinksInPath().standardizedFileURL
        var info = stat()
        guard lstat(resolved.path, &info) == 0,
              info.st_mode & S_IFMT == S_IFREG else { return nil }
        path = resolved.path
        bytes = info.st_size
        modifiedSeconds = info.st_mtimespec.tv_sec
        modifiedNanoseconds = info.st_mtimespec.tv_nsec
        changedSeconds = info.st_ctimespec.tv_sec
        changedNanoseconds = info.st_ctimespec.tv_nsec
        inode = info.st_ino
        device = info.st_dev
    }
}
