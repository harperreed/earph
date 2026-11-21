// ABOUTME: Uses a system sidebar and toolbar to expose each existing controller workflow.
// ABOUTME: Shows actual Engine observations and errors beside separate local draft controls.
import SwiftUI

enum EarphSection: String, CaseIterable, Identifiable {
    case connect = "Connect", queue = "Queue", output = "Output and Safety", profiles = "Profiles and DSP", diagnostics = "Signal Path and Recovery", receipts = "Requests and Receipts"
    var id: String { rawValue }
    var sidebarTitle: String {
        switch self {
        case .connect: "Connect"
        case .queue: "Queue"
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
    @State private var selection: EarphSection? = .connect
    var body: some View {
        NavigationSplitView {
            List(EarphSection.allCases, selection: $selection) { section in
                Label(section.sidebarTitle, systemImage: section.symbol).tag(section)
            }.navigationTitle("Earph")
                .navigationSplitViewColumnWidth(min: 210, ideal: 240, max: 320)
        } detail: {
            VStack(spacing: 0) {
                if let error = store.error ?? store.value("error").string {
                    HStack {
                        Text(error).textSelection(.enabled).foregroundStyle(.red)
                        Spacer()
                        Button(store.value("error") == .null ? "Dismiss" : "Dismiss and Refresh") { store.dismissError() }
                    }.padding()
                    Divider()
                }
                switch selection ?? .connect {
                case .connect: ConnectView(store: store, actions: actions)
                case .queue: QueueView(store: store, actions: actions)
                case .output: OutputView(store: store, actions: actions)
                case .profiles: ProfilesView(store: store, actions: actions)
                case .diagnostics: DiagnosticsView(store: store, actions: actions)
                case .receipts: ReceiptsView(store: store, actions: actions)
                }
                Divider()
                HStack {
                    Text(store.flag("connected") ? "Connected" : "Disconnected")
                    Text(store.snapshot?.state["transport"].string ?? "No transport observation")
                    if store.flag("busy") { ProgressView().controlSize(.small) }
                    Spacer()
                    Text("\(store.snapshot?.state["muted"].bool == true ? "Muted" : "Mute state unknown or off")")
                }.font(.caption).padding(8)
            }
            .navigationTitle((selection ?? .connect).rawValue)
            .toolbar {
                ToolbarItemGroup {
                    Button { store.send("refresh") } label: { Label("Refresh", systemImage: "arrow.clockwise") }.disabled(!store.flag("attached"))
                    Button("Mute") { store.send("mute") }.disabled(!store.flag("attached"))
                    Button("Stop") { store.send("stop") }.disabled(!store.flag("attached"))
                }
            }
        }
    }
    var actions: NativeActions { NativeActions(store: store, snapshot: store.snapshot) }
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
