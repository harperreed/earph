# Admitted FLAC worker plan

Doctor Biz authorized autonomous implementation and parallel work.
Root accepts the concrete worker option in `target/network-owner-preparation/round-01/design.md`.
The network reader and media decoder have independent review receipts.
Their combined canonical check remains pending temporary inode recovery.
Root reopens the scoped network files for this next unit.
Preserve both predecessor freezes and every artifact.

## Goal and success

Compose admitted HTTP transport and FLAC decoding in one engine-owned lifecycle.
Expose measured format, optional duration, one pending frame, and distinct terminal outcomes.
Reject stale binding, cancellation, and observed transport failure before frame handoff.
Retain every thread until confirmed retirement.
Prove the public worker API through real HTTP and an independent process consumer.
This unit prepares source ownership; owner onboarding and physical playback follow separately.

## Scope and ownership

One implementation owner edits these files in the existing network task worktree:

- `crates/earph-network/src/policy.rs` and `src/fetch.rs` for immutable binding and observation access.
- Network transport tests for real observation and handoff ordering.
- `crates/earph-engine/src/network_decode.rs` for one concrete worker.
- Engine module export, path dependency, and real worker tests.
- `Cargo.lock` for the existing engine-to-network path dependency edge.
- `build_support/provenance.rs` for actual network runtime inputs.
- Network lifecycle docs, state machine docs, and this plan.

Add no third-party dependency.
Keep `earph-media` frozen.
Keep Core commands, queue contracts, graph acknowledgements, and output contracts outside this unit.
Root owns final inventory reconciliation, integration, commits, publication, and combined canonical verification.
Never delete files or artifacts without Doctor Biz's approval.

## Contracts

Expose `earph-engine::network_decode::NetworkFlacWorker` as a concrete Rust API.
Accept a `FetchPermit` and an independently captured current `SourceBinding`.
Compare every binding field before starting transport.
Require current binding again for readiness, frame, and completion adoption.
Document live owner serialization as the caller's authority precondition.
Comparing a permit with its own copied identity does not establish that authority.

Use `DecodedPcmFrame` directly.
Preserve `Option<u64>` duration, including after completion.
Do not fabricate queue entries, epochs, time domains, or playback observations.

Expose a cloneable read-only transport observer before moving the reader into decoding.
Reuse one observation implementation.
Observer drop neither cancels nor retires transport.
Use a short transport publication guard for frame handoff.
A diagnostic snapshot alone cannot close the terminal-publication race.
Reject failed, cancelled, or exited-without-terminal transport under that guard.
Allow completed transport while accepted buffered frames drain.
Require matching transport `Complete` for successful media completion.

Use one lock order: engine job state, then transport pipe.
Never decode, wait, retire, call arbitrary callbacks, or perform output I/O under either publication lock.
The frame move defines this worker's acceptance boundary.
Later output integration needs its own acceptance fence.
Do not claim to retract a frame accepted before a later failure.

## Storage and lifetime

Own one decoder thread and the existing transport worker.
Retain one pending PCM frame and wait before decoding another frame.
Keep one readiness record and one terminal outcome.
Keep observations and terminal history bounded.
Measure the handoff payload bound separately from upstream parser and decoder allocations.

Cancellation revokes handoff authority even after HTTP completion.
Wake the frame-slot wait and blocked transport reads.
Preserve the first genuine failure through later cancellation and retirement.
Keep completion, cancellation, media failure, and transport failure distinct.

Use explicit idle, active, and retiring ownership.
Block replacement until both threads retire.
Request cancellation before waiting for decoder completion.
Retain handles after cleanup timeout and support another retirement attempt.
Document decoder and transport waits separately when each permits one second.
A final Drop safeguard may block rather than detach ownership.
Handle partial startup and unexpected decoder exit without losing handles or stranding consumers.

## Verification

Write failing behavior tests before implementation.
Retain numeric exits and logs in a fresh `target/network-flac-worker` packet.
Keep temporary files under a private HOME task directory.
Reuse the current task Cargo target and pinned tools.
Disable Cargo automatic cache cleanup and incremental compilation in focused builds.
Do not run the full canonical suite while temporary inodes remain exhausted.
This restriction does not waive the final canonical gate.

- Reject each mismatched binding field before transport starts.
- Verify exact integers, spans, and optional duration through real known-length and chunked HTTP.
- Fill the single frame slot and verify backpressure without a second handoff frame.
- Fail actual HTTP framing while PCM waits and reject the pending frame.
- Cancel while PCM waits, including after transport completion, and reject handoff.
- Verify both transport-failure and frame-handoff orderings with explicit synchronization.
- Replace current authority while frames or completion wait and reject old results.
- Cancel stalled startup, body reads, and full buffers; verify peer closure and thread retirement.
- Preserve corrupt or partial media tails as failure after HTTP completion.
- Verify media EOF plus matching transport completion before success.
- Exercise the production Rust API in a child process, including cancellation and replacement.
- Exercise retained ownership after cleanup timeout through real owned operations where practical.
- Explain any impractical timeout baseline without weakening the implementation or claiming coverage.
- Run focused network and engine checks, formatting, strict Clippy, and provenance checks.
- Obtain one independent main-model concurrency and ownership review.
- Run the combined canonical check after storage recovery and Root integration.

No application mock mode, production loopback exception, device opening, or audio emission belongs here.
Music Assistant, UPnP, Sendspin, queue takeover, and native output remain later qualification stages.

## Worker implementation checkpoint

The concrete worker and independent transport observer are implemented in the authorized footprint.
The API preserves exact bindings, measured duration, one pending frame, and distinct outcomes.
No Core schema, EngineOwner behavior, native output, or media implementation changed.
Both build provenance input lists include the network manifest and source.
The inventory producer excludes local path packages; its existing inventory validates unchanged.
Cargo.lock adds only the engine-to-network local edge over the supplied predecessor.

Focused evidence lives under `target/network-flac-worker/round-01`.
The public worker scenarios pass ten tests, including real HTTP and a child process.
Three deterministic ownership scenarios cover timeout retention, startup rollback, and media rejection ordering.
The network package passes 30 tests; the frozen media package passes 76 tests.
Existing worker playback tests pass 56 scenarios; Core staging passes twelve scenarios.
Compiled Engine provenance and shared build inputs each pass six scenarios.
Strict Clippy passes for both complete changed packages and all targets.
The independent main-model review and final integrated canonical check remain required.

The media-rejection regression first fails with an unexpected owner-cancellation outcome.
Its fix distinguishes reader destruction from explicit owner cancellation.
The decoder then preserves the real media failure.
The provenance regression first fails when an actual network manifest change leaves the digest unchanged.
Both actual input lists now include that manifest and its source tree.

The decoder timeout test gates an owned thread around the real transport reader.
The startup test injects a thread-creation error result without exhausting system resources.
Existing transport tests cover their separate retirement timeout.
The ordering test holds its guard across a bounded server-close acknowledgement for deterministic ordering.
Production never waits under the publication guard.

The shared `/tmp` mount still has zero free inodes.
Existing owner IPC and FFI tests hardcode paths there and remain pending with `scripts/check`.
The task adds no test bypass or alternate application mode.
Generated predecessor license text retains existing whitespace; this task preserves those bytes.
Root owns integration, canonical verification, and shipping after independent review.
