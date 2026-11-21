// ABOUTME: Sends real control frames through generated Swift bindings.
// ABOUTME: Verifies typed failures and releases client references before reconnecting.
import Foundation

enum SmokeFailure: Error { case invalidFixture, missingError, retainedClient }
func decodeHex(_ value: String) throws -> Data {
    guard value.utf8.count % 2 == 0 else { throw SmokeFailure.invalidFixture }
    var bytes: [UInt8] = []
    var cursor = value.startIndex
    while cursor < value.endIndex {
        let end = value.index(cursor, offsetBy: 2)
        guard let byte = UInt8(value[cursor..<end], radix: 16) else { throw SmokeFailure.invalidFixture }
        bytes.append(byte)
        cursor = end
    }
    return Data(bytes)
}
func encodeHex(_ value: Data) -> String { value.map { String(format: "%02x", $0) }.joined() }
func failure(_ body: () throws -> Void) throws -> String {
    do { try body() } catch ClientError.Engine(let code, let detail) {
        guard !detail.isEmpty else { throw SmokeFailure.invalidFixture }
        return code
    }
    throw SmokeFailure.missingError
}
@main struct Smoke {
    static func main() throws {
        guard CommandLine.arguments.count == 3 else { throw SmokeFailure.invalidFixture }
        let runtime = CommandLine.arguments[1]
        let text = try String(contentsOfFile: CommandLine.arguments[2], encoding: .utf8)
        let commands = text.split(separator: "\n").map { $0.split(separator: "\t", maxSplits: 1).map(String.init) }
        guard commands.count > 1, commands.allSatisfy({ $0.count == 2 }) else { throw SmokeFailure.invalidFixture }
        var client: LocalClient? = try LocalClient(runtimeDirectory: runtime)
        weak let released = client
        for row in commands.dropLast() {
            print(row[0] + "\t" + encodeHex(try client!.request(command: decodeHex(row[1]))))
        }
        print("preflight_empty\t!" + (try failure { _ = try client!.request(command: Data()) }))
        print("preflight_oversized\t!" + (try failure { _ = try client!.request(command: Data(repeating: 0, count: 1_048_577)) }))
        client = nil
        guard released == nil else { throw SmokeFailure.retainedClient }
        let fresh = try LocalClient(runtimeDirectory: runtime)
        let last = commands.last!
        print(last[0] + "\t" + encodeHex(try fresh.request(command: decodeHex(last[1]))))
        print("path_empty\t!" + (try failure { _ = try LocalClient(runtimeDirectory: "") }))
        print("path_oversized\t!" + (try failure { _ = try LocalClient(runtimeDirectory: String(repeating: "é", count: 31)) }))
    }
}
