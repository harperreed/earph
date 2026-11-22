// ABOUTME: Serializes every Rust controller call and publishes coalesced immutable native presentations.
// ABOUTME: Bounds pending actions and releases the controller off the main thread after cleanup.
import AppKit
import Combine
import Foundation

@MainActor
final class ControllerStore: ObservableObject {
    @Published private(set) var snapshot: NativeSnapshot?
    @Published var error: String?
    @Published var runtime: String
    @Published var executable: String
    @Published var graphText = ""
    @Published private(set) var profileDraftLoadCount: UInt64 = 0
    @Published private(set) var trackSelection: PlaybackSelection?
    private(set) var loadedProfileDraft: JSONValue = .null
    private var driver: ControllerDriver!
    private var closing = false
    private var shutdownRequested = false
    private var closeCallbacks: [() -> Void] = []
    private var pendingFiles: [URL] = []
    private var pendingFileOwner: JSONValue = .null
    private var fileInFlight = false
    private var activeFilePath: String?
    private var activeFileRequestIDs: Set<String> = []
    private var filePendingObserved = false
    private var canonicalGraph: String?
    var drafts: [String: JSONValue] = [:]
    var graphDirty: Bool { graphText != canonicalGraph }

    init() {
        runtime = FileManager.default.homeDirectoryForCurrentUser.appendingPathComponent(".earph").path
        executable = Bundle.main.executableURL?.deletingLastPathComponent().appendingPathComponent("earph-engine").path ?? ""
        driver = ControllerDriver { [weak self] snapshot, error, completed in self?.adopt(snapshot, error: error, completed: completed) }
    }
    func value(_ key: String) -> JSONValue { snapshot?.root[key] ?? .null }
    func flag(_ key: String) -> Bool { snapshot?.flag(key) ?? false }

    func connectAutomatically() {
        guard !closing, !flag("attached"), !flag("busy") else { return }
        if CommandLine.arguments.contains("--attach-only")
            || ["owned_running", "attached_existing"].contains(value("host")["kind"].string ?? "") {
            send("attach", fields: ["runtime": .string(runtime)])
        } else {
            send("launch", fields: ["runtime": .string(runtime), "executable": .string(executable)])
        }
    }
    func selectTrack(_ id: String?, owner: JSONValue) {
        guard owner != .null else { return }
        if let id, snapshot?.state["owner_id"] == owner,
           snapshot?.state["active_entry_id"].string == id,
           snapshot?.state["transport"].string == "playing", snapshot?.state["muted"].bool == false {
            trackSelection = nil
        } else { trackSelection = PlaybackSelection(entryID: id, owner: owner) }
    }
    func play(captured: NativeSnapshot?) {
        let selected = trackSelection?.entryToPlay ?? .null
        if let selection = trackSelection, selection.phase != .selected {
            error = "Playback is already being prepared."
            return
        }
        if selected != .null, trackSelection?.owner != captured?.state["owner_id"] {
            error = "The audio player changed. Select the track again."
            return
        }
        let submissionID = UUID()
        if send("play_local", fields: ["takeover": .bool(false), "entry_id": selected], captured: captured, submissionID: submissionID),
           var selection = trackSelection {
            selection.phase = .queued(submissionID)
            trackSelection = selection
        }
    }

    @discardableResult
    func send(_ kind: String, fields: [String: JSONValue] = [:], captured: NativeSnapshot? = nil, submissionID: UUID? = nil) -> Bool {
        guard !closing else { return false }
        let action = (captured ?? snapshot)?.action(kind, fields: fields) ?? .object(fields.merging(["version": .uint(1), "kind": .string(kind)]) { _, value in value })
        let admitted = driver.send(action, safety: kind == "mute" || kind == "stop", submissionID: submissionID)
        if !admitted { error = "Too many pending actions. Wait for the current requests to finish." }
        return admitted
    }
    @discardableResult
    func command(_ kind: String, method: String, fields: [String: JSONValue] = [:]) -> Bool {
        var command = fields
        command["method"] = .string(method)
        return send(kind, fields: ["command": .object(command)])
    }
    func perform(_ operation: () throws -> Void) {
        do { try operation() } catch { self.error = error.localizedDescription }
    }
    func dismissError() {
        error = nil
        if value("error") != .null { send("refresh") }
    }
    func close(completion: @escaping () -> Void) {
        if flag("ready_to_close") { closing = true; completion(); return }
        closeCallbacks.append(completion)
        guard !closing else { return }
        closing = true
        pendingFiles.removeAll()
        driver.close(.object(["version": .uint(1), "kind": .string("close_controller")]))
    }
    func shutdownAndQuit(captured: NativeSnapshot? = nil) {
        guard !closing else { return }
        pendingFiles.removeAll()
        if send("shutdown_and_quit", captured: captured) { shutdownRequested = true }
    }
    private func adopt(_ snapshot: NativeSnapshot?, error: String?, completed: [NativeCompletion]) {
        let completedKinds = completed.compactMap { completion -> String? in
            guard case .snapshot = completion.outcome else { return nil }
            return completion.kind
        }
        if let error {
            self.error = error; pendingFiles.removeAll(); fileInFlight = false
        }
        if let snapshot {
            if self.snapshot?.state["owner_id"] != snapshot.state["owner_id"] {
                trackSelection = nil
            }
            self.snapshot = snapshot
            if let graph = snapshot.root["local_draft"]["graph_json"].string, graph != canonicalGraph {
                canonicalGraph = graph
                graphText = graph
            }
            if completedKinds.contains(where: { ["graph_accept_json", "graph_reset", "graph_restore", "graph_load_profile", "profile_draft_reset"].contains($0) }), snapshot.root["local_draft"]["graph_error"] == .null {
                graphText = canonicalGraph ?? ""
            }
        }
        for completion in completed where completion.kind == "play_local" {
            guard let submissionID = completion.submissionID else { continue }
            switch completion.outcome {
            case .snapshot(let snapshot): trackSelection = trackSelection?.observing(snapshot, completionID: submissionID)
            case .failed: trackSelection = trackSelection?.failed(completionID: submissionID)
            }
        }
        if let snapshot { trackSelection = trackSelection?.observing(snapshot) }
        for completion in completed {
            guard case .snapshot(let completedSnapshot) = completion.outcome,
                  completedSnapshot.root["error"] == .null else { continue }
            if completion.kind == "profile_draft_load" {
                loadedProfileDraft = completedSnapshot.root["local_draft"]
                profileDraftLoadCount &+= 1
            } else if completion.kind == "profile_draft_reset" {
                loadedProfileDraft = .null
            }
        }
        if closing && flag("ready_to_close") {
            let callbacks = closeCallbacks
            closeCallbacks.removeAll()
            callbacks.forEach { $0() }
            return
        }
        if shutdownRequested && flag("ready_to_close") {
            shutdownRequested = false
            NSApplication.shared.terminate(nil)
            return
        }
        if fileInFlight {
            if completedKinds.contains("queue_edit") || flag("queue_edit_pending") { filePendingObserved = true }
            if filePendingObserved {
                let receipt = value("receipts").array.last {
                    $0["method"].string == "queue.append" && $0["command"]["path"].string == activeFilePath
                        && $0["request_id"].string.map { !activeFileRequestIDs.contains($0) } == true
                }
                let status = receipt?["presentation"].string ?? ""
                if receipt?["owner_changed"].bool == true || ["rejected", "failed", "cancelled", "owner_error", "uncertain"].contains(status) {
                    failFileBatch(receipt)
                } else if flag("queue_edit_available") {
                    if status == "applied", receipt?["in_flight"].bool == false { finishFileAppend() }
                    else { failFileBatch(receipt) }
                }
            }
        }
        appendNextFile()
    }
    private func finishFileAppend() {
        fileInFlight = false
        filePendingObserved = false
        activeFilePath = nil
        activeFileRequestIDs.removeAll()
    }
    private func failFileBatch(_ receipt: JSONValue?) {
        pendingFiles.removeAll()
        error = value("error").string ?? receipt?["outcome"]["error"]["message"].string
            ?? receipt?["failure"]["detail"].string ?? error
            ?? "The Engine did not confirm the selected file. Review Requests and Receipts before continuing."
        finishFileAppend()
    }
    private func appendNextFile() {
        guard !closing, !fileInFlight, !pendingFiles.isEmpty, flag("queue_edit_available") else { return }
        guard snapshot?.state["owner_id"] == pendingFileOwner else {
            pendingFiles.removeAll()
            error = "The Engine changed while adding files. Choose the remaining files again."
            return
        }
        let url = pendingFiles.removeFirst()
        activeFilePath = url.path
        activeFileRequestIDs = Set(value("receipts").array.compactMap { $0["request_id"].string })
        fileInFlight = true
        filePendingObserved = false
        if !command("queue_edit", method: "queue.append", fields: ["path": .string(url.path)]) {
            pendingFiles.insert(url, at: 0)
            fileInFlight = false
        }
    }
    func appendFiles(_ urls: [URL]) {
        guard flag("connected"), let owner = snapshot?.state["owner_id"], owner != .null else {
            error = "Connect to an Engine before adding files."
            return
        }
        guard pendingFiles.count + urls.count <= 1024 else {
            error = "Choose at most 1024 pending files at once."
            return
        }
        if pendingFiles.isEmpty && !fileInFlight { pendingFileOwner = owner }
        guard pendingFileOwner == owner else {
            error = "The Engine changed during this file batch. Finish or reconnect before adding more files."
            return
        }
        pendingFiles.append(contentsOf: urls)
        appendNextFile()
    }
    func chooseFiles() {
        let owner = snapshot?.state["owner_id"]
        let panel = NSOpenPanel()
        panel.title = "Add Audio Files"
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = true
        panel.begin { [weak self] response in
            guard response == .OK, let self else { return }
            guard self.snapshot?.state["owner_id"] == owner else { self.error = "The Engine changed while choosing files. Choose the files again."; return }
            self.appendFiles(panel.urls)
        }
    }
    func chooseFolder() {
        let owner = snapshot?.state["owner_id"]
        choosePath(directory: true) { [weak self] path in
            guard let self else { return }
            guard self.snapshot?.state["owner_id"] == owner else { self.error = "The Engine changed while choosing a folder. Choose the folder again."; return }
            self.command("queue_edit", method: "queue.append_directory", fields: ["path": .string(path)])
        }
    }
    func chooseEngine() { choosePath(directory: false) { [weak self] path in self?.executable = path } }
    func choosePath(directory: Bool, completion: @escaping (String) -> Void) {
        let panel = NSOpenPanel()
        panel.canChooseFiles = !directory
        panel.canChooseDirectories = directory
        panel.allowsMultipleSelection = false
        panel.begin { response in
            if response == .OK, let url = panel.url { completion(url.path) }
        }
    }
}

private struct NativeCompletion: Sendable {
    enum Outcome: Sendable { case snapshot(NativeSnapshot), failed(String) }
    let kind: String
    let submissionID: UUID?
    let outcome: Outcome
}

private final class ControllerDriver: @unchecked Sendable {
    private let queue = DispatchQueue(label: "org.earph.native-controller")
    private let lock = NSLock()
    private var controller: NativeController?
    private var ordinaryPending = 0
    private var safetyPending = 0
    private var closeRequested = false
    private var latest: NativeSnapshot?
    private var latestError: String?
    private var publicationPending = false
    private var completedActions: [NativeCompletion] = []
    private let deliver: @MainActor @Sendable (NativeSnapshot?, String?, [NativeCompletion]) -> Void

    init(deliver: @escaping @MainActor @Sendable (NativeSnapshot?, String?, [NativeCompletion]) -> Void) {
        self.deliver = deliver
        queue.async { [self] in controller = NativeController(); tick() }
    }
    func send(_ action: JSONValue, safety: Bool, submissionID: UUID?) -> Bool {
        lock.lock()
        let admitted = !closeRequested && (safety ? safetyPending < 2 : ordinaryPending < 16)
        if admitted {
            if safety { safetyPending += 1 } else { ordinaryPending += 1 }
        }
        lock.unlock()
        guard admitted else { return false }
        queue.async { [self] in
            call(action, submissionID: submissionID)
            lock.lock()
            if safety { safetyPending -= 1 } else { ordinaryPending -= 1 }
            lock.unlock()
        }
        return true
    }
    func close(_ action: JSONValue) {
        lock.lock()
        let alreadyClosing = closeRequested
        closeRequested = true
        lock.unlock()
        guard !alreadyClosing else { return }
        queue.async { [self] in call(action) }
    }
    private func call(_ action: JSONValue, submissionID: UUID? = nil) {
        guard let controller else { return }
        do {
            let snapshot = try NativeSnapshot(json: controller.send(actionJson: action.encoded()))
            publish(snapshot, completed: NativeCompletion(kind: action["kind"].string ?? "", submissionID: submissionID, outcome: .snapshot(snapshot)))
        }
        catch {
            publish(nil, error: error.localizedDescription,
                    completed: NativeCompletion(kind: action["kind"].string ?? "", submissionID: submissionID, outcome: .failed(error.localizedDescription)))
        }
    }
    private func tick() {
        guard let controller else { return }
        do {
            let snapshot = try NativeSnapshot(json: controller.tick())
            if snapshot.flag("ready_to_close") {
                self.controller = nil
                lock.lock(); closeRequested = true; lock.unlock()
                publish(snapshot)
                return
            }
            publish(snapshot)
            queue.asyncAfter(deadline: .now() + (snapshot.flag("busy") ? 0.05 : 0.25)) { [self] in tick() }
        } catch {
            publish(nil, error: error.localizedDescription)
            queue.asyncAfter(deadline: .now() + 0.25) { [self] in tick() }
        }
    }
    private func publish(_ snapshot: NativeSnapshot?, error: String? = nil, completed: NativeCompletion? = nil) {
        lock.lock()
        if let snapshot { latest = snapshot }
        if let error { latestError = error }
        if let completed { completedActions.append(completed) }
        let schedule = !publicationPending
        publicationPending = true
        lock.unlock()
        guard schedule else { return }
        DispatchQueue.main.async { [self] in
            lock.lock()
            let snapshot = latest
            let error = latestError
            let completed = completedActions
            completedActions.removeAll()
            latestError = nil
            publicationPending = false
            lock.unlock()
            deliver(snapshot, error, completed)
        }
    }
}
