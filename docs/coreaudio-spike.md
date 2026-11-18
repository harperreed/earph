# Core Audio precision spike

T04 supplies real macOS SDK inspection and silent callback-registration checks.
It does not implement playback or complete M0/M1 qualification.
See [the decision](adr/0003-coreaudio-spike.md) and [the plan](superpowers/plans/2026-11-17-coreaudio-spike.md).

## Commands

Build on macOS with the pinned Rust 1.98.1 toolchain and an installed Xcode SDK.
List actual output devices:

```sh
cargo run --locked -p earph-coreaudio -- devices
```

Inspect an explicit UID returned by discovery:

```sh
cargo run --locked -p earph-coreaudio -- inspect --device-uid 'UID_FROM_DEVICES'
```

Replace `UID_FROM_DEVICES` with the actual UID.
The command reports nominal rate, buffer size, ownership availability, and physical and virtual stream descriptors.
It also reports advertised format ranges and property writability.
These observations do not establish acquired ownership or measured sample preservation.
Incomplete discovery includes its failures and exits with status 2.

Assess an explicit integer PCM request:

```sh
cargo run --locked -p earph-coreaudio -- precision-check --request request.json
```

The version 1 request contains `device_uid`, the existing core `format`, and `buffer_frames`.
For example:

```json
{
  "schema_version": 1,
  "device_uid": "UID_FROM_DEVICES",
  "format": {"sample_rate_hz": 48000, "channels": "stereo", "representation": "signed24"},
  "buffer_frames": 256
}
```

Use the desired format and explicit buffer size.
A conflict exits with status 2 and retains separate physical and virtual explanations.
A match still reports `opened: false` and `payload_preservation: unverified`.
The spike accepts packed little-endian signed 16-, 24-, and 32-bit PCM assessment.
Float, planar, big-endian, aligned, and different container layouts do not match this narrow contract.

Register and destroy an IOProc without starting it:

```sh
cargo run --locked -p earph-coreaudio -- registration-check --device-uid 'UID_FROM_DEVICES'
```

The command checks settings before and after registration.
Success requires zero callbacks, zero submitted samples, unchanged settings, and successful close.
Its evidence level is `registered_not_started`.
Each registration owns a distinct context, counter, and native identifier.
Failed native destruction reports an error and retains callback data until process exit.
Restart the probe process before retrying that failure.

No command starts audio, acquires hog mode, changes rate, changes gain, or sets a stream format.
Missing UIDs fail without another output selection.
Native diagnostics fail explicitly on other platforms.
Help and report redaction work on Linux too.

## Shareable records

Raw reports can contain personal device names and persistent UIDs.
Retain them privately and derive a public record:

```sh
cargo run --locked -p earph-coreaudio -- redact --record private-report.json
```

The command replaces device names and UIDs, preserves observations, and records the source file's SHA-256 hash.
It marks the result `identifiers_redacted: true`.
It does not edit the input file or turn an observation into a passing hardware test.

## Verification

Run `scripts/check` on Linux and macOS.
Native tests use real Core Audio discovery, inspection, and registration APIs.
They verify missing UIDs, distinct registration ownership, close/reopen, and unchanged settings.
CLI tests run the actual executable and redact actual native reports on macOS.
Pure contract tests cover byte layouts, rates, channels, formats, unknown ownership, and missing streams.

For an approved remote Mac, use `scripts/check-coreaudio-remote`.
Create a separate task folder with a Git clone named `checkout` or `checkout-SUFFIX`.
Put the marker `earph-coreaudio-build-v1` in `.earph-coreaudio-root` in its parent folder.
The script accepts the SSH alias and full checkout path.
Stage new source files first; it copies tracked working-tree content and runs the canonical check.
It retains all archives and never deletes remote files.
An optional verified peer address preserves the SSH alias and requires its existing host key.
Every connection forces strict host-key checking, including the normal path without a peer override.
Use `--ssh-config` before the arguments to inspect that effective policy without connecting or copying files.
The script selects the real compiler with `rustup which --toolchain 1.98.1 rustc` after mise installation.

## Findings and remaining gates

The tested arm64 Mac runs macOS 27.2.1 and Xcode 27.2.
Its selected route reports stereo 48 kHz, 24 valid integer bits in 32-bit physical containers, and float32 virtual samples.
The packed-24 request correctly fails both format comparisons.
Silent registration, independent cleanup, and close/reopen passed without changed settings or emitted samples.
The [clean-build archive](evidence/coreaudio/README.md) retains redacted observations from commit `489170e`.

Live PCM submission, format changes, hog acquisition, route-change recovery, timing, unplug, and hardware preservation remain unqualified.
The binding publisher's SDK distribution review remains a release gate.
M0 still needs the GUI, storage, and IPC decisions and an agreed qualification matrix.
