import Foundation

@main
struct AppModelAuditTests {
    @MainActor
    static func main() {
        let suite = "DV8Audit-" + UUID().uuidString
        let preferences = UserDefaults(suiteName: suite)!
        defer { preferences.removePersistentDomain(forName: suite) }
        let model = AppModel(preferences: preferences)
        model.mode = .checker
        let work = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try! FileManager.default.createDirectory(at: work, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: work) }
        assert(!model.saveEnhancementLayer && model.standardArchiveArguments == ["-n"], "Upgrade must keep the app's no-archive default")
        model.saveEnhancementLayer = true
        assert(model.archiveError != nil, "Archiving cannot silently use a default destination")
        let archive = work.appendingPathComponent("EL archive")
        try! FileManager.default.createDirectory(at: archive, withIntermediateDirectories: true)
        try! model.rememberArchiveFolder(archive)
        assert(model.archiveError == nil)
        assert(model.standardArchiveArguments == ["--archive-dir", archive.path])
        let restored = AppModel(preferences: preferences)
        assert(restored.saveEnhancementLayer && restored.archiveURL?.resolvingSymlinksInPath() == archive.resolvingSymlinksInPath(), "Archive setting and bookmark must survive restart: \(restored.saveEnhancementLayer), \(String(describing: restored.archiveURL))")
        try! FileManager.default.removeItem(at: archive)
        assert(model.archiveError != nil, "Unavailable archive destination must block conversion")
        model.saveEnhancementLayer = false
        assert(model.archiveError == nil && model.standardArchiveArguments == ["-n"])
        assert(ConversionMode.allCases.map(\.rawValue) == ["Hybrid", "DV7 → DV8", "Checker"])
        model.mode = .hybrid
        model.scratchURL = work
        model.accept(work.appendingPathComponent("donor.mp4"), role: "dv")
        assert(model.dvSource == nil && !model.canRun, "Main must reject the removed P5 MP4 workflow")
        model.mode = .checker
        let progressID = UUID()
        model.beginOutputTracking(id: progressID, report: nil)
        model.receiveOutput(Data("{\"event\":\"phase_started\",\"index\":1,\"total\":1}\n{\"event\":\"phase_progress\",\"fraction\":0.5}\n".utf8), stream: "stdout", id: progressID)
        assert(model.progress == 0.5, "Single-phase progress must reflect the measured fraction")
        let old = work.appendingPathComponent("old.mkv")
        try! Data("original".utf8).write(to: old)
        model.checkerSource = old
        model.bindCheckedInput(InputIdentity(old))
        model.checkResults = [CheckResult(id: "sync", label: "Sync", status: "fail", detail: "Offset", fixAction: "sync-offset", fixValue: 5)]
        assert(model.canFix)
        try! Data("changed in place".utf8).write(to: old)
        assert(!model.canFix, "Changed bytes/size at the same path must block stale repair")
        model.bindCheckedInput(InputIdentity(old))
        assert(model.canFix)
        let prior = try! FileManager.default.attributesOfItem(atPath: old.path)[.modificationDate] as! Date
        try! Data("same-size-change".utf8).write(to: old)
        try! FileManager.default.setAttributes([.modificationDate: prior], ofItemAtPath: old.path)
        assert(!model.canFix, "Restoring mtime does not restore ctime identity")
        model.bindCheckedInput(InputIdentity(old))
        try! FileManager.default.removeItem(at: old)
        try! Data("replacement".utf8).write(to: old)
        assert(!model.canFix, "A replaced file must block stale repair")
        model.accept(URL(fileURLWithPath: "/tmp/new.mkv"), role: "checker")
        assert(model.checkResults.isEmpty && !model.canFix, "A new input must not inherit the previous file's repair")
        model.isRunning = true
        model.accept(URL(fileURLWithPath: "/tmp/third.mkv"), role: "checker")
        assert(model.checkerSource?.lastPathComponent == "new.mkv", "Running input must stay stable")
        model.isRunning = false
        assert(model.checkerOutcome == .inconclusive, "Empty checks are not a pass")
        for status in ["inconclusive", "future-unknown-status"] {
            model.checkResults = [CheckResult(id: "area", label: "Active area", status: status, detail: "Unresolved", fixAction: nil, fixValue: nil)]
            assert(model.checkerOutcome == .inconclusive)
            assert(model.completionTitle == "Completed — validation incomplete")
            model.checkResults.append(CheckResult(id: "sync", label: "Sync", status: "fail", detail: "Offset", fixAction: "sync-offset", fixValue: 5))
            assert(!model.canFix, "Unresolved validation must disable automatic repair")
            assert(model.checkerOutcome == .failures, "A failure takes priority over uncertainty")
        }
        model.mode = .hybrid
        model.checkResults = [CheckResult(id: "area", label: "Active area", status: "inconclusive", detail: "Sources retained", fixAction: nil, fixValue: nil)]
        assert(model.completionTitle == "Completed — validation incomplete")
        model.mode = .standard
        model.checkResults = []
        assert(model.completionTitle == "Completed")
        // Force termination ahead of the last pipe data and a split JSON line.
        let id = UUID()
        model.beginOutputTracking(id: id, report: nil)
        model.isRunning = true
        model.receiveExit(0, id: id)
        assert(model.isRunning)
        model.receiveOutput(Data("{\"event\":\"completed\",\"output\":\"/tmp/out.mkv\"}\n{\"event\":\"check_".utf8), stream: "stdout", id: id)
        model.receiveOutput(Data("result\",\"key\":\"late\",\"label\":\"Late check\",\"status\":\"inconclusive\",\"detail\":\"Unknown\"}".utf8), stream: "stdout", id: id)
        model.receiveEOF("stderr", id: id)
        assert(model.isRunning)
        model.receiveEOF("stdout", id: id)
        assert(!model.isRunning && model.phase == "Completed — validation incomplete")
        assert(model.checkResults.contains { $0.id == "late" })
        let oldPhase = model.phase
        model.receiveOutput(Data("{\"event\":\"failed\",\"error\":\"stale\"}\n".utf8), stream: "stdout", id: UUID())
        assert(model.phase == oldPhase, "Events from an older run cannot mutate the current result")
        let missing = UUID()
        model.beginOutputTracking(id: missing, report: nil)
        model.errorMessage = nil
        model.receiveEOF("stdout", id: missing)
        model.receiveEOF("stderr", id: missing)
        model.receiveExit(0, id: missing)
        assert(model.phase == "Failed", "Exit zero without completion is not success")
        // The saved report must agree with the terminal event and exit status.
        let report = work.appendingPathComponent("report.json")
        let saved: [String: Any] = ["schema_version": 1, "execution": "completed", "validation": "inconclusive"]
        try! JSONSerialization.data(withJSONObject: saved).write(to: report)
        let withReport = UUID()
        model.beginOutputTracking(id: withReport, report: report)
        model.errorMessage = nil
        model.phase = "Working"
        model.isRunning = true
        let terminal: [[String: Any]] = [
            ["event": "completed", "output": old.path],
            ["event": "job_finalized", "report": report.path, "status": "inconclusive"]
        ]
        for e in terminal {
            var data = try! JSONSerialization.data(withJSONObject: e); data.append(0x0A)
            model.receiveOutput(data, stream: "stdout", id: withReport)
        }
        model.receiveExit(0, id: withReport)
        model.receiveEOF("stdout", id: withReport)
        model.receiveEOF("stderr", id: withReport)
        assert(model.phase == "Completed — validation incomplete" && model.reportURL == report)
        try! Data("broken report".utf8).write(to: report)
        let badReport = UUID()
        model.beginOutputTracking(id: badReport, report: report)
        for e in terminal {
            var data = try! JSONSerialization.data(withJSONObject: e); data.append(0x0A)
            model.receiveOutput(data, stream: "stdout", id: badReport)
        }
        model.receiveEOF("stdout", id: badReport)
        model.receiveEOF("stderr", id: badReport)
        model.receiveExit(0, id: badReport)
        assert(model.phase == "Failed" && !model.canFix, "An invalid saved report cannot authorize repair")
        // Authored controls for all terminal statuses, including cancellation.
        for (execution, validation, terminalEvent, exit, expected) in [
            ("completed", "pass", "completed", Int32(0), "All checks passed"),
            ("completed", "warn", "completed", Int32(0), "Completed with warnings"),
            ("completed", "inconclusive", "completed", Int32(0), "Completed — validation incomplete"),
            ("failed", "fail", "failed", Int32(1), "Failed"),
            ("cancelled", "inconclusive", "cancelled", Int32(1), "Cancelled")
        ] {
            let item = AppModel(preferences: preferences)
            item.mode = .checker
            let path = work.appendingPathComponent(execution + validation + ".json")
            let saved: [String: Any] = ["schema_version":1, "execution":execution, "validation":validation]
            try! JSONSerialization.data(withJSONObject: saved).write(to: path)
            let id = UUID()
            item.beginOutputTracking(id: id, report: path)
            item.isRunning = true
            item.receiveExit(exit, id: id)
            for event: [String: Any] in [
                ["event":"check_result", "key":"result", "label":"Result", "status":validation, "detail":"Scenario"],
                ["event":"job_finalized", "report":path.path, "status":validation],
                ["event":terminalEvent, "output":old.path, "error":"Scenario failure"]
            ] {
                var bytes = try! JSONSerialization.data(withJSONObject: event); bytes.append(10)
                item.receiveOutput(bytes, stream: "stdout", id: id)
            }
            item.receiveEOF("stderr", id: id)
            assert(item.isRunning)
            item.receiveEOF("stdout", id: id)
            assert(item.phase == expected && !item.canFix, "Terminal mismatch: \(execution)/\(validation): \(item.phase)")
        }
        if let path = ProcessInfo.processInfo.environment["DV8_APP_REPLAY_MANIFEST"] {
            replayActualJobs(URL(fileURLWithPath: path), preferences: preferences)
        }
        print("Passed: input identity, stale repair, pipe drain ordering, trailing JSON, missing completion and outcome precedence")

    }

    @MainActor static func replayActualJobs(_ manifest: URL, preferences: UserDefaults) {
        let jobs = try! JSONSerialization.jsonObject(with: Data(contentsOf: manifest)) as! [[String: Any]]
        assert(jobs.count >= 20)
        for job in jobs {
            let model = AppModel(preferences: preferences)
            let report = URL(fileURLWithPath: job["report"] as! String)
            let saved = try! JSONSerialization.jsonObject(with: Data(contentsOf: report)) as! [String: Any]
            model.mode = saved["operation"] as? String == "standard" ? .standard : .checker
            let status = saved["validation"] as! String
            let execution = saved["execution"] as! String
            let id = UUID()
            model.beginOutputTracking(id: id, report: report)
            model.isRunning = true
            model.receiveExit(Int32(job["exit_status"] as! Int), id: id)
            let data = try! Data(contentsOf: URL(fileURLWithPath: job["log"] as! String))
            // Fragment across JSON boundaries, with process exit delivered first.
            for start in stride(from: 0, to: data.count, by: 137) {
                model.receiveOutput(data.subdata(in: start..<min(start+137, data.count)), stream: "stdout", id: id)
            }
            model.receiveEOF("stderr", id: id)
            assert(model.isRunning)
            model.receiveEOF("stdout", id: id)
            let expected: String
            if execution == "failed" { expected = "Failed" }
            else if status == "inconclusive" { expected = "Completed — validation incomplete" }
            else if status == "warn" { expected = "Completed with warnings" }
            else { expected = model.mode == .checker ? "All checks passed" : "Completed" }
            assert(model.phase == expected, "Replay \(job["name"]!): expected \(expected), got \(model.phase)")
            assert(model.reportURL == report)
            assert(!model.canFix, "A replay without bound source identity cannot authorize repair")
            for check in saved["checks"] as! [[String: Any]] {
                let key = check["key"] as! String
                assert(model.checkResults.contains { $0.id == key && $0.status == check["status"] as! String }, "Missing replayed check: \(key)")
            }
        }
        print("Passed: \(jobs.count) actual CLI job/report traces replayed through AppModel with early exit and fragmented output")
    }
}
