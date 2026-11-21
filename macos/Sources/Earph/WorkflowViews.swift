// ABOUTME: Presents ordinary native forms for explicit Engine connection, queue, output, and transport actions.
// ABOUTME: Keeps each playback and safety decision separate from local draft edits.
import AppKit
import SwiftUI

struct ConnectView: View {
    @ObservedObject var store: ControllerStore
    let actions: NativeActions
    @State private var confirmShutdown = false
    @State private var shutdownBasis: NativeSnapshot?
    var body: some View {
        Form {
            Section("Local Engine") {
                Text(actions.snapshot?.flag("connected") == true ? "Connected to the local Engine." : "Start the Engine or connect to an Engine already running.")
                HStack {
                    Button("Start Engine") { actions.send("launch", fields: ["runtime": .string(store.runtime), "executable": .string(store.executable)]) }
                    Button("Connect") { actions.send("attach", fields: ["runtime": .string(store.runtime)]) }
                }.disabled(actions.flag("attached") || actions.flag("busy"))
                HStack {
                    Button("Refresh") { actions.send("refresh") }.disabled(!actions.flag("attached"))
                    Button("Disconnect") { actions.send("disconnect") }.disabled(!actions.flag("attached"))
                }
                Text("Closing Earph leaves the Engine running.")
                DisclosureGroup("Advanced") {
                    TextField("Runtime directory", text: $store.runtime).accessibilityIdentifier("runtime-directory")
                    TextField("Engine executable", text: $store.executable).accessibilityIdentifier("owner-executable")
                    Button("Choose Engine…") { store.chooseEngine() }
                    Text("The runtime path must be absolute, private, and at most 60 UTF-8 bytes.")
                    ExactObservation("Host observation", value: actions.value("host"))
                }
            }
            Section("Shared Engine") {
                Button("Stop Engine and Quit…") { shutdownBasis = actions.snapshot; confirmShutdown = true }.disabled(!actions.flag("connected"))
            }
        }
        .formStyle(.grouped)
        .confirmationDialog("Stop the shared Engine?", isPresented: $confirmShutdown) {
            Button("Stop Engine and Quit", role: .destructive) { store.shutdownAndQuit(captured: shutdownBasis) }
            Button("Cancel", role: .cancel) {}
        } message: { Text("This stops playback for every connected controller.") }
    }
}

struct QueueView: View {
    @ObservedObject var store: ControllerStore
    let actions: NativeActions
    @State private var takeover = false
    @State private var seek = "0"
    var rows: [QueueRow] { actions.value("queue")["entries"].array.compactMap(QueueRow.init) }
    var selected: String? {
        guard !actions.flag("queue_stale") else { return nil }
        if let intent = store.trackSelection, intent.owner == actions.snapshot?.state["owner_id"] { return intent.entryID }
        return actions.snapshot?.state["active_entry_id"].string ?? actions.value("queue")["selected"].string
    }
    var body: some View {
        VStack(alignment: .leading) {
            HStack {
                Button("Add Folder…") { store.chooseFolder() }.disabled(!actions.flag("queue_edit_available"))
                Spacer()
                Text("\(actions.value("queue")["total"].uint ?? 0) tracks")
            }
            if rows.isEmpty {
                ContentUnavailableView("Open some music", systemImage: "music.note",
                                       description: Text("Choose Open to add audio files, then press Play."))
            } else {
                Table(rows, selection: Binding(get: { selected }, set: { id in
                    store.selectTrack(id, owner: actions.snapshot?.state["owner_id"] ?? .null)
                })) {
                    TableColumn("Track", value: \.name)
                    TableColumn("Status", value: \.status)
                }.frame(minHeight: 220)
                .disabled(!actions.flag("queue_edit_available"))
            }
            HStack {
                Button("Move Up") { move(-1) }.disabled(!canMove(-1))
                Button("Move Down") { move(1) }.disabled(!canMove(1))
                Button("Remove") {
                    if let selected { actions.command("queue_edit", method: "queue.remove", fields: ["id": .string(selected)]) }
                }.disabled(selected == nil || !actions.flag("queue_edit_available"))
                Button("Clear Selection") { store.selectTrack(nil, owner: actions.snapshot?.state["owner_id"] ?? .null) }
                    .disabled(!actions.flag("queue_edit_available"))
            }
            if actions.flag("queue_stale") { Text("Waiting for a current queue observation.").foregroundStyle(.secondary) }
            DisclosureGroup("Advanced Transport") {
                VStack(alignment: .leading) {
                    HStack {
                        Button("Prepare Engine Selection") { actions.send("start_local", fields: ["takeover": .bool(takeover)]) }
                            .disabled(!actions.flag("queue_edit_available") || actions.value("queue")["selected"] == .null)
                        Button("Resume") { actions.command("control_edit", method: "sessions.resume") }.disabled(!actions.flag("control_edit_available"))
                        Button("Refresh") { actions.send("refresh") }.disabled(!actions.flag("attached"))
                    }
                    Text("Prepare uses the retained Engine selection shown below and leaves audio muted. Use Play for the highlighted track.")
                    LabeledContent("Engine selection", value: rows.first { $0.id == actions.value("queue")["selected"].string }?.name ?? "None")
                    Toggle("Allow takeover of the retained session", isOn: $takeover)
                    HStack {
                        TextField("Source frame", text: $seek)
                        Button("Seek") {
                            store.perform {
                                let frame = try wholeNumber(seek, name: "Source frame")
                                actions.command("control_edit", method: "sessions.seek", fields: ["position_frames": .uint(frame)])
                            }
                        }.disabled(!actions.flag("control_edit_available") || actions.value("flags")["seek_bounds"] == .null)
                    }
                    ExactObservation("Seek bounds", value: actions.value("flags")["seek_bounds"])
                    ExactObservation("Playback observations", value: actions.snapshot?.state ?? .null)
                    ExactObservation("Queue details", value: actions.value("queue"))
                }.frame(maxWidth: .infinity, alignment: .leading)
            }
        }.padding()
        .onAppear {
            takeover = store.drafts["queue"]?["takeover"].bool ?? false
            seek = store.drafts["queue"]?["seek"].string ?? "0"
        }
        .onDisappear {
            var saved = store.drafts["queue"]?.object ?? [:]
            saved["takeover"] = .bool(takeover); saved["seek"] = .string(seek)
            store.drafts["queue"] = .object(saved)
        }
    }
    private func canMove(_ delta: Int) -> Bool {
        guard actions.flag("queue_edit_available"), let index = rows.firstIndex(where: { $0.id == selected }) else { return false }
        return index + delta >= 0 && index + delta < Int(actions.value("queue")["total"].uint ?? 0)
    }
    private func move(_ delta: Int) {
        guard let selected, let index = rows.firstIndex(where: { $0.id == selected }), canMove(delta) else { return }
        actions.command("queue_edit", method: "queue.reorder", fields: ["id": .string(selected), "new_index": .uint(UInt64(index + delta))])
    }
}

struct QueueRow: Identifiable {
    let id: String
    let name: String
    let status: String
    init?(_ value: JSONValue) {
        guard let id = value["id"].string, let path = value["path"].string else { return nil }
        self.id = id
        self.name = URL(fileURLWithPath: path).lastPathComponent
        switch value["status"]["state"].string {
        case "unprepared": self.status = "Queued"
        case "preparing": self.status = "Opening…"
        case "ready": self.status = "Ready"
        case "dsd_ready": self.status = "DSD file"
        case "failed": self.status = "Unavailable"
        default: self.status = value["status"]["state"].string?.capitalized ?? "Unknown"
        }
    }
}

struct OutputView: View {
    @ObservedObject var store: ControllerStore
    let actions: NativeActions
    @State private var backend = "core_audio"
    @State private var endpoint = ""
    @State private var route = "system"
    @State private var device = "mac-output"
    @State private var profile = "listening"
    @State private var geometry = OutputGeometryDraft()
    @State private var mode = "software"
    @State private var fidelity = "compatible_conversion"
    @State private var level = "-30000"
    @State private var ramp = "64"
    @State private var softwareCap = "-20000"
    @State private var hardwareCap = ""
    @State private var inventoryRequested = false
    var state: JSONValue { actions.snapshot?.state ?? .null }
    var devices: [JSONValue] { actions.value("discovery")["observation"]["devices"].array }
    var body: some View {
        Form {
            Section("Output") {
                HStack {
                    Picker("Device", selection: $endpoint) {
                        Text("Choose an output").tag("")
                        ForEach(devices, id: \.self) { item in
                            Text(item["display_name"].string ?? item["endpoint"].text).tag(item["endpoint"].string ?? "")
                        }
                    }
                    Button("Refresh Devices") { actions.send("discover", fields: ["backend": .string(backend), "endpoint": .null]) }
                        .disabled(!actions.flag("control_edit_available"))
                }
                Button("Select Output") { selectOutput() }.disabled(!actions.flag("control_edit_available") || endpoint.isEmpty)
                Text("Selected output: \(selectedDeviceName)")
                Text(state["output_consented"].bool ? "Audio output approved." : "Approve the selected output to allow sound.")
                Button("Approve Audio Output") { currentOutput("output.consent") }
                    .disabled(!actions.flag("control_edit_available") || state["output"] == .null || state["output_consented"].bool)
            }
            Section("Volume") { PlaybackVolumeControl(store: store) }
            DisclosureGroup("Advanced Output and Safety") {
                TextField("Level (millidecibels)", text: $level)
                Text("−30000 means −30 dB. This draft changes no Engine setting until you choose Set Level.")
                Button("Set Level") {
                    store.perform {
                        actions.command("control_edit", method: "volume.set", fields: ["millidecibels": .int(try signedNumber(level, name: "Level")), "ramp_frames": .uint(try wholeNumber(ramp, name: "Ramp frames"))])
                    }
                }.disabled(!actions.flag("control_edit_available"))
                Text("Observed level: \(state["software_gain_db"].text) dB · exact zero: \(state["software_gain_zero"].bool ? "Yes" : "No")")
                Picker("Backend", selection: $backend) { Text("Core Audio").tag("core_audio"); Text("ALSA").tag("alsa") }
                Picker("Route", selection: $route) { Text("System").tag("system"); Text("Direct Hardware").tag("direct_hardware") }
                TextField("Endpoint", text: $endpoint)
                HStack {
                    Button("Inspect Endpoint") { actions.send("discover", fields: ["backend": .string(backend), "endpoint": .string(endpoint)]) }
                    Button("Arm Current Fixed Output") { currentOutput("volume.arm_fixed") }.disabled(state["output"]["volume_mode"].string != "fixed")
                }.disabled(!actions.flag("control_edit_available"))
                TextField("Configured Device ID", text: $device)
                TextField("Configured Profile ID", text: $profile)
                TextField("Period frames", text: $geometry.period)
                TextField("Buffer frames", text: $geometry.buffer)
                Picker("Volume mode", selection: $mode) { Text("Software").tag("software"); Text("Fixed").tag("fixed") }
                FidelityPicker(value: $fidelity)
                TextField("Ramp frames", text: $ramp)
                TextField("Software cap (millidecibels)", text: $softwareCap)
                TextField("Hardware cap (optional millidecibels)", text: $hardwareCap)
                Button("Set Independent Caps") {
                    store.perform {
                        actions.command("control_edit", method: "volume.caps.set", fields: ["software_millidecibels": .int(try signedNumber(softwareCap, name: "Software cap")), "hardware_millidecibels": hardwareCap.isEmpty ? .null : .int(try signedNumber(hardwareCap, name: "Hardware cap"))])
                    }
                }.disabled(!actions.flag("control_edit_available"))
                Text("Hardware volume is unsupported. Fixed arming has no State readback.")
                ExactObservation("Device Inventory", value: actions.value("discovery"))
                ExactObservation("Device Inspection", value: actions.value("inspection"))
                ExactObservation("Actual State", value: state)
            }
        }.formStyle(.grouped)
        .onAppear {
            if let saved = store.drafts["output"] {
                backend = saved["backend"].string ?? backend; endpoint = saved["endpoint"].string ?? endpoint
                route = saved["route"].string ?? route; device = saved["device"].string ?? device
                profile = saved["profile"].string ?? profile; geometry.period = saved["period"].string ?? geometry.period
                geometry.buffer = saved["buffer"].string ?? geometry.buffer; mode = saved["mode"].string ?? mode
                fidelity = saved["fidelity"].string ?? fidelity; level = saved["level"].string ?? level
                ramp = saved["ramp"].string ?? ramp; softwareCap = saved["software_cap"].string ?? softwareCap
                hardwareCap = saved["hardware_cap"].string ?? hardwareCap
            }
            loadDevicesIfNeeded()
        }
        .onChange(of: actions.flag("control_edit_available")) { _, ready in if ready { loadDevicesIfNeeded() } }
        .onChange(of: actions.snapshot?.state["owner_id"]) { _, _ in inventoryRequested = false; loadDevicesIfNeeded() }
        .onDisappear {
            store.drafts["output"] = .object(["backend": .string(backend), "endpoint": .string(endpoint), "route": .string(route), "device": .string(device), "profile": .string(profile), "period": .string(geometry.period), "buffer": .string(geometry.buffer), "mode": .string(mode), "fidelity": .string(fidelity), "level": .string(level), "ramp": .string(ramp), "software_cap": .string(softwareCap), "hardware_cap": .string(hardwareCap)])
        }
    }
    private var selectedDeviceName: String {
        let selected = state["output"]["endpoint"].string
        return devices.first { $0["endpoint"].string == selected }?["display_name"].string ?? selected ?? "None"
    }
    private func loadDevicesIfNeeded() {
        guard !inventoryRequested, actions.flag("control_edit_available"), actions.value("discovery") == .null else { return }
        inventoryRequested = actions.send("discover", fields: ["backend": .string(backend), "endpoint": .null])
    }
    private func currentOutput(_ method: String) {
        actions.command("control_edit", method: method, fields: ["device_id": state["output"]["device_id"], "profile_id": state["output"]["profile_id"]])
    }
    private func selectOutput() {
        store.perform {
            let output: JSONValue = .object(try geometry.fields().merging([
                "backend": .string(backend), "route": .string(route), "endpoint": .string(endpoint),
                "device_id": .string(device), "profile_id": .string(profile), "volume_mode": .string(mode), "fidelity": .string(fidelity)
            ]) { _, value in value })
            actions.command("control_edit", method: "output.select", fields: ["output": output])
        }
    }
}

struct FidelityPicker: View {
    @Binding var value: String
    var body: some View {
        Picker("Fidelity", selection: $value) {
            Text("Compatible Conversion").tag("compatible_conversion")
            Text("Strict Preservation").tag("strict_preservation")
            Text("Expert Override").tag("expert_override")
        }
    }
}
