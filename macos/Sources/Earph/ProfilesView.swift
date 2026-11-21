// ABOUTME: Edits saved listening intent with native fields and one canonical Rust graph draft.
// ABOUTME: Keeps preview, preparation, activation, and correction review as explicit steps.
import SwiftUI

private let plainProfileFields: [String: String] = ["id":"", "name":"", "backend":"core_audio", "route":"system", "device":"mac-output", "setup":"", "mode":"software", "fidelity":"compatible_conversion", "software_cap":"-20000", "hardware_cap":"", "latency":"balanced", "resident":"268435456", "preparation":"268435456", "latency_frames":""]

struct ProfilesView: View {
    @ObservedObject var store: ControllerStore
    let actions: NativeActions
    @State private var selected: String?
    @State private var fields = plainProfileFields
    @State private var loadedReference: JSONValue = .null
    @State private var adoptedProfileLoadCount: UInt64 = 0
    var profiles: JSONValue { actions.value("profiles") }
    var record: JSONValue { profiles["record"] }
    var rows: [ProfileRow] { profiles["catalog"]["entries"].array.compactMap(ProfileRow.init) }
    var body: some View {
        ScrollView {
            VStack(alignment: .leading) {
                HStack {
                    Button("Refresh Profiles") { list(offset: 0) }.disabled(!actions.flag("control_edit_available"))
                    Button("Next Page") { if let next = profiles["catalog"]["next_offset"].uint { list(offset: next) } }
                        .disabled(!actions.flag("control_edit_available") || profiles["catalog"]["next_offset"] == .null)
                    Button("Get Selected Profile") {
                        if let selected { actions.command("profile_action", method: "profiles.get", fields: ["id": .string(selected)]) }
                    }.disabled(selected == nil || !actions.flag("control_edit_available"))
                    Button("Load Saved Draft") { actions.send("profile_draft_load"); actions.send("graph_load_profile") }
                        .disabled(record == .null || record["profile"]["id"].string != selected)
                }
                Table(rows, selection: $selected) {
                    TableColumn("Name", value: \.name)
                    TableColumn("Profile ID", value: \.id)
                    TableColumn("Revision", value: \.revision)
                }.frame(height: 180)
                .onChange(of: selected) { _, value in
                    if value != store.drafts["profile_selected"]?.string { actions.send("discard_prepared_graph") }
                    if value != loadedReference["id"].string { loadedReference = .null }
                }
                Form {
                    Section("Profile Draft") {
                        TextField("Name", text: field("name"))
                        TextField("Profile ID", text: field("id"))
                        HStack {
                            Button("New Plain Draft") { loadedReference = .null; fields = plainProfileFields; actions.send("profile_draft_reset") }
                            Button("Create Profile") { save(update: false) }
                            Button("Update Loaded Revision") { save(update: true) }.disabled(loadedReference == .null)
                        }.disabled(!actions.flag("control_edit_available") || graphDirty)
                        DisclosureGroup("Output, Volume, and Resource Intent") {
                            Picker("Backend", selection: field("backend")) { Text("Core Audio").tag("core_audio"); Text("ALSA").tag("alsa") }
                            Picker("Route", selection: field("route")) { Text("System").tag("system"); Text("Direct Hardware").tag("direct_hardware") }
                            TextField("Configured Device ID", text: field("device"))
                            TextField("Manual Listening Setup", text: field("setup"))
                            Picker("Volume mode", selection: field("mode")) { Text("Software").tag("software"); Text("Fixed").tag("fixed") }
                            FidelityPicker(value: field("fidelity"))
                            TextField("Software Cap (millidecibels)", text: field("software_cap"))
                            TextField("Hardware Cap (optional)", text: field("hardware_cap"))
                            Picker("Latency", selection: field("latency")) { Text("Balanced").tag("balanced"); Text("Quality Focused").tag("quality_focused"); Text("Low Latency").tag("low_latency") }
                            TextField("Resident Byte Limit", text: field("resident"))
                            TextField("Preparation Byte Limit", text: field("preparation"))
                            TextField("Latency Frame Limit (optional)", text: field("latency_frames"))
                            Button("Resolve Profiles") {
                                store.perform { actions.command("profile_action", method: "profiles.resolve", fields: ["manual_profile_id": selected.map(JSONValue.string) ?? .null, "output": try selector()]) }
                            }.disabled(!actions.flag("control_edit_available"))
                        }
                        Text("Saving does not select output, apply processing, approve, arm, change level, or unmute.")
                    }
                }.formStyle(.grouped)
                ExactObservation("Saved Profile and Correction Documents", value: record)
                ExactObservation("Profile Resolution", value: profiles["resolution"])
                GraphView(store: store, actions: actions, selectedReference: reference)
                CorrectionView(store: store, actions: actions, profileDraft: { try draft() }, profileID: fields["id"] ?? "", loadedReference: loadedReference)
                PackageView(store: store, actions: actions, selectedReference: reference)
            }.padding()
        }
        .onAppear {
            if let saved = store.drafts["profile_fields"]?.object { fields = saved.compactMapValues(\.string) }
            loadedReference = store.drafts["profile_loaded"] ?? .null
            selected = store.drafts["profile_selected"]?.string
            adoptedProfileLoadCount = store.drafts["profile_load_count"]?.uint ?? 0
            adoptLoadedDraft()
        }
        .onDisappear {
            store.drafts["profile_fields"] = .object(fields.mapValues(JSONValue.string))
            store.drafts["profile_loaded"] = loadedReference
            store.drafts["profile_selected"] = selected.map(JSONValue.string) ?? .null
            store.drafts["profile_load_count"] = .uint(adoptedProfileLoadCount)
        }
        .onChange(of: store.profileDraftLoadCount) { _, _ in adoptLoadedDraft() }
    }
    func adoptLoadedDraft() {
        guard adoptedProfileLoadCount != store.profileDraftLoadCount else { return }
        adoptedProfileLoadCount = store.profileDraftLoadCount
        let draft = store.loadedProfileDraft
        guard draft["profile_loaded"] != .null, draft["profile_fields"].object != nil else { return }
        for key in fields.keys { if let text = draft["profile_fields"][key].string { fields[key] = text } }
        loadedReference = draft["profile_loaded"]
    }
    var graphDirty: Bool { store.graphDirty || actions.value("local_draft")["graph_dirty"].bool }
    var reference: JSONValue {
        guard record["profile"]["id"].string == selected else { return .null }
        return .object(["id": record["profile"]["id"], "revision": record["profile"]["revision"], "profile_sha256": record["profile_sha256"]])
    }
    func field(_ key: String) -> Binding<String> {
        Binding(get: { fields[key] ?? "" }, set: { value in
            fields[key] = value
            actions.send("invalidate_correction")
            actions.send("discard_prepared_graph")
        })
    }
    func selector() throws -> JSONValue {
        .object(["backend": .string(fields["backend"] ?? ""), "route": .string(fields["route"] ?? ""), "device_id": .string(fields["device"] ?? ""), "manual_setup": (fields["setup"] ?? "").isEmpty ? .null : .string(fields["setup"] ?? "")])
    }
    func draft() throws -> JSONValue {
        .object([
            "name": .string(fields["name"] ?? ""), "source_policy": .string("local_file"), "fidelity": .string(fields["fidelity"] ?? ""), "output": try selector(),
            "volume": .object(["mode": .string(fields["mode"] ?? ""), "software_cap_millidecibels": .int(try signedNumber(fields["software_cap"] ?? "", name: "Software cap")), "hardware_cap_millidecibels": try optionalSigned(fields["hardware_cap"] ?? "", name: "Hardware cap")]),
            "latency": .object(["mode": .string(fields["latency"] ?? ""), "max_resident_bytes": .uint(try wholeNumber(fields["resident"] ?? "", name: "Resident bytes")), "max_preparation_bytes": .uint(try wholeNumber(fields["preparation"] ?? "", name: "Preparation bytes")), "max_latency_frames": try optionalWhole(fields["latency_frames"] ?? "", name: "Latency frames")]),
            "corrections": .array([])
        ])
    }
    func save(update: Bool) {
        store.perform {
            if actions.value("local_draft")["profile_corrected"].bool && loadedReference != .null { throw DraftError.invalid("Use explicit correction import to edit a corrected profile.") }
            var payload: [String: JSONValue] = ["id": .string(fields["id"] ?? ""), "draft": try draft(), "graph": actions.value("local_draft")["graph"]]
            if update {
                guard loadedReference["id"].string == fields["id"] else { throw DraftError.invalid("Keep the loaded profile ID before updating its revision.") }
                payload["expected_profile_revision"] = loadedReference["revision"]
            }
            actions.command("profile_action", method: update ? "profiles.update" : "profiles.create", fields: payload)
        }
    }
    func list(offset: UInt64) { actions.command("profile_action", method: "profiles.list", fields: ["offset": .uint(offset), "limit": .uint(16)]) }
}

struct ProfileRow: Identifiable {
    let id: String
    let name: String
    let revision: String
    init?(_ value: JSONValue) {
        guard let id = value["id"].string else { return nil }
        self.id = id
        self.name = value["name"].text
        self.revision = value["revision"].text
    }
}

struct GraphView: View {
    @ObservedObject var store: ControllerStore
    let actions: NativeActions
    let selectedReference: JSONValue
    @State private var kind = 0
    var graph: JSONValue { actions.value("local_draft")["graph"] }
    var prepared: JSONValue { actions.value("profiles")["prepared_graph"] }
    var dirty: Bool { store.graphDirty }
    var body: some View {
        GroupBox("DSP Graph") {
            VStack(alignment: .leading) {
                Text("Edit GraphIntent JSON. Accept edits before saving or preparing a graph.")
                NativeTextEditor(text: $store.graphText, label: "GraphIntent JSON").frame(minHeight: 250)
                    .onChange(of: store.graphText) { _, _ in
                        if dirty && prepared != .null { actions.send("discard_prepared_graph") }
                    }
                HStack {
                    Button("Accept Edits") { actions.send("graph_accept_json", fields: ["text": .string(store.graphText)]) }
                    Button("Restore") { store.graphText = actions.value("local_draft")["graph_json"].string ?? ""; actions.send("graph_restore") }
                    Button("Reset Graph") { actions.send("graph_reset") }
                }
                if let error = actions.value("local_draft")["graph_error"].string { Text(error).foregroundStyle(.red) }
                HStack {
                    Button("Inspect Graph") { actions.command("profile_action", method: "graph.inspect") }
                    Button("Validate") { graphCommand("graph.validate") }.disabled(dirty || !actions.flag("graph_source_available") || !actions.flag("graph_review_available"))
                    Button("Prepare Graph") { graphCommand("graph.prepare") }.disabled(dirty || !actions.flag("graph_source_available") || !actions.flag("graph_review_available"))
                    Button("Prepare Selected Profile") { prepareProfile() }.disabled(dirty || selectedReference == .null || !actions.flag("graph_source_available") || !actions.flag("profile_review_available"))
                }.disabled(!actions.flag("control_edit_available"))
                HStack {
                    Button("Commit Prepared Graph") {
                        actions.command("profile_action", method: "graph.commit", fields: ["token": prepared["token"], "graph_hash": prepared["preview"]["graph_hash"]])
                    }.disabled(prepared["preview"]["profile_application"] != .null)
                    Button("Apply Prepared Profile") { applyProfile() }.disabled(prepared["preview"]["profile_application"] == .null)
                }.disabled(dirty || !actions.flag("prepared_action_available"))
                Text("Activation stays muted. Review the actual State and receipt before separate consent, arming, or unmute.")
                DisclosureGroup("Common Graph Nodes") {
                    Picker("Node", selection: $kind) {
                        ForEach(Array(actions.value("local_draft")["graph_node_kinds"].array.enumerated()), id: \.offset) { index, item in Text(item["label"].text).tag(index) }
                    }
                    Button("Add Node") {
                        let kinds = actions.value("local_draft")["graph_node_kinds"].array
                        if kinds.indices.contains(kind) { actions.send("graph_chain_edit", fields: ["operation": .string("add"), "node_kind": kinds[kind]["node_kind"]]) }
                    }.disabled(dirty)
                    ForEach(Array(actions.value("local_draft")["graph_chain_nodes"].array.enumerated()), id: \.offset) { index, node in
                        HStack {
                            Text(node["id"].text)
                            Button("Move Up") { move(index, to: index - 1) }.disabled(index < 2)
                            Button("Move Down") { move(index, to: index + 1) }.disabled(index + 2 >= actions.value("local_draft")["graph_chain_nodes"].array.count)
                            Button("Remove") { actions.send("graph_chain_edit", fields: ["operation": .string("remove"), "from": .uint(UInt64(index))]) }.disabled(index == 0 || index + 1 == actions.value("local_draft")["graph_chain_nodes"].array.count)
                        }
                    }.disabled(dirty)
                    Text("Edit parameters, formats, ports, and arbitrary topology in GraphIntent JSON. Rust validates each accepted edit.")
                }
                ExactObservation("Graph Inspection", value: actions.value("profiles")["graph_inspection"])
                ExactObservation("Graph Diagnostic", value: actions.value("profiles")["graph_diagnostic"])
                ExactObservation("Validation Preview", value: actions.value("profiles")["graph_preview"])
                ExactObservation("Prepared Candidate", value: prepared)
            }.frame(maxWidth: .infinity, alignment: .leading)
        }
    }
    func graphCommand(_ method: String) { actions.command("profile_action", method: method, fields: ["graph": graph["graph"], "processing": graph["processing"]]) }
    func prepareProfile() {
        actions.command("profile_action", method: "profiles.prepare", fields: ["id": selectedReference["id"], "expected_profile_revision": selectedReference["revision"], "profile_sha256": selectedReference["profile_sha256"]])
    }
    func applyProfile() {
        let ref = prepared["preview"]["profile_application"]["profile"]
        actions.command("profile_action", method: "profiles.apply", fields: ["id": ref["id"], "expected_profile_revision": ref["revision"], "profile_sha256": ref["profile_sha256"], "token": prepared["token"], "graph_hash": prepared["preview"]["graph_hash"]])
    }
    func move(_ from: Int, to: Int) { actions.send("graph_chain_edit", fields: ["operation": .string("move"), "from": .uint(UInt64(from)), "to": .uint(UInt64(to))]) }
}
