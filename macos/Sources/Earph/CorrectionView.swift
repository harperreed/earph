// ABOUTME: Reviews correction sources and actual imported documents through native forms and text editors.
// ABOUTME: Submits the Rust preview input without rebuilding parser or FIR provenance in Swift.
import SwiftUI

struct CorrectionView: View {
    @ObservedObject var store: ControllerStore
    let actions: NativeActions
    let profileDraft: () throws -> JSONValue
    let profileID: String
    let loadedReference: JSONValue
    @State private var format = "rew"
    @State private var source = "text_file"
    @State private var path = ""
    @State private var text = ""
    @State private var rate = "48000"
    @State private var channels = "stereo"
    @State private var quantum = "256"
    @State private var preamp = ""
    @State private var purpose = false
    @State private var metadata: [String: String] = [:]
    private let metadataKeys = ["headphone_model", "measurement_provider", "target", "parameter_set", "revision", "redistribution_license"]
    var preview: JSONValue { actions.value("local_review")["correction_preview"] }
    var record: JSONValue { actions.value("profiles")["record"] }
    var asset: JSONValue { actions.value("local_review")["fir_asset"] }
    var reference: JSONValue { .object(["id": record["profile"]["id"], "revision": record["profile"]["revision"], "profile_sha256": record["profile_sha256"]]) }
    var ready: Bool { actions.flag("control_edit_available") && actions.flag("correction_preview_current") && !store.graphDirty }
    var body: some View {
        GroupBox("Correction Source and Provenance") {
            VStack(alignment: .leading) {
                Form {
                    Picker("Format", selection: $format) { Text("REW").tag("rew"); Text("AutoEq").tag("autoeq"); Text("CamillaDSP").tag("camilladsp"); Text("FIR WAV").tag("fir") }
                    Picker("Source", selection: $source) { Text("Source File").tag("text_file"); Text("Pasted Text").tag("text") }
                    HStack {
                        TextField("Source Path", text: $path)
                        Button("Choose…") { store.choosePath(directory: false) { path = $0 } }
                    }
                    NativeTextEditor(text: $text, label: "Correction source text").frame(minHeight: 150)
                    TextField("Sample Rate (Hz)", text: $rate)
                    Picker("Channels", selection: $channels) { Text("Stereo").tag("stereo"); Text("Mono").tag("mono") }
                    TextField("Quantum Frames", text: $quantum)
                    if format == "fir" {
                        TextField("FIR Preamp (dB)", text: $preamp)
                        Toggle("FIR Purpose: Correction", isOn: $purpose)
                    }
                    DisclosureGroup("Authored Metadata") {
                        ForEach(metadataKeys, id: \.self) { key in
                            TextField(key.replacingOccurrences(of: "_", with: " ").capitalized, text: Binding(get: { metadata[key] ?? "" }, set: { metadata[key] = $0 }))
                        }
                    }
                }.formStyle(.grouped)
                HStack {
                    Button("Preview Source") { previewSource() }.disabled(!actions.flag("control_edit_available"))
                    Button("Cancel Local Preview") { actions.send("invalidate_correction") }.disabled(!actions.flag("local_correction_pending"))
                }
                Text(actions.value("local_review")["local_correction_status"].text)
                ExactObservation("Captured Correction Preview", value: preview)
                HStack {
                    Button("Import as New Profile") { importCorrection(update: false) }
                    Button("Import into Loaded Revision") { importCorrection(update: true) }.disabled(loadedReference == .null)
                    Button("Verify FIR Asset") {
                        actions.command("profile_action", method: "assets.import_fir", fields: ["path": preview["path"], "purpose": preview["manifest"]["asset"]["purpose"]])
                    }.disabled(preview["kind"].string != "fir")
                }.disabled(!ready)
                Text("The Engine rereads a FIR path. Local parser facts do not prove native playback, privacy, or redistribution rights.")
                ForEach(Array(record["corrections"].array.enumerated()), id: \.offset) { _, object in
                    ExactObservation("Saved Correction \(object["object_sha256"].text)", value: object["document_json"])
                    if object["kind"].string == "text_report" {
                        Button("Reconstruct Saved Text Source") {
                            actions.send("correction_reconstruct", fields: ["report_json": object["document_json"]])
                        }
                    }
                }
                Button("Review Actual Imported Correction") {
                    actions.send("review_correction", fields: ["owner": actions.snapshot?.state["owner_id"] ?? .null, "reference": reference])
                }.disabled(!actions.flag("control_edit_available") || record["corrections"].array.isEmpty)
                Text("Actual correction reviewed: \(actions.value("local_review")["correction_reviewed"].bool ? "Yes" : "No")")
                ExactObservation("Verified FIR Asset", value: asset)
                HStack {
                    Button("Review Actual Verified FIR Asset") {
                        actions.send("review_fir_asset", fields: ["owner": actions.snapshot?.state["owner_id"] ?? .null, "asset": asset])
                    }.disabled(asset == .null || !actions.flag("control_edit_available"))
                    Button("Add Reviewed FIR to Graph") { actions.send("graph_add_fir") }.disabled(!actions.flag("fir_asset_reviewed") || store.graphDirty)
                }
            }.frame(maxWidth: .infinity, alignment: .leading)
        }
        .onAppear { restoreDraft() }
        .onDisappear { store.drafts["correction"] = draftFields }
        .onChange(of: draftIdentity) { _, _ in
            if store.drafts["correction"] != draftFields && preview != .null { actions.send("invalidate_correction") }
            store.drafts["correction"] = draftFields
        }
        .onChange(of: actions.value("local_draft")["correction_request"]) { _, request in load(request) }
    }
    var draftIdentity: [String] { [format, source, path, text, rate, channels, quantum, preamp, purpose ? "true" : "false"] + metadataKeys.map { metadata[$0] ?? "" } }
    var draftFields: JSONValue {
        .object(["format": .string(format), "source": .string(source), "path": .string(path), "text": .string(text), "rate": .string(rate), "channels": .string(channels), "quantum": .string(quantum), "preamp": .string(preamp), "purpose": .bool(purpose), "metadata": .object(metadata.mapValues(JSONValue.string))])
    }
    func restoreDraft() {
        guard let fields = store.drafts["correction"] else { return }
        format = fields["format"].string ?? format
        source = fields["source"].string ?? source
        path = fields["path"].string ?? path
        text = fields["text"].string ?? text
        rate = fields["rate"].string ?? rate
        channels = fields["channels"].string ?? channels
        quantum = fields["quantum"].string ?? quantum
        preamp = fields["preamp"].string ?? preamp
        purpose = fields["purpose"].bool
        metadata = fields["metadata"].object?.compactMapValues(\.string) ?? [:]
    }
    func previewSource() {
        store.perform {
            var input: [String: JSONValue]
            if format == "fir" {
                guard source != "text", purpose else { throw DraftError.invalid("FIR requires a source file and explicit Correction purpose.") }
                input = ["kind": .string("fir"), "path": .string(path), "purpose": .string("correction")]
            } else {
                input = ["kind": .string(source), "format": .string(format)]
                input[source == "text" ? "text" : "path"] = .string(source == "text" ? text : path)
            }
            var meta: [String: JSONValue] = [:]
            for key in metadataKeys { let value = metadata[key] ?? ""; meta[key] = value.isEmpty ? .null : .string(value) }
            var firPreamp: JSONValue = .null
            if format == "fir" {
                guard let number = Double(preamp), number.isFinite else { throw DraftError.invalid("FIR preamp must be a finite number.") }
                firPreamp = .double(number)
            }
            actions.send("preview_correction", fields: ["request": .object([
                "source": .object(input), "context": .object(["sample_rate_hz": .uint(try wholeNumber(rate, name: "Sample rate")), "channels": .string(channels), "quantum_frames": .uint(try wholeNumber(quantum, name: "Quantum frames"))]), "metadata": .object(meta), "fir_preamp": firPreamp
            ])])
        }
    }
    func importCorrection(update: Bool) {
        store.perform {
            if update && loadedReference["id"].string != profileID { throw DraftError.invalid("Keep the exact loaded profile ID before importing an update.") }
            actions.command("profile_action", method: "profiles.import_correction", fields: ["id": .string(profileID), "expected_profile_revision": update ? loadedReference["revision"] : .null, "draft": try profileDraft(), "input": preview["input"]])
        }
    }
    func load(_ request: JSONValue) {
        guard request != .null else { return }
        let input = request["source"]
        source = input["kind"].string ?? "text"
        format = input["format"].string ?? "fir"
        text = input["text"].string ?? ""
        path = input["path"].string ?? ""
        rate = request["context"]["sample_rate_hz"].text
        channels = request["context"]["channels"].string ?? "stereo"
        quantum = request["context"]["quantum_frames"].text
        for key in metadataKeys { metadata[key] = request["metadata"][key].string ?? "" }
    }
}

struct PackageView: View {
    @ObservedObject var store: ControllerStore
    let actions: NativeActions
    let selectedReference: JSONValue
    @State private var directory = ""
    @State private var destination = ""
    var candidate: JSONValue { actions.value("profiles")["candidate"] }
    var review: JSONValue { actions.value("local_review") }
    var body: some View {
        GroupBox("Packages and Associations") {
            VStack(alignment: .leading) {
                HStack {
                    TextField("Package Directory", text: $directory)
                    Button("Choose…") { store.choosePath(directory: true) { directory = $0 } }
                }
                HStack {
                    TextField("Export Destination", text: $destination)
                    Button("Choose Parent…") { store.choosePath(directory: true) { destination = $0 + "/Earph Package" } }
                }
                HStack {
                    Button("Inspect Package") { actions.command("profile_action", method: "profiles.package_inspect", fields: ["directory": .string(directory)]) }
                    Button("Preview Selected Profile Package") { actions.command("profile_action", method: "profiles.package_preview", fields: ["profile": selectedReference]) }.disabled(selectedReference == .null)
                    Button("Cancel Local Review") { actions.send("cancel_local_package_review") }.disabled(!actions.flag("package_files_pending"))
                }.disabled(!actions.flag("control_edit_available"))
                ExactObservation("Package Candidate", value: candidate)
                ForEach(Array(candidate["review_sources"].array.enumerated()), id: \.offset) { index, source in
                    HStack {
                        Text(source["object"]["kind"].text)
                        Text(source["local_path"].text).textSelection(.enabled)
                        Button("Read Exact Bytes") { actions.send("read_package_object", fields: ["index": .uint(UInt64(index))]) }
                    }.disabled(!actions.flag("control_edit_available"))
                }
                if let exact = review["package_displayed_object"]["text"].string {
                    NativeTextEditor(text: .constant(exact), editable: false, label: "Exact package object bytes").frame(minHeight: 200)
                }
                ExactObservation("Displayed Object and WAV Geometry", value: review["package_displayed_object"])
                Button("Mark Displayed Object Reviewed") { actions.send("mark_package_object_reviewed") }.disabled(review["package_displayed_object"] == .null)
                Toggle("Acknowledge Exact Candidate", isOn: acknowledgment("package_exact"))
                Toggle("Acknowledge No Private Data", isOn: acknowledgment("package_private"))
                Toggle("Reviewed Required Measurements", isOn: acknowledgment("package_measurements"))
                Text("Review every exact object before export. Refusing required measurements causes an Engine rejection.")
                HStack {
                    Button("Import Inspected Package") {
                        actions.command("profile_action", method: "profiles.import_package", fields: ["directory": .string(directory), "expected_manifest_sha256": candidate["manifest_sha256"], "profile": candidate["manifest"]["profile"]])
                    }.disabled(!actions.flag("package_import_available"))
                    Button("Export Reviewed Package") { actions.send("export_reviewed_package") }.disabled(!actions.flag("package_export_available"))
                    Button("Bind Selected Profile to Opened Device") {
                        actions.command("profile_action", method: "profiles.bind", fields: ["id": selectedReference["id"], "expected_profile_revision": selectedReference["revision"], "profile_sha256": selectedReference["profile_sha256"], "expected_device_epoch": actions.snapshot?.state["device_epoch"] ?? .null])
                    }.disabled(selectedReference == .null || !actions.flag("association_available"))
                }
                ExactObservation("Package Issues", value: actions.value("profiles")["package_issue"])
                ExactObservation("Unresolved Package Write", value: review["unresolved_package_write"])
                ExactObservation("Device Association", value: actions.value("profiles")["bound"])
            }.frame(maxWidth: .infinity, alignment: .leading)
        }
        .onAppear {
            directory = review["package_directory"].string ?? ""
            destination = review["package_destination"].string ?? ""
        }
        .onChange(of: directory) { _, _ in inputs() }
        .onChange(of: destination) { _, _ in inputs() }
    }
    func inputs() { actions.send("package_inputs", fields: ["directory": .string(directory), "destination": .string(destination)]) }
    func acknowledgment(_ key: String) -> Binding<Bool> {
        Binding(get: { review[key].bool }, set: { value in
            actions.send("package_acknowledgments", fields: ["exact": .bool(key == "package_exact" ? value : review["package_exact"].bool), "private": .bool(key == "package_private" ? value : review["package_private"].bool), "measurements": .bool(key == "package_measurements" ? value : review["package_measurements"].bool)])
        })
    }
}
