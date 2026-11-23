// ABOUTME: Renders only the owned SwiftUI view in a standard in-process AppKit window.
// ABOUTME: Retains native screenshots without screen capture, accessibility grants, or general clipboard access.
import AppKit
import SwiftUI

@main
struct ViewPresentationTests {
    @MainActor
    static func main() throws {
        guard CommandLine.arguments.count == 2 else { throw ViewCheckFailure.message("usage: view-presentation-tests NEW_ABSOLUTE_PNG_PATH") }
        let path = CommandLine.arguments[1]
        guard path.hasPrefix("/"), !FileManager.default.fileExists(atPath: path) else { throw ViewCheckFailure.message("Use a new absolute PNG path") }
        _ = NSApplication.shared
        let store = ControllerStore()
        let deadline = Date().addingTimeInterval(5)
        while store.snapshot == nil && Date() < deadline { RunLoop.current.run(until: Date().addingTimeInterval(0.01)) }
        guard store.snapshot != nil else { throw ViewCheckFailure.message("No actual Rust snapshot") }
        guard !store.flag("attached"), store.value("queue") == .null,
              store.error == nil, store.value("error") == .null else {
            throw ViewCheckFailure.message("The presentation fixture must contain no connected owner, queue, or private error")
        }
        let hosting = NSHostingView(rootView: ContentView(store: store))
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 1040, height: 760), styleMask: [.titled, .closable, .miniaturizable, .resizable], backing: .buffered, defer: false)
        window.title = "Earph Native Presentation Test"
        window.contentView = hosting
        window.makeKeyAndOrderFront(nil)
        RunLoop.current.run(until: Date().addingTimeInterval(0.25))
        hosting.layoutSubtreeIfNeeded()
        guard let bitmap = hosting.bitmapImageRepForCachingDisplay(in: hosting.bounds) else { throw ViewCheckFailure.message("Owned SwiftUI view cannot cache its display") }
        hosting.cacheDisplay(in: hosting.bounds, to: bitmap)
        guard let png = bitmap.representation(using: .png, properties: [:]) else { throw ViewCheckFailure.message("Owned view PNG encoding failed") }
        try png.write(to: URL(fileURLWithPath: path), options: .withoutOverwriting)
        var closed = false
        store.close { closed = true }
        let closeDeadline = Date().addingTimeInterval(5)
        while !closed && Date() < closeDeadline { RunLoop.current.run(until: Date().addingTimeInterval(0.01)) }
        guard closed else { throw ViewCheckFailure.message("Owned view controller cleanup timed out") }
        window.orderOut(nil)
        print("PASS owned ContentView construction and controller cleanup; diagnostic PNG: \(path)")
    }
}

enum ViewCheckFailure: Error { case message(String) }
