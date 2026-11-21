// ABOUTME: Presents actual signal paths, recovery events, and explicitly reviewed diagnostic artifacts.
// ABOUTME: Keeps exact request reconciliation separate from refreshed observations and export actions.
import SwiftUI

struct DiagnosticsView: View {
    @ObservedObject var store: ControllerStore
    let actions: NativeActions
    @State private var cursor = ""
    @State private var directory = ""
    @State private var digest = ""
    var diagnostic: JSONValue { actions.value("diagnostics") }
    var body: some View {
        ScrollView {
            VStack(alignment: .leading) {
                HStack {
                    Button("Refresh State") { actions.send("refresh") }.disabled(!actions.flag("attached"))
                    Button("Inspect Actual Graph") { actions.command("profile_action", method: "graph.inspect") }.disabled(!actions.flag("control_edit_available"))
                }
                ExactObservation("Actual State and Playback Observations", value: actions.snapshot?.state ?? .null)
                ExactObservation("Actual Graph and Recovery State", value: actions.value("profiles")["graph_inspection"])
                HStack {
                    Button("Read Signal Path") { read("signal_path.get") }
                    Button("Read Metrics") { read("metrics.get") }
                    Button("Read Recovery Events") { read("events.get", fields: ["cursor": .null]) }
                }.disabled(!actions.flag("diagnostic_read_available"))
                ExactObservation("Signal Path", value: diagnostic["signal_path"])
                ExactObservation("Metrics", value: diagnostic["metrics"])
                ExactObservation("Recovery Events", value: diagnostic["events"])
                HStack {
                    TextField("Event cursor (after sequence)", text: $cursor)
                    Button("Read Event Page") {
                        store.perform {
                            read("events.get", fields: ["cursor": .object(["owner_id": actions.snapshot?.state["owner_id"] ?? .null, "after_sequence": .uint(try wholeNumber(cursor, name: "Event cursor"))])])
                        }
                    }
                    Button("Next Event Page") {
                        read("events.get", fields: ["cursor": .object(["owner_id": diagnostic["events"]["binding"]["owner_id"], "after_sequence": diagnostic["events"]["report"]["next_sequence"]])])
                    }.disabled(diagnostic["events"] == .null)
                }.disabled(!actions.flag("diagnostic_read_available"))
                GroupBox("Redacted Diagnostic Artifact") {
                    VStack(alignment: .leading) {
                        Button("Capture Redacted Preview") { read("diagnostics.preview") }.disabled(!actions.flag("diagnostic_read_available"))
                        HStack {
                            TextField("Destination Directory", text: $directory)
                            Button("Choose Parent…") { store.choosePath(directory: true) { directory = $0 + "/Earph Diagnostics" } }
                        }
                        if let bytes = diagnostic["preview"]["report"]["bytes"].string {
                            NativeTextEditor(text: .constant(bytes), editable: false, label: "Exact redacted diagnostic preview").frame(minHeight: 220)
                        }
                        ExactObservation("Preview Binding and Digest", value: diagnostic["preview"])
                        HStack {
                            Button("Mark Preview Reviewed") { actions.send("review_diagnostic_preview") }
                            Button("Export Reviewed Preview") {
                                read("diagnostics.export", fields: ["directory": .string(directory), "expected_sha256": diagnostic["preview"]["report"]["sha256"]])
                            }.disabled(!actions.flag("diagnostic_preview_reviewed"))
                        }.disabled(!actions.flag("control_edit_available") || diagnostic["preview"] == .null)
                        TextField("Inspection SHA-256", text: $digest)
                        Button("Inspect Diagnostic Artifact") { read("diagnostics.inspect", fields: ["directory": .string(directory), "expected_sha256": .string(digest)]) }
                            .disabled(!actions.flag("diagnostic_read_available"))
                        Text("Inspection does not repair files. Cancellation does not prove worker retirement or absence of filesystem effects.")
                    }.frame(maxWidth: .infinity, alignment: .leading)
                }
                ReceiptsView(store: store, actions: actions, embedded: true)
            }.padding()
        }
        .onAppear {
            directory = diagnostic["directory"].string ?? ""
            cursor = store.drafts["diagnostic_cursor"]?.string ?? ""
            digest = store.drafts["diagnostic_digest"]?.string ?? ""
        }
        .onDisappear {
            store.drafts["diagnostic_cursor"] = .string(cursor)
            store.drafts["diagnostic_digest"] = .string(digest)
        }
        .onChange(of: directory) { _, value in actions.send("diagnostic_destination", fields: ["directory": .string(value)]) }
    }
    func read(_ method: String, fields: [String: JSONValue] = [:]) { actions.command("diagnostic_action", method: method, fields: fields) }
}

struct ReceiptsView: View {
    @ObservedObject var store: ControllerStore
    let actions: NativeActions
    var embedded = false
    var body: some View {
        if embedded { receiptList }
        else { ScrollView { receiptList.padding() } }
    }
    var receiptList: some View {
        VStack(alignment: .leading) {
            Text("Accepted means pending Engine work. Applied describes the operation and does not prove physical audio presentation.")
            ForEach(actions.value("receipts").array, id: \.self) { receipt in
                DisclosureGroup("\(receipt["method"].text) · \(receipt["presentation"].text)") {
                    VStack(alignment: .leading) {
                        Text("Request: \(receipt["request_id"].text)").textSelection(.enabled)
                        if receipt["owner_changed"].bool { Text("The Engine owner changed. Reconciliation of the old target is blocked.") }
                        ExactObservation("Outcome", value: receipt["outcome"])
                        ExactObservation("Client or Protocol Failure", value: receipt["failure"])
                        Button("Reconcile Exact Request") { actions.send("reconcile", fields: ["request_id": receipt["request_id"]]) }
                            .disabled(!actions.flag("attached") || receipt["in_flight"].bool || receipt["owner_changed"].bool || !["accepted", "uncertain"].contains(receipt["presentation"].string?.lowercased() ?? ""))
                        capturedArtifactActions(receipt)
                        NativeTextEditor(text: .constant(receipt["request_json"].string ?? "No exact request retained."), editable: false, label: "Exact local request").frame(minHeight: 130)
                        NativeTextEditor(text: .constant(receipt["reply_json"].string ?? "No exact reply received."), editable: false, label: "Exact local reply").frame(minHeight: 130)
                    }
                }
            }
            if actions.value("receipts").array.isEmpty { Text("No retained requests yet.").foregroundStyle(.secondary) }
        }.frame(maxWidth: .infinity, alignment: .leading)
    }
    @ViewBuilder
    func capturedArtifactActions(_ receipt: JSONValue) -> some View {
        let command = receipt["command"]
        if ["diagnostics.export", "diagnostics.inspect"].contains(receipt["method"].string ?? "") {
            HStack {
                Button("Inspect Captured Destination") {
                    actions.command("diagnostic_action", method: "diagnostics.inspect", fields: ["directory": command["directory"], "expected_sha256": command["expected_sha256"]])
                }
                Button("Request Diagnostic Cancellation") {
                    actions.command("diagnostic_action", method: "diagnostics.cancel", fields: ["request_id": receipt["request_id"]])
                }
            }.disabled(!actions.flag("diagnostic_read_available") || receipt["owner_changed"].bool)
        }
        if receipt["method"].string == "profiles.export" {
            Button("Inspect Captured Export Destination") {
                actions.command("profile_action", method: "profiles.package_inspect", fields: ["directory": command["directory"]])
            }.disabled(!actions.flag("control_edit_available") || receipt["owner_changed"].bool || ["accepted", "uncertain", "queued"].contains(receipt["presentation"].string?.lowercased() ?? ""))
        }
    }
}
