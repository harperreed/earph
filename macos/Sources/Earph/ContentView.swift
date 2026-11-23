// ABOUTME: Uses a system sidebar and toolbar to expose each existing controller workflow.
// ABOUTME: Shows actual Engine observations and errors beside separate local draft controls.
import SwiftUI

enum EarphSection: String, CaseIterable, Identifiable {
    case queue = "Music", output = "Output", profiles = "Profiles", connect = "Connection", diagnostics = "Diagnostics", receipts = "Requests"
    var id: String { rawValue }
    var sidebarTitle: String {
        switch self {
        case .connect: "Connection"
        case .queue: "Music"
        case .output: "Output"
        case .profiles: "Profiles"
        case .diagnostics: "Diagnostics"
        case .receipts: "Requests"
        }
    }
    var symbol: String {
        switch self {
        case .connect: "powerplug"
        case .queue: "music.note.list"
        case .output: "speaker.wave.2"
        case .profiles: "slider.horizontal.3"
        case .diagnostics: "waveform.path"
        case .receipts: "list.bullet.rectangle"
        }
    }
}

struct ContentView: View {
    @ObservedObject var store: ControllerStore
    @State private var selection: EarphSection? = .queue
    var body: some View {
        let actions = NativeActions(store: store, snapshot: store.snapshot)
        let playing = actions.snapshot?.pausesPlayback(selectedEntry: store.trackSelection?.entryToPlay ?? .null,
                                                      selectionOwner: store.trackSelection?.owner ?? .null) ?? false
        NavigationSplitView {
            List(selection: $selection) {
                ForEach([EarphSection.queue, .output, .profiles]) { section in
                    Label(section.sidebarTitle, systemImage: section.symbol).tag(section)
                }
                DisclosureGroup("Advanced") {
                    ForEach([EarphSection.connect, .diagnostics, .receipts]) { section in
                        Label(section.sidebarTitle, systemImage: section.symbol).tag(section)
                    }
                }
            }.navigationTitle("Earph")
                .navigationSplitViewColumnWidth(min: 210, ideal: 240, max: 320)
        } detail: {
            VStack(spacing: 0) {
                if let error = store.error ?? store.value("action_error").string {
                    HStack {
                        Text(error).textSelection(.enabled).foregroundStyle(.red)
                        Spacer()
                        Button(store.value("error") == .null ? "Dismiss" : "Dismiss and Refresh") { store.dismissError() }
                    }.padding()
                    Divider()
                }
                if store.flag("connected"), store.snapshot?.state["output_consented"].bool != true, selection != .output {
                    HStack {
                        Label("Choose and approve your audio output before the first play.", systemImage: "speaker.wave.2")
                        Spacer()
                        Button("Set Up Audio…") { selection = .output }
                    }.padding()
                    Divider()
                }
                switch selection ?? .queue {
                case .connect: ConnectView(store: store, actions: actions)
                case .queue: QueueView(store: store, actions: actions)
                case .output: OutputView(store: store, actions: actions)
                case .profiles: ProfilesView(store: store, actions: actions)
                case .diagnostics: DiagnosticsView(store: store, actions: actions)
                case .receipts: ReceiptsView(store: store, actions: actions)
                }
                Divider()
                HStack {
                    if !store.flag("connected") { Text(store.flag("busy") ? "Connecting…" : "Not connected") }
                    if let status = store.value("playback_action_status").string, !status.isEmpty {
                        Text(status)
                    } else if let transport = store.snapshot?.state["transport"].string {
                        Text(transport.capitalized)
                    }
                    if store.snapshot?.showsPlaybackProgress == true { ProgressView().controlSize(.small) }
                    if let warning = store.value("observation_error").string {
                        Text("Refreshing music…").help(warning)
                    }
                    Spacer()
                    if store.snapshot?.state["muted"].bool == true { Text("Muted") }
                    else if store.snapshot?.state["software_gain_zero"].bool == true { Text("Silent") }
                    if !store.flag("attached"), !store.flag("busy") {
                        Button("Retry Connection") { store.connectAutomatically() }
                    }
                }.font(.caption).padding(8)
            }
            .navigationTitle((selection ?? .queue).rawValue)
            .toolbar {
                ToolbarItemGroup {
                    Button { store.chooseFiles() } label: { Label("Open…", systemImage: "folder") }
                        .disabled(!store.flag("queue_edit_available"))
                    Button {
                        if playing { actions.command("control_edit", method: "sessions.pause") }
                        else { store.play(captured: actions.snapshot) }
                    } label: { Label(playing ? "Pause" : "Play", systemImage: playing ? "pause.fill" : "play.fill") }
                        .disabled(playing ? !store.flag("control_edit_available") : !store.flag("play_local_available"))
                    Button { store.send("stop") } label: { Label("Stop", systemImage: "stop.fill") }
                        .disabled(!store.flag("attached"))
                    Button {
                        if store.snapshot?.state["muted"].bool == true {
                            actions.command("control_edit", method: "mute.set", fields: ["muted": .bool(false)])
                        } else { store.send("mute") }
                    } label: { Label(store.snapshot?.state["muted"].bool == true ? "Unmute" : "Mute", systemImage: store.snapshot?.state["muted"].bool == true ? "speaker.slash" : "speaker.wave.2") }
                        .disabled(!store.flag("attached") || (store.snapshot?.state["muted"].bool == true &&
                            (!store.flag("control_edit_available") || store.snapshot?.state["session_id"] == .null || store.snapshot?.state["output_consented"].bool != true)))
                    PlaybackVolumeControl(store: store)
                }
            }
        }
    }
}

struct PlaybackVolumeControl: View {
    @ObservedObject var store: ControllerStore
    @State private var draft: Double?
    @State private var basis: NativeSnapshot?
    var body: some View {
        let volume = PlaybackVolume(snapshot: store.snapshot)
        if let level = volume.decibels, let maximum = volume.maximumDecibels, maximum > -120, level <= maximum {
            Slider(value: Binding(get: { draft ?? level }, set: { draft = $0 }),
                   in: -120...maximum, step: 0.5, label: { Text("Volume") },
                   onEditingChanged: volumeEditingChanged)
                .frame(width: 160)
                .disabled(!store.flag("volume_edit_available"))
                .help(store.snapshot?.state["software_gain_zero"].bool == true ? "Silent" : "\(level.formatted()) dB")
                .accessibilityValue(store.snapshot?.state["software_gain_zero"].bool == true ? "Silent" : "\(level.formatted()) decibels")
        }
    }
    private func volumeEditingChanged(_ editing: Bool) {
        if editing { basis = store.snapshot }
        else {
            defer { draft = nil; basis = nil }
            guard let draft, let basis, let ramp = basis.state["gain_ramp_frames"].uint else { return }
            store.perform {
                let fields = try PlaybackVolume.command(decibels: draft, rampFrames: ramp)
                NativeActions(store: store, snapshot: basis).send("set_volume", fields: fields)
            }
        }
    }
}

@MainActor
struct NativeActions {
    let store: ControllerStore
    let snapshot: NativeSnapshot?
    func value(_ key: String) -> JSONValue { snapshot?.root[key] ?? .null }
    func flag(_ key: String) -> Bool { snapshot?.flag(key) ?? false }
    @discardableResult
    func send(_ kind: String, fields: [String: JSONValue] = [:]) -> Bool { store.send(kind, fields: fields, captured: snapshot) }
    func command(_ kind: String, method: String, fields: [String: JSONValue] = [:]) {
        var payload = fields
        payload["method"] = .string(method)
        send(kind, fields: ["command": .object(payload)])
    }
}

struct ExactObservation: View {
    let title: String
    let value: JSONValue
    init(_ title: String, value: JSONValue) { self.title = title; self.value = value }
    var body: some View {
        DisclosureGroup(title) {
            NativeTextEditor(text: .constant(value.text), editable: false, label: title).frame(minHeight: 160)
        }
    }
}

func signedNumber(_ text: String, name: String) throws -> Int64 {
    guard let value = Int64(text) else { throw DraftError.invalid("\(name) must be a whole number.") }
    return value
}
func optionalWhole(_ text: String, name: String) throws -> JSONValue { text.isEmpty ? .null : .uint(try wholeNumber(text, name: name)) }
func optionalSigned(_ text: String, name: String) throws -> JSONValue { text.isEmpty ? .null : .int(try signedNumber(text, name: name)) }
