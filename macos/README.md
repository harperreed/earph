# Earph for macOS

This frontend uses SwiftUI windows and AppKit text controls.
Rust owns the Engine connection, command fencing, review gates, and cleanup.

Build the generated desktop UniFFI bindings before compiling the app.
The repository packaging script supplies the generated module and static library.

Run the native control contract without the Engine:

```sh
swiftc -parse-as-library -warnings-as-errors -framework AppKit -framework SwiftUI \
  macos/Sources/Earph/NativeTextEditor.swift macos/Tests/NativeControlsTests.swift \
  -o /private/task-output/native-controls-tests
/private/task-output/native-controls-tests
```

The tests create a private named pasteboard.
They never read or write the general clipboard.
