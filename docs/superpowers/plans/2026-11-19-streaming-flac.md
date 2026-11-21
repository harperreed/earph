# Bounded Streaming FLAC Implementation Plan

> Execute this plan with subagent-driven-development or executing-plans and test-driven-development.

**Goal:** Decode admitted, nonseekable FLAC without inventing duration, accepting discarded input, or changing local-file behavior.

**Architecture:** Share the existing integer PCM decoder state within `earph-media`.
Keep bounded streaming admission and byte audits in one stream module.
Return decoded frames before the engine assigns epochs or a time domain.

**Tech Stack:** Existing Rust1.98 and pinned Symphonia0.6.1; admitted `earph-network` readers for integration tests.

**Spec:** `docs/SPEC.md` sections6.2 and16.2, SRC-04/SRC-06, and applicable AC-18/AC-20 controls.

## Authority and ownership

Doctor Biz authorized autonomous completion and parallel implementation.
Root selected this design after the private real-codec extent gate passed.
Reuse this T14 worktree and branch; preserve the preceding network reader unit.
Root owns integration, dependency decisions and later engine contracts.
One implementation owner edits media files; network source remains frozen.
No owner command, renderer, discovery, output, device or playback activation belongs to this unit.
Never delete any file or artifact without Doctor Biz's approval.

## Existing evidence

Root's private `target/streaming-flac-preparation/source-freeze-02/design.md` records exact public APIs and primary source spans.
The adjacent `extent-probe-01/RESULT.md` records72 genuine variants and216 packet observations.
All918288 samples per decoder path matched generated PCM.
Both CRC-valid damaged candidates passed ordinary decoding and failed the extent check.
The initial private assertion failure and corrected resume remain preserved.
This evidence qualifies the first extent gate only; actual streaming and resource bounds still require tests.

## Public API

Add `StreamingFlac::open<R: Read + Send + Sync + 'static>(reader: R) -> Result<Self, EngineError>`.
Expose `format() -> PcmFormat`, `total_frames() -> Option<u64>`, and `next_frame() -> Result<Option<DecodedPcmFrame>, EngineError>`.
Do not expose seek.

`DecodedPcmFrame` has private fields and checked internal construction.
Expose getters for format, frame position, sequence, frame count and samples.
Expose `into_samples(self) -> PcmSamples` for ownership transfer.
Do not assign epochs, queue identities or a time domain to this frame.
The later engine owner must fence and tag publication.

Keep every existing `LocalFile` method signature and behavior.
Keep known-count admission, regular-file checks, seek transitions, packet limits, integer conversion and DoP rejection.
Local blocks retain their existing epochs and `LocalFrames` headers.
Keep one shared conversion and PCM state implementation.
Move existing code where necessary; do not discard a file or introduce a decoder framework.

## Footprint

- Modify `crates/earph-media/src/lib.rs` for shared state and public exports.
- Add a private shared decoder module when it makes the extraction clear.
- Add `crates/earph-media/src/streaming_flac.rs` for admission, audit and stream lifetime.
- Add focused media tests and `docs/streaming-flac.md`.
- Add only an `earph-network` development dependency if real HTTP tests require it.
- Root owns any required Cargo lock and inventory reconciliation.
- Preserve all15 frozen network-unit source files except an explicitly reviewed lockfile edge change.

## Admission and byte audit

Require raw `fLaC` and first STREAMINFO of exactly34 bytes.
Reject duplicate STREAMINFO and reserved block type127.
Before reading or allocating bodies, enforce256 blocks,65536 bytes per body, and1048576 total prefix bytes.
Require the final-metadata flag within those limits.
Replay the admitted prefix once; mirror only subsequent audio reads.
Use public `FlacReader::try_new` directly and an explicit65536-byte MediaSourceStream buffer.

Use a fixed2097152-byte raw-audio ring and a separate2097152-byte read allowance per demux call.
Cover initialization read-ahead with the first allowance.
Reset only the allowance at public call boundaries; never clear unread mirror bytes.
Retain the first source error or budget failure across every later call.
Never hold a guard mutex while blocking on the source.
Check the guard after constructor and packet results, including apparent EOF or library errors.

Require every packet to match the next mirror prefix exactly.
Reject skipped bytes; never search forward for a match.
Keep the existing1048576-byte packet and65536-frame limits.
Require expected track, zero trim, exact next PTS and matching decoded duration.
Apply the pinned FLAC extent check before accepting PCM: `len-3` must fail with an I/O error, then `len-2` must decode.
Use borrowed PacketRef slices and one decoder; retain the qualified MD5 update behavior.
Drain exactly one accepted packet from the mirror.

At EOF, require healthy guard, observed source EOF, empty mirror and exact frame count when declared.
Enable available PCM MD5 verification and reject `Some(false)`; `None` remains unavailable.
Preserve unknown total as `None` after EOF.
Report only observed valid end when original completeness cannot be proven.
Qualify16/24-bit mono/stereo initially; reject unqualified32-bit streaming FLAC.
Keep local32-bit support unchanged.

## Tests and execution

- [x] Write failing real-decoder tests before implementing the stream wrapper or shared extraction.
- [x] Verify generated integer PCM for known/unknown count and present/absent MD5.
- [x] Verify one-byte and irregular source reads, padding and short final frames.
- [x] Verify both qualified CRC-valid adversaries fail before exposing invalid PCM.
- [x] Reject partial headers, payload/footer tails, middle/final CRC damage and discarded bytes.
- [x] Exercise each metadata and read/mirror budget, including swallowed read errors and repeated failed calls.
- [x] Measure fixed mirror and prefix capacities plus a genuine maximum-block fixture; upstream allocations remain unqualified.
- [x] Use real HTTP bodies through the admitted reader for known/chunked transport, truncation, stalls and cancellation.
- [x] Keep network terminal observation and engine publication fencing explicitly outside media's authority.
- [x] Pass existing local decode, seek, CLI, correction, FIR and offline-render tests after extraction.
- [ ] Obtain one independent main-model review of shared behavior, budgets and cleanup.
- [ ] Run the combined canonical check after temporary inode recovery; keep it unqualified until actual exit0.
- [ ] Freeze the scoped media delta, evidence and known limits for Root integration.

### Implementation evidence

The focused `earph-media` package run passes 76 tests with zero failures or ignored tests.
The retained log is `target/streaming-flac/logs/all-media-final-01.log`.
Strict package Clippy and scoped Rustfmt also pass.
The real-codec matrix checks 136 variants, 408 packets and 1,705,392 integer samples.
The largest observed genuine packet is 399,399 bytes in a 2,437,462-byte audio body.
Metadata prefix capacity reaches 1,049,312 bytes at the 1,048,576-byte input limit.
The fixed raw mirror remains 2,097,152 bytes.
These observations do not establish a complete upstream heap or CPU bound.

The initial API RED and EOF-audit removal RED remain preserved with numeric exits.
The EOF regression passes after restoring the required mirror check.
The only permitted predecessor lock change adds the existing `earph-network` development edge to `earph-media`.
Independent review and the combined canonical check remain explicit Root integration gates.

## Review focus and limits

Do not convert swallowed errors or unmatched bytes into EOF.
Do not count the expected short-prefix failure twice in MD5.
Do not let unknown duration disable declared-count checks for local files.
Do not change local seek poisoning or publish old pending blocks after a failed seek.
Do not claim the mirror bounds all parser allocations or CPU time.
Do not claim asynchronous transport state from errors the generic reader has not observed.
This unit does not qualify Music Assistant flow, network gapless, physical output or the whole T14 source.
