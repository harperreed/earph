# Core Audio spike evidence

Source commit: `489170eaf8c4b0b03eeead7dcb540d770417e00a`.
Records identify that clean build on arm64 macOS 27.2.1.
The native build used Xcode 27.2 and Rust 1.98.1.

`devices.json` records output-device discovery and its failure list.
`inspect.json` records the selected device's current physical and virtual descriptors.
`registration.json` records a never-started registration, zero callbacks, unchanged settings, and successful close.
`precision.json` preserves the expected format conflict; it is not rewritten as a successful open.
`verification.json` summarizes the scoped checks and remaining gates.

The selected physical stream uses 24 valid bits in 32-bit containers, stereo at 48 kHz.
Its virtual stream uses float32.
The packed-24 request conflicts with both descriptors.
No device configuration was changed, and no samples were submitted.

The Linux canonical check passed 91 tests; the Mac canonical check passed 83.
Both totals include the existing compile-fail documentation test.
Both checks passed formatting, Clippy with denied warnings, catalog validation, and dependency validation.
The independent review found one SSH-policy issue, which was fixed and verified for both connection paths.
It reported no remaining material findings.

Device names and persistent UIDs are redacted by the tested executable.
Each report hashes its unchanged private source record.
Those hashes were independently checked against `shasum` on the Mac before archiving.
The Linux and Mac build records report the same source and lock digests.
Private reports remain outside Git in the preserved task folders.

Verify archive integrity from this directory:

```sh
sha256sum --check checksums.sha256
```

On macOS, use `shasum -a 256 --check checksums.sha256`.

Reproduce the native operations using [the spike guide](../../coreaudio-spike.md).
See [the review](../../coreaudio-review.md) for scope and limits.
Checksums and redaction do not authenticate the author or prove hardware preservation.
This archive does not pass through the offline foundation's separate evidence schema.

Playback, actual format changes, exclusive acquisition, unplug, timing, and hardware payload measurement remain unqualified.
The binding publisher's SDK distribution question remains a release gate.
