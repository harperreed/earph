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
Doctor Biz played a track through his headphones with `earph` on Orbit on November 22, 2026, and reported that it worked.
That report names no duration and no build, and no state was retained from that run.
An earlier speaker run on the same day showed no validated queue clock reading after 11 seconds of playback.
Whether that clock settles later remains unexamined.
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

Thirteen process tests run the real binary against a private ALSA capture device.
They cover finished, refused, failed, stopped, interrupted, attached, replaced, and repositioned playback.
Independent review found two cases where `play` stopped playback that it did not start; both are corrected.
The Mac build passes strict Clippy, and `earph devices` lists outputs through Core Audio.
Doctor Biz heard a 44.1 kHz FLAC through the Orbit speakers at 0 dB on November 22, 2026.
The first runs seemed silent because the track opens near -38 dB and the Engine starts at -40 dB.
`play` therefore prints a hint at -30 dB or lower, and `earph seek` skips a quiet opening.
With no named output, `play` uses the only listed output and refuses to pick between several.
Package candidates bundle `earph`; no Mac package with it has been built or installed.
`earph seek` and the device-rate display remain unverified on a Mac output.
Use `earph` for Engine regression checks.

## Requirement reconciliation

This section records step 6 of the integration order, done on November 23, 2026 against source `93133c7`.
It compares each first-release requirement in [requirements.json](../../requirements.json) with code, tests, and recorded evidence.
The reconciliation read source and evidence; it ran no cargo build and no test suite.
A source reading supports an implementation label. It supports no verification label.

### Label rules

`implemented` means every clause of the requirement has code, tests, and at least one recorded run.
`in_progress` means code exists, and one or more clauses have no code or no reachable entry point.
`partial` means recorded evidence covers part of the acceptance scenario.
`pass` requires the complete acceptance scenario on its named environments.
No requirement moved to `pass`.
A requirement with software evidence and a physical gate stays `partial`; the table names its gate.
The catalog value `blocked` stays unused, because every gated requirement also has recorded software evidence.

### Counts

The catalog holds 59 first-release requirements. Only `ARCH-06` is complete, before and after.

| Implementation status | Before | After |
|---|---|---|
| `implemented` | 4 | 25 |
| `in_progress` | 29 | 33 |
| `not_started` | 26 | 1 |

| Verification status | Before | After |
|---|---|---|
| `pass` | 1 | 1 |
| `partial` | 32 | 56 |
| `not_run` | 26 | 2 |

Thirty-six requirements changed a label. Twenty-five of them carried `not_started` while tested code and evidence already existed.
`REL-04` stays `not_started`. `SRC-05` is `in_progress` with `not_run`, because no record covers queue authority.

### Open requirements

Gap kinds: (a) software work we can do now; (b) needs a physical setup or Doctor Biz; (c) needs signing material or another approval.
A row with two letters has both gaps. The first letter is the gap that blocks next.

| ID | Status | Gap | Kind |
|---|---|---|---|
| `ARCH-01` | implemented / partial | No check proves that transport libraries stay off the playback worker. | a |
| `ARCH-02` | in_progress / partial | No run compares the same commands across GUI, CLI, Rust, Swift, and Kotlin (AC-02). The foreign bindings lack cancellation. | a |
| `ARCH-03` | in_progress / partial | The owner lock covers one runtime directory, not one output. `earph` and the desktop app use separate directories. | a |
| `ARCH-04` | implemented / partial | PCM comparison on a real route and a DSD hardware route remain open. | b |
| `ARCH-05` | in_progress / partial | The playback worker allocates and locks. Only prepared processing is measured as allocation-free. | a |
| `ARCH-07` | in_progress / partial | Every schema has one version. No migration path and no older-version fixture exist. | a |
| `PLAY-01` | implemented / partial | No AC-08 record with source hashes exists. A bad later entry may fail the whole start ⚠️ (source reading, untested). | a |
| `PLAY-02` | implemented / partial | No AC-09 timeline record exists. `earph seek` is unverified on a Mac output. | a, b |
| `PLAY-03` | implemented / partial | Gapless is exact at the ALSA capture boundary only. Core Audio and hardware joins are unqualified. | b |
| `PLAY-04` | in_progress / partial | A queue with a rate or format change refuses to start. No muted reconfiguration exists. | a |
| `PLAY-05` | in_progress / partial | Sendspin refuses every handoff with busy. No test delivers old Sendspin chunks to a new session. | a |
| `PLAY-06` | implemented / partial | No test kills the owner during playback. AC-14 needs two physical outputs. | a, b |
| `PLAY-07` | in_progress / partial | Public DSD submission is refused in code until an endpoint is qualified. Core Audio has no DSD output. | b, a |
| `SRC-01` | in_progress / partial | No shipped command starts the renderer. SSDP is Linux-only. The AC-15 controller matrix is incomplete. | a |
| `SRC-02` | in_progress / partial | The player offers 48 kHz 16-bit PCM only. It has no discovery, no FLAC, and no server volume or mute. | a |
| `SRC-03` | in_progress / partial | No drift correction exists. Qualification needs two independent output clocks and a capture setup. | a, b |
| `SRC-04` | in_progress / partial | State has no field for original source metadata. The Sendspin observation carries no format. | a |
| `SRC-05` | in_progress / not_run | Earph sends no control upstream. No record covers queue authority (AC-19). | a |
| `SRC-06` | in_progress / partial | No fuzz corpus and no single AC-20 run exist. A refused Sendspin authentication over HTTP retries without end. | a |
| `OUT-01` | in_progress / partial | ALSA discovery reports no native formats and no stable identity. AC-21 needs the named machines and DACs. | a, b |
| `OUT-02` | in_progress / partial | The system route has run on the host, but never beside another application on a PipeWire desktop. | b |
| `OUT-03` | in_progress / partial | No measured level, no stored capability record, and no invalidation on a changed environment exist. | a |
| `OUT-04` | implemented / partial | AC-17 needs independent clocks. Core Audio reports no backend delay. | b |
| `OUT-05` | in_progress / partial | A fault reports an error code, not recovery choices. Unplug needs two physical outputs. | a, b |
| `OUT-06` | in_progress / partial | The manual policy has no behavior of its own. A rejected override carries no explanation. | a |
| `DSP-01` | implemented / partial | No AC-24 record binds graph fixtures to a compiled plan. | a |
| `DSP-02` | implemented / partial | No AC-25, AC-26, or AC-27 record exists for current source. The component numbers bind a November 18 commit. | a |
| `DSP-03` | implemented / partial | Same as `DSP-02`: current-source acceptance records are missing. | a |
| `DSP-04` | implemented / partial | No AC-28 record exists. Fractional alignment is absent by design. | a |
| `DSP-05` | implemented / partial | No owner-level run commits two revisions at once (AC-29). | a |
| `DSP-06` | implemented / partial | No AC-30 round-trip record exists for current source. | a |
| `DSP-07` | implemented / partial | No AC-31 record exists. A measurement impulse response is identified by its manifest only. | a |
| `DSP-08` | in_progress / partial | The named latency modes change nothing. The two-hour endurance run needs reference hardware. | a, b |
| `SAFE-01` | implemented / partial | No run changes volume from several controllers at once (AC-33). AC-14 needs two physical outputs. | a, b |
| `SAFE-02` | in_progress / partial | Saturation changes no fidelity status. No owner test injects a non-finite sample. | a |
| `SAFE-03` | in_progress / partial | Hardware volume is refused as unsupported. Its test needs a device with hardware volume. | a, b |
| `SAFE-04` | in_progress / partial | Mute closes the output; no DSD-aware transition is verified. That needs a qualified DSD endpoint. | b |
| `SAFE-05` | in_progress / partial | The server checks no peer credential. No test connects as another user. | a |
| `SAFE-06` | in_progress / partial | No fuzz target exists in the repository. | a |
| `PROF-01` | implemented / partial | Profile application covers software mode only on current output. AC-35 needs several DACs. | b |
| `PROF-02` | in_progress / partial | Exact-device association works on Core Audio only. ALSA has no stable identity. | a, b |
| `PROF-03` | implemented / partial | A foreign-client package import and a Mac package run are unqualified. | a |
| `CTL-01` | implemented / partial | No AC-36 record exists. Events are pull-only with a cursor and a gap flag. | a |
| `CTL-02` | implemented / partial | No AC-36 record exists. The dedupe window does not survive a restart. | a |
| `CTL-03` | in_progress / partial | Neither GUI controls a network source. The Mac GUI shows the signal path as raw JSON. No composited visual check exists. | a, b |
| `CTL-04` | implemented / partial | No evidence record covers the `earph` player commands. | a |
| `CTL-05` | in_progress / partial | The bindings have no version call and no cancellation. | a |
| `CTL-06` | implemented / partial | No test shows playback continuing beside a slow client. | a |
| `OBS-01` | implemented / partial | No AC-38 comparison against backend instrumentation exists. | a |
| `OBS-02` | in_progress / partial | Node versions are always null. No conversion reason is reported. | a |
| `OBS-03` | in_progress / partial | Underruns, deadline misses, CPU load, and queue depth always report `unsupported`. | a |
| `OBS-04` | in_progress / partial | The bundle attaches no environment, fixture, or test outcome. | a |
| `COR-01` | implemented / partial | The target is free text. No AC-30 record exists for current source. | a |
| `REL-01` | in_progress / partial | No SBOM exists. Two dual-license elections are unrecorded. One upstream notice and the Apple SDK distribution review stay open. | a, c |
| `REL-02` | in_progress / partial | Three candidates stay `unselected` with no recorded decision. No protocol version lock file exists. | a |
| `REL-03` | in_progress / partial | No install, upgrade, rollback, or uninstall tool exists. Every candidate is unsigned. | a, c |
| `REL-04` | not_started / not_run | No tool requires passing evidence for each requirement. No stress suite and no hardware matrix exist. | a, b |
| `REL-05` | implemented / partial | No run blocks the network and inspects outbound connections (AC-34). | a |

### Gates that software cannot close

- Independent clocks and a calibrated capture: `SRC-03`, `OUT-04`.
- A qualified DSD endpoint on each launch system: `PLAY-07`, `SAFE-04`, `ARCH-04`.
- Named DACs and two physical outputs: `OUT-01`, `OUT-05`, `PLAY-03`, `PLAY-06`, `SAFE-01`, `SAFE-03`, `PROF-01`, `PROF-02`.
- A PipeWire desktop with another application playing: `OUT-02`.
- Reference hardware for endurance and the published matrix: `DSP-08`, `REL-04`.
- The Mac and a viewer for composited appearance and Mac seek: `CTL-03`, `PLAY-02`.
- Signing material: `REL-03`.
- A legal review of one upstream notice and of Apple SDK distribution: `REL-01`.

### Ranked build list

The rank follows the definition of done: receive both network protocols, install, show reproducible evidence, inspect the pipeline, use every control surface, survive failures.
Sizes are source lines plus test lines.

| Rank | Build | Requirements | Likely files | Size |
|---|---|---|---|---|
| 1 | Start the UPnP renderer from the owner with an explicit option, and add macOS multicast. Default ingress stays off until Doctor Biz approves it. | `SRC-01` | `crates/earph-engine/src/main.rs`, `src/lib.rs`, `src/renderer.rs`, `src/bin/earph/main.rs`, `crates/earph-network/src/upnp/ssdp.rs`, `crates/earph-engine/tests/renderer_owner.rs` | 400 + 500 |
| 2 | Finish the Sendspin player: server volume and mute, FLAC, and discovery. | `SRC-02` | `crates/earph-sendspin/src/player.rs`, `src/message.rs`, `crates/earph-network/src/sendspin/player.rs`, `crates/earph-engine/src/worker/sendspin.rs`, `src/bin/earph/sendspin.rs` | 900 + 1000 |
| 3 | Hand playback into and out of Sendspin, and reject old chunks after a handoff. | `PLAY-05` | `crates/earph-core/src/session_sendspin.rs`, `src/session.rs`, `crates/earph-engine/src/host.rs`, `crates/earph-engine/tests/sendspin_owner.rs` | 300 + 450 |
| 4 | Send next, seek, play, and pause upstream, and keep the local queue apart. | `SRC-05` | `crates/earph-network/src/sendspin/worker.rs`, `crates/earph-core/src/sendspin_control.rs`, `crates/earph-engine/src/sendspin_runtime.rs`, `src/bin/earph/main.rs` | 450 + 500 |
| 5 | Build the release evidence gate: require a passing record for each requirement, and write the software acceptance records. | `REL-04`, the `DSP`, `CTL`, and `PLAY` rows that lack a record | `crates/earph-harness/src/evidence.rs`, `src/catalog.rs`, `src/main.rs`, `docs/evidence/` | 600 + 500 |
| 6 | Add install, upgrade, rollback, and uninstall tools, and one control reference. | `REL-03` | `scripts/`, `crates/earph-harness/src/package.rs`, `docs/native-packaging.md` | 450 + 350 |
| 7 | Reconfigure the output, muted, between tracks that differ in rate or format. | `PLAY-04` | `crates/earph-engine/src/worker.rs`, `crates/earph-core/src/session.rs`, `crates/earph-engine/tests/worker_playback.rs` | 400 + 450 |
| 8 | Count underruns, deadline misses, queue depth, and CPU load. | `OBS-03` | `crates/earph-core/src/session_diagnostics.rs`, `src/diagnostics.rs`, `crates/earph-engine/src/worker.rs`, `crates/earph-alsa/src/output.rs` | 350 + 350 |
| 9 | Add network source controls to both GUIs, and a typed Mac signal path. | `CTL-03` | `crates/earph-desktop/src/gui/queue.rs`, `src/gui/app.rs`, `macos/Sources/Earph/` | 700 + 600 |
| 10 | Add fuzz targets for every untrusted parser, and stop the endless Sendspin retry. | `SAFE-06`, `SRC-06` | new `fuzz/`, `crates/earph-media/src/correction_text.rs`, `src/dsd.rs`, `crates/earph-network/src/upnp/`, `crates/earph-engine/src/sendspin_runtime.rs` | 500 + 150 |
| 11 | Correct Sendspin drift with the resampler's ratio control. | `SRC-03` | `crates/earph-engine/src/sendspin_schedule.rs`, `src/worker/sendspin.rs`, `crates/earph-dsp/src/resample.rs` | 500 + 600 |
| 12 | Report original, received, and output formats apart for every network source. | `SRC-04` | `crates/earph-core/src/sendspin_source.rs`, `src/network_source.rs`, `src/diagnostics.rs` | 200 + 250 |
| 13 | Emit an SBOM and record the dual-license elections. | `REL-01`, `REL-02` | `crates/earph-harness/src/inventory.rs`, `docs/dependency-lock.json`, `docs/adr/` | 250 + 150 |
| 14 | Store capability evidence by device and environment, and invalidate it on change. | `OUT-03`, `OUT-01`, `PROF-02` | `crates/earph-core/src/devices.rs`, `crates/earph-engine/src/devices/`, `crates/earph-alsa/src/native.rs` | 600 + 600 |
| 15 | Complete the diagnostics: node versions, conversion reasons, environment, and a fidelity status that follows saturation. | `OBS-02`, `OBS-04`, `SAFE-02` | `crates/earph-core/src/diagnostics.rs`, `src/diagnostics_artifacts.rs`, `crates/earph-engine/src/render.rs` | 400 + 450 |
| 16 | Add a binding version call and cancellation, and one cross-surface equivalence run. | `CTL-05`, `ARCH-02` | `crates/earph-ffi/src/lib.rs`, `crates/earph-ffi/tests/foreign/`, `scripts/check-ffi` | 250 + 400 |
| 17 | Bind the latency modes to declared limits. | `DSP-08` | `crates/earph-core/src/profiles.rs`, `crates/earph-engine/src/profiles.rs` | 150 + 200 |
| 18 | Close the small safety and ownership gaps: peer credentials, one owner per output, the manual policy, recovery choices, and worker allocation. | `SAFE-05`, `ARCH-03`, `OUT-06`, `OUT-05`, `ARCH-05`, `ARCH-07` | `crates/earph-engine/src/ipc.rs`, `src/runtime.rs`, `src/worker.rs`, `crates/earph-core/src/safety.rs` | 900 + 1200 |
| 19 | Implement hardware volume and Core Audio DoP output. Their verification waits on hardware. | `SAFE-03`, `PLAY-07` | `crates/earph-alsa/src/`, `crates/earph-coreaudio/src/`, `crates/earph-core/src/control.rs` | 850 + 800 |

### Catalog notes

The newest Sendspin work has no evidence record. Its runs live in prose in [the runtime notes](../../sendspin-runtime.md), which the catalog now cites.
The same holds for the `earph` player process tests and the Quit regressions.
Most evidence records name no requirement, so each link in the catalog is an editorial reading of a record's scope.
The acceptance scenario statuses were left as they were. Several say `not_run` while a record names them as partial.
The catalog's top-level `status` text and SPEC Appendix B still describe an earlier state.
