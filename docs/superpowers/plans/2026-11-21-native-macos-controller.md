# Native macOS controller

Doctor Biz requested standard macOS windows and controls after the first Orbit preview.
The egui build cannot paste from other applications with its current disabled clipboard feature.
The Mac replacement uses SwiftUI and AppKit controls.
Linux keeps its existing renderer.

## Outcome and scope

Use the standard title bar, menus, sidebar, forms, tables, text fields, and file dialogs.
Use the system appearance by default.
Keep useful text selectable and editable through the standard responder chain.
Move technical connection and output fields under Advanced.
Keep every current desktop capability available.

Reuse `GuiController` for owner observations, request admission, preflight, review, and cleanup.
Keep the standalone Engine authoritative for playback and output safety.
Keep engine launch, output selection, approval, Play, fixed arming, and Unmute explicit.
Never start an owner during application construction.
Closing the window leaves the owner running.
Explicit engine shutdown needs its existing confirmation.

## Architecture

Export a narrow UniFFI facade from `earph-desktop`.
Statically link its Rust archive into the Swift application.
Use the existing two-product application packager without adding a helper service.
The Mac executable replaces the packaged desktop product.
It does not call the egui renderer.

Run bridge calls and final release on one serial background queue.
Coalesce ticks and publish immutable snapshots on the main actor.
Resolve bounded render tokens to captured Rust State references.
Reject expired tokens instead of silently rebinding actions to fresh State.
Retain exact request bytes for reconciliation.
Reuse existing profile, graph, correction, package, and diagnostic review helpers.

## Implementation owners

- Rust bridge worker: desktop manifest, scaffolding, native facade, projection serialization, and focused tests.
- Swift views worker: `macos/`, native forms, menus, lifecycle, and component tests.
- Root: build/check scripts, provenance, integration, package verification, and Orbit preparation.

Workers use separate task branches and preserve all existing checkouts.
Root integrates explicit owned files after reviewing their before and after hashes.
No worktree or retained artifact deletion is authorized.

## Verification and completion

- [x] Prove stale-token, malformed-action, and lifecycle regressions before their fixes.
- [x] Run real-owner bridge integration tests, including pending and uncertain requests.
- [x] Compile generated Swift against the actual Rust static library on Orbit.
- [x] Test native text selection and private pasteboard import without touching the user's clipboard.
- [x] Test standard menus, file selection configuration, and close behavior.
- [x] Verify every existing desktop capability has a native control path.
- [x] Run strict Rust and Swift checks plus the workspace canonical check.
- [x] Complete one independent review of the final bridge and native lifecycle.
- [x] Build and verify a separate retained native application package.
- [x] Preserve the earlier preview and report actual native acceptance limits.

No software result establishes physical audio delivery.
Orbit build permission does not grant new Accessibility or screen recording permissions.
Do not grant permissions or replay consumed hardware approvals.

## Progress

The native application is built, verified, and open on the chosen Mac.
Six current native checks pass through the actual Rust controller and standalone owner.
Doctor Biz confirmed external paste works in the native runtime field.
Independent review found two defects; real regressions proved both fixes.
The final workspace canonical check passes.
The [native control receipt](../../evidence/desktop/macos-native-controls-20261121.json) binds the exact archive and products.

The first real Play attempt exposed a separate Core Audio compatibility limit.
The file decoded at 44.1 kHz; the device runs at 48 kHz with 512-frame callbacks.
The current route demands matching rate and callback size instead of converting them.
Playback remains blocked while that route is corrected without retuning the device.
Native UI acceptance does not qualify physical audio.
Doctor Biz's explicit native-control request supplies the design direction.
His standing instruction authorizes routine implementation decisions without another design approval gate.
