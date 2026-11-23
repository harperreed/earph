// ABOUTME: Tests native batch admission and repeated profile loading against a real standalone Engine.
// ABOUTME: Uses actual Store publications and native field editing without output, mocks, or global clipboard access.
import AppKit
import Combine
import Darwin
import SwiftUI

@main
struct BatchIntegrationTests {
    @MainActor
    static func main() throws {
        guard (4...5).contains(CommandLine.arguments.count) else { throw RegressionFailure.message("usage: batch-integration-tests FRESH_RUNTIME ABSOLUTE_ENGINE NEW_ARTIFACT_DIRECTORY [batch|profile|all]") }
        let runtime = CommandLine.arguments[1]
        let executable = CommandLine.arguments[2]
        let directory = URL(fileURLWithPath: CommandLine.arguments[3], isDirectory: true)
        let mode = CommandLine.arguments.count == 5 ? CommandLine.arguments[4] : "all"
        try require(["batch", "profile", "all"].contains(mode), "Unknown regression mode")
        try require(runtime.hasPrefix("/") && runtime.utf8.count <= 60 && !FileManager.default.fileExists(atPath: runtime), "Use a fresh short absolute runtime")
        try require(executable.hasPrefix("/") && FileManager.default.isExecutableFile(atPath: executable), "Use the actual absolute Engine executable")
        try require(!FileManager.default.fileExists(atPath: directory.path), "Use a new artifact directory")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
        _ = NSApplication.shared
        let store = ControllerStore()
        store.runtime = runtime
        store.executable = executable
        var launched = false
        var owner: JSONValue = .null
        var ownedPID: UInt64?
        defer {
            if launched {
                do {
                    let cleanup = NativeController()
                    _ = try cleanup.send(actionJson: action("attach", fields: ["runtime": .string(runtime)]).encoded())
                    let observed = try waitNative(cleanup) { $0.flag("control_edit_available") }
                    if owner != .null { try require(observed.state["owner_id"] == owner, "Cleanup must retain the exact owned identity") }
                    _ = try cleanup.send(actionJson: observed.action("shutdown_and_quit").encoded())
                    let retired = try waitNative(cleanup) { $0.flag("ready_to_close") }
                    try write(retired.root, to: directory.appendingPathComponent("cleanup.json"))
                } catch {
                    try? Data(String(describing: error).utf8).write(to: directory.appendingPathComponent("cleanup-error.txt"))
                }
            }
            var closed = false
            store.close { closed = true }
            let closeDeadline = Date().addingTimeInterval(5)
            while !closed && Date() < closeDeadline { RunLoop.current.run(until: Date().addingTimeInterval(0.01)) }
        }
        do {
            launched = true
            store.connectAutomatically()
            try waitStore(store) { store.flag("queue_edit_available") && store.value("host")["kind"].string == "owned_running" }
            owner = store.snapshot?.state["owner_id"] ?? .null
            ownedPID = store.value("host")["pid"].uint
            try require(owner != .null && ownedPID != nil, "Launch must expose the real owner and PID")
            store.connectAutomatically()
            try require(store.snapshot?.state["owner_id"] == owner, "Automatic connection must keep an attached owner")
            try write(.object(["owner_id": owner, "pid": ownedPID.map(JSONValue.uint) ?? .null, "runtime": .string(runtime)]), to: directory.appendingPathComponent("owned-process.json"))
            try silent(store)
            if mode != "profile" { try batchRegression(store, runtime: runtime, directory: directory) }
            if mode != "batch" { try profileRegression(store, directory: directory) }
            try silent(store)
            let cleanup = NativeController()
            _ = try cleanup.send(actionJson: action("attach", fields: ["runtime": .string(runtime)]).encoded())
            let observed = try waitNative(cleanup) { $0.flag("control_edit_available") }
            try require(observed.state["owner_id"] == owner, "Successful cleanup must retain the exact owned identity")
            _ = try cleanup.send(actionJson: observed.action("shutdown_and_quit").encoded())
            _ = try waitNative(cleanup) { $0.flag("ready_to_close") }
            try waitStore(store) { store.value("host")["kind"].string == "exited" }
            try require(store.value("host")["pid"].uint == ownedPID && store.value("host")["success"].bool, "Explicit shutdown must retire the successful owned process")
            launched = false
            try write(.object(["status": .string("pass"), "mode": .string(mode), "owner_id": owner, "pid": ownedPID.map(JSONValue.uint) ?? .null, "output_selected": .bool(false), "source_started": .bool(false)]), to: directory.appendingPathComponent("result.json"))
            print("PASS real native \(mode) regression; retained: \(directory.path)")
        } catch {
            if let snapshot = store.snapshot { try? write(snapshot.root, to: directory.appendingPathComponent("failure-snapshot.json")) }
            try? Data(String(describing: error).utf8).write(to: directory.appendingPathComponent("failure.txt"))
            throw error
        }
    }

    @MainActor
    static func batchRegression(_ store: ControllerStore, runtime: String, directory: URL) throws {
        let first = directory.appendingPathComponent("first.wav")
        let second = directory.appendingPathComponent("second.wav")
        let external = directory.appendingPathComponent("external.wav")
        for file in [first, second, external] { try silentWAV().write(to: file, options: .withoutOverwriting) }
        store.appendFiles([first, second])
        try waitStore(store) { store.value("queue")["entries"].array.count == 2 && store.flag("queue_edit_available") }
        let rows = store.value("queue")["entries"].array.compactMap(QueueRow.init)
        try require(rows.count == 2 && rows.allSatisfy { $0.status == "Queued" }, "Fresh audio files must appear as queued music")
        let owner = store.snapshot?.state["owner_id"] ?? .null
        var selectionPublished = false
        let selectionObservation = store.objectWillChange.sink { selectionPublished = true }
        store.selectTrack(rows[1].id, owner: owner)
        selectionObservation.cancel()
        try require(selectionPublished, "Selecting another track must update the toolbar before the next Engine observation")
        store.play(captured: store.snapshot)
        try waitStore(store) { store.value("error") != .null && !store.flag("play_local_pending") }
        try require(store.trackSelection?.entryID == rows[1].id && store.trackSelection?.phase == .selected,
                    "Refused playback must preserve the selected track for retry")
        try silent(store)
        store.dismissError()
        try waitStore(store) { store.value("error") == .null && store.flag("queue_edit_available") }
        try require(store.value("receipts").array.filter { $0["method"].string == "queue.append" && $0["presentation"].string == "applied" }.count == 2, "Both successful batch files need real Applied receipts")
        let staleGeneration = store.snapshot?.root["generation"]
        let retainedIDs = Set(store.value("receipts").array.compactMap { $0["request_id"].string })
        let externalController = NativeController()
        _ = try externalController.send(actionJson: action("attach", fields: ["runtime": .string(runtime)]).encoded())
        var state = try waitNative(externalController) { $0.flag("queue_edit_available") }
        try require(state.root["queue"]["selected"] == .null, "Selecting a row must remain a draft until approved playback can proceed")
        // Keep MainActor publication blocked until the external mutations finish.
        for entry in state.root["queue"]["entries"].array {
            _ = try externalController.send(actionJson: state.action("queue_edit", fields: ["command": .object(["method": .string("queue.remove"), "id": entry["id"]])]).encoded())
            state = try waitNative(externalController) { snapshot in snapshot.flag("queue_edit_available") && !snapshot.root["queue"]["entries"].array.contains { $0["id"] == entry["id"] } }
        }
        _ = try externalController.send(actionJson: state.action("queue_edit", fields: ["command": .object(["method": .string("queue.append"), "path": .string(external.path)])]).encoded())
        state = try waitNative(externalController) { $0.flag("queue_edit_available") && $0.root["queue"]["entries"].array.count == 1 }
        try require(store.snapshot?.root["generation"] == staleGeneration, "The Store must retain the deliberately stale published basis")
        store.appendFiles([first, second])
        try waitStore(store) { store.value("error") != .null || store.error != nil || store.value("queue")["entries"].array.count > 1 }
        let settled = Date().addingTimeInterval(1)
        while Date() < settled { RunLoop.current.run(until: Date().addingTimeInterval(0.01)) }
        let verifier = NativeController()
        _ = try verifier.send(actionJson: action("attach", fields: ["runtime": .string(runtime)]).encoded())
        state = try waitNative(verifier) { $0.flag("queue_edit_available") && $0.root["queue"]["entries"].array.contains { $0["path"].string == external.path } }
        let paths = state.root["queue"]["entries"].array.compactMap { $0["path"].string }
        try write(store.snapshot?.root ?? .null, to: directory.appendingPathComponent("stale-batch-store.json"))
        try write(state.root, to: directory.appendingPathComponent("stale-batch-owner.json"))
        try require(paths == [external.path], "A stale batch must stop before adding or skipping either selected file")
        try require(store.error != nil || store.value("error") != .null, "A refused batch must preserve a visible error")
        let unexpected = store.value("receipts").array.filter { !retainedIDs.contains($0["request_id"].string ?? "") && $0["method"].string == "queue.append" }
        try require(unexpected.isEmpty, "Stale preflight must not submit the next file or reuse an old Applied receipt")
        _ = try verifier.send(actionJson: state.action("close_controller").encoded())
        _ = try waitNative(verifier) { $0.flag("ready_to_close") }
        _ = try externalController.send(actionJson: action("close_controller").encoded())
        _ = try waitNative(externalController) { $0.flag("ready_to_close") }
    }

    @MainActor
    static func profileRegression(_ store: ControllerStore, directory: URL) throws {
        store.dismissError()
        try waitStore(store) { store.flag("control_edit_available") && store.error == nil && store.value("error") == .null }
        let savedName = "Saved native reload intent"
        store.command("profile_action", method: "profiles.create", fields: ["id": .string("native-reload"), "draft": .object([
            "name": .string(savedName), "source_policy": .string("local_file"), "fidelity": .string("compatible_conversion"),
            "output": .object(["backend": .string("core_audio"), "route": .string("system"), "device_id": .string("offline"), "manual_setup": .null]),
            "volume": .object(["mode": .string("software"), "software_cap_millidecibels": .int(-20000), "hardware_cap_millidecibels": .null]),
            "latency": .object(["mode": .string("balanced"), "max_resident_bytes": .uint(268435456), "max_preparation_bytes": .uint(268435456), "max_latency_frames": .null]), "corrections": .array([])
        ]), "graph": store.value("local_draft")["graph"]])
        try waitStore(store) { store.flag("control_edit_available") && store.value("profiles")["record"]["profile"]["id"].string == "native-reload" }
        let hosting = NSHostingView(rootView: ProfileRegressionHost(store: store))
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 1040, height: 760), styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: false)
        window.contentView = hosting
        window.makeKeyAndOrderFront(nil)
        defer { window.orderOut(nil) }
        RunLoop.current.run(until: Date().addingTimeInterval(0.1))
        try require(store.send("profile_draft_load"), "First profile load was not admitted")
        try waitStore(store) { textFields(hosting).contains { $0.stringValue == savedName } }
        let reference = store.value("local_draft")["profile_loaded"]
        guard let field = textFields(hosting).first(where: { $0.stringValue == savedName }) else { throw RegressionFailure.message("Missing actual native Name field") }
        try require(window.makeFirstResponder(field), "The native Name field must accept focus")
        guard let editor = window.fieldEditor(true, for: field) as? NSTextView else { throw RegressionFailure.message("The native Name field must provide its standard field editor") }
        let edited = "Unsaved native edit"
        editor.setSelectedRange(NSRange(location: 0, length: (editor.string as NSString).length))
        editor.insertText(edited, replacementRange: editor.selectedRange())
        try require(window.makeFirstResponder(nil), "The native Name edit must commit")
        try waitStore(store) { textFields(hosting).contains { $0.stringValue == edited } && store.flag("control_edit_available") }
        RunLoop.current.run(until: Date().addingTimeInterval(0.3))
        try require(textFields(hosting).contains { $0.stringValue == edited }, "Background ticks must preserve unsaved native fields")
        try require(store.send("profile_draft_load"), "Repeated profile load was not admitted")
        let deadline = Date().addingTimeInterval(2)
        while !textFields(hosting).contains(where: { $0.stringValue == savedName }) && Date() < deadline { RunLoop.current.run(until: Date().addingTimeInterval(0.01)) }
        try write(store.snapshot?.root ?? .null, to: directory.appendingPathComponent("reloaded-profile.json"))
        try require(store.value("local_draft")["profile_loaded"] == reference, "The test must reload the exact same saved reference")
        try require(textFields(hosting).contains { $0.stringValue == savedName } && !textFields(hosting).contains { $0.stringValue == edited }, "Reloading the same saved profile must restore the actual native draft field")
    }

    @MainActor
    static func textFields(_ view: NSView) -> [NSTextField] { (view as? NSTextField).map { [$0] } ?? view.subviews.flatMap(textFields) }
    @MainActor
    static func waitStore(_ store: ControllerStore, predicate: () -> Bool) throws {
        let deadline = Date().addingTimeInterval(15)
        while !predicate() {
            if Date() >= deadline { throw RegressionFailure.message("Store timed out: \(store.snapshot?.root.encoded() ?? "no snapshot"); \(store.error ?? "no local error")") }
            RunLoop.current.run(until: Date().addingTimeInterval(0.01))
        }
    }
    static func waitNative(_ controller: NativeController, predicate: (NativeSnapshot) -> Bool) throws -> NativeSnapshot {
        let deadline = Date().addingTimeInterval(15)
        while true {
            let snapshot = try NativeSnapshot(json: controller.tick())
            if predicate(snapshot) { return snapshot }
            if Date() >= deadline { throw RegressionFailure.message("Native controller timed out: \(snapshot.root.encoded())") }
            Thread.sleep(forTimeInterval: 0.005)
        }
    }
    static func action(_ kind: String, fields: [String: JSONValue] = [:]) -> JSONValue { .object(fields.merging(["version": .uint(1), "kind": .string(kind)]) { _, value in value }) }
    static func require(_ value: @autoclosure () -> Bool, _ message: String) throws { if !value() { throw RegressionFailure.message(message) } }
    @MainActor
    static func silent(_ store: ControllerStore) throws {
        try require(store.snapshot?.state["muted"].bool == true && store.snapshot?.state["source"] == .null && store.snapshot?.state["output"] == .null, "Native regressions must remain muted without a source or output")
    }
    static func write(_ value: JSONValue, to file: URL) throws { try Data(value.encoded(pretty: true).utf8).write(to: file, options: .withoutOverwriting) }
    static func silentWAV() -> Data {
        var data = Data("RIFF".utf8)
        func u16(_ value: UInt16) { data.append(UInt8(value & 255)); data.append(UInt8(value >> 8)) }
        func u32(_ value: UInt32) { for shift in stride(from: 0, to: 32, by: 8) { data.append(UInt8((value >> shift) & 255)) } }
        u32(52); data.append(Data("WAVEfmt ".utf8)); u32(16); u16(1); u16(1)
        u32(48000); u32(96000); u16(2); u16(16); data.append(Data("data".utf8)); u32(16); data.append(Data(repeating: 0, count: 16))
        return data
    }
}

private struct ProfileRegressionHost: View {
    @ObservedObject var store: ControllerStore
    var body: some View { ProfilesView(store: store, actions: NativeActions(store: store, snapshot: store.snapshot)) }
}
private enum RegressionFailure: Error { case message(String) }
