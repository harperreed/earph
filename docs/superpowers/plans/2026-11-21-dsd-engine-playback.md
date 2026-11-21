---
title: Engine DSD ownership and observation contract
date: 2026-11-21
execution: software-implementation-authorized
baseline: 4d8f6d5421fd5779a21e33140fb10030e507065b
---
# Engine DSD ownership and observation contract

Doctor Biz, add DSD to the existing worker owner without changing PCM wire documents.
The core decision is one owned source and one canonical observation discriminant.
Expose a separate typed DSD projection from that discriminant.
Never construct a PCM source observation from carrier parameters.

The contract preparation ran no builds, tests, native output, or repository edits.
Doctor Biz authorized continued first-release implementation; Root released the combined software unit to its isolated owner.
Root owns shared integration and selects the implementation units below.
Root must retain independent implementation review before integration.

## Goal and authority

The current DSF/DFF reader, DoP codec, and typed ALSA writer already exist.
Their software boundary excludes Engine playback and physical endpoint qualification.
Implement the missing worker ownership and typed contracts as a separate unit.
Keep stereo DSD64/128 and packed three-byte little-endian carriers as the only supported formats.

Root reviewed and authorized the software integration contract.
Root explicitly permits additive DSD observations and a typed source-bit seek operation.
These new domain operations require no compatibility shim or repeated approval.
The implementation assignment covers isolated source edits and software tests.
It does not grant physical DSD signals, hardware changes, credential access, or deletion authority.

ARCH-04 requires distinct PCM, DSP, and DSD payload paths.
PLAY-07 requires qualified DSF/DFF DoP playback.
SAFE-04 requires incompatible processing refusal and verified DSD transitions.
AC-13 also requires qualified hardware routes on both launch operating systems.
This software integration cannot close PLAY-07, SAFE-04, or AC-13.

## Success criteria

1. A generated DSF/DFF reaches the real DSD worker and typed ALSA file-to-null output.
2. An independent consumer recovers exactly the expected source bits and markers.
3. Every accepted-prefix observation agrees with the capture and native accepted counter.
4. Cancellation and failure retire the sole output before ownership transfers.
5. DSD cannot reach PCM gain, graph, resampling, limiting, or PCM output APIs.
6. PCM wire observations and existing PCM behavior remain unchanged.
7. Public projections never describe PCM, HTTP FLAC, and DSD as simultaneously active.
8. Unqualified endpoints remain unqualified after file admission, native open, and software capture.

## Settled shared contract

### One canonical observation

Introduce a core observation discriminant with local PCM, local DSD, and HTTP FLAC variants.
Store at most one variant as the authoritative current or retained playback observation.
Use the existing PlaybackObservation and NetworkPlaybackObservation as the PCM and network payloads.
Add DsdPlaybackObservation as the new payload.

Derive SessionSnapshot's public playback, network_playback, and dsd_playback fields from that discriminant.
Keep the existing PCM and network JSON shapes, nullability, and omission rules.
Omit dsd_playback when absent; serialize the new typed payload when present.
If DSD is present, serialize playback as null and omit network_playback.
Do not maintain writable PCM and DSD copies of the same facts.
A private serialization projection is sufficient; do not introduce a versioned replacement endpoint.
Rust callers may switch on the canonical discriminant; this is ordinary in-scope integration.

Keep the worker's local events, latest progress, stopped prefix, and terminal observation on one local discriminant.
Its variants contain the existing PCM observation or the new DSD observation.
Retain the existing token, event epoch, graph epoch, device epoch, and owner/session fences.
Do not introduce a DSD event channel, detached source owner, or separate terminal cache.

Snapshot retention is distinct from active ownership.
Idle and Failed may retain one terminal observation after source resources close.
Starting another source clears that retained observation before publishing the next source's facts.
Never use a retained observation as current endpoint qualification or permission.

### DSD source and output facts

Add a validated DsdSourceFormat describing source bit_rate_hz and channels.
It accepts only 2822400 or 5644800 bits per second and stereo.
Share validation with the existing codec where needed; do not add a PCM source format.
Add a typed DopCarrierFormat describing carrier_rate_hz, stereo, and Packed24Le transport words.
Carrier rates are 176400 and 352800 frames per second.
Each carrier frame contains sixteen source bits per channel and six bytes.

DsdPlaybackObservation must contain these facts:

| Field | Meaning |
| --- | --- |
| source_format | Validated source bit rate and channel layout. |
| total_source_bits | Length of the named source entry, per channel. |
| source_entry_id | Stable queue identity for the current accepted source frontier. |
| source_position_bits | Accepted source frontier within that entry, including its explicit starting offset. |
| decoded_position_bits | Reader frontier within that entry; it can exceed the accepted frontier. |
| source_epochs | Epochs of the current reader/packer source boundary. |
| carrier_format | Typed transport format, validated against actual native negotiation. |
| output | Optional live native-instance observation, defined below. |
| last_boundary | Optional immutable record of the immediately preceding close/failure boundary. |
| qualification | Unverified until concrete endpoint-specific evidence exists. |

A live output observation contains output_epochs, submitted_carrier_frames, and the backend negotiation observation.
It may contain estimated_consumed_carrier_frames and backend_delay_carrier_frames with the existing estimate basis.
These estimates never establish audible bits or a restart cursor.
A closed output is represented by output=None and last_boundary.
Do not query progress after closing the handle.

A boundary contains the old output epochs, accepted carrier count, source identity, source bit frontier, and close reason.
Keep the irreversible accepted count when progress, close, or cancellation fails.
A boundary describes an old native instance; it is not a second current progress cache.
After reopen, reset native counters and retain only that one previous boundary.
Use checked arithmetic for every carrier-frame-to-source-bit conversion.

Use existing BackendObservation only for native carrier negotiation and adapter storage.
Its signed24 client metadata describes the carrier container, not decoded PCM audio.
Keep preservation=Unverified unless actual evidence establishes a stronger result.
DSD observations have no PCM peaks, graph timing, modified sample metrics, or invented audible cursor.

Keep top-level legacy decoded_position_frames at zero for a DSD source.
Keep audible_position_frames=None.
The DSD observation owns all DSD positions; do not copy bits or carrier frames into PCM counters.
Desktop DSD views must show typed bit positions before the generic PCM position display.
A zero PCM-frame counter means no decoded PCM frames, rather than a DSD position.

### Queue preparation

Retain PreparationStatus::Ready { format: PcmFormat, total_frames } unchanged.
Add PreparationStatus::DsdReady { format: DsdSourceFormat, total_source_bits }.
This status means source admission only.
It establishes neither native output readiness nor endpoint qualification.
Do not add a fabricated PCM format, compatibility constructor, or transport-qualified flag to Ready.

Restore continues resetting every readiness variant to Unprepared.
Publish DsdReady only after complete supported-container admission succeeds.
Reject stale preparation epochs through the existing Session setter.
Graph readiness consumes only PCM Ready; DsdReady cannot become a PCM graph input.

### Typed seek

Retain Command::Seek { position_frames } as the PCM source-frame operation.
Add a distinct DSD seek command with position_bits per channel.
Use an explicit wire method such as transport.seek_dsd and field position_bits.
The exact method spelling belongs beside existing transport command spellings.
Reject this command for PCM or HTTP FLAC.
Reject the PCM seek command for a DSD source.
Require alignment to sixteen bits and position_bits <= total_source_bits.
Validate before epoch mutation or native I/O.
The exact end is a valid seek target and subsequently produces software EOF.

### Graph and volume

Determine SampleDomain from the canonical source observation or admitted source kind.
Stop hard-coding SampleDomain::Pcm for DSD gain and unmute validation.
Refuse all active/configured PCM graph application, profiles, EQ, limiting, and resampling for DSD.
Repeat that refusal in the worker before accepting a prepared PCM pipeline.
A missing PCM format must not trigger a graph default or output-format fallback.

Allow only fixed, explicitly armed zero-dB passthrough in this unit.
Reject software volume mode, nonzero gain, software exact zero, and nonzero ramps.
Keep existing output consent, fixed arming, fidelity, cap, device, and profile checks.
Do not pretend hardware volume works without actual set/readback support.
Do not call GainRamp or any PCM silence helper for DSD.

Safety controls still close the DSD owner even when prospective settings are incompatible.
For example, a cap change that forces attenuation must close before reporting the incompatible passthrough state.
Do not let gain validation prevent Stop, Shutdown, or mute-driven native closure.

## Worker ownership and lifecycle

### Structure

Use one active-source slot in worker::run.
Its variants own the current PCM/HTTP playback or DSD playback.
Keep existing network preparation/retirement ownership unchanged.
When adapting network helpers, borrow only the PCM/HTTP variant; never reinterpret DSD as Reader::Local.
Root owns worker.rs, worker/network.rs, host.rs, and shared core integration.
A focused worker/dsd.rs module may own DsdFile, DopPacker, pending DopBlock, and DopOutput together.
The module has no thread, event channel, global state, or detached resource owner.

Classify local files by admitted container magic, not extension.
A small media admission helper may use the existing regular-file admission path to read that magic.
Reopening through DsdFile or LocalFile must validate the actual newly opened file again.
Do not fall back to a PCM decoder after admitting a malformed DSD container.
Do not duplicate media parsing in the worker.

Create and use the non-Send/non-Sync native output on the existing worker thread.
Check cancellation before source admission, after admission, before native open, and before each write.
Check the latest owner/session/output epochs, graph epoch, and device epoch at control boundaries.
Drop/close the old native owner before constructing the replacement source/output owner.
Release QueueReservation only after source, packer, pending bytes, and output retire.
Keep retirement and genuine close errors visible through the current terminal token protocol.

### Feeding and accepted prefixes

Read at most min(4096, period_frames * 2) bytes per channel.
This keeps each complete carrier block within one native period and the existing codec limit.
Validate reader sequence and source bit position before pushing its block.
DsdBlock itself contains no sequence or source-position metadata, so the worker must check DecodedDsdFrame.
Keep exactly one pending DopBlock and its accepted offset.
Do not read or pack another block while that block has an unaccepted suffix.

Pass DopBlock directly to DopOutput::write.
Advance accepted source progress only by the returned accepted carrier frames times sixteen.
WouldBlock retains the same object, marker phase, sequence, offset, and bytes.
Do not regenerate, replay, reprepare, recover underruns, or insert zeros.
Keep progress in the native writer; do not count reader reads as accepted frames.

Charge codec DSD_WORKING_BYTES, actual adapter scratch capacity, queue metadata, spans, and profile retention separately.
The codec's 24580-byte allowance is not total worker memory.
Keep source blocks short-lived after packer push; do not retain the whole decoded file.

### Queue joins

Preflight the selected entry and following queue entries before opening output, matching existing PCM preparation semantics.
Accept only DSD entries with the same source rate and supported layout in one DSD queue run.
Reject mixed PCM/DSD or changed-rate joins before any output opens during initial preparation.
Use existing protected active/reserved-next identities for later queue edits.
Recheck newly appended entries when reserving them; preserve the accepted prefix if admission fails.

For compatible DSD entries, keep the same packer and native stream across the join.
Each admitted file length is a multiple of sixteen source bits, so no incomplete carrier frame crosses the entry boundary.
Keep marker and carrier sequence continuous across the queue.
Track bounded entry spans, as the existing PCM path already does.
Map the native accepted frontier to the correct queue entry and per-entry source-bit position.
Never label cumulative stream bits as that entry's source position.
This establishes software continuity only; physical gapless transition safety remains unqualified.

### Control transitions

| Trigger | Required software behavior | Observation and receipt |
| --- | --- | --- |
| Start while muted | Admit typed source; validate policy; open without writes if permitted by existing output rules. | Typed prepared observation; qualification remains Unverified. |
| Play/unmute | Check the current domain, graph, gain, consent, arming, epochs, and concrete DSD submission authority. | Started requires actual accepted/running output; readiness alone never applies playback. |
| Pause | Stop and close; cancel packer; discard unaccepted suffix; retain the accepted source frontier. | Acknowledge only after close; no audible-position claim. |
| Resume | Reopen reader at the retained accepted bit frontier; create fresh packer and output. | New output counters start at zero; marker starts at 0x05 and sequence zero. |
| Mute | Close like Pause; submit no PCM silence or DSD filler. | Advance stream epoch even without an active graph, so reopen cannot reuse old transport epochs. |
| Unmute after mute | Revalidate domain and submission authority; reopen at accepted frontier. | Keep previous closed-prefix boundary separate from the new live native instance. |
| DSD seek while playing | Validate target; close old output; reopen reader/packer/output at target under the new epoch. | Preserve old accepted-prefix boundary; never use consumed estimates as seek positions. |
| DSD seek while paused/muted | Validate target; retire old pending work; seek/reopen source only; keep output closed. | Publish target bit position and zero new accepted frames; remain paused/muted. |
| Seek to exact end | Finish packer after observing reader EOF; close without filler. | Terminal software completion; no physical drain claim. |
| EOF | Finish packer only after every pending carrier frame is accepted; stop and close. | Report source-submission completion; never claim native drain or audible EOF. |
| Stop/Shutdown | Set cancellation; close owner; cancel reader/packer; discard pending suffix. | Retain accepted-prefix facts; acknowledge only the matching control/token fence. |
| Native/read/packer failure | Close all owned resources without recovery or fallback. | Failed observation preserves accepted prefix; uncertain consumption becomes unavailable. |
| Output replacement/takeover | Retire old owner first; reject stale work; prepare a fresh typed source boundary. | Preserve device/session fences and existing consent/arming invalidation. |

A paused or muted source owns no live DopOutput.
Its resume cursor is the accepted source frontier, not an acoustic cursor.
Stopping can discard accepted frames still buffered downstream.
Therefore this software pause behavior makes no gapless or no-skip physical claim.

Do not emit the existing native-Drained semantic merely because DopOutput::stop succeeds.
Generalize the internal terminal event to source completion with an explicit completion basis, or use a typed DSD completion branch.
Keep the public transport's existing Idle settlement but expose the DSD completion boundary honestly.
Do not add native suspend/finish to DopOutput to fit the PCM trait.

### Safety admission and retirement

Preserve the existing ordinary-command capacity and reserved safety capacity.
The existing FIFO does not reorder safety commands; do not describe it as an overtaking priority queue.
Preserve cancellation when safety admission or delivery fails.
Check cancellation again immediately before any DSD write after processing a control.
Stop and Shutdown always win over pending source suffixes and graph activation.

Reuse SourceTerminalToken and StopRequest matching for DSD.
Include DSD accepted-prefix observations in failure, stopped-source, and late-control reconciliation.
A stale event must not mutate another source's observation, queue selection, or pending receipt.
Retire the exact terminal token only after Session accepts the corresponding terminal event.

## Caller and cleanup obligations

| Caller | Required adaptation |
| --- | --- |
| core Session state/advance | Assign one canonical observation; project typed public fields; retain terminal facts without retaining resources. |
| Session Start/OutputSelect | Clear retained observations before a different source becomes active; preserve existing consent invalidation. |
| Session Pause/Seek/Stop/failure | Keep bit-domain positions separate; clear uncertain consumed/audible facts; settle only matching epochs and operations. |
| session_network | Replace the canonical variant with HTTP facts; never leave a current DSD or local PCM projection beside it. |
| session_graph | Extract PCM facts only from PCM variants; explicitly refuse live DSD before preparing or activating graphs. |
| session_diagnostics | Omit unavailable PCM measured facts for DSD; add a typed DSD report only if needed within this unit. |
| Engine host | Forward typed local observation; validate terminal entry identity through the discriminant; preserve source-token acknowledgment. |
| Worker latest progress | Publish one local observation discriminant, never a second DSD mutex. |
| Worker/network | Adapt active-source borrowing and retirement; preserve HTTP's existing cleanup protocol and pure timeline behavior. |
| Desktop projection | Parse optional dsd_playback and DsdReady; reject multiple simultaneous active projections; keep PCM documents unchanged. |
| Desktop transport/status | Display source bits and carrier frames for DSD; do not send frame-domain seek or show PCM DSP controls as applicable. |
| FFI | Preserve opaque JSON envelopes; run existing consumers against unchanged PCM documents and a typed DSD state. |
| Checkpoint restore | Restore queue intent as Unprepared and disarm output authority; never persist runtime DSD qualification as readiness. |

Do not edit Sendspin source, its pure timeline behavior, credentials, dependency manifests, or locks for this work.

## Implementation units and owners

| Unit | Sole owner and files | Dependencies and proof |
| --- | --- | --- |
| U1 Typed core contract | Root: core audio/dop domain types, queue.rs, control.rs, session.rs, session_network.rs, session_graph.rs, session_diagnostics.rs; focused core tests. | Preserve PCM JSON; prove source-domain refusal, seek bounds, exclusive projections, and stale-event rejection. |
| U2 Worker source integration | Root: Engine worker.rs, worker/network.rs, host.rs, output.rs, optional worker/dsd.rs; small media admission helper if required. | U1 first. Use one active-source slot. Real output tests follow every lifecycle boundary. |
| U3 Software E2E | One Root-assigned test owner: Engine tests/dsd_worker.rs, tests/dsd_output.rs, existing test support. | U2. Reuse generated container helpers and child-local ALSA file/null configs. No application runtime test mode. |
| U4 Typed consumer and evidence | Root: desktop projection/playback.rs, projection.rs, typed queue/transport views and focused tests; DSD docs, state machines, requirements evidence. | U1/U2. Preserve PCM projection tests. Keep physical acceptance partial. |

Root may combine these units to avoid shared-file contention.
Do not delegate overlapping core/session/worker files to another implementation owner.
A reviewer remains read-only unless Root explicitly changes that assignment.

## TDD and verification contract

No command below ran during preparation.
Root schedules every build and native software check under the existing qualification lock.
Use Root's pinned launcher and retained logs; avoid the known pruning mise wrapper.

First add a real owner test using generated DSF/DFF and the current file-to-null fixture.
Prove today's baseline fails because local PCM preparation cannot produce typed DSD readiness.
Retain the actual failure; do not count a stale epoch, missing fixture, or unsupported test syntax as the regression.
Then add core domain and typed observation tests before implementation.
A new API compile failure is an API-introduction red, not a runtime playback regression.

Required unit cases:

- Serialize existing PCM and network examples exactly as before; serialize DSD without a PCM source format.
- Refuse overlapping active projections and invalid source/carrier rates or layouts.
- Distinguish decoded bits, accepted bits, carrier counts, and old closed-prefix boundaries.
- Reject misaligned, oversized, stale, and wrong-domain seeks without changing state.
- Reject graphs, profiles, software gain, exact zero, and ramps for DSD, including ExpertOverride.
- Preserve Stop/Shutdown closure when a settings change becomes incompatible.
- Exercise partial acceptance, WouldBlock, cancellation between reads/writes, and terminal-prefix retention as pure state tests.

Required integration cases:

- Start prepares muted DSD with zero captured bytes and no qualification claim.
- Run both source rates, DSF bit orders, and uncompressed DFF through real typed worker output.
- Consume capture independently; check exact payload, channel order, markers, length, and final short period.
- Pause/mute closes; Resume restarts at accepted bits with marker 0x05 under the new stream boundary.
- Seek while paused emits nothing; unpause emits the exact requested suffix.
- Seek to exact EOF closes and reports software completion without native drain.
- A compatible two-entry queue preserves markers and source identity across the join.
- Mixed-domain/rate preparation fails before writes; a bad appended entry preserves the earlier accepted prefix.
- /dev/full produces a genuine native error, closes descriptors, and requires explicit recovery.
- Stop, takeover, stale progress, stale terminals, and safety admission failure cannot replay suffixes or replace current facts.
- Independent streams retain separate configs, outputs, epochs, counters, and captured bytes.

Reuse tests/worker_playback.rs and tests/service_playback.rs patterns for private IPC and executable ownership.
Use tests/dsd_output.rs's independent consumer and media/tests/support/dsd.rs's generated containers.
Extract shared test-only helpers only when needed; retain a single fixture implementation.
Do not use DopUnpacker as the only capture oracle.
Do not use fake native outputs or runtime switches to claim native behavior.

Run focused Core contract tests, Engine dsd_output/dsd_worker/worker_playback/service_playback tests, and Desktop projection tests.
Run existing codec, reader, ALSA PCM, typed ALSA, and compile-fail tests when their contracts are touched.
Run scripts/check once the focused checks pass.
Retain source hashes, test commands, exit statuses, captured files, and independent decoded comparisons.
Require one independent review of the actual shared ownership diff and test evidence.

## Qualification boundary and real blockers

The supplied baseline contains no DSD endpoint-qualification authority.
OutputConsent and FixedArm establish existing route/volume consent; they do not prove a DSD-capable endpoint.
Native Signed24 negotiation and file admission cannot manufacture that authority.
No inspected code establishes safe physical pause, mute, drain, or format-switch behavior.

The public owner must not treat Unverified as permission for physical DSD playback.
Root can implement and test typed preparation now, while refusing unqualified public unmute.
The private DSD module can exercise its real reader/packer/DopOutput against file/null under test-owned submission authority.
That is real software output, not physical qualification and not a public owner write E2E.
Keep the existing file-to-native E2E as the complete software producer/consumer proof.
If Root requires a public owner write E2E, first define real route qualification or a supported nonphysical output authority.
Do not add an endpoint-name allowlist, environment bypass, mock sink, or implicit System-route exemption to obtain that claim.
This is a missing capability contract, not a request for routine design approval.

Core Audio remains PCM-only; report typed DSD output unsupported on macOS until separately implemented and tested.
Software transitions do not establish safe physical transitions on either OS.
Real file/null may never short-write or return EAGAIN.
Pure cursor tests establish state handling, not those actual native outcomes.
No hardware action is needed to finish the software units above.
No existing physical authorization is renewed by this packet.

## Preparation review and confidence

Direct review checked producer metadata, native progress, Session controls, terminal tokens, and typed consumers.
The contract uses no second native owner and preserves existing PCM public documents.
No independent document-review agent ran because this assignment forbids additional agents.
Root owns the required independent implementation review.

Confidence is high for typed contracts and the single-owner software design.
Confidence is bounded for public playback because endpoint qualification is absent.
Do not describe that gap as a completed playback feature or infer a hardware capability from software tests.
