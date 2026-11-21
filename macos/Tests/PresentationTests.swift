// ABOUTME: Checks native presentation parsing and action construction at the Rust boundary.
// ABOUTME: Keeps integer identities exact and separates drafts from explicit owner actions.
import Foundation

@main
struct PresentationTests {
    static func main() throws {
        let snapshot = try NativeSnapshot(json: """
        {"version":1,"generation":9007199254740993,"basis_token":"captured-a","queue_basis_token":"captured-q","flags":{"connected":true,"control_edit_available":false},"state":{"revision":18446744073709551615},"receipts":[]}
        """)
        precondition(snapshot.root["generation"].uint == 9007199254740993)
        precondition(snapshot.state["revision"].uint == UInt64.max)
        precondition(snapshot.flag("connected"))
        precondition(!snapshot.flag("control_edit_available"))
        precondition(!snapshot.flag("missing"))
        let action = snapshot.action("queue_edit", fields: ["command": .object([
            "method": .string("queue.select"), "id": .string("owned-entry")
        ])])
        precondition(action["basis_token"].string == "captured-q")
        precondition(action["command"]["method"].string == "queue.select")
        let control = snapshot.action("control_edit", fields: ["command": .object([
            "method": .string("sessions.seek"), "position_frames": .uint(UInt64.max)
        ])])
        let decoded = try JSONDecoder().decode(JSONValue.self, from: Data(control.encoded().utf8))
        precondition(decoded["basis_token"].string == "captured-a")
        precondition(decoded["command"]["position_frames"].uint == UInt64.max)
        precondition(snapshot.action("mute")["basis_token"] == .null)
        precondition(snapshot.action("shutdown_and_quit")["basis_token"].string == "captured-a")
        do {
            _ = try NativeSnapshot(json: "{\"version\":2,\"flags\":{}}")
            preconditionFailure("Unsupported presentation versions must fail")
        } catch {}
        do {
            _ = try NativeSnapshot(json: "{\"version\":1,\"flags\":false}")
            preconditionFailure("Malformed availability must fail closed")
        } catch {}
        var geometry = OutputGeometryDraft()
        let outputAction = snapshot.action("control_edit", fields: ["command": .object([
            "method": .string("output.select"), "output": .object(try geometry.fields())
        ])])
        let output = try JSONDecoder().decode(JSONValue.self, from: Data(outputAction.encoded().utf8))
        precondition(output["basis_token"].string == "captured-a")
        precondition(output["command"]["output"]["period_frames"].uint == 64)
        precondition(output["command"]["output"]["buffer_frames"].uint == 4096, "Mac output defaults must provide the measured 4096-frame headroom")
        geometry.buffer = "256"
        let explicit = try geometry.fields()
        precondition(explicit["buffer_frames"]?.uint == 256, "Explicit buffer choices must remain unchanged")
        geometry.buffer = "-1"
        do {
            _ = try geometry.fields()
            preconditionFailure("Invalid geometry must fail before constructing an action")
        } catch {}
        print("PASS exact counters, captured actions, output geometry, availability, and malformed snapshots")
    }
}
