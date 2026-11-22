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
        precondition(snapshot.action("play_local", fields: ["takeover": .bool(false)])["basis_token"].string == "captured-a",
                     "Play must retain the exact visible owner basis")
        precondition(snapshot.action("play_local", fields: ["takeover": .bool(false), "entry_id": .string("local-42")])["entry_id"].string == "local-42")
        let playing = try NativeSnapshot(json: """
        {"version":1,"flags":{"connected":true,"busy":true},"state":{"owner_id":"owner-a","transport":"playing","muted":false,"active_entry_id":"track-a"}}
        """)
        precondition(!playing.showsPlaybackProgress, "A background refresh must not animate steady playback")
        let connecting = try NativeSnapshot(json: """
        {"version":1,"flags":{"busy":true,"connected":false},"state":null}
        """)
        precondition(connecting.showsPlaybackProgress, "An actual connection attempt still shows progress")
        let preparing = try NativeSnapshot(json: """
        {"version":1,"flags":{"connected":true,"busy":false},"state":{"transport":"preparing"}}
        """)
        precondition(preparing.showsPlaybackProgress, "Native source preparation still shows progress")
        let buffering = try NativeSnapshot(json: """
        {"version":1,"flags":{"connected":true},"state":{"transport":"buffering"}}
        """)
        precondition(buffering.showsPlaybackProgress, "Actual buffering still shows progress")
        let disconnected = try NativeSnapshot(json: """
        {"version":1,"flags":{"connected":false,"busy":false},"state":{"transport":"preparing"}}
        """)
        precondition(!disconnected.showsPlaybackProgress, "Disconnected observations must not imply active preparation")
        let starting = try NativeSnapshot(json: """
        {"version":1,"flags":{"connected":true,"busy":true,"play_local_pending":true},"state":{"transport":"playing"}}
        """)
        precondition(starting.showsPlaybackProgress, "A requested track change still shows progress")
        precondition(playing.pausesPlayback(selectedEntry: .string("track-a"), selectionOwner: .string("owner-a")))
        precondition(!playing.pausesPlayback(selectedEntry: .string("track-b"), selectionOwner: .string("owner-a")),
                     "Selecting another track must offer Play without first pausing the active track")
        precondition(!playing.pausesPlayback(selectedEntry: .string("track-a"), selectionOwner: .string("owner-b")))
        precondition(playing.pausesPlayback(selectedEntry: .null, selectionOwner: .null))
        let paused = try NativeSnapshot(json: """
        {"version":1,"flags":{"connected":true,"busy":true},"state":{"owner_id":"owner-a","transport":"paused","muted":false,"active_entry_id":"track-a"}}
        """)
        precondition(!paused.showsPlaybackProgress, "A background refresh must not animate paused playback")
        precondition(!paused.pausesPlayback(selectedEntry: .string("track-a"), selectionOwner: .string("owner-a")))
        let muted = try NativeSnapshot(json: """
        {"version":1,"flags":{},"state":{"owner_id":"owner-a","transport":"playing","muted":true,"active_entry_id":"track-a"}}
        """)
        precondition(!muted.pausesPlayback(selectedEntry: .string("track-a"), selectionOwner: .string("owner-a")))
        let firstPlay = UUID(uuidString: "00000000-0000-0000-0000-000000000001")!
        let secondPlay = UUID(uuidString: "00000000-0000-0000-0000-000000000002")!
        let queuedSelection = PlaybackSelection(entryID: "track-a", owner: .string("owner-a"), phase: .queued(firstPlay))
        precondition(queuedSelection.observing(playing)?.phase == .queued(firstPlay),
                     "An older background read cannot consume an enqueued selection")
        let preparingSnapshot = try NativeSnapshot(json: """
        {"version":1,"flags":{"play_local_pending":true},"state":{"owner_id":"owner-a"},"error":null}
        """)
        let preparingSelection = queuedSelection.observing(preparingSnapshot, completionID: firstPlay)
        precondition(preparingSelection?.phase == .preparing(firstPlay))
        precondition(preparingSelection?.observing(playing) == nil,
                     "A completed Play consumes its selection even if the queue has already advanced")
        let refreshingQueue = try NativeSnapshot(json: """
        {"version":1,"flags":{"play_local_pending":false,"queue_stale":true},"state":{"owner_id":"owner-a"},"error":"Queue changed during read","action_error":null,"observation_error":"Queue changed during read"}
        """)
        precondition(preparingSelection?.observing(refreshingQueue) == nil,
                     "A queue refresh warning must not turn completed playback into a refusal")
        let refusedPlay = try NativeSnapshot(json: """
        {"version":1,"flags":{"play_local_pending":false},"state":{"owner_id":"owner-a"},"error":"Output changed","action_error":"Output changed","observation_error":null}
        """)
        precondition(queuedSelection.observing(refusedPlay, completionID: firstPlay)?.phase == .selected,
                     "A refused Play must preserve the chosen track for retry")
        let nextSelection = PlaybackSelection(entryID: "track-b", owner: .string("owner-a"))
        precondition(nextSelection.observing(playing, completionID: firstPlay)?.entryID == "track-b",
                     "An older Play completion cannot consume a later selection")
        let queuedNext = PlaybackSelection(entryID: "track-b", owner: .string("owner-a"), phase: .queued(secondPlay))
        let afterOldRefusal = queuedNext.observing(refusedPlay, completionID: firstPlay)
        precondition(afterOldRefusal?.phase == .queued(secondPlay), "An old refusal cannot reset a newer queued Play")
        precondition(afterOldRefusal?.failed(completionID: firstPlay).phase == .queued(secondPlay),
                     "An old driver failure cannot reset a newer queued Play")
        let preparingNext = afterOldRefusal?.observing(preparingSnapshot, completionID: secondPlay)
        precondition(preparingNext?.phase == .preparing(secondPlay))
        precondition(preparingNext?.observing(playing) == nil)
        let quiet = try NativeSnapshot(json: """
        {"version":1,"flags":{"connected":true},"state":{"software_gain_db":-30,"software_gain_zero":false,"volume_policy":{"software_cap_db":-20}}}
        """)
        precondition(PlaybackVolume(snapshot: quiet).decibels == -30)
        precondition(PlaybackVolume(snapshot: quiet).maximumDecibels == -20)
        let volumeCommand = try PlaybackVolume.command(decibels: -29.5, rampFrames: 256)
        precondition(volumeCommand["millidecibels"] == .int(-29500))
        precondition(volumeCommand["ramp_frames"] == .uint(256))
        precondition(snapshot.action("set_volume", fields: volumeCommand)["basis_token"].string == "captured-a")
        let capped = try NativeSnapshot(json: """
        {"version":1,"flags":{},"state":{"software_gain_db":-30,"volume_policy":{"software_cap_db":-20.0004}}}
        """)
        precondition(PlaybackVolume(snapshot: capped).maximumDecibels == -20.001)
        let fractionalVolume = try PlaybackVolume.command(decibels: -20.0004, rampFrames: 64)
        precondition(fractionalVolume["millidecibels"] == .int(-20001), "Volume quantization must stay below its cap")
        do {
            _ = try PlaybackVolume.command(decibels: .nan, rampFrames: 64)
            preconditionFailure("Nonfinite volume must not construct an action")
        } catch {}
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
