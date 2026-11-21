# Earph for macOS

This frontend uses SwiftUI windows and AppKit text controls.
Rust owns the Engine connection, command fencing, review gates, and cleanup.

## Play a local file

1. In **Connect**, choose **Connect** for a running Engine, or **Start Engine** when none runs.
2. In **Queue**, choose **Add Files…**, then select a track.
3. In **Output**, choose **Refresh Devices**, pick an output, then choose **Select Output**.
4. Choose **Approve Current Output** for that device.
5. In **Queue**, choose **Play**. Play does not approve the output or unmute it.
6. If muted, choose **Unmute** in **Output** to start listening.

The level field starts at −30000 millidecibels (−30 dB). **Set Level** applies it.
Use **Pause** and **Resume** in Queue. The toolbar always exposes **Mute** and **Stop** while attached.
If another controller changes the Engine, choose **Refresh** before the next action.
Closing the window leaves the Engine running. **Stop Engine and Quit…** stops the shared Engine.

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
