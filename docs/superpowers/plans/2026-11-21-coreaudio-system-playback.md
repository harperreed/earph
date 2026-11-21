# Selected-device System playback

Doctor Biz authorized the ordinary-music playback fix within the existing owner and safety controls.
The approved boundary uses AudioQueue for System and retains strict HAL for DirectHardware.
Doctor Biz approved hardware playback and evidence on the selected Mac.
Each native run binds its source, selected device, and executable under a retained Root release.

## Success criteria

- Prepare 44.1 kHz Signed16 stereo against observed 48 kHz/512 hardware using application geometry64/256.
- Bind and freshly verify the exact selected UID before enqueue and start.
- Verify the source-rate Float32 client ASBD and report the distinct observed physical ASBD.
- Bound staging and SDK PCM slots and include both capacities in admission.
- Preserve accepted prefixes, epochs, permission gates, fault latches, cancellation, and synchronous disposal ownership.
- Map actual buffer schedules to a conservative restart prefix and observe drain transitions for EOF.
- Keep callbacks bounded, allocation-free, and independent of DSP or session ownership.
- Preserve strict HAL mismatch rejection and StrictPreservation rejection of System conversion.

## Work

1. Capture the exact mismatch regression and retain its failing output.
2. Implement and test bounded slot, clock, drain, and cancellation state.
3. Add the native queue adapter and minimal existing-owner dispatch.
4. Add native prepare-only and hardware-gated generated-silence integration cases.
5. Run focused software checks, strict Clippy, and formatting.
6. Hand back the isolated patch and evidence for Root's Mac compile gate and independent review.

## Evidence and gates

Design: retained `live-play-rejection-01/SYSTEM-ROUTE.md`, SHA256 `8cb613eb174b23e1aa5683e264f3d471cbc41ff9d3319c3284823c132b7fbbb2`.
Foundation: 81 files verified against `publication-21/manifest.json` before copying into this isolated worktree.
Implementation receipts: private `coreaudio-system-playback-implementation-01` packet.
Linux tests cannot qualify Apple's runtime conversion, queue timing, or driver settings.
Root owns Mac compilation, native preparation, hardware approval, and live qualification.
EOF and restart require native timing evidence before product release.

## Implementation notes

The queue client window admits at most one ring capacity ahead of its retired payload prefix.
Returned SDK slots alone never replenish that window.
Rust staging and fixed SDK PCM slots each hold at most one ring capacity.
After a drain request, retirement holds because native stop resets its clock.
Proven natural completion retires the remaining enqueued payload.

Root authorized the exact permissive binding dependency and its narrow inventory policy update.
The cached package matches the existing reviewed objc2 monorepo revision and license hash.
The generated inventory preserves `requires_apple_sdk_distribution_review`.
No distribution qualification is claimed.

The first affected suite used a changing source tree and lacked the required media CLI binary.
It failed the digest test and the real correction CLI test for those explicit reasons.
Retain those failures and verify a frozen source tree after building the actual media binaries.

The silent owner gate asserts the 60000-frame suffix lasts at least 60000/44100 seconds minus 20 ms.
The 20 ms tolerance covers the observed callback, four application periods, and the observer interval.
That lower bound excludes 60000/48000 seconds.
The boundary runs from the resume request to natural owner completion, with the existing 10-second observation deadline.
It does not measure acoustic presentation.
Independent review required this assertion before the first native START.

## First native START and readiness correction

The first gated silent START failed at clock read with SDK status `-66678` (`kAudioQueueErr_InvalidRunState`).
The retained fault recorded 128 accepted and queued frames, zero acquisition callbacks, and confirmed close.
Full device settings, generated source, and product bytes remained unchanged.
The report did not capture actual `IsRunning`; asynchronous startup is a supported hypothesis, not a proven sole cause.

Apple documents that actual start and its notification can follow `AudioQueueStart`.
See [IsRunning](https://developer.apple.com/documentation/audiotoolbox/kaudioqueueproperty_isrunning).
Its value means running whenever nonzero.
[GetCurrentTime](https://developer.apple.com/documentation/audiotoolbox/audioqueuegetcurrenttime(_:_:_:_:)) reports client-rate time without guaranteeing immediate readiness.

Root authorized a bounded readiness model and the necessary internal streaming caller change.
Only the exact startup run-state error may remain pending before the first valid clock.
A five-second no-progress limit never resets from pending reads.
The original enqueue window and all timestamp guards remain intact.
Same-queue resume starts a readiness episode without resetting its client frame origin.
Queue diagnostics retain actual running, last clock status, and first-clock readiness.

The internal Output accessor defaults false and only CoreAudio reports initial pending clock retirement.
Normal streaming observations retain replay until that pending clock becomes known.
Pause, replacement, final retirement, other backend unknown cursors, and native faults remain strict.
The regression uses real SRC, replay, PCM writes, and ALSA null with controlled timing observations.
It does not qualify Apple's runtime behavior.

DELTA04 retains five failing software regressions and the original native failure.
Root owns its narrow independent review, new canonical source check, and fresh native technical release.
No additional SSH or hardware START ran in this implementation subtask.

## Second native failure and diagnostic stop

The next single silent START reached a successful clock read and failed the existing clock-value guard.
It recorded SDK status0, observed runningtrue, readinessfalse, submitted512, acquired256, and four callbacks.
The old error grouped non-finite, negative, precision-range, and above-enqueued values together.
It therefore did not prove that the timestamp exceeded submitted or enqueued frames.
The numeric sample time and flags were absent.
Cleanup, device settings, source, and product equality passed under Root's retained qualification.

After two native failures, Root stopped further clock-fix attempts.
The mistaken model equated a queue-relative sample coordinate with a payload prefix starting at zero.
Source-rate units alone do not establish that origin.
Apple's [buffer scheduling API](https://developer.apple.com/documentation/audiotoolbox/audioqueueenqueuebufferwithparameters(_:_:_:_:_:_:_:_:_:_:)) exposes actual buffer start times.
No scheduling or mapping implementation is adopted from that observation.

DELTA06 changes diagnostics only.
It splits the four existing rejection classifications without changing their conditions or order.
It captures raw successful SDK timestamp bits, finite value, flags, discontinuity, and enqueue bound before validation.
It exposes current enqueued and last accepted frame alongside existing submitted, acquired, and readiness observations.
Exact integer bits preserve NaN and infinity payloads through JSON.
No callback, enqueue credit, permission, deadline, scheduling API, or rejection policy changes.
No additional SDK query runs.
Root owns review, compilation, no-start checks, and any separately released single diagnostic silent START.

## Observed pre-zero first clock

Diagnostic round05 proved the first sample time was exactly `-522`.
The raw IEEE754 bits were `13871174813231349760` (`0xc080500000000000`).
SDK status was0, timestamp flags7, discontinuityfalse, observed runningtrue, and readinessfalse.
The enqueue bound and current enqueue count were256; no prior clock frame was validated.
The producer had submitted512 frames and callbacks had acquired256.
Root retained the exact raw receipt and did not retry that phase.

DELTA08 handles this observed pre-zero value at the readiness boundary.
Only finite negative samples with a valid sample-time flag, no discontinuity, and no accepted clock in the current episode may wait.
They return an unknown cursor under the original fixed credit and five-second progress deadline.
Advancing negative samples do not refresh that deadline or change the stored frame.
The pure Clock frontier validator still rejects negatives.
All post-ready, non-finite, discontinuity, precision, ahead-bound, backward, and other native error guards remain strict.
Same-queue resume retains the prior validated origin.
No schedule mapping, window expansion, test delay, pause relaxation, or hardware setting change is added.

The actual `-522` value anchors the RED regression.
Tests cover negative-to-zero-to-positive readiness, post-ready rejection, deadline exhaustion, fixed credit, and resume origin.
Apple's client time description includes when the queue has started or will start.
This supports bounded pre-origin readiness, not an exact physical start-time claim.
Root owns narrow review, new source checks, compilation, no-start checks, and any fresh qualifying silent START.

## Queue timeline and payload mapping

Root authorized DELTA09 after the measured queue time3293 exceeded the enqueued payload768.
The exact cause of that coordinate gap remains unproven.
The old queue-time-equals-payload invariant was wrong.

The worker records each returned actual buffer schedule with contiguous payload and output epochs.
A fixed ledger reserves one record per ring frame before playback.
Every outstanding record owns at least one frame inside the unchanged enqueue window.
Callbacks release PCM slots without retiring schedule metadata.
Validated queue time retires only complete scheduled spans.
Gaps advance no payload and grant no unearned credit.
Raw queue continuity and mapped payload retirement remain separate authorities.

Pause captures a fresh mapped prefix while running and freezes it after native pause succeeds.
No post-pause timestamp advances that prefix.
Same-queue resume preserves the schedule origin and strictly validates its running-clock continuity.
Engine pause keeps its existing close/reopen and bounded renderer replay.
After asynchronous Stop(false), retirement holds until the current drain generation completes.
No reset clock becomes active timeline evidence.
Proven completion retires all enqueued payload before final counters and disposal.

Schedule failures retain exact returned timestamp bits, flags, payload span, and output epochs.
Successful SDK acceptance commits its payload count before schedule validation.
Invalid schedule evidence preserves prior retirement and history, then faults the queue without retry.
The existing noStart entrypoint checks actual enqueue timestamps before permission denial and close/reopen.
The Engine silent lifecycle test remains unchanged.
Pure schedule tests cover origins, gaps, tiny EOF spans, slot reuse, saturation, epochs, and malformed timestamps.
Real renderer tests cover replay saturation, whole-span recovery, and exact held samples.
Root owns independent review, canonical checks, fresh Mac compile/noStart, and any separately released silent START.

## Native qualification and Mac defaults

The reviewed schedule implementation passed native preparation and the generated-silence owner lifecycle.
That lifecycle covers pause, seek, resume, natural EOF, second-session mute/stop, and shutdown.
Full selected-device snapshots, source bytes, and all four compiled products stayed unchanged.
The full repository check and six native UI checks passed for that source snapshot.

A separate comparison used the actual packaged Engine with identical polling and generated silence.
The 256-frame window showed an acceptance slope near23935 frames/second.
The 4096-frame window showed an acceptance slope near44078 frames/second for the44100 Hz source.
Startup observations were similar; the larger difference occurred before full submission.
These observations support a4096-frame Mac draft default while retaining explicit user choices.
They do not establish acoustic speed, pitch, or underrun counts.
The default change passed all six native UI checks and the full repository check.
Its qualified source archive is SHA256 `f65d15dcc5c9ddeea792d97fdb41804feb3a3220a476487ff29fc3ab78ddc17d`.
See the [redacted qualification](../../evidence/local-playback/coreaudio-system-20261121.json).

## Live handover result

The qualified development package replaced the failed Engine through a graceful owner shutdown.
The existing queue and old window survived; the new native window opened separately.
The selected local track started once at the existing −30 dB software level.
Two fresh observations confirmed Playing, advancing frames, and nonzero software PCM.
The track submitted and accepted all 11,543,616 frames, then reached Idle without an Engine error.
The adapter confirmed output closure, and the Engine returned to muted.
Actual hearing remains unconfirmed.
The source fix is published as `2c6ab56a38aec2248e5cf02174560124b9ef193b`.
