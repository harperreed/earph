---
title: Live Sendspin clock integration - Plan
type: feat
date: 2026-11-21
origin: docs/SPEC.md
---

# Live Sendspin clock integration - Plan

The [combined Linux receipt](../../evidence/sendspin/runtime-timing-native-linux-20261121.json) qualifies the existing numeric filter and native service bridge.
Live transport timestamps and writer-bound probes remain this plan's separate implementation unit.

Doctor Biz, connect the existing authenticated transport to TimeSync before enabling a player.
This is the smallest next integration slice needed by the player timeline.
It ends with a real clock mapping from actual socket exchanges.
It does not end with advertised player support or audio output.

## Goal Capsule

The current timing reducer consumes supplied microseconds, while Network exports truncated receive milliseconds.
The generic producer path also accepts client/time bodies before queueing and encryption.
Those paths cannot establish the required T1 and T4 measurements for a live filter.

Build one roleless clock driver inside the existing Network worker.
Capture receive time at WebSocket delivery and send time immediately before probe encryption.
Keep one canonical receive timestamp through delayed session processing and final-fragment reassembly.
Use the existing TimeSync algorithm without another filter or thread.

Estimated executable change: roughly 450-750 Rust lines, plus tests and mechanical caller updates.
This estimate excludes a player reducer, codec work, scheduling, ASRC, and native output.

## Product Contract

### Requirement trace

SRC-02 requires player integration against a pinned specification and named Music Assistant version.
SRC-03 requires timeline scheduling and exactly-once delay accounting, with independent-clock qualification.
AC-16 additionally requires actual format negotiation, stream lifecycle, reconnect, and upstream controls.
AC-17 requires two independent output clocks and instrumented capture.
This unit supplies their missing live-clock prerequisite; it completes neither acceptance criterion.

The pin requires an active player's initial client/state to wait until filter count reaches one.
A mapped clock remains distinct from source admission, output readiness, and accuracy qualification.

### Requirements

- R1. Capture nonnegative monotonic microseconds once at each complete WebSocket message delivery.
  Preserve the final fragment's original stamp through every deferred plaintext or ciphertext queue.
- R2. Keep one canonical receive stamp; derive milliseconds only for existing reducer deadline calculations.
  Never multiply truncated milliseconds into fabricated microsecond facts.
- R3. Generate each client/time payload at the actual write opportunity, after application queueing.
  Sample T1 immediately before serialization/encryption after the socket accepts readiness.
- R4. Feed only authenticated, direction-valid server/time payloads into the existing TimeSync owner.
  Validate integer ranges and bind replies to the exact outstanding probe and clock domain.
- R5. Preserve the pinned burst, best-positive-error, timeout, age, and numeric policies already owned by TimeSync.
  Deliver captured matching replies before advancing its timeout tick.
- R6. Suspend probes during send suppression or blocked ordered session delivery; preserve the filter across re-handshake.
  Abandon the incomplete burst before advancing clock time; reconnect, server change, or domain change creates a fresh filter.
- R7. Keep current queue credits, bounded service work, cancellation, acknowledged commits, and retirement intact.
  Clock work gets one bounded pending probe and no separate thread or unbounded completion queue.
- R8. Publish redacted mapping facts with binding, mapping revision, age, and status.
  Emit no clock-ready or output-ready authority token.

### Excluded work

Keep player roles, playback activities, client/state availability, codecs, audio queues, and output absent from this unit.
Keep credential provisioning, recovery acknowledgment, startup, listeners, discovery, and automatic reconnect absent.
Do not add the Rust SDK, another numeric filter, or a generic clock-provider framework.
Do not claim Music Assistant interoperability or AC-17 from loopback clock traffic.

## Planning Contract

### Existing evidence

- docs/SPEC.md section 6.3 requires server-to-local mapping and a complete physical-presentation latency ledger.
- docs/sendspin-timing.md explicitly leaves transport microsecond export and output-clock mapping unresolved.
- earph-network/src/sendspin/worker.rs captures an Instant at poll_next delivery, then passes millis(start, arrival).
- earph-sendspin/src/message.rs stores Complete.arrival_ms.
- earph-sendspin/src/session.rs retains RawInput.arrival and reassembled Complete values during delayed processing.
- Network next_write encrypts before PendingWrite polls socket readiness.
- Native CredentialServices supplies admission and persistence only; it supplies no clock or output authority.
- PreparedStreamRender exposes staged PCM periods and output positions; it supplies no Sendspin schedule or latency ledger.

All source paths above are under crates/ unless they start with docs/.
The retained preparation packet records the inspected source hashes.

### Proposed public contracts

These names describe the intended interfaces; they are additions or changes, not claims that these APIs exist today.

| Contract | Required behavior |
| --- | --- |
| MonotonicDomain | Own a private monotonic origin and explicit nonzero generation; clones retain the same domain. Future output adapters can sample that same domain. |
| ClockConfiguration | Give start one explicit MonotonicDomain and TimePolicy. Do not infer timing authority from a transport permit. |
| ReceiveStamp | Store checked local microseconds as the single receive-time fact. Expose derived milliseconds for reducer deadlines. |
| Complete receive metadata | Carry ReceiveStamp through authentication, fragmentation, rotation queues, and consumer event leases. Replace the millisecond field rather than retaining independently writable duplicates. |
| Session clock context | Expose the authenticated channel's server binding, actual send suppression, and whether ordered delivery is blocked. Do not perform another credential lookup or infer these states from an unrelated role flag. |
| Deferred probe write | Retain an opaque ProbeToken until the next valid write opportunity. Encode client_transmitted from the sampled T1 and return exact probe-start metadata with that write. |
| Clock observation | Return a copied TimeSync Snapshot and domain binding. Keep Debug and ordinary observations redacted. |

Adopt ClockConfiguration and ReceiveStamp across their in-repository callers atomically.
Root adopted these shared-contract changes for the next implementation unit.
Provide one API for each contract; add no legacy shim, silent timestamp fallback, or two clock implementations.

### Ownership and ordering

The existing Network thread owns the live driver and TimeSync for its connection.
The session continues to own encryption, sequencing, reassembly, and its authoritative pairing catalog.
The Engine caller owns the clock domain, admission policy, and native supervisor lifecycle.

A probe stays unstarted while earlier writes or send suppression block it.
Record probe_started only when start_send accepts its timestamped ciphertext.
Retain that exact probe identity through complete flush or the existing uncertain-write shutdown path.
Do not mistake a local flush for remote processing.

Consume deliverable clock replies within the pump before publishing ordinary consumer events or advancing the timing tick.
Keep each reply's captured T4 even when an acknowledged commit or rotation delayed its delivery.
If ordered session delivery becomes blocked, suppress timing and abandon its incomplete burst before advancing TimeSync.
A reply released from that blocked interval then belongs to an abandoned probe and cannot update the filter.
Resume with a fresh probe after the session drains and the clock advances normally.
Do not replay an older captured T4 into a reducer already advanced beyond it.
Old or unmatched replies cannot move the current clock or extend its deadline.
Preserve the existing rule that an uncertain write closes the socket and never retries the message.

The pump must use one ordering rule for captured replies, suppression changes, and clock ticks.
A matching reply already delivered by the session wins before its timeout tick.
A protocol-held reply is handled by the explicit burst-abandonment policy above, not by timestamp repair.
Keep the established mapping until its ordinary age policy marks it stale.

Generic try_send must no longer accept caller-stamped client/time payloads once the clock driver owns them.
Migrate the existing fixture echoes and callers to the dedicated probe path.
Retain client/goodbye and existing rejection ownership behavior.

~~~mermaid
flowchart LR
  Domain[Caller clock domain] --> Pump[Existing Network pump]
  Pump -->|late sampled T1| Writer[Session writer and original socket]
  Socket[Authenticated socket delivery] -->|captured T4| Session[Session queues and reassembly]
  Session -->|original receive stamp| Pump
  Pump --> Filter[Existing TimeSync]
  Filter --> Snapshot[Redacted mapping snapshot]
~~~

## Implementation Units

### U1. Preserve capture time through the real session

Own crates/earph-sendspin/src/message.rs and crates/earph-sendspin/src/session.rs.
Own the timestamp-facing changes in crates/earph-network/src/sendspin/worker.rs.
Update the existing session, message, timing-pipeline, and transport tests that consume this metadata.
Keep one implementation owner across these overlapping paths.

Introduce R1-R2 and the receive part of R4.
Keep existing session protocol deadlines in milliseconds, derived from the captured stamp.
Do not rewrite the pairing reducer or its timeout policies.

Prove a submillisecond final-fragment stamp survives reassembly and deferred delivery without rounding or restamping.
Prove a message held behind acknowledged commit or re-handshake retains its original receive fact.
Prove unrelated or wrong-domain samples cannot affect the mapping.

### U2. Drive TimeSync at the real writer boundary

Depends on U1.
Own a focused crates/earph-network/src/sendspin/clock.rs, its export in mod.rs, and worker integration.
Own the minimal session writer changes needed for deferred probes and shared clock context.
Own crates/earph-network/tests/sendspin_timing_transport.rs and extensions to the existing pinned peer fixture.
Update existing Network and Engine bridge test callers for the explicit clock configuration.
Keep docs/sendspin-runtime.md and docs/sendspin-timing.md accurate.
Update provenance inputs only if the actual new source lies outside an already covered source tree.

Implement R3-R8 using the current TimeSync methods and existing one-socket worker.
The new module handles pump ownership and wire validation, not timing equations.
No credential-crate source or native operator change belongs to this unit.

## Verification Contract

Use TDD for both units and retain the expected RED failure before each implementation.
Run unit, integration, and real transport end-to-end tests; mock-only success is insufficient.

| Scenario | Observable success |
| --- | --- |
| Eight actual exchanges | The real authenticated peer receives eight sequential unique echoed T1 values; exactly one best positive-error sample feeds the existing filter. |
| Queued probe and blocked writer | The encoded T1 is sampled after queue delay and readiness; no unstarted probe counts as emitted. |
| Fragmented or deferred reply | Its T4 equals the captured final-message stamp, including a submillisecond remainder, regardless of dequeue delay. |
| Reply and tick ready together | A matching reply delivered by the session before its captured deadline wins before the clock tick. |
| Reply held behind admission, commit, or rotation | The blocked interval abandons the probe before later clock advancement; the released reply cannot alter the filter; its event metadata keeps original T4. |
| Invalid, duplicate, unmatched, or expired response | No unauthorized sample update, deadline extension, extra allocation queue, or manufactured mapping appears. |
| Re-handshake | Filter state survives; the incomplete burst is abandoned; no probe is emitted during suppression; activation permits a fresh probe. |
| Reconnect or domain replacement | Old probes, replies, bindings, and snapshots cannot authorize the new filter. |
| Close during acknowledged credential commit | Network still delivers the actual completion to the original session and retires with the existing retained-owner rules. |
| Cancellation, expiry, and queue pressure | Socket and clock work settle without new threads, leaked credit, detached work, or a commit cancellation. |

The E2E fixture must use the real pinned Python Noise/WebSocket server path and live monotonic samples.
A controlled real server may delay or fragment replies; do not substitute a mocked transport for this gate.
Reuse the pinned peer's source verifier and private credential fixtures.
Use hand-calculated deterministic vectors for the numeric oracle; the Python client's filter differs from the spec.

Future validation commands are the canonical scripts/check plus focused earph-sendspin, earph-network, and touched Engine tests.
Run formatting, strict Clippy, and one independent main-model shared-contract/concurrency review before integration.
Root owns serialized qualification and publication.
No command or test in this section ran during this preparation.

## Definition of Done

An explicitly constructed roleless worker produces a mapping from actual authenticated eight-probe exchanges.
Its timestamps survive real writer backpressure, fragmentation, delayed session delivery, and re-handshake.
The native credential bridge remains usable and its acknowledged-commit regression still passes.
The report names its wire and clock capture boundaries and distinguishes measured facts from mapping estimates.
SRC-02 and SRC-03 remain incomplete; AC-16 and AC-17 remain unclaimed.

## Dependent First Player Slice

After U1-U2, implement one explicit PCM player timeline connected to the existing single output owner.
Start with PCM signed 16-bit, 48 kHz, stereo only; advertise that single usable format and no unsupported commands.
That format is an implementation recommendation, not an already qualified Earph capability.
Default unpaired playback stays disabled.

That unit must require native session credentials, affirmative playback admission, source takeover, a mapped live clock, and a prepared output path.
It must implement typed stream/start, ID-4 whole-frame decoding, full and from_timestamp clear, and stream/end.
It must preserve format ownership on queued chunks, bound bytes and frames, and prevent stale replay after clear or source change.
It must handle the already-submitted backend prefix when a partial clear cannot retract it.

Before emitting initial player state, bind the pinned count >= 1 rule to a real mapping and actual operational readiness.
Before emitting samples, complete a latency ledger and map output progress into the same monotonic domain.
Subtract external output_delay_ms once at the port target; account for pre-port DSP and backend latency separately.
Declare synchronization processing honestly and reject a preservation policy that cannot meet the pinned timing contract.

Do not collapse that work into this clock slice.
The current transport, native bridge, and timing reducer all deliberately refuse or omit player authority.
Removing those boundaries without the timeline, source owner, and output contracts would falsely advertise a usable player.
FLAC, broader formats, live drift correction qualification, real Music Assistant 2.10.12, and independent-clock capture remain later required gates.

## Sources and Confidence

The normative pin is Sendspin/spec 9d2f15ee8f172c67ce00dc0d9f6603c3b1030d9f.
Use its [clock and timestamp rules](https://github.com/Sendspin/spec/blob/9d2f15ee8f172c67ce00dc0d9f6603c3b1030d9f/messaging.md#clock-synchronization) and [player role](https://github.com/Sendspin/spec/blob/9d2f15ee8f172c67ce00dc0d9f6603c3b1030d9f/roles/player/v1.md).
The retained messaging.md SHA256 is a9992e957b62bf1847a127a08963a64d7c9050465b0bc547154f21997f7fe296.
The retained player/v1.md SHA256 is dca703727509eb21e2047a0bb4c5ecebb3d3bc7a4fd8f873998b6b9db471aca0.
Both retained files were checked against SOURCES.json during this preparation; no new web retrieval was necessary.

The actual peer target remains aiosendspin 10.2.1 at 692831b07bc453a38d5a39a16ee566141547bfa1.
The later named product server target remains Music Assistant 2.10.12 at 6ab5bfa63f02d0daa237d342097a42feae5c5aae.
The existing Rust SDK comparison pin is fce4c78e0833e8046f333f978fb972d31548a6c7; no SDK adoption is proposed.

Confidence is high in the timestamp and ownership dependency because it is visible in current callers.
Exact API placement and the deferred-write implementation need the next unit's focused shared-contract design and tests.
A single-context consistency review covered the brief; no independent document review, extra agent, or runtime validation ran.
The implementation runs in a separate worktree.
Root owns integration, final qualification, and publication.
