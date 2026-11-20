# Owner HTTP-FLAC playback plan

Doctor Biz authorized autonomous completion and parallel implementation.
Root accepts the owner integration design after inspecting its callers and shared contracts.
The reviewed transport, decoder, and engine decode worker provide the implementation base.
Storage is available again, and the previous ignored build evidence is missing.
The fresh combined Linux canonical check passes all 1,305 tests.
Five existing tests remain ignored.
Native audio, Mac GUI, Swift, and release gates remain separate.

## Outcome

A local controller grants exact origins, stages a URI, and explicitly starts HTTP-FLAC.
The owner uses the already selected and consented output.
Preparation starts muted and exposes measured readiness.
The first Unmute requires an explicitly activated graph.
Pause, Resume, Mute, Stop, shutdown, expiry, revocation, and takeover have observable settlement.
The local queue survives independently.
Known and unknown frame totals remain truthful.

Use the existing floating PCM streaming renderer for the initial network lane.
Support qualified 16/24-bit mono/stereo FLAC and nonseekable playback.
Reject strict network preservation before fetching.
Keep strict integer network preservation as a later qualification stage.
Do not add UPnP, Sendspin, discovery, or another protocol listener here.

## Decisions and contracts

Preserve `LocalFile`, local queue records, `PlaybackObservation`, and `GraphActivationAck`.
Add network-specific observations and graph acknowledgements.
Share internal installation, gain, output, graph-tail, and retirement logic.
Never fabricate queue IDs, known duration, or a server presentation clock.
Use a concrete private source enum, not a source plugin framework.
Keep one playback worker and one native output owner.

Add `SourceKind::HttpFlac` and an explicit HTTP source-frame domain.
Add optional network playback state and omit it when absent.
Local-only state retains its existing field shape.
Update bundled strict desktop parsers with the owner feature.
Do not claim universal compatibility with unknown external exhaustive parsers.
CLI and UniFFI retain their raw JSON transport contracts.

Keep one volatile grant registry in Session-owned source state.
Use immutable shared documents across transaction staging and retained source bindings.
Core must not depend on the network crate.
The host validates real URL/grant policy before committing authority.
Feature-disabled Session callers reject unsupported network operations.
Existing URI staging remains unadmitted and causes no I/O.
The trusted descriptor accessor requires its exact media ID and staging generation.
Derive the network lifecycle from active, closing, then final source facts.

Use these additive methods through the existing command envelope:

- `sources.network.get`: capabilities, generation, expiry, counts, and active source lifecycle.
- `sources.grants.replace`: expected generation, exact origins, pins, DER roots, and lifetime.
- `sources.grants.revoke`: expected generation and explicit active-source retirement.
- `sources.start_http_flac`: exact staged identity/generation, grant generation, device, and takeover intent.

Network start uses an explicit fresh session ID.
Treat explicit grant revocation as a safety mutation.
Use separate checked staging, source, and grant generations.
Bind every result to owner, session, media, source generation, and grant generation.
Capture current authority from committed Session state when constructing permits.
Comparing a permit with its own clone does not establish live authority.

Reuse the existing eight-origin, eight-pin, root-count, and root-byte caps.
Keep the one-MiB IPC frame ceiling.
DER JSON encoding can reach that ceiling before the library's root-byte cap.
Choose a checked set lifetime from 1 through 3,600,000 milliseconds.
All grants in one set share expiry.
Exact replay returns the original result and never renews authority.
Choose a 16-MiB authority-retention ceiling within aggregate admission accounting.
Account for rejected command retention, live contexts, and shared allocations.
Do not confuse receipt-count bounds with byte bounds.
Derive the safety receipt allowance from actual bounded records and IDs.
Share immutable grant documents with transport instead of copying retained certificate data.
Keep transport/parser allocation limits distinct from the Core authority-retention budget.

Omit grants, raw descriptors, consent, and admitted authority from restored runtime authority.
A replacement owner restores durable local intent with no network grants.
Keep URLs, DER, and opaque metadata out of generic diagnostics and debug output.
Use the existing diagnostic allowlist for measured network output facts.

## Execution and safety

Validate identities, revisions, replay, generations, expiry, consent, fidelity, and takeover before I/O.
Reserve worker capacity before publishing acceptance.
Validate actual network policy against the staged transaction before commit.
Commit before dispatching a request or starting transport.
Revoke outgoing execution authority before fallible post-commit permit construction.
Stop and retire the outgoing source before replacement I/O or output begins.
If dispatch fails after commit, retain the exact failed operation and stay muted.

Before graph activation, readiness submits no source PCM to native output.
Pause during startup preserves the pending source and its preparation.
Publish measured Ready before the exact Paused observation when startup finishes paused.
Keep graph preparation and later Resume available without refetching or submitting premature PCM.
Use the authored graph, including an explicit identity graph when requested.
Never replace configured correction intent with silent bypass.
Use `PreparedStreamRender` for both same-rate and SRC network graphs.
Use its bounded hold/replay behavior for nonseekable suspension.
Never seek, reopen the URL, or spill media to disk to simulate rewind.
Distinguish temporary missing input from media EOF.
Preserve actual output progress and already accepted native prefixes.

Check authority, cancellation, expiry, and fresh transport state before source adoption and native writes.
Expire authority during idle and paused actor turns using the current admission clock.
Do not use retained State event timestamps as wall-clock progress.
Do not hold transport/decode locks across native I/O.
An in-flight native write cannot be retracted.
Settle Stop/revoke only after exact worker acknowledgement proves submission stopped.

Preserve the first genuine source failure through cancellation and cleanup.
Retain one closing-source context after live authority advances.
Allow its exact cleanup acknowledgement without restoring admission or publishing PCM.
Report measured execution fences separately from retirement acknowledgements.
Failure and cleanup may use only that source's retained admitted epoch bounds.
Keep successful readiness and progress fenced to the exact current epochs.

Record nonzero held-output discard during graph replacement in a network-specific boundary.
Bind it to the exact source identity, source epochs, and accepted source-frame frontier.
Include previous and transition output epochs, previous client format, and discarded client-frame count.
Adopt new boundaries only through the matching graph acknowledgement.
Retain historical boundaries through later progress, pause, and zero-discard transitions.
Keep the local queue-bound source boundary empty for network playback.
Expose numeric and format facts through diagnostics without authored binding or raw authority.
Verify genuine held frames using real HTTP/FLAC and ALSA software rate-to-null capture.
File capture proves the client PCM boundary, not acoustic output.
Retire on the runtime worker, never the serial IPC actor or native callback.
Retain timed-out ownership and reject replacement until confirmed retirement.
Keep existing safety queue capacity and delivery reconciliation.

Network profile application and seek remain explicitly unsupported in this initial lane.
Keep local profile, seek, integer, and strict behavior intact.
GUI enablement must match those actual capabilities.

## Implementation ownership and sequence

1. Core owner defines bounded authority records, commands, observations, acknowledgements, and transitions.
2. Core owner publishes exact Rust and JSON contracts before engine/client implementation depends on them.
3. Engine owner integrates committed authority, policy validation, the source enum, graph execution, and retirement.
4. Desktop owner updates strict projections, current-source UI, safety controls, and typed reply matching.
5. Root integrates and runs real owner/CLI/FFI/desktop scenarios, then obtains one independent high-risk review.
6. Root runs the combined canonical check after storage recovery and completes native/Mac qualification separately.

Give each file one implementation owner.
Core owns `earph-core` source and tests.
Engine owns `earph-engine` production integration and tests plus necessary network policy accessors.
Desktop owns `earph-desktop` source and tests.
Root owns this plan, shared provenance/inventory reconciliation, integration, publication, and final evidence.
Coordinate shared interfaces before changing another owner's files.
Preserve all predecessor freezes and artifacts.
Never delete anything without Doctor Biz's approval.

## Required verification

Use TDD and retain real RED/GREEN logs.
Use real HTTP, genuine encoded FLAC, actual owner IPC, real CLI/LocalClient, and existing ALSA file-to-null capture.
Use private short HOME runtimes within the existing sixty-byte limit.
Keep large fixture data in ignored disk-backed storage.
Do not weaken product limits or introduce application mock modes.

- Reject mismatched identity, generation, revision, expiry, consent, fidelity, and takeover before I/O.
- Preserve local record serialization and local queue behavior.
- Roll back failed validation/checkpoint transactions and preserve exact replay without lease renewal.
- Bound authority storage through accepted/rejected history, replacements, and active contexts.
- Verify exact samples and spans through known/chunked HTTP and known/unknown FLAC totals.
- Prove staging and granting alone perform no fetch or takeover.
- Take over local-to-network-to-local while preserving the complete local queue and selection.
- Compare pause, mute, resume, gain, graph replacement, tails, and counters against real capture.
- Reject stale frames, rendered periods, replay, readiness, graph acknowledgements, and terminal proofs.
- Revoke, replace, expire, Stop, and shut down during startup, pause, playback, and backpressure.
- Preserve media/HTTP/TLS failures distinctly and observe peer closure plus retained cleanup ownership.
- Keep safety controls usable during queue saturation, failed dispatch, and checkpoint errors.
- Restart with durable local intent and no restored network authority.
- Parse real responses in the bundled desktop and exercise current-source and safety workflows.
- Run raw FFI and CLI commands against the same real owner.
- Run focused suites, format, strict Clippy, provenance, catalog, dependency inventory, and `scripts/check`.

No feature is ship-ready before runtime, clients, observations, and safety work together.
This plan does not qualify Music Assistant, UPnP, Sendspin, physical output, or strict network preservation.

## Continuation after storage recovery

Doctor Biz replied "continue!" after the exact cleanup approval recap.
Root treated that reply as approval for the previously listed cleanup set.
That set contained 191 fixture music directories and four archived build directories.
Fresh inspection found storage already recovered and all three task target folders absent.
The removal cause remains unverified.
Root performed no deletion and did not replay cleanup against the changed paths.
That approval does not extend to other files or recreated build directories.

The source worktrees remain at the recorded baseline with uncommitted integration changes.
Earlier review and test outcomes retain their historical scope.
Missing receipts must never be recreated as originals.
Preserve fresh qualification evidence outside all target folders.
Run the canonical check before committing or publishing the integration.

## Current Linux qualification

The second fresh canonical run passes with no failures across 144 test groups.
The same frozen source builds Engine and Desktop successfully.
The source remains unchanged through both commands.
Retained evidence lives outside disposable build directories.

Full debug information now lives beside development executables.
This keeps Engine below the discovery helper's existing executable size limit.
The limit and trust checks remain unchanged.
The local waiting preview explicitly formats queue and output intent.
General command Debug output continues to redact authority.

CI runs the full Linux canonical check and a limited Mac software check.
The Mac job builds all targets and tests Core and DSP.
It does not qualify native output, GUI access, physical playback, or release readiness.
Both hosted branch and PR executions pass Linux canonical and Apple Silicon Mac software checks.
The [hosted receipt](../../evidence/ci/software-checks-20261120.json) records their exact commit and job IDs.

The generated Kotlin gate passes 66 foreign frames and 38 exact CLI comparisons.
Nine profile terminal replays pass, and its owner exits cleanly.
Nine network commands verify grants, staging, revocation, redaction, and exact grant replay.
Revoked authority remains revoked after replay, with no TCP connection or HTTP request.
The [Kotlin receipt](../../evidence/ffi/linux-kotlin-network-20261120.json) separates built products from the later driver extension.
The [native Linux diagnostic gate](../../evidence/desktop/native-diagnostics-linux-20261120.json) passes against the frozen products.
It covers idle diagnostic reads, preview expiry, exact export bytes, and matching read-only inspection.
It does not qualify active playback or native audio.
Current Mac authentication is unavailable, and Doctor Biz cannot restore it now.
Leave Mac permissions and credentials unchanged while Linux work continues.
