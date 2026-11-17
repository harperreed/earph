# Read-only Core Audio risk spike

Status: accepted for T04 on November 17, 2026.

Use `earph-coreaudio` beside the Linux adapter.
Keep platform handles outside `earph-core`.
Pin the Core Audio, Core Audio Types, and Core Foundation bindings to 0.3.2.
Disable their default features and enable only the hardware, base types, and string surfaces used.

Inspect physical and virtual stream descriptors independently.
Keep the SDK format ID and flags as their single source of truth.
Derive precision assessment from those descriptors; do not store duplicate encoding flags.
Compare sample rate, channels, valid bits, container width, packet layout, and buffer size exactly.
The spike supports packed little-endian signed integer PCM assessment only.
Other layouts remain unsupported, even when their valid bit depth matches.

Require one explicit device UID for inspection and registration.
Report incomplete discovery with its failures.
Never select a default or fall back after an unavailable UID.
Keep a format match separate from an opened device or preservation measurement.

Register a never-started IOProc to test the SDK ownership boundary.
Give every registration its own owned callback context and counter.
Real tests showed that concurrent registrations sharing a null context return the HAL error `nope`.
Unique contexts fixed the collision and made callback counts local to each registration.
Destroy only the owned registration, then release its context.

Treat successful close as terminal and make repeated close harmless.
Treat failed destruction as terminal too; do not blindly retry an uncertain native operation.
If destruction fails, retain the tiny callback context until process exit to avoid a dangling native pointer.
Report that failure and restart the probe process before another attempt.
Use explicit checked close when the outcome matters; destruction also attempts cleanup.

This spike never starts hardware or changes device properties.
It does not acquire hog mode, switch rates, set stream formats, or change gain.
Those operations belong to the T08–T09 safety and playback integration.
No physical signals were authorized for T04.

The observed Mac route uses 24 valid integer bits in 32-bit physical containers.
Its current virtual format uses 32-bit floating point.
Both run at 48 kHz with two channels.
The strict packed-24 request correctly reports physical and virtual conflicts.
Advertised formats and writable properties do not prove that another configuration will preserve samples.

## Dependency provenance

The published binding crates omit license files and point to their repository's `LICENSE.md`.
All three published `.cargo_vcs_info.json` files identify revision `7b1abfd750a2cacaea71d6a56ecfb83cb7de560b`.
Retain that revision's actual license declaration under `docs/dependency-licenses/objc2-0.3.2/`.
Bind the inventory exception to exact package names, version, repository, source revision, and declaration hash.
Other missing license sources still require manual review.

The publisher declares Zlib, Apache-2.0, or MIT terms and notes Apple SDK distribution questions.
Record that unresolved distribution review as a release gate.
This spike does not distribute the SDK or qualify a release package.

## Evidence and privacy

Raw device names and persistent UIDs remain private.
Create public records with the tested `redact` command.
Preserve format observations and a hash of each unchanged private source report.
Redaction and checksums do not authenticate an author or measure hardware preservation.
Keep registration evidence labeled `registered_not_started`.

## Sources

- [Apple IOProc documentation](https://developer.apple.com/documentation/coreaudio/audiodeviceioproc): callback contract and zeroed output buffers.
- [Apple property API](https://developer.apple.com/documentation/coreaudio/audioobjectgetpropertydata): typed property reads and returned sizes.
- [Publisher license declaration](https://github.com/madsmtm/objc2/blob/7b1abfd750a2cacaea71d6a56ecfb83cb7de560b/LICENSE.md): exact pinned licensing notice.

The native build also checked the installed Xcode 27.2 headers for CFString ownership and property contracts.
