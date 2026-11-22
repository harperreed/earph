// ABOUTME: Exercises generated Swift bindings against a real standalone Engine without selecting output or starting sources.
// ABOUTME: Retains fresh owner identity, exact snapshots, fixture bytes, and Quit cleanup results.
import Darwin
import Foundation

@main
struct OwnerIntegrationTests {
    @MainActor
    static func main() throws {
        guard CommandLine.arguments.count == 5 && CommandLine.arguments[4] == "--attach-only" else {
            throw CheckFailure.message("usage: owner-integration-tests ABSOLUTE_FRESH_RUNTIME ABSOLUTE_ENGINE ABSOLUTE_NEW_ARTIFACT_DIRECTORY --attach-only")
        }
        let runtime = CommandLine.arguments[1]
        let executable = CommandLine.arguments[2]
        let output = URL(fileURLWithPath: CommandLine.arguments[3], isDirectory: true)
        let manager = FileManager.default
        try check(runtime.hasPrefix("/") && runtime.utf8.count <= 60, "Use a short absolute fresh runtime")
        try check(!manager.fileExists(atPath: runtime), "Runtime must not exist before this test")
        try check(executable.hasPrefix("/") && manager.isExecutableFile(atPath: executable), "Engine must be an existing absolute executable")
        try check(!manager.fileExists(atPath: output.path), "Artifact directory must be new")
        try manager.createDirectory(at: output, withIntermediateDirectories: false, attributes: [.posixPermissions: 0o700])
        let fixture = output.appendingPathComponent("generated-silent.wav")
        let fixtureBytes = silentWAV()
        try fixtureBytes.write(to: fixture, options: .withoutOverwriting)
        var controller: NativeController? = NativeController()
        var launchAttempted = false
        var shutdownFinished = false
        var ownerID: JSONValue = .null
        var ownerPID: UInt64?
        defer {
            if launchAttempted && !shutdownFinished {
                do {
                    let cleanup = NativeController()
                    _ = try cleanup.send(actionJson: envelope("attach", fields: ["runtime": .string(runtime)]).encoded())
                    let state = try wait(cleanup) { $0.flag("connected") && $0.flag("control_edit_available") }
                    if ownerID != .null { try check(state.state["owner_id"] == ownerID, "Cleanup must retain exact owned identity") }
                    _ = try cleanup.send(actionJson: state.action("shutdown_and_quit").encoded())
                    let closed = try wait(cleanup) { $0.flag("ready_to_close") }
                    try write(closed.root, to: output.appendingPathComponent("failure-cleanup.json"))
                } catch {
                    try? Data(String(describing: error).utf8).write(to: output.appendingPathComponent("failure-cleanup-error.txt"))
                }
            }
        }
        do {
            try attachOnly(runtime: runtime, executable: executable, ownerID: .null, output: output)
            guard let first = controller else { throw CheckFailure.message("Missing initial controller") }
            launchAttempted = true
            _ = try first.send(actionJson: envelope("launch", fields: ["runtime": .string(runtime), "executable": .string(executable)]).encoded())
            var state = try wait(first) { $0.flag("connected") && $0.flag("queue_edit_available") && $0.root["host"]["kind"].string == "owned_running" }
            ownerID = state.state["owner_id"]
            ownerPID = state.root["host"]["pid"].uint
            try check(ownerID != .null && ownerPID != nil, "Launch must expose owned identity and PID")
            try write(.object(["owner_id": ownerID, "pid": ownerPID.map(JSONValue.uint) ?? .null, "runtime": .string(runtime)]), to: output.appendingPathComponent("owned-process.json"))
            try write(state.root, to: output.appendingPathComponent("launched.json"))
            try silent(state)
            try attachOnly(runtime: runtime, executable: executable, ownerID: ownerID, output: output)
            _ = try first.send(actionJson: envelope("refresh").encoded())
            let polling = try wait(first) { $0.flag("connected") && $0.flag("busy") }
            try check(!polling.showsPlaybackProgress, "A real background read must not show playback preparation")
            try silent(polling)
            try write(polling.root, to: output.appendingPathComponent("quiet-background-refresh.json"))
            state = try wait(first) { $0.flag("connected") && $0.flag("queue_edit_available") }
            _ = try first.send(actionJson: state.action("queue_edit", fields: ["command": .object(["method": .string("queue.append"), "path": .string(fixture.path)])]).encoded())
            state = try wait(first) { $0.root["queue"]["entries"].array.count == 1 && $0.flag("queue_edit_available") }
            guard let entry = state.root["queue"]["entries"].array.first?["id"].string else { throw CheckFailure.message("Append must expose the real queue entry") }
            try check(state.root["receipts"].array.contains { $0["method"].string == "queue.append" && $0["presentation"].string?.lowercased() == "applied" }, "Append must retain an applied receipt")
            _ = try first.send(actionJson: state.action("play_local", fields: ["takeover": .bool(false)]).encoded())
            state = try wait(first) { !$0.flag("play_local_pending") && $0.root["error"] != .null }
            try check(state.state["owner_id"] == ownerID, "Refused Play must retain the actual owner")
            try silent(state)
            try write(state.root, to: output.appendingPathComponent("play-without-output-refused.json"))
            _ = try first.send(actionJson: state.action("queue_edit", fields: ["command": .object(["method": .string("queue.select"), "id": .string(entry)])]).encoded())
            state = try wait(first) { $0.root["queue"]["selected"].string == entry && $0.flag("queue_edit_available") }
            try silent(state)
            try write(state.root, to: output.appendingPathComponent("selected.json"))
            _ = try first.send(actionJson: state.action("close_controller").encoded())
            state = try wait(first) { $0.flag("ready_to_close") }
            try write(state.root, to: output.appendingPathComponent("closed-controller.json"))
            controller = nil
            let second = NativeController()
            _ = try second.send(actionJson: envelope("attach", fields: ["runtime": .string(runtime)]).encoded())
            state = try wait(second) { $0.flag("connected") && $0.flag("queue_edit_available") }
            try check(state.state["owner_id"] == ownerID, "Closing the controller must leave the same Engine alive")
            try check(state.root["queue"]["selected"].string == entry, "Reattachment must retain actual queue intent")
            try silent(state)
            try write(state.root, to: output.appendingPathComponent("reattached.json"))
            _ = try second.send(actionJson: state.action("close_controller").encoded())
            _ = try wait(second) { $0.flag("ready_to_close") }
            // Application termination calls Store close. It must stop the attached Engine.
            let quitting = ControllerStore()
            quitting.runtime = runtime
            quitting.executable = executable
            let quitDeadline = Date().addingTimeInterval(15)
            while quitting.snapshot == nil && Date() < quitDeadline { RunLoop.current.run(until: Date().addingTimeInterval(0.01)) }
            quitting.connectAutomatically()
            while !quitting.flag("connected") && Date() < quitDeadline { RunLoop.current.run(until: Date().addingTimeInterval(0.01)) }
            try check(quitting.flag("connected") && quitting.snapshot?.state["owner_id"] == ownerID, "The quitting Store must attach the same Engine")
            var quit = false
            quitting.close { quit = true }
            while !quit && Date() < quitDeadline { RunLoop.current.run(until: Date().addingTimeInterval(0.01)) }
            try check(quit, "Quit must finish its bounded cleanup")
            if let ownerPID {
                let deadline = Date().addingTimeInterval(15)
                while Darwin.kill(pid_t(ownerPID), 0) == 0 && Date() < deadline {
                    _ = try first.tick()
                    Thread.sleep(forTimeInterval: 0.01)
                }
                try check(Darwin.kill(pid_t(ownerPID), 0) != 0 && errno == ESRCH, "Quit must retire the actual owned process")
            }
            shutdownFinished = true
            try check(try Data(contentsOf: fixture) == fixtureBytes, "Generated media bytes must stay unchanged")
            try write(.object(["status": .string("pass"), "owner_id": ownerID, "pid": ownerPID.map(JSONValue.uint) ?? .null, "output_selected": .bool(false), "source_started": .bool(false), "controller_close_kept_owner": .bool(true), "explicit_shutdown": .bool(true)]), to: output.appendingPathComponent("result.json"))
            print("PASS real Swift/Rust/Engine launch, queue append/select, close, reattach, Quit")
        } catch {
            if let controller, let snapshot = try? controller.snapshot() { try? Data(snapshot.utf8).write(to: output.appendingPathComponent("failure-snapshot.json")) }
            try? Data(String(describing: error).utf8).write(to: output.appendingPathComponent("failure.txt"))
            throw error
        }
    }
    @MainActor
    static func attachOnly(runtime: String, executable: String, ownerID: JSONValue, output: URL) throws {
        let store = ControllerStore()
        store.runtime = runtime
        store.executable = executable
        defer {
            // Store close is Quit, which stops an attached Engine. Detach first; this probe must leave it running.
            if store.flag("attached") {
                store.send("disconnect")
                let detached = Date().addingTimeInterval(5)
                while store.flag("attached") && Date() < detached { RunLoop.current.run(until: Date().addingTimeInterval(0.01)) }
            }
            var closed = false
            store.close { closed = true }
            let deadline = Date().addingTimeInterval(5)
            while !closed && Date() < deadline { RunLoop.current.run(until: Date().addingTimeInterval(0.01)) }
        }
        let initialDeadline = Date().addingTimeInterval(5)
        while store.snapshot == nil && Date() < initialDeadline { RunLoop.current.run(until: Date().addingTimeInterval(0.01)) }
        try check(store.snapshot != nil, "Attach-only Store needs an initial native snapshot")
        store.connectAutomatically()
        let deadline = Date().addingTimeInterval(15)
        while !store.flag("connected") && store.value("error") == .null && store.error == nil && Date() < deadline {
            RunLoop.current.run(until: Date().addingTimeInterval(0.01))
        }
        guard let snapshot = store.snapshot else { throw CheckFailure.message("Missing attach-only snapshot") }
        let connected = store.flag("connected")
        let spawned = store.value("host")["kind"].string == "owned_running"
        try write(snapshot.root, to: output.appendingPathComponent(ownerID == .null ? "attach-only-absent.json" : "attach-only-existing.json"))
        if spawned {
            // Retire an accidentally started test owner before reporting the regression.
            let cleanup = NativeController()
            _ = try cleanup.send(actionJson: envelope("attach", fields: ["runtime": .string(runtime)]).encoded())
            let state = try wait(cleanup) { $0.flag("control_edit_available") }
            try check(state.state["owner_id"] == snapshot.state["owner_id"], "Cleanup must retain the accidentally started test owner")
            _ = try cleanup.send(actionJson: state.action("shutdown_and_quit").encoded())
            let retired = try wait(cleanup) { $0.flag("ready_to_close") }
            try write(retired.root, to: output.appendingPathComponent("attach-only-regression-cleanup.json"))
            guard let pid = snapshot.root["host"]["pid"].uint else { throw CheckFailure.message("Started test owner lacks a PID") }
            let retirementDeadline = Date().addingTimeInterval(15)
            while Darwin.kill(pid_t(pid), 0) == 0 && Date() < retirementDeadline { Thread.sleep(forTimeInterval: 0.01) }
            try check(Darwin.kill(pid_t(pid), 0) != 0 && errno == ESRCH, "Accidentally started test owner must actually retire")
        }
        try check(!spawned, "Attach-only startup must never launch an Engine")
        if ownerID == .null {
            try check(!connected && (snapshot.root["error"] != .null || store.error != nil), "Missing owner must produce a visible connection error")
        } else {
            try check(connected && snapshot.state["owner_id"] == ownerID, "Attach-only startup must retain the existing owner")
            try silent(snapshot)
        }
    }
    static func envelope(_ kind: String, fields: [String: JSONValue] = [:]) -> JSONValue {
        .object(fields.merging(["version": .uint(1), "kind": .string(kind)]) { _, value in value })
    }
    static func wait(_ controller: NativeController, predicate: (NativeSnapshot) -> Bool) throws -> NativeSnapshot {
        let deadline = Date().addingTimeInterval(15)
        while true {
            let snapshot = try NativeSnapshot(json: controller.tick())
            if predicate(snapshot) { return snapshot }
            if Date() >= deadline { throw CheckFailure.message("Timed out: \(snapshot.root.encoded())") }
            Thread.sleep(forTimeInterval: 0.005)
        }
    }
    static func silent(_ snapshot: NativeSnapshot) throws {
        try check(snapshot.state["muted"].bool, "Control-only tests must remain muted")
        try check(snapshot.state["output"] == .null && snapshot.state["source"] == .null && snapshot.state["session_id"] == .null, "Control-only tests must not select output or start sources")
    }
    static func check(_ condition: @autoclosure () throws -> Bool, _ message: String) throws {
        if try !condition() { throw CheckFailure.message(message) }
    }
    static func write(_ value: JSONValue, to file: URL) throws { try Data(value.encoded(pretty: true).utf8).write(to: file, options: .withoutOverwriting) }
    static func silentWAV() -> Data {
        var data = Data("RIFF".utf8)
        func u16(_ value: UInt16) { data.append(UInt8(value & 255)); data.append(UInt8(value >> 8)) }
        func u32(_ value: UInt32) { for shift in stride(from: 0, to: 32, by: 8) { data.append(UInt8((value >> shift) & 255)) } }
        u32(52); data.append(Data("WAVEfmt ".utf8)); u32(16); u16(1); u16(1)
        u32(48000); u32(96000); u16(2); u16(16); data.append(Data("data".utf8)); u32(16)
        data.append(Data(repeating: 0, count: 16))
        return data
    }
}

enum CheckFailure: Error { case message(String) }
