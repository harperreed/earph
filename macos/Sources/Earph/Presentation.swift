// ABOUTME: Reads immutable Rust presentations without losing integer identities or exact receipt text.
// ABOUTME: Builds local actions with the captured Rust basis used by the visible controls.
import Foundation

enum JSONValue: Codable, Equatable, Hashable, Sendable {
    case object([String: JSONValue]), array([JSONValue]), string(String)
    case uint(UInt64), int(Int64), double(Double), bool(Bool), null

    init(from decoder: Decoder) throws {
        let value = try decoder.singleValueContainer()
        if value.decodeNil() { self = .null }
        else if let result = try? value.decode(Bool.self) { self = .bool(result) }
        else if let result = try? value.decode(UInt64.self) { self = .uint(result) }
        else if let result = try? value.decode(Int64.self) { self = .int(result) }
        else if let result = try? value.decode(Double.self) { self = .double(result) }
        else if let result = try? value.decode(String.self) { self = .string(result) }
        else if let result = try? value.decode([JSONValue].self) { self = .array(result) }
        else { self = .object(try value.decode([String: JSONValue].self)) }
    }

    func encode(to encoder: Encoder) throws {
        var value = encoder.singleValueContainer()
        switch self {
        case .object(let item): try value.encode(item)
        case .array(let item): try value.encode(item)
        case .string(let item): try value.encode(item)
        case .uint(let item): try value.encode(item)
        case .int(let item): try value.encode(item)
        case .double(let item): try value.encode(item)
        case .bool(let item): try value.encode(item)
        case .null: try value.encodeNil()
        }
    }

    subscript(_ key: String) -> JSONValue {
        guard case .object(let fields) = self else { return .null }
        return fields[key] ?? .null
    }
    var object: [String: JSONValue]? { if case .object(let value) = self { return value }; return nil }
    var array: [JSONValue] { if case .array(let value) = self { return value }; return [] }
    var string: String? { if case .string(let value) = self { return value }; return nil }
    var uint: UInt64? { if case .uint(let value) = self { return value }; return nil }
    var bool: Bool { if case .bool(let value) = self { return value }; return false }
    var text: String {
        switch self {
        case .null: return "Unknown"
        case .string(let value): return value
        default: return encoded(pretty: true)
        }
    }
    func encoded(pretty: Bool = false) -> String {
        let encoder = JSONEncoder()
        encoder.outputFormatting = pretty ? [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes] : [.sortedKeys, .withoutEscapingSlashes]
        // Every case contains a valid JSON value. Nonfinite user input is rejected before construction.
        return String(decoding: try! encoder.encode(self), as: UTF8.self)
    }
}

struct NativeSnapshot: Sendable {
    let root: JSONValue
    init(json: String) throws {
        let root = try JSONDecoder().decode(JSONValue.self, from: Data(json.utf8))
        guard root["version"].uint == 1, root["flags"].object != nil else {
            throw PresentationError.invalidSnapshot
        }
        self.root = root
    }
    var state: JSONValue { root["state"] }
    func flag(_ name: String) -> Bool { root["flags"][name].bool }
    func pausesPlayback(selectedEntry: JSONValue, selectionOwner: JSONValue) -> Bool {
        guard state["transport"].string == "playing", !state["muted"].bool else { return false }
        guard selectedEntry != .null else { return true }
        return selectionOwner == state["owner_id"] && selectedEntry == state["active_entry_id"]
    }
    func action(_ kind: String, fields: [String: JSONValue] = [:]) -> JSONValue {
        var fields = fields
        fields["version"] = .uint(1)
        fields["kind"] = .string(kind)
        let basisKinds = ["control_edit", "start_local", "play_local", "set_volume", "profile_action", "diagnostic_action", "preview_correction", "graph_load_profile", "profile_draft_load", "graph_add_fir", "review_diagnostic_preview", "mark_package_object_reviewed", "package_acknowledgments", "export_reviewed_package", "read_package_object", "shutdown_and_quit"]
        if kind == "queue_edit" { fields["basis_token"] = root["queue_basis_token"] }
        else if basisKinds.contains(kind) { fields["basis_token"] = root["basis_token"] }
        return .object(fields)
    }
}

struct PlaybackSelection: Equatable {
    enum Phase: Equatable { case selected, queued(UUID), preparing(UUID) }
    let entryID: String?
    let owner: JSONValue
    var phase: Phase = .selected
    var entryToPlay: JSONValue { phase == .selected ? entryID.map(JSONValue.string) ?? .null : .null }

    func observing(_ snapshot: NativeSnapshot, completionID: UUID? = nil) -> PlaybackSelection? {
        guard snapshot.state["owner_id"] == owner else { return completionID == nil ? nil : self }
        var next = self
        if let completionID {
            guard case .queued(let expected) = phase, completionID == expected else { return self }
            if snapshot.root["action_error"] != .null { next.phase = .selected }
            else if snapshot.flag("play_local_pending") { next.phase = .preparing(completionID) }
            else { return nil }
            return next
        }
        if case .preparing = phase, !snapshot.flag("play_local_pending") {
            if snapshot.root["action_error"] == .null { return nil }
            next.phase = .selected
        }
        return next
    }
    func failed(completionID: UUID) -> PlaybackSelection {
        var next = self
        switch phase {
        case .queued(let expected), .preparing(let expected):
            if expected == completionID { next.phase = .selected }
        case .selected: break
        }
        return next
    }
}

struct PlaybackVolume {
    let decibels: Double?
    let maximumDecibels: Double?
    init(snapshot: NativeSnapshot?) {
        decibels = Self.level(snapshot?.state["software_gain_db"] ?? .null)
        maximumDecibels = Self.level(snapshot?.state["volume_policy"]["software_cap_db"] ?? .null)
            .map { ($0 * 1000).rounded(.down) / 1000 }
    }
    private static func level(_ value: JSONValue) -> Double? {
        let number: Double
        switch value {
        case .double(let value): number = value
        case .int(let value): number = Double(value)
        case .uint(let value): number = Double(value)
        default: return nil
        }
        return number.isFinite && (-120...0).contains(number) ? number : nil
    }
    static func command(decibels: Double, rampFrames: UInt64) throws -> [String: JSONValue] {
        guard decibels.isFinite, (-120...0).contains(decibels), rampFrames <= UInt64(UInt32.max) else {
            throw DraftError.invalid("Choose a volume between −120 and 0 dB.")
        }
        return ["millidecibels": .int(Int64((decibels * 1000).rounded(.down))), "ramp_frames": .uint(rampFrames)]
    }
}

enum PresentationError: Error { case invalidSnapshot }

struct OutputGeometryDraft {
    var period = "64"
    var buffer = "4096"

    func fields() throws -> [String: JSONValue] {
        ["period_frames": .uint(try wholeNumber(period, name: "Period")),
         "buffer_frames": .uint(try wholeNumber(buffer, name: "Buffer"))]
    }
}

enum DraftError: LocalizedError {
    case invalid(String)
    var errorDescription: String? { if case .invalid(let message) = self { return message }; return nil }
}
func wholeNumber(_ text: String, name: String) throws -> UInt64 {
    guard let value = UInt64(text) else { throw DraftError.invalid("\(name) must be a nonnegative whole number.") }
    return value
}
