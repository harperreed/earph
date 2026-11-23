// ABOUTME: Starts Earph in a standard macOS window with system menus, navigation, and appearance.
// ABOUTME: Delays application termination until Rust stops the Engine and finishes bounded cleanup.
import AppKit
import SwiftUI

@main
struct EarphApp: App {
    @NSApplicationDelegateAdaptor(EarphApplicationDelegate.self) private var delegate
    var body: some Scene {
        Window("Earph", id: "main") {
            ContentView(store: delegate.store).frame(minWidth: 780, minHeight: 560)
        }
        .defaultSize(width: 1040, height: 760)
        .commands {
            CommandGroup(after: .newItem) {
                Button("Open…") { delegate.store.chooseFiles() }
                    .keyboardShortcut("o", modifiers: .command)
                    .disabled(!delegate.store.flag("queue_edit_available"))
                Button("Add Folder…") { delegate.store.chooseFolder() }.disabled(!delegate.store.flag("queue_edit_available"))
            }
        }
    }
}

@MainActor
final class EarphApplicationDelegate: NSObject, NSApplicationDelegate {
    let store = ControllerStore()
    private var terminationPending = false
    func applicationDidFinishLaunching(_ notification: Notification) {
        store.connectAutomatically()
    }
    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }
    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        if store.flag("ready_to_close") { return .terminateNow }
        if !terminationPending {
            terminationPending = true
            store.close { sender.reply(toApplicationShouldTerminate: true) }
        }
        return .terminateLater
    }
}
