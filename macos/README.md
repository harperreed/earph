# Earph for macOS

This frontend uses SwiftUI windows and AppKit text controls.
Rust owns the Engine connection, command fencing, review gates, and cleanup.

## Play a local file

1. Choose **Open…** or press Command-O to add audio files.
2. Choose **Play** in the toolbar. An empty selection starts the first queued file.

Earph connects automatically. Play prepares the file and starts sound through an approved output.
Before first playback, **Set Up Audio…** opens the output picker.
Choose a device, select it, and approve audio for that output.
Changing the output requires fresh approval. Play never grants that approval itself.

The toolbar keeps Play/Pause, Stop, Mute, and ordinary volume controls visible.
The volume control respects the observed cap and retains the configured ramp.
Mute and Stop cancel an unfinished Play action.
Play preserves the observed volume level.
Connection details, runtime paths, and exact observations sit under **Advanced**.

Closing the window leaves the Engine running. **Stop Engine and Quit…** stops the shared Engine.
The default quit and background-playback policy remains a deferred product decision.

System output uses macOS format conversion. The default buffer has 4096 frames; the period has 64 frames.
Keep explicit buffer choices when they suit the selected output.

## Build and check

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
