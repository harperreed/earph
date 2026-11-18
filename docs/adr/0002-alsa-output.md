# Explicit ALSA output

Status: accepted for the T05 Linux spike on November 17, 2026.

Use `earph-alsa` for Linux output and its diagnostic executable.
Keep the existing core platform-free and the existing harness commands offline.
Reuse the harness fixture loader, hashes, and build provenance in the spike.

Use `alsa-sys` 0.6.1 and the existing `libc` 0.2.191.
Keep unsafe calls inside `native.rs`.
Make native handles thread-owned and close them through RAII.
Do not expose native pointers through the public API.

The inspected `alsa` 0.12.1 release lacks a safe PCM-type query.
Direct output must inspect the opened type, so use a small native boundary.
Reject a configured `hw:` alias unless ALSA reports `SND_PCM_TYPE_HW`.
Disable automatic rate, channel, format, and software-volume conversion on direct opens.

Set all client parameters exactly, including period and buffer frames.
Inspect the installed parameters and rational sample rate before returning an open stream.
Reject unsupported packed 24-bit formats instead of silently using a different storage layout.
Support signed 16-bit, packed 24-bit, and signed 32-bit little-endian PCM.
Float and DSD output remain unsupported.

Treat `system` as an explicit compatibility route.
Its observed format describes the ALSA client boundary, including when a plugin converts downstream.
Keep downstream transformations unknown and preservation unverified.
Opening a PipeWire route does not qualify concurrent playback or server-rate changes.

Opening and sample submission are separate actions.
Require signal permission for every write.
The CLI requires `--allow-signal` after approval for the selected route.
Do not probe additional formats or select another route automatically.

Preallocate one bounded packing buffer before opening.
Use nonblocking writes and return accepted frames or `would_block`.
Require contiguous input spans within one epoch.
Advance counters only after native acceptance.
Keep submitted frames separate from the backend-delay consumption estimate.
That estimate does not measure acoustic presentation or downstream latency.

Make `failed` and `stopped` terminal states.
Stop and close on fatal native errors without recovery or fallback.
Require a fresh explicit open for another epoch or format.
Cleanup discards queued samples; it never drains them during shutdown.

This spike runs on an owner thread.
Nonblocking transfers do not qualify native calls for realtime callback use.
Backend allocation, blocking, xrun, unplug, and endurance qualification remain later work.
Full gain policy and fixed-output arming remain T08 responsibilities.

Link the host ALSA library dynamically and report its runtime version.
The observed host library is ALSA 1.2.15.3, under LGPL-2.1-or-later.
The Rust binding is MIT; other Cargo dependencies retain their recorded terms.
This spike does not produce a release package or complete its distribution review.

## Sources checked

- [ALSA PCM interface](https://www.alsa-project.org/alsa-doc/alsa-lib/pcm.html): nonblocking writes and device states.
- [ALSA PCM plugins](https://www.alsa-project.org/alsa-doc/alsa-lib/pcm_plugins.html): hardware versus conversion routes.
- [alsa-sys 0.6.1](https://crates.io/crates/alsa-sys/0.6.1): pinned native bindings and license.
- [alsa 0.12.1 PCM API](https://docs.rs/alsa/0.12.1/alsa/pcm/struct.PCM.html): inspected wrapper surface.

Verify these claims again when changing the binding or backend.
