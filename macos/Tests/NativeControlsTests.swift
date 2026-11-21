// ABOUTME: Verifies native editing through isolated AppKit text controls and private pasteboards.
// ABOUTME: Preserves the general clipboard while checking exact text and standard responder actions.
import AppKit

@main
struct NativeControlsTests {
    @MainActor
    static func main() throws {
        _ = NSApplication.shared
        let board = NSPasteboard(name: .init("org.earph.tests.\(UUID().uuidString)"))
        defer { board.releaseGlobally() }
        let text = "{\n  \"name\": \"Doctor Biz 🎧\"\n}"
        let editor = NativeTextEditor.makeTextView(text: text)
        precondition(editor.isEditable && editor.isSelectable && !editor.isRichText)
        precondition(editor.allowsUndo)
        editor.setSelectedRange(NSRange(location: 0, length: (text as NSString).length))
        let types = editor.writablePasteboardTypes
        precondition(!types.isEmpty)
        precondition(editor.writeSelection(to: board, types: types))
        precondition(types.contains { board.string(forType: $0) == text }, "Copy must retain exact graph JSON")
        let destination = NativeTextEditor.makeTextView(text: "")
        precondition(destination.readSelection(from: board))
        precondition(destination.string == text, "Paste must preserve multiline Unicode text")
        destination.setSelectedRange(NSRange(location: 0, length: (text as NSString).length))
        destination.insertText("replacement", replacementRange: destination.selectedRange())
        precondition(destination.string == "replacement")
        precondition(destination.responds(to: #selector(NSText.copy(_:))))
        precondition(destination.responds(to: #selector(NSText.paste(_:))))
        precondition(destination.responds(to: #selector(NSText.cut(_:))))
        precondition(destination.responds(to: #selector(NSText.selectAll(_:))))
        print("PASS native multiline editing, private copy/paste, and responder actions")
    }
}
