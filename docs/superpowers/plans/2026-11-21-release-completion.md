# Finish the first Earph release

Doctor Biz authorized continued implementation of the whole first release.
Use the definition of done in [SPEC.md section 19](../../SPEC.md#19-release-evidence-and-definition-of-done).
Its M0–M4 requirements define this release. Requirements marked LATER remain future work.
Keep [requirements.json](../../requirements.json) as the canonical requirement catalog.
Keep exact execution evidence separate from intended behavior and catalog progress labels.

## Completion rule

A passing unit, review, commit, or package is a checkpoint.
Continue through dependent work after each checkpoint.
Finish only when the first-release gates pass or a real external blocker prevents further authorized progress.
If one path needs physical evidence or approval, continue independent implementation and verification.
Ask Doctor Biz before deletion, credential expansion, or a new irreversible action.
Preserve all earlier builds, worktrees, evidence, user files, and unsaved windows.

## Verified starting point

The Play fix is published in `2c6ab56`; its evidence and usage notes are published in `4d8f6d5`.
The full repository check and six native Mac control checks pass for their recorded source snapshots.
The selected local track submitted all frames and closed output without an Engine error.
Doctor Biz later confirmed audible local PCM playback from the installed Orbit app.
He reported that Stop did nothing; a fresh fenced owner command stopped playback and confirmed output closure.
The controller fix passes independent review, the full Linux check, and hosted native Mac UI checks.
The later simpler-player installation described below supersedes its pending Orbit delivery.
See the [Stop evidence](../../evidence/local-playback/controller-stop-20261121.json).

The same unsigned Mac development app now occupies the user's `Applications/Earph.app` path.
All 27 bundle files match the verified source package.
The copied app launches, while the existing Engine and window remain running.
This verifies one same-user delivery, not the complete AC-50 installation and rollback matrix.
See [delivery evidence](../../evidence/packaging/macos-user-install-20261121.json).

## Integration order and ownership

1. Implement the [pure Sendspin player timeline](2026-11-21-sendspin-player.md).
   Its isolated owner changes only Sendspin protocol, mapping, timeline, and focused tests.
2. Implement [typed DSD ownership](2026-11-21-dsd-engine-playback.md) in a separate worktree.
   Its owner holds the Core, Engine, and DSD projection files until Root integrates the result.
3. Resolve UPnP controller interoperability from actual producer and consumer evidence.
   Keep strict SOAP argument validation unless the inspected contract proves our behavior wrong.
4. Integrate Sendspin's paired admission, single source lease, scheduling, latency ledger, and live correction.
   Root starts shared Core/Engine edits only after DSD integration releases those files.
5. Run real Music Assistant and independent-controller scenarios against the resulting owner.
   Keep software, native, independent-clock, and acoustic evidence boundaries explicit.
6. Reconcile the remaining first-release requirements with their actual implemented entry points and evidence.
   Repair missing behavior before changing its status to complete.
7. Build and install the final named Mac/Linux candidates, then run the remaining release gates.
   Complete notices, source obligations, performance, failure, upgrade, rollback, and signing work within approved authority.

Root owns canonical integration, full checks, independent review, commits, and publication.
Workers own disjoint implementation files and retain failing-baseline and passing-test evidence.
The dashboard maintainer owns the existing progress page and its status source.
Do not create another output owner, credential store, transport, requirement catalog, or progress page.

## Gates that implementation cannot manufacture

DSD requires endpoint-specific qualification before physical submission.
PCM device negotiation cannot establish that qualification.
Sendspin readiness requires current admission, clock mapping, source ownership, and honest output timing.
A timestamp queue alone does not meet that contract.
Independent-clock and acoustic results require the named real setup and capture boundary.
Doctor Biz confirmed that no calibrated two-output capture setup is available.
Physical Sendspin sync qualification is blocked until that setup becomes available.
Keep output timing unqualified; this does not waive the first-release timing requirement.
Continue independent software integration and controller qualification.
Signing requires authorized signing material; an unsigned build remains unsigned.
Do not claim any of these gates from source inspection or a software file/null capture.

## Current work

Doctor Biz approved simplifying the native player to Open a file, then Play.
Connect and preparation become controller work; output approval remains explicit.
Keep Play, Pause, Stop, and ordinary volume controls easy to reach.
Keep runtime paths and Engine diagnostics under Advanced.
Prioritize this usable local-player flow before resuming shared Sendspin runtime integration.
Preserve the sealed Sendspin components and private integration work during this change.

Verify the simplified flow against these concrete outcomes:

- Launch attaches the existing owner or starts one owner without selecting output or starting audio.
- Open adds files without playback. Play starts the selected or first file through an already approved output.
- Play waits for actual preparation before one fenced Unmute. Mute and Stop cancel that continuation.
- Pause and Play resume the current file without restarting it. A different local selection replaces that local source.
- After automatic queue advance, default Play resumes the active file instead of the older Engine selection.
- Consume a row selection after its Play completes. Preserve a later selection and any refused selection.
- Volume uses current output facts and caps. Ordinary playback progress cannot invalidate an otherwise unchanged volume gesture.
- Native controls pass Mac checks without clipboard, recording, or accessibility grants.
- Inspect actual visual evidence before claiming the simplified interface is visually verified.
- Quit and background playback retain their current policy until the deferred product decision.

The pure Sendspin timeline is integrated and passes the full repository check.
Its independent review cleared both findings after focused fixes and failing-baseline regressions.
See the [scoped evidence](../../evidence/network/sendspin-player-timeline-20261121.json).
Typed Network admission and bounded player leases are integrated after independent review.
Their paired native policy grants no source or output authority.
See the [transport evidence](../../evidence/network/sendspin-player-transport-20261121.json).
Engine ownership, scheduling, and actual output qualification remain the next Sendspin work.
Typed DSD software ownership is integrated after independent review and focused worker checks.
The combined source passes the full Linux canonical check with the published Stop fix preserved.
See the [retirement evidence](../../evidence/dsd/engine-retirement-linux-20261121.json).
Public DSD submission and Core Audio DSD remain unavailable pending their release gates.
Native output timing adapters pass source review, the full Linux check, and hosted Mac compilation.
See the [timing evidence](../../evidence/local-playback/native-timing-observations-20261121.json).
UPnP's independent SDK passes a native GetMute against interim source `ef87602` and clean scoped retirement.
See the [scoped controller evidence](../../evidence/network/upnp-native-getmute-20261121.json).
The full controller matrix and qualification against the later DSD source remain pending.
Root continues release integration while the dashboard records verified milestones.
Git commits and scoped evidence record completed work; this document defines the continuing execution boundary.

The native Mac build now shares one deployment target across Rust, C, and Swift.
Hosted Linux and Mac checks pass for source `6318d6a`, including strict linking and native controls.
The later DSD source exposed a Linux-only test variant in the Mac test build.
Its conditional build boundary now matches its Linux-only consumer.
See the [build evidence](../../evidence/packaging/macos-deployment-target-20261121.json).

Published checkpoint `700ad16` passes hosted Linux and Mac checks in run `44939701838`.
Those checks precede the simpler player interface and do not qualify its current changes.
The simpler player now passes the full Linux check and six native Mac checks on the same 2,097 source files.
Nineteen real-owner playback cases pass, including cancellation, automatic advance, Stop, and guarded volume.
Independent review cleared the controller, native selection, refresh classification, and guarded-volume changes.
See the [simpler-player evidence](../../evidence/local-playback/simple-player-20261121.json).

One earlier full run failed an existing renderer callback-expiry test at its local IPC deadline.
The unchanged isolated test and the later unchanged full run pass.
The cause remains unestablished. No product or test deadline changed.

Core accepts Stop after playback progress only for its exact owner and active session with a supplied revision.
Guarded volume preserves its original output, source, consent, policy, mute, and exact-silence observations.
An older gesture cannot clear a later repeated Silence.
The native row intent preserves later selections and follows the actual active entry after Play completes.

Orbit returned online. The simpler player is installed at `~/Applications/Earph.app` with all 27 files verified.
The prior bundle, prior window, and checkpoint backups remain retained.
Doctor Biz confirmed audible local playback after raising the app volume.
Keep his current playback, queue, volume, and output untouched during later interface updates.
See the [delivery and user confirmation](../../evidence/packaging/macos-player-delivery-20261122.json).
AppKit bitmap and layer captures omit composited SwiftUI content. They do not qualify the visual result.
These checks do not complete the first release or its physical output gates.

Hosted Linux and Mac software checks pass for `fcfc892` in run `44963876067`.
Two unsigned candidates now pass package integrity checks with their exact retained products.
The Linux package passes an actual owner and CLI lifecycle without selecting output.
The Mac package passes native plist validation and the scoped installation checks recorded above.
These products retain working-snapshot provenance; they are not named `fcfc892` builds.
See the [candidate evidence](../../evidence/packaging/simple-player-candidates-20261122.json).

The independent native UPnP SDK now passes rendering reads against frozen clean `fcfc892` source.
SetMute and SetVolume correctly refuse without an active renderer source.
The SDK, owner, and discovery tasks join, and the isolated bridge retains no peers.
See the [current rendering evidence](../../evidence/network/upnp-current-rendering-20261121.json).

The muted active-source scenario now passes all nine native actions with HTTP 200.
Actual source, output, and graph readiness precede Mute true and volume zero.
Native Stop retires the source. Submitted frames and captured bytes stay zero.
All native tasks join, grants are revoked, containers stop, and the bridge retains no peers.
The earlier harness failure remains retained; its correction polls only original device-read receipts.
The broader controller matrix remains open.
See the [muted-source evidence](../../evidence/network/upnp-muted-source-20261122.json).

The independent native controller also passes muted Pause settlement and same-source Resume admission.
Twelve actions include the declared Seek and Next refusals.
Resume remains Buffering while muted; resumed audio progress remains unqualified.
All tasks join, containers stop, and the bridge retains no peers.
See the [muted transport evidence](../../evidence/network/upnp-muted-transport-20261122.json).

The footer now separates playback preparation from ordinary background reads.
Its four-file snapshot passes six native Mac checks and the full Linux canonical check.
Independent delivery review caught an owner-launch fallback during normal GUI startup.
Explicit attach-only startup now passes the actual failing-baseline regression, six native checks, and independent source review.
The replacement package passed delivery review and installed with explicit attach-only startup.
Fresh reads verify the new GUI, all 27 files, the original Engine, and all 12 queue entries.
The installer preserved the observed Idle/muted state and sent no playback or volume mutation.
Its wrapper used the wrong receipt path after successful activation; read-only recovery verified the actual result without replay.
See the [attach-only delivery evidence](../../evidence/packaging/macos-attach-only-delivery-20261122.json).
See the [native status and startup evidence](../../evidence/local-playback/native-status-20261122.json).

Doctor Biz's next Play attempt exposed an existing AudioQueue startup clock failure in the unchanged Engine.
The native call succeeded but returned a discontinuous negative sample before any accepted clock origin.
The narrow correction keeps that reading unknown under the existing five-second deadline.
It preserves faults after an established origin and does not retire payload from the invalid reading.
The actual regression fails before the correction; all 56 CoreAudio unit tests pass afterward.
Independent review and the full Linux canonical check pass for this correction.
The same 2,107 source files remained unchanged throughout that check.
The replacement Mac Engine passes its build, 32 pure clock tests, strict CoreAudio Clippy, and control-only owner integration.
The replacement remains uninstalled. Real playback recovery remains unverified.

Hosted checks for `2d37afe` pass on Mac but fail one Linux Pause-to-Play scenario.
Core accepts Pause before the worker acknowledges it. The controller previously enabled Resume during that interval.
The controller now derives its waiting state from the exact accepted Pause receipt and current owner and session.
Ordinary controls wait for acknowledgement. Stop and Mute retain their safety paths.
Four model tests pass through real Core receipts and the reply decoder.
All 19 real-owner playback cases pass, including the previously failing Pause-to-Play scenario.
Independent review clears the exact-owner predicate, receipt polling, and Stop and Mute safety paths.
Combined canonical checks and native delivery remain pending for this change.

## Running clock retreat on headphones

Doctor Biz reported that playback through his headphones failed.
The native queue clock read one to two frames behind its last accepted reading during ordinary playback.
The strict validator treated that reading as a backward clock and faulted the output.
Two retained failures show the same shape after substantial playback.
The cause being the headphone route or the 44.1 kHz to 48 kHz conversion remains unproved.

The correction keeps the strict validator.
For ordinary running progress reads only, a retreat of at most one whole coordinate stays unknown.
It retires no payload, releases no enqueue credit, and keeps the existing five-second deadline.
Pause, drain, cancellation, a finished source, discontinuities, and larger retreats stay strict.
This is Earph policy, not an Apple guarantee.

Both regression tests fail on the Mac before the correction for the intended assertions.
All 62 CoreAudio unit tests, every CoreAudio test target, and strict Clippy pass on the Mac afterward.
Independent review found no defect.
The macOS wiring that selects the progress purpose has no direct test; the pure decision does.
A retreat after the source finishes still faults, as the policy states.
The corrected Engine is installed on Orbit, paused and muted, with the queue and volume preserved.
Real headphone playback remains unverified.
See the [running-retreat evidence](../../evidence/local-playback/coreaudio-running-retreat-20261122.json).

## Product decision: Quit stops playback

Doctor Biz decided the quit and background-playback policy on November 21, 2026.
Quit stops playback and shuts down the Engine. Earph offers no background-playback service.
See [the quit decision](../../adr/0007-quit-stops-playback.md).
Both desktop apps now send Quit when the user quits or closes the only window.
Quit reads current State on the safety lane, then sends the fenced owner shutdown.
It retries a stale shutdown twice and closes within five seconds if the Engine never confirms.
Five regressions fail against the earlier close-only behavior and pass afterward.
They cover a changed revision, real playback, an owned child, an unconfirmed shutdown, and the native action.
A sixth regression covers the deadline while the Engine holds its reply.
The Mac owner integration test now quits through the Swift store and checks that the Engine process exits.
Independent review found no path that holds the application open.
Its four findings about closing without a shutdown are corrected.
The full Linux canonical check and the native Mac check pass on the same unchanged source.
An Engine that refuses or never answers can outlive the application; the application never kills a process.
A Quit during Engine launch still leaves that idle Engine running.
The separate confirmed **Stop Engine and Quit…** control remains.
The corrected application is not installed on Orbit.

## Command-line player

Doctor Biz chose a command-line player as the main way to exercise playback on November 22, 2026.
Both headphone faults were Engine faults, and each desktop delivery cost a full package and install.
The desktop application stays at its current state until the Engine work needs it again.

`earph` plays local files through the same Engine and reports the result by exit status.
It starts an Engine in its own process when none runs at its runtime directory.
Its default runtime is separate from the desktop application's runtime.
Naming an output on the command line approves that output. The volume changes only on request.
See [the player guide](../../../crates/earph-engine/PLAYER-CLI.md).

Nine process tests run the real binary against a private ALSA capture device.
They cover finished, refused, failed, stopped, interrupted, attached, and replaced playback.
Independent review found two cases where `play` stopped playback that it did not start; both are corrected.
The Mac build passes strict Clippy, and `earph devices` lists outputs through Core Audio.
Real audio through `earph` on a Mac remains unverified.
The package does not yet bundle `earph`.
Use `earph` for the next headphone check and for Engine regression checks.
