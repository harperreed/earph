---
title: Typed ALSA DoP Submission - Plan
type: feat
date: 2026-11-21
execution: code
---
# Typed ALSA DoP Submission - Plan

## Goal Capsule

Doctor Biz, implement the missing typed DoP submission boundary before adding DSD to the Engine playback worker.
Prove the complete software chain through a real ALSA file-to-null route.
Use generated DSF/DFF files, the existing reader and packer, and an independent capture consumer.

This unit delivers a necessary Engine integration prerequisite.
It does not deliver Engine DSD playback, a qualified endpoint, or physical DSD acceptance.
Root assigns one implementation owner and one independent review on the session's main model.

The expected change is about 650–900 handwritten lines, including tests and fixture moves.
Allow about 200–300 implementation lines, 350–450 test lines, and 100–150 fixture/doc lines.
These are planning estimates, not measured changes.

## Product Contract

### Problem Frame

The codec already emits a typed `DopBlock`, but every current playback output accepts `PcmBlock`.
Wrapping carrier words in PCM would bypass the typed safety boundary.
Direct worker integration would also change PCM-only queue and playback observations.
That combines transport, public contracts, lifecycle changes, and device transitions in one large unit.

The smallest useful vertical slice extends the existing ALSA owner with an explicit typed writer.
The actual native byte writer already supports the required six-byte stereo carrier frames.
It also preserves exact negotiation and fails without recovery or fallback.
The retained DSD preparation packet records evidence E1–E8.

### Requirements

- R1. Preserve the typed DSD lane through native submission. Covers `ARCH-04` and part of `PLAY-07`.
- R2. Submit opaque carrier bytes without gain, numeric conversion, resampling, limiting, or PCM filler. Covers `SAFE-04`.
- R3. Preserve accepted prefixes, bounded writes, explicit epochs, and terminal native errors. Covers the existing output ownership contract.
- R4. Separate software transport support, native parameter negotiation, and physical endpoint qualification. Preserve partial `AC-13` status.
- R5. Preserve existing PCM APIs, behavior, wire documents, and Engine ownership. Introduce no legacy shim or runtime test mode.

### Boundaries

Implement only stereo DSD64/DSD128 carried as packed three-byte little-endian DoP words.
Use 176400 or 352800 carrier frames per second.
Exclude padded 24-in-32 output, native DSD formats, and automatic carrier width selection.

Exclude Engine Start dispatch, queue transitions, playback observations, GUI changes, and network sources.
Exclude hardware discovery or capability claims, hardware volume, physical playback, SSH, and device testing.
Exclude DSD-aware mute, native drain qualification, seamless transitions, and acoustic acceptance.

## Planning Contract

- KTD1. Add a distinct public `DopOutput` writer in `earph-alsa`.
  It accepts `DopBlock` directly and never exposes its underlying PCM-facing owner.
  It cannot accept `PcmBlock`.
  Keep `AlsaOutput::write` PCM-only.
  This is a new domain operation, not a compatibility path for a replaced operation.

- KTD2. Reuse the existing native handle, scratch, counters, and failure cleanup.
  Keep one private native submission helper within `output.rs`.
  Both writers use that helper after their own typed validation and packing or byte copy.
  A private owned `AlsaOutput` inside `DopOutput` is sufficient if its PCM methods stay inaccessible.
  Do not add a second device owner, thread, ring, native implementation, or counter cache.

- KTD3. Retain the existing `OpenRequest` as the native carrier parameter request.
  Accept only stereo `Signed24` requests at the two carrier rates for `DopOutput`.
  Bind the writer to an explicit `DopStream` for epochs and initial source position.
  Verify that the request matches that stream before opening anything.
  The request describes carrier parameters, not decoded PCM source audio.
  Keep source bit rate and carrier rate distinct in names and assertions.

- KTD4. Preserve existing route validation and `SignalPermission` checks.
  A successful open only proves the existing exact native parameter readback.
  Keep `OpenReport.payload_preservation` unverified and retain downstream uncertainty.
  A system plugin's carrier acceptance never proves DSD-capable hardware.
  Nothing in this unit makes the Engine call the new writer.

- KTD5. Expose only open, typed write, report, state, storage, progress, bounded wait, and stop on `DopOutput`.
  Do not expose generic PCM suspend, drain, or silence through this type.
  Stop and Drop close the native owner without adding samples.
  Full playback EOF and pause behavior depend on later DSD transition qualification.
  The software capture verifies submitted bytes, not final physical playback.

- KTD6. Keep test artifacts and generated source bytes under existing ignored test storage.
  Use a child-local `ALSA_CONFIG_PATH` with a file plugin whose slave is explicitly `null`.
  Reuse the existing isolated ALSA/Engine test pattern.
  Add no environment switch or fake output mode to application code.

### Required Submission Contract

One DoP carrier frame contains sixteen source bits per channel and six transport bytes.
Keep source positions in bits and native progress in carrier frames.
Do not label either value as PCM source frames.

Before each write, validate permission, native state, epochs, carrier format, sequence, and the offered span.
Validate the stream's first marker and marker continuity across blocks and partial writes.
Bind the first block to the supplied stream's aligned starting position and sequence zero.
Reject zero, overflowing, oversized, repeated, skipped, stale, or noncontiguous spans before native I/O.

Keep a small typed cursor for sequence, marker phase, and the current block span.
Advance it only by native accepted frames.
Advance the block sequence only when the complete offered block has been accepted.
Keep the accepted native counter in its existing owner.
Do not derive accepted progress from decoder reads or packer output.

If the backend would block, retain the same pending block and cursor.
If the backend returns a genuine error, retire the native handle and preserve the accepted prefix.
Never recover an underrun by preparing, zeroing, replaying, or falling back automatically.

Keep the current per-period native scratch allocation.
Copy typed carrier bytes into that scratch without treating them as signed samples.
Reuse the PCM submission and cleanup code without changing its public contract.
Charge actual scratch capacity separately from `DSD_WORKING_BYTES`.
The existing 24580-byte allowance covers codec buffers, not this adapter or total process memory.

### Assumptions

Root adopted this prerequisite and assigned its isolated implementation.
The current Linux qualification lock remains exclusive until Root schedules the checks.
No hardware authority remains available.
The missing Mac SSH identity remains unavailable and is not a recovery task.
No new dependency is needed.

## Implementation Units

### U1. Typed carrier submission with real software capture

One implementer owns all files listed below.
Use one isolated task worktree from a Root-selected base.
Do not edit Root's dirty checkout.

| File | Change | Boundary |
| --- | --- | --- |
| `crates/earph-alsa/src/output.rs` | Add `DopOutput`, typed cursor, and shared native submission helper. | Preserve existing PCM entry points and cleanup. |
| `crates/earph-alsa/src/lib.rs` | Export the typed writer on Linux. | No Engine module registration. |
| `crates/earph-alsa/tests/dop.rs` | Add typed adapter contract and real ALSA integration tests. | No physical devices. |
| `crates/earph-engine/tests/dsd_output.rs` | Add real reader-to-ALSA capture E2E. | Existing dependencies already include media, core, and Linux ALSA. |
| `crates/earph-media/tests/support/dsd.rs` | Extract existing generated container builders into shared test support. | Test-only source of truth; no production fixture API. |
| `crates/earph-media/tests/dsd.rs` | Import those builders after the extraction. | Preserve existing test behavior and independent parser. |
| `docs/dsd-software.md` | Document the exact native software boundary and remaining gaps. | Keep `PLAY-07`, `SAFE-04`, and `AC-13` partial. |

The fixture extraction moves `chunk`, `properties`, `form`, `dff`, and `dsf` from the existing media test.
Keep fixture constructors usable by their existing malformed-container tests.
Import the same helper from the Engine test using the repository's existing test support path pattern.
Keep the Engine capture consumer independent of `DopUnpacker` and the production packer.

Do not edit `worker.rs`, `worker/network.rs`, Engine `output.rs`, `lib.rs`, or any Sendspin file.
Do not edit queue/session schemas, credentials, source manifests, dependency locks, or qualification receipts.
Root owns shared docs such as `docs/requirements.json` and `docs/state-machines.md`.
If implementation proves another shared file necessary, report the exact dependency before extending the unit.

### Test Scenarios

Use TDD and retain the red and green outputs.
Start with the first typed adapter contract and then the real capture regression.
An absent API initially gives a compile failure; record that honestly as an API-introduction red.
Once the new API compiles, run the capture regression before completing typed submission.
Retain the expected missing-submission or incorrect-byte failure from that TDD stage.
If that baseline proof is impractical, record the exact gap and run the available producer/consumer checks.

1. Verify opening never submits frames or marks a device qualified.
   Check both rates, exact signed24 negotiation, and an unverified preservation report.
2. Capture known positive and negative-looking transport words without interpreting them numerically.
   Split a block after one carrier frame and across an odd marker boundary.
   Check exact bytes and independent recovered source bits.
3. Test stream cursor transitions with zero progress, partial progress, full completion, and failure.
   Check that sequence and marker phase advance only over accepted frames.
   These pure state tests make no native-backend claim.
4. Refuse stale epochs, changed rates, changed layouts, wrong sequence, wrong marker phase, and wrong initial position.
   Refuse repeated/skipped offsets, an oversized period, zero frames, and arithmetic overflow.
   Check that captures contain no rejected bytes and counters retain their previous values.
5. Deny writes without `SignalPermission::Granted`.
   Confirm that denial submits no frames and does not advance the typed cursor.
6. Use the existing `/dev/full` file-plugin pattern for a real native write error.
   Preserve the actual errno, accepted prefix, terminal failure, and descriptor cleanup.
   Capture expected ALSA diagnostics in the child process.
7. Cancel between accepted prefix and pending suffix by stopping and dropping the typed owner.
   Confirm that the capture ends at the accepted boundary without filler or suffix replay.
   Reopen with a fresh epoch and packer, then reject the old epoch.
8. Seek the actual source reader to an aligned nonzero bit position.
   Open a fresh typed boundary with marker `0x05` and sequence zero.
   Verify independent recovered bytes against the exact source suffix.
9. Generate DSF in both supported bit orders and uncompressed DFF at both rates.
   Read fragmented blocks across the DSF channel-block boundary and a final short period.
   Run production `DsdFile`, `DopPacker`, and `DopOutput` into actual ALSA capture.
   Reopen the capture independently and compare payload, channel order, marker phase, length, and source suffix.
10. Check that Drop and explicit stop close owned descriptors and remain safe after a native failure.
    Check that retained captures and source fixtures remain available and source bytes stay unchanged.
11. Run independent child-owned streams with distinct configs, files, epochs, and marker phases.
    Verify that no stream observes another stream's bytes or accepted counts.
    Keep one native handle owner per stream; introduce no shared mutable runtime state.
12. Keep compile-fail type examples proving `DopOutput` rejects `PcmBlock` and PCM consumers reject `DopBlock`.
    Run existing PCM ALSA tests to verify the shared helper preserves prior behavior.

Bound the E2E reader request to `min(4096, period_frames * 2)` bytes per channel.
That produces at most one carrier period and respects the existing codec limit.
Complete `DopPacker::finish()` before closing the software capture.
Close the output with stop; do not test generic native drain as DSD EOF behavior.

Actual short native writes and `EAGAIN` may not occur on a file-to-null route.
Do not claim those native outcomes from cursor unit tests or caller-selected small write spans.
Retain this coverage limit unless a real software endpoint produces those outcomes without new tools or hardware.
Do not add mocked native claims to satisfy it.

## Verification Contract

No commands in this section ran during preparation.
Root must schedule verification after the shared Linux qualification lock becomes available.
Use Root's approved pinned toolchain launcher and retained-log environment.
Avoid an unreviewed `mise exec` wrapper because the machine notes record tool pruning.
Do not change global tool settings or delete earlier artifacts.

| Command | Purpose | Done signal |
| --- | --- | --- |
| `cargo test --locked -p earph-alsa --test dop` | Typed adapter and real software ALSA integration. | All new tests pass with retained captures. |
| `cargo test --locked -p earph-engine --test dsd_output` | Generated source through native software output and independent consumer. | Exact source bits and accepted prefixes agree. |
| `cargo test --locked -p earph-alsa --test alsa` | Existing PCM native contract. | Existing PCM behavior stays correct. |
| `cargo test --locked -p earph-core --test dop` | Existing codec contracts. | Marker, budget, ordering, and policy tests pass. |
| `cargo test --locked -p earph-core --doc` | Existing and added typed boundaries. | Compile-fail examples pass. |
| `cargo test --locked -p earph-alsa --doc` | Typed native writer API. | PCM/DoP misuse remains a compile failure. |
| `cargo test --locked -p earph-media --test dsd` | Reader and extracted fixture support. | Existing reader cases pass. |
| `scripts/check` | Canonical format, lint, build, workspace tests, catalog, and dependency checks. | Complete exit zero after serialized execution. |

The new Engine E2E test uses real libraries and the existing native ALSA byte writer.
It does not start an owner or claim Engine playback controls work for DSD.
After checks, require one independent read-only review of the actual diff and relevant callers.
Use the session's main model for this shared native transport and ownership change.

## Definition of Done

- Production typed carrier submission exists without a PCM block conversion or new dependency.
- A real generated-file pipeline reaches ALSA file-to-null capture and an independent consumer.
- Rejected work emits no bytes; genuine native failures retire the one owner.
- Partial writes preserve accepted progress and never regenerate or replay the accepted prefix.
- Source and native frame domains remain explicit, bounded, and separately observed.
- Existing PCM contracts and the canonical check pass.
- Independent review findings are fixed or rebutted with caller/test evidence.
- Docs state that Engine DSD playback and physical endpoint qualification remain incomplete.

## Dependent Follow-Up

After U1, plan Engine DSD source ownership and typed observations as the next integration unit.
First settle a DSD observation contract alongside the PCM-only `PlaybackObservation` and `PreparationStatus::Ready` contracts.
That contract must describe source bit rate, source bit position, carrier format, accepted carrier frames, and qualification state.
Do not manufacture a `PcmFormat` source observation or readiness from file admission alone.

Then add one DSD source variant under the existing worker's single source/output owner.
Keep cancellation, epoch fencing, accepted prefix retention, safety priority, and retirement in that owner.
Do not create a detached DSD owner or add DSD to `Reader::Local` as PCM.
Keep graph activation and software attenuation incompatible with the typed DSD lane.

That follow-up must explicitly define pause, seek, stop, EOF, queue changes, and failed output transitions.
Native PCM drain or suspend cannot establish safe DSD transitions by inheritance.
The Core Audio adapter also remains PCM-only and needs its own typed output integration and software qualification.
Physical qualification remains separate and requires new concrete authorization for the chosen endpoint and signal.

## Known Blockers and Limits

| Severity | Finding | Consequence |
| --- | --- | --- |
| Blocking for execution now | Another session holds the shared Linux qualification lock. | Preparation only; Root schedules builds/tests later. |
| Blocking for Engine playback | Queue and playback observations only describe PCM. | The dependent worker unit needs explicit typed shared contracts. |
| Blocking for product DSD readiness | No inspected capability path or current physical evidence establishes a qualified DSD endpoint. | Keep support and readiness partial. |
| Blocking for physical acceptance | Previous hardware authorizations are consumed. | No signal, hardware experiment, or inferred renewed approval. |
| Blocking for Mac qualification | The SSH identity is unavailable. | No SSH retry, credential recovery, or Mac result claim. |
| Coverage limit | File-to-null may not produce actual short writes or `EAGAIN`. | Cursor tests prove state handling only. |
| Coverage limit | Stop-based capture proves submitted bytes, not physical EOF, mute, or acoustic behavior. | Do not close `AC-13`. |

This brief was checked directly against the retained source evidence.
No independent document-review agent ran during preparation because the assignment forbids extra agents.
Root owns the required independent implementation review before integration.
