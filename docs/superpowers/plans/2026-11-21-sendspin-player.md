---
title: First authenticated PCM Sendspin player - Plan
type: feat
date: 2026-11-21
origin: docs/superpowers/plans/2026-11-21-sendspin-live-clock.md
---

# First authenticated PCM Sendspin player - Plan

## Goal Capsule

Doctor Biz, make the existing authenticated connection feed real timestamped PCM into Earph's single output owner.
Offer only PCM signed 16-bit, 48000 Hz, stereo.
Keep the protocol timeline independent of Engine, native output, and credentials.
Complete this slice through actual wire, owner, and output consumers before advertising operational readiness.

## Product Contract

### Problem Frame

The live connection now produces authenticated clock mappings.
It still rejects every player role and playback activity.
The credential bridge also rejects player offers and playback admission.
Output progress has frame counts without a presentation anchor in the connection's monotonic domain.
Fixed-rate playback alone cannot maintain the pinned synchronization contract.

### Scope and assumptions

Use the existing outbound connection, native credential supervisor, owner IPC, and playback worker.
Use an existing committed native document and an existing authorized connection endpoint.
Require a matched paired identity; leave unpaired playback denied.
Do not provision credentials, grant network access, select startup credentials, add discovery, or reconnect automatically.
An explicit owner command starts this path; merely launching Earph does not make a player ready.

Use one 48 kHz stereo output path with a declared synchronization transformation.
Initially admit a graph only when its complete timing and frame projection are known.
The identity graph is the smallest first operational configuration.
Reject strict sample preservation when synchronization can alter samples.
Keep FLAC, other formats, native DSD carriage, broader controls, and Music Assistant qualification as later work.
Keep independent-clock and acoustic qualification as explicit later release gates.
None of those deferred gates permits claiming full SRC-02, SRC-03, AC-16, or AC-17 now.

### Requirements

R1. Accept only an explicit, current, native-backed player connection bound to the owner and paired server.
Require affirmative playback admission, current source takeover, output preparation, and compatible processing policy independently.
Do not infer any of these permissions from a transport permit, mapped clock, or credential load.

R2. Advertise exactly `player@v1` with one supported format: `{codec: pcm, channels: 2, sample_rate: 48000, bit_depth: 16}`.
Advertise `supported_commands: []` and ignore unoffered player commands.
Offer no controller, source, metadata, FLAC, Opus, DSD, or unsupported player control.

R3. Send initial player state only after the current filter count reaches one and its mapping remains usable.
Report `available: true` only while all R1 conditions and the output timing contract hold.
The state contains the full player object and every required numeric field.
Every reactivation requires a fresh player object before binary data can be admitted.

R4. Process activation, stream/start, audio, stream/clear, and stream/end in original authenticated delivery order.
Apply lifecycle messages even while unavailable or discarding audio.
Ignore inactive-role objects without validating their contents, as the pinned messaging rules require.
Reject active player formats outside the single advertised format.

R5. Decode ID-4 only after authentication and reassembly.
Validate its complete 13-byte header, numeric envelope, whole stereo frames, and bounded payload before allocation.
Decode signed little-endian i16 samples without changing their values.
Keep each queued chunk's format, server timestamp, captured arrival, wire size, and stream generation together.

R6. Full clear discards queued and staged player audio while retaining the negotiated format.
Partial clear discards whole buffered chunks whose first timestamp is at or after `from_timestamp`.
Keep earlier chunks intact, including a chunk that extends across the cutoff.
Replacement audio must not replay any already submitted, irretractable backend suffix.

R7. End immediately makes the player stream inactive and stops and clears its output.
Do not reinterpret end as source EOF, graph-tail completion, or an instruction to drain old audio.
The pinned player role defines no completion override to the core stop-and-clear rule.

R8. Bound encoded input, decoded frames, metadata, transferred work, staging, and backend ownership.
Reserve downstream capacity before releasing upstream credits.
Retained leases and in-flight decoded work count until released.
No clear, cancellation, or clock update waits behind an unbounded audio queue.

R9. Compute the port target by the existing canonical server-to-local mapping, then subtract external output delay once.
Account separately for pre-port graph, synchronization, staging, backend, and known device delays.
Use actual output progress in the same monotonic domain before submitting timestamped samples.
Unknown timing or a stale binding cannot become a guessed zero delay.

R10. Maintain a declared synchronization stage and honest preservation observations.
Meet the pinned speed cap and recovery rules in the tested operating envelope.
Unqualified steady-state accuracy remains unclaimed until measured at its stated output boundary.
Do not enable a usable player merely because timestamped chunks can reach a fixed-rate sink.

R11. Fence asynchronous work by owner, source/session, connection, clock-domain, stream-content, and output generations.
An in-place same-format start changes format ownership without clearing earlier valid chunks.
Clear and takeover revoke stale submissions before later work can commit.
Re-handshake preserves the filter and valid buffered stream only while its identities and admission remain valid.

R12. Failure and retirement preserve existing uncertain-write and acknowledged native-commit ownership.
Cancellation releases audio queues, requests output stop, and retains actual accepted-write facts.
Network retires before its credential supervisor.
No replacement reuses an unretired socket, source lease, or output owner.

### Verifiable success criteria

- S1. A real pinned Python producer sends typed start, ID-4 PCM, both clears, and end through authenticated WebSocket/Noise.
- S2. The existing real owner consumes the stream and writes identifiable sample data to an isolated native digital sink.
- S3. No player state opens binary admission before a real mapped clock and an actual prepared timing path exist.
- S4. Tagged captures prove exact clear boundaries, submitted-prefix suppression, endian conversion, and no stale replay.
- S5. Deterministic independent clocks prove scheduling, bounded correction, freshness refusal, and delay subtraction exactly once.
- S6. Cancellation and owner/source replacement leave no extra worker, lease, credit, or acknowledged commit abandoned.
- S7. The canonical check and one independent main-model concurrency/shared-contract review pass before integration.

## Planning Contract

### Existing contracts and missing seams

| Inspected code | Existing behavior | Required addition |
| --- | --- | --- |
| `crates/earph-sendspin/src/lib.rs` | `decode_player_audio` validates header size, ID, and byte budget | Typed player payloads, PCM geometry, and timeline reducer |
| `crates/earph-sendspin/src/session.rs` | Authenticated ordered messages, activation notice, paired identity, bounded admission | Preserve these authorities; expose no alternate pairing or player owner |
| `crates/earph-sendspin/src/timing.rs` | Sole filter and canonical mapping/delay arithmetic | Immutable usable mapping facts with checked expiry and the same conversion implementation |
| `crates/earph-network/src/sendspin/clock.rs` | Sole live filter driver and captured arrival domain | Export mapping facts for a downstream scheduler without another filter |
| `crates/earph-network/src/sendspin/worker.rs` | Rejects roles, playback, and all application sends except goodbye | One explicit player profile, bounded player-state send, ordered data handoff |
| `crates/earph-engine/src/sendspin_credentials.rs` | Native load, paired identity, idle/pairing-only policy | Explicit paired playback policy and one bounded player hello |
| `crates/earph-core/src/control.rs` | `SourceKind` is Virtual, LocalFile, or HttpFlac | Sendspin source and explicit lifecycle/read observations through current owner contracts |
| `crates/earph-core/src/session.rs` | Owns source/session state and takeover | Current Sendspin source lease; no adapter-local duplicate source state |
| `crates/earph-engine/src/stream_render.rs` | Bounded stream PCM, accepted-write tracking, replay epochs | Scheduled source adapter and clear-aware invalidation owned by Engine |
| `crates/earph-engine/src/output.rs` | Counters expose submitted, optional consumed/restart/delay, and basis | Same-domain output presentation estimate, validity, uncertainty, and irretractable prefix |
| `crates/earph-dsp/src/resample.rs` | `set_offline_ratio_ppm` explicitly forbids live device loops | Do not repurpose it; provide a declared live correction stage separately |

KTD1. Keep `earph-sendspin` pure and free of Engine/Core audio dependencies.
It owns typed wire meaning, chunk identity, clear selection, bounded PCM, and deterministic timeline decisions.
Engine adapts one transferred PCM allocation into its native `PcmBlock` representation.
Engine alone owns output, source takeover, processing, scheduling execution, and native cleanup.

KTD2. Reuse one conversion implementation in `timing.rs` for both TimeSync and copied mapping facts.
Mapping facts carry binding, revision, estimate, and an absolute usable-until value in their own domain.
Conversion requires current local time and expected binding; it rejects stale, nonfinite, or mismatched facts.
Network remains the only filter owner.
These facts grant no permission and retain no independently updated clock state.

KTD3. Retain server timestamps until render planning.
Map unsubmitted work with the current usable revision rather than baking one deadline at network arrival.
Keep each submitted span's actual mapping revision and source-to-output projection for later clear accounting.
Do not reschedule bytes already accepted by native output.

KTD4. Use one explicit connection profile in the existing worker, rather than a second player transport.
Core-only and PCM-player profiles have distinct allowed roles and payloads within that one implementation.
The player profile never bypasses native playback admission or widens arbitrary `try_send` traffic.
Construct hello/state from typed capability facts; do not trust caller JSON to declare output readiness.

KTD5. The first correction strategy uses the pin's whole-frame deletion/duplication option at 48 kHz.
The stage operates outside native callbacks with preallocated buffers and explicit counters.
It records every inserted silence frame, duplicate frame, dropped frame, and disturbance.
Select ASRC later only through a qualified live contract; the existing offline method is unsuitable.
This choice does not establish correction audibility or native timing accuracy by itself.

### Proposed types and ownership

Names below describe new contracts, not APIs present at the inspected HEAD.

| Contract | Owner | Required contents and behavior |
| --- | --- | --- |
| `PlayerOffer` | Sendspin | Exact fixed format and admitted wire capacity; encodes hello support |
| `PlayerEvent` | Sendspin | Typed Start, Audio, Clear, End with original ordered receive facts |
| `StreamFormat` | Sendspin | Validated PCM codec/rate/channels/depth; absent codec_header for PCM |
| `PlayerBinding` | Sendspin value, Engine authority | Connection/domain/source/content generations; output generation stays Engine-owned |
| `PcmChunk` | Timeline, then Engine | Immutable server start, frame count, format revision, arrival, and owned interleaved i16 samples |
| `PlayerTimeline` | One Engine-side source owner | Sole pending chunk queue, active format, clear generation, bounded wire/frame/entry usage |
| `UsableMapping` | timing value from Network | Bound estimate and checked expiry; canonical conversion, no readiness token |
| `OutputTimelineObservation` | Engine/output | Output epoch, domain generation, sample instant, absolute output frame anchor, rate, basis, uncertainty, validity |
| `SubmissionSpan` | Engine | Actual accepted output range projected to server audio intervals under the applied transform |
| `ClearResult` | Engine to timeline | Actual retracted ranges and irretractable suffix endpoint; no inferred rollback |
| `PlayerOperationalState` | Engine | State derived from source authority, admission, mapping, prepared output, ledger, and processing policy |

The timeline accepts copied output facts; it never invokes native output or credential APIs.
The Engine source owner serially applies events and obtains bounded render spans.
An outstanding span cannot outlive its generation without an explicit rejected or accepted result.
Cancellation cannot erase a native write that already succeeded.

### Wire and timeline details

Accept unknown payload keys without rejecting the message.
Validate only recognized active-role fields and required core fields.
Use integer JSON accessors, not floating-point coercions.
Check server timestamps and all arithmetic against the existing ±(2^53−1) envelope.
Keep existing future role compatibility in the session layer.

For active player start, require integer `server_transmitted` and the exact advertised format.
PCM's known `codec_header` field must be absent.
Repeated same-format start keeps previous chunks and continues the existing timeline.
It must not reset buffer accounting, clear generation, or output history.
Start after end creates a fresh stream generation and empty timeline.
Discard audio without an active player/playback role before inspecting its role payload.
For active-role ID-4, enforce the truncated-header close rule even when unavailable.
Valid unavailable audio is discarded without closing solely for arrival.
Active-role audio outside an active stream is a protocol fault.

Use the existing decoded ID-4 header rather than creating another wire parser.
`Complete.body` excludes the message ID; avoid copying the whole message merely to prepend byte 4.
Factor a shared header decoder or add a checked ID/body entry point used by the existing decoder.
Keep one layout implementation and no legacy layout detection.
Accept zero PCM frames as a bounded no-op without constructing Engine's nonempty `PcmBlock`.
Retain required header accounting until its completion/reset rule permits release.
Maximum PCM chunk duration is 150 ms: 7200 frames, 28800 sample bytes, and 28813 total wire bytes.
Do not enforce the server's 15 ms SHOULD as a minimum; short final chunks remain valid.

Use exact rational frame durations and cumulative source frame positions.
Do not repeatedly round each frame to 20 or 21 microseconds.
Integer chunk timestamps can truncate on the server's time grid; tolerate that subframe quantization.
Reject actual conflicting overlap outside explicit clear replacement; do not reorder away protocol delivery order.
Timestamp gaps create scheduled silence or explicit late-drop decisions, with transformation observations.
`send_ahead` never changes a target.
Its saturated values remain excluded from arrival-delay measurements.

Full clear cancels old source/staging/replay spans and advances content generation.
Stop/reset backend queued audio when the actual adapter supports that operation.
Retain the actual irreversible residue and report any stop failure.
The clear keeps the latest format and accepts only later ordered audio into its new content generation.

Partial clear selects chunks by their original first timestamp, not by decoded sample timestamps.
Do not slice a kept earlier chunk merely because its tail crosses the cutoff.
Remove matching unsubmitted data from timeline, processing staging, and output replay.
If the backend cannot retract an already submitted suffix, preserve its measured endpoint.
Drop replacement frames up to that endpoint, including a leading prefix inside a replacement chunk.
Compare rational server frame positions and round dropped frame counts upward to avoid replay.
Do not infer that submitted native frames equal source frames after correction or rate conversion.
If a chunk boundary is no longer retained, do not invent a history entry to validate the server cutoff.
Keep bounded diagnostic history and treat unverifiable peer-cutoff claims as such.

End, role removal, playback removal, source takeover, and cancellation clear all future work.
End keeps the connection and mapped filter alive when the session remains valid.
Role or playback removal also revokes output participation, even after an earlier end.
Clear and end remain ordered control events; a separate priority queue must not move them ahead of earlier audio.
Cancellation may bypass ordinary capacity because it terminates that entire ownership generation.

### Bounded resources

Use the existing transport ceilings without increasing them.
Initial player defaults are at most 1048576 outstanding wire bytes, 262144 decoded stereo frames, and 512 chunk descriptors.
Allow callers only to tighten those values.
Enforce byte, frame, and descriptor bounds independently before decoding or transferring ownership.
Charge allocation capacity, including one admitted decode overlap, rather than only sample length.
Include Engine staging, output replay, correction scratch, native adapter storage, and metadata in its preparation budget.
Native opaque storage stays named separately when it cannot be measured.

Advertise only the wire capacity supported by the complete downstream preparation.
Wire accounting uses each chunk's 13-byte header plus encoded payload.
Retain immutable frame count and original format for completion-time calculations.
Server outstanding-byte accounting is distinct from current client allocation usage.
Do not free allocation credits merely because protocol completion time passed.
Do not free protocol history merely because the client decoded or dropped a chunk.
Use compact bounded metadata for accounting facts after sample release.
Full clear and end reset protocol accounting; partial clear removes only selected chunks.
An in-place start changes neither old duration nor accounting.

If a valid peer exceeds local capacity, report a local budget refusal and resynchronize or retire explicitly.
Do not classify every local limit as malformed protocol.
Do not add another unbounded overflow queue.
The first slice keeps output_delay fixed for the live connection, so delay-decrease excess is deferred with the unoffered control.
Persist its per-output value through the existing owner settings contract before using a nonzero value.
If no existing compatible persisted field exists, add one canonical settings field; do not keep an adapter-side file.

### Readiness, latency, and synchronization

Construct `player@v1_support` only after preparing a genuinely usable fixed-format player implementation.
Hello describes capabilities, not initial operational availability.
Activation does not open output admission before R1 and R3 hold.
Do not emit an initial `available: false` as a fabricated clock-ready substitute.
Wait for count≥1 before either initial state value while the player role is active.

Initial player state carries persisted output_delay_ms, measured/admitted required_lead_time_ms, min_buffer_ms, and no commands.
The required lead covers the maximum irretractable backend audio while running.
Exclude external output_delay_ms from lead and min-buffer values.
Choose explicit conservative admitted startup values, then update them only from sustained real measurements.
Do not report constants as measured device latency.
If the minimum supported lead exceeds the server's offered lead, drop late material and report recovery.
Use `available: false` for the pin's external non-interruptible-source meaning.
For an output fault or unusable clock, stop samples and retire or recover the connection explicitly.
Do not mislabel broken timing as an external source takeover.

The latency ledger records each stage's boundary, rate domain, frames/time, measurement basis, uncertainty, and generation.
Its target is `map(server_timestamp) − output_delay_ms×1000` at the audio port.
Derive submission time by subtracting only ledger stages between submission and that port.
Do not subtract backend buffering twice when the presentation anchor already includes it.
Network jitter is a startup/buffering constraint, not another signal-path delay.
Preserve unknown values and refuse operational readiness when the selected path cannot place samples honestly.
Sample output progress between two same-domain monotonic reads when native timestamps cannot be mapped directly.
Retain that bracket as uncertainty; it is an estimate, not a DAC timestamp.
Use a native presentation timestamp only with a documented conversion into the same domain.
Invalidate the anchor and ledger on output epoch, route, format, graph, or clock-domain changes.
If native startup needs silent priming before an anchor exists, model that bounded phase explicitly.
Respect the existing signal gate during priming and never advertise operational readiness from its mere start.
An adapter that cannot establish its required anchor without unsupported behavior stays unavailable for this path.

For initial discrete correction, use a 100 µs dead band and whole stereo frame edits.
At 48 kHz, the pin's nominal step is one frame per decoded chunk.
Keep a sliding 7200-output-frame correction ledger to enforce the ±0.5% speed limit over 150 ms.
Do not rely only on a per-chunk cap; short chunks could violate the sliding window.
Choose larger bounded steps only when they meet the same cap and recovery rule.
Startup, full clear, underrun, and unrecoverable large errors use explicit drop-prefix or silence resynchronization.
Use current mapped target versus predicted output presentation to choose early/late correction direction.
Update disturbance state when a mapping change moves error beyond ±1 ms.
Record altered payload status even when most frames pass unchanged.
Digital sink qualification proves producer-to-consumer content and declared scheduling decisions, not acoustic synchronization.

## Implementation Units

### U1. Pure typed player and timeline

Ownership: Sendspin implementation owner; no Engine files.
Files: `crates/earph-sendspin/src/player.rs`, `src/player/timeline.rs`, `src/lib.rs`, `src/timing.rs`.
Tests: `crates/earph-sendspin/tests/player.rs`, `tests/player_timeline.rs`, and existing timing tests.
Implement R4–R8, R11's pure generations, and KTD1–KTD3.
Factor the canonical ID-4 and mapping conversions rather than duplicating them.
Give every proposed type a bounded constructor and redacted Debug where it holds bindings or payload data.

Test actual typed encode/decode round trips, little-endian extrema, missing fields, numeric bounds, and unknown keys.
Test zero-frame no-op, 1-frame and 7200-frame chunks, 7201-frame refusal, incomplete stereo frames, and truncated headers.
Test in-place start retention, full clear, partial clear with a straddling kept chunk, and end.
Test format ownership, nonaligned replacement trimming, expired mapping, old generations, and exact capacity release.
Test mapping revision changes with unchanged server timestamps and no second delay subtraction.

### U2. Explicit player transport and native admission

Depends on U1's typed offer/state contracts.
Ownership: one Network/native-services implementation owner, coordinated with Root for shared changes.
Files: `crates/earph-network/src/sendspin/{mod.rs,worker.rs,clock.rs}` and `crates/earph-engine/src/sendspin_credentials.rs`.
Tests: `crates/earph-network/tests/sendspin_player_transport.rs`, existing transport/clock tests, and native credential process tests.
Implement R1–R4 and preserve R8/R12 transport and service ownership.
Retain the existing core-only operation within the same profile-aware implementation.
Allow only the exact player's declared roles/activity; reject other roles and mixed activity sets outside that explicit policy.
Require `AdmissionProposal::paired_identity()` for playback and bind it to the loaded native connection lease.
Keep policy expiry/revocation and the completion-delivery fence.

Use the real session producer and consumer to prove delivery order through fragmentation, rotation, and deferred admission.
Prove player hello/state cannot be forged through generic sends.
Prove no player state precedes clock count one, and reactivation requires a fresh role object.
Prove missing native storage, unpaired identity, revoked policy, expired policy, and stale lease deny playback.
Retain the actual commit completion after close, even when player handoff queues are full.

### U3. Single-owner scheduling and output contract

Depends on U1 and U2; Root owns this integration area.
Root coordinates with the DSD/Engine owner before any shared edit.
Files: `crates/earph-engine/src/sendspin_player.rs`, `src/worker.rs`, `src/output.rs`, `src/stream_render.rs`, `src/lib.rs`.
Shared contracts: `crates/earph-core/src/control.rs`, `src/session.rs`, and relevant existing source/settings modules discovered there.
Native timing edits, if required: `crates/earph-alsa/src/{native.rs,output.rs}` and Engine Core Audio adapter files.
Tests: `crates/earph-engine/tests/sendspin_player.rs`, existing worker/service/stream-render tests, and applicable native adapter tests.
Implement R1/R3/R6–R12 and KTD5.
Use existing owner source takeover and output retirement; do not open a second native owner inside the adapter.
Add precise output timing facts and the latency ledger before enabling operational state.
Use one serialized source owner to apply the pure timeline and output write results.
Keep correction as an explicit Engine processing stage; never change offline SRC semantics.

Test strict-policy refusal, takeover conflicts, route invalidation, early silence, late prefixes, and mapping expiry.
Test actual partial native writes racing clear/cancel and prevent replay of their submitted prefix.
Test full clear and end stop rather than finish/drain; preserve original stop failure and timing uncertainty.
Test correction at ±0.5%, tiny chunks, 150 ms rolling boundaries, startup, underrun, and large-error recovery.
Test output progress against absolute backend_origin_frames, including pause/reopen epochs.

### U4. Real producers, consumers, and documentation

Depends on U1–U3; Root owns serialized qualification.
Files: existing Python peer support plus focused player scenarios; `docs/sendspin-runtime.md`, `docs/sendspin-timing.md`.
Add a focused player contract document and honest requirement evidence only after checks run.
Reuse `crates/earph-engine/tests/service_playback.rs` private ALSA file/null sink pattern.
Use actual owner IPC, actual native credential helper/store, real WebSocket/Noise, and real PCM capture.
Keep all synthetic credentials, raw transcripts, and endpoint details in private retained storage.

Test missing readiness with zero output and absent available-true state.
Then prove clock establishment, source takeover, typed state, scheduled PCM, both clears, and stop on end.
Use tagged silence/impulses or ramps with unique chunk markers and exact expected native sample counts.
The file/null sink cannot certify a physical clock or an audio port timestamp.
Use deterministic output-clock tests for arithmetic and later instrumented native tests for timing claims.
Real Music Assistant 2.10.12 and two independent outputs remain separate named qualification gates.

## Verification Contract

Use TDD and retain each expected RED failure before changing behavior.
Run unit and integration checks for Sendspin, Network, Core, and Engine paths touched by the implementation.
Run the real pinned Python producer gate with the existing exact-source verifier.
Run `scripts/check`, which includes format, strict Clippy, workspace tests, catalog checks, and dependency checks.
Run applicable platform checks on each changed native adapter.
Root serializes full suites, shared provider checks, physical output, and timing qualification.
Isolated controller regressions may run alongside independent credential checks using separate processes, sockets, files, and file/null outputs.
That exception grants no physical output, shared settings, performance measurement, or native clock accuracy claim.
Run one independent main-model review of the actual shared-contract/concurrency diff and its test evidence.
Resolve material findings before integration.

The contract preparation ran no implementation or runtime checks.
Doctor Biz authorized continued first-release implementation; Root released U1 to its isolated implementation owner.

## Definition of Done

S1–S7 pass with actual source and binary provenance.
The owner reaches operational state only from current native admission, clock facts, source ownership, and output timing facts.
Player captures show the requested PCM path and stream lifecycle without stale replay.
Observations distinguish source PCM, synchronization edits, actual output format, and timing uncertainty.
The requirement catalog keeps full interoperability and independent-clock synchronization incomplete until their named gates pass.
Remove abandoned implementation paths, compatibility shims, and experimental fallback modes before integration.

## Risks, blockers, and handoff

The pure U1 work is implementation-ready from this contract.
Operational player readiness depends on U3's new same-domain output observation and submitted-span projection.
Those contracts do not exist at the inspected HEAD.
The current offline SRC interface cannot supply a live correction loop.
The proposed discrete correction needs deterministic timing tests and later native quality evidence.
If a backend cannot supply adequate presentation facts, keep that selected route unavailable and continue supported-route work.
Do not quietly substitute a fixed-rate player and declare the milestone complete.

## Sources and confidence

Root retains the preparation evidence packet with hashes for inspected local and pinned upstream sources.
The normative pin remains Sendspin/spec 9d2f15ee8f172c67ce00dc0d9f6603c3b1030d9f.
The actual Python peer remains aiosendspin 10.2.1 at 692831b07bc453a38d5a39a16ee566141547bfa1.
Music Assistant 2.10.12 remains pinned to 6ab5bfa63f02d0daa237d342097a42feae5c5aae.

The Rust SDK comparison remains fce4c78e0833e8046f333f978fb972d31548a6c7.
Its owned AudioBuffer retains a timestamp and format, which supports per-chunk format ownership.
Its generation fences offer a useful ownership comparison, not an implementation dependency.
Its retained StreamClear has only roles and lacks this pin's server_transmitted and from_timestamp fields.
Its synchronized player still names static_delay_ms; do not adopt that schema or a second output thread.
The retained Python player model provides the pinned ID-4 header producer and format structures.
Use those real producers for interoperability fixtures, not as the timing arithmetic oracle.
No upstream source is copied; no new dependency or license incorporation is proposed.

Confidence is high in the pure protocol and ownership contract.
Output timing feasibility remains backend-specific and unqualified by this read-only work.
One direct consistency review covered this private plan and brief.
Root reviewed this contract before assigning isolated U1 implementation.
Independent implementation review and runtime qualification remain required gates.
