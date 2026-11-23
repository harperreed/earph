// ABOUTME: Exercises the serial Swift driver against the real Rust native controller.
// ABOUTME: Checks bounded close and duplicate close handling without opening audio output.
import AppKit
import Foundation

@main
struct ControllerLifecycleTests {
    @MainActor
    static func main() throws {
        _ = NSApplication.shared
        let store = ControllerStore()
        let chosenRuntime = "/private/tmp/earph-explicit-draft"
        store.runtime = chosenRuntime
        let initialDeadline = Date().addingTimeInterval(5)
        while store.snapshot == nil && Date() < initialDeadline {
            RunLoop.current.run(until: Date().addingTimeInterval(0.01))
        }
        precondition(store.snapshot != nil, "The serial driver must publish a real Rust snapshot")
        precondition(store.snapshot?.flag("attached") == false)
        precondition(store.snapshot?.state["output"] == .null)
        precondition(store.runtime == chosenRuntime, "Snapshot updates must preserve explicit runtime drafts")
        store.graphText = "unfinished JSON draft"
        let oldGeneration = store.snapshot?.root["generation"]
        let draftDeadline = Date().addingTimeInterval(2)
        while store.snapshot?.root["generation"] == oldGeneration && Date() < draftDeadline {
            RunLoop.current.run(until: Date().addingTimeInterval(0.01))
        }
        precondition(store.graphText == "unfinished JSON draft" && store.graphDirty)
        store.shutdownAndQuit()
        let refusedShutdownDeadline = Date().addingTimeInterval(2)
        while store.value("error") == .null && store.error == nil && Date() < refusedShutdownDeadline {
            RunLoop.current.run(until: Date().addingTimeInterval(0.01))
        }
        precondition(store.send("refresh"), "A refused shutdown must leave recovery actions available")
        precondition(store.send("attach", fields: ["runtime": .string("relative")]))
        let refusedAttachDeadline = Date().addingTimeInterval(2)
        while store.value("error") == .null && Date() < refusedAttachDeadline {
            RunLoop.current.run(until: Date().addingTimeInterval(0.01))
        }
        precondition(store.value("error") != .null, "The real controller must refuse a relative runtime")
        store.dismissError()
        let dismissDeadline = Date().addingTimeInterval(2)
        while (store.value("error") != .null || store.error != nil) && Date() < dismissDeadline {
            RunLoop.current.run(until: Date().addingTimeInterval(0.01))
        }
        precondition(store.value("error") == .null && store.error == nil, "Dismiss must clear the displayed Rust refusal")
        precondition(store.snapshot?.flag("attached") == false, "Dismissing an error must not attach or start an Engine")
        precondition(store.runtime == chosenRuntime && store.snapshot?.state["output"] == .null)
        var first = 0
        var second = 0
        store.close { first += 1 }
        store.close { second += 1 }
        let closeDeadline = Date().addingTimeInterval(5)
        while (first == 0 || second == 0) && Date() < closeDeadline {
            RunLoop.current.run(until: Date().addingTimeInterval(0.01))
        }
        precondition(first == 1 && second == 1, "Every close callback must complete once")
        precondition(store.snapshot?.flag("ready_to_close") == true)
        precondition(!store.send("refresh"), "Closed controllers must reject new work")
        print("PASS actual Rust snapshots, disconnected safety, and bounded duplicate close")
    }
}
