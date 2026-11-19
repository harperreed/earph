# Native Connect and Queue

Doctor Biz approved the GUI contract retained under `target/desktop-gui-contract`.
Root released `1bab` on `feat/desktop-gui` at `9bc97128b96315a7e599cad95e4b3e207571ba54`.
Root added pinned eframe 0.37.0 with AccessKit, default fonts, Glow, Wayland, and X11.
Root owns manifests, lockfiles, notices, inventory, and native harness provenance.
The GUI implementation owns new GUI source, narrow library wiring, tests, and this document.
Existing client, dispatcher, and projection source remain frozen.
Root later released the narrow model resume seam and its tests, recorded below.
Commits, pushes, installs, deletion, Mac actions, output selection, source starts, and unmute are outside this unit.

## Deliverable

Build one native Connect and Queue controller over the existing `DesktopClient`.
Keep the standalone owner authoritative.
Show actual state, receipt progress, errors, and uncertainty.
Use explicit path entry and explicit owner launch.
Disconnect and Close controller leave the owner running.
Keep Stop engine and quit separate.
Paint an opaque panel and matching clear color in Light and Dark.

## Checks

- [x] Watch workflow tests fail before implementation.
- [x] Implement bounded scheduling, queue page fencing, draft preflight, and exact reconciliation.
- [x] Implement explicit launch, identity handshake, cleanup, and shutdown observations.
- [x] Implement labeled native Connect, Queue, state, and receipt views.
- [x] Pass real-owner workflow integration and standalone process E2E without source/output commands.
- [x] Pass final formatting, strict Clippy, and complete package tests after the ownership fix.
- [x] Ask Root to run actual product window checks through the isolated native harness.
- [x] Freeze source and retain evidence for one independent lifecycle/concurrency review.

## Interfaces and ownership

`GuiApp` owns local drafts, navigation, and theme choices.
`GuiController` owns its `DesktopClient`, scheduled work, immutable observations, and cleanup handles.
`OwnerHost` owns child launch, handshake, retained logs, and process exit observations.
Views emit `UiIntent` values and never call IPC directly.
Mutation preflight compares the captured owner/session/revision before building the request.
Mute and Stop use the latest actual snapshot through the independent safety lane.
Accepted work uses exact-byte polling; uncertainty requires explicit reconciliation.
All query and history retention stays bounded.
Retain unresolved records across disconnect; never silently clear unknown outcomes on reconnect.

## Evidence and rulings

First RED output: `target/desktop-gui-validation/round-01/red.log`.
Use new retained output names for later checks.
Use the pinned proof Cargo cache with automatic cache cleaning disabled.
Invoke the installed Rust 1.98.1 binaries directly.
Root's dependency/license preparation can leave inventory red until supplements land.
Report scoped Rust checks separately from the canonical check.

Ruling: keep logs in private retained files instead of long-lived child pipes.
This lets the intentionally surviving owner outlive the controller without blocking pipe-reader joins.
Reading a bounded prefix limits UI memory; it does not claim a disk quota for owner logs.

Ruling: reuse the existing real-owner and transport-proxy test fixtures.
Do not add a second `.scratch` infrastructure or a mock application mode.
This follows the approved source boundary and repository test conventions.

Ruling: retain artifacts and defer commits to Root's independent review.
The task's explicit preservation and no-commit instructions override generic skill cleanup and commit steps.

Ruling: prove independent GUI safety delivery with mute-on.
The initial Stop test incorrectly expected Applied without an active session.
Core rejects that target; the UI must retain its actual rejection.
Playback Stop application remains part of the later transport qualification.
The failed package and canonical attempts remain in `round-01`.

The archived-snapshot regression uses actual owner replies and exact State replay.
It proves that a fresh client model can return an old retained snapshot after reconnect.
Root rejected a separate GUI snapshot fence because the internal client model could still adopt another owner's stale reply.
Root released a crate-private model resume constructor as the ownership correction.
Disconnect transfers the entire presentation model back to the GUI.
Reconnect resumes that same model only after prior request workers finish.
The model preserves ordinals, latest observation, records, and owner-change flags.
The GUI no longer owns a second snapshot cache or receipt archive.
Resume failure returns the intact disconnected model.
Existing public APIs and owner wire contracts stay unchanged.

Canonical attempt two reached the dependency harness and failed four inventory-dependent tests.
Root owns the pending dependency inventory and license supplements.
Keep that failure separate from scoped GUI Rust checks.

## Scoped qualification checkpoint

The final package run passes 39 tests with no failures or ignored tests.
See `target/desktop-gui-validation/round-01/package-tests-05.log`.
Strict all-target desktop Clippy passes without warnings in `clippy-04.log`.
Workspace formatting passes in `format-ownership-fix.log`.
The model resume RED is `model-resume-red.log`.
The original actual snapshot rollback RED remains `archived-snapshot-red.log`.
The owner A→B replay test uses actual owner replies and preserves B's latest observation and request flags.
It also preserves the disconnected model after a failed resume.
The client, dispatcher, and projection source stay unchanged from `9bc`.
Root released only the constructor seam and its tests in the shared model.

Native product-window validation and the independent lifecycle review remain pending with Root.
No Mac, native window, device, installation, commit, push, or deletion ran in this implementation task.
The canonical software suite used only its separately authorized existing Linux scope.
Do not rerun canonical until Root finishes dependency license and inventory integration.

## Same-review corrections

Root accepted shutdown recovery and Accepted safety polling findings from `target/desktop-gui-review/round-01/review.md`.
Root released these corrections after the dependency canonical passed 753/0/4 without compiler diagnostics.
The earlier 39-test freeze and earlier canonical outcomes retain their original source scope.

Rejected and uncertain shutdown now retire the active quit workflow without removing its exact receipt.
Refresh, Disconnect, and reattachment remain usable.
An uncertain shutdown blocks replacement intent until explicit exact-request reconciliation.
Resolving that historical request does not automatically quit the controller.

Accepted ordinary, safety, and shutdown work now shares one bounded round-robin scheduler.
Fixed slots prevent changing ordinary identities from starving the two safety requests.
At most one replay admission occurs per 100 ms.
Permanent reconciliation refusal retires automatic tracking while preserving model history.
Dispatch saturation delays polling without losing the request.

Real-owner tests cover stale rejection, retained receipts, and a dropped shutdown delivery followed by exact reconciliation.
A controlled Unix receipt producer covers Accepted safety scheduling and refusal after actual owner change.
It serializes Core receipt types and uses the real LocalClient transport consumer.
It never executes Stop against playback or claims playback qualification.
The first fixture attempt used an invalid socket prefix and failed before IPC.
The corrected private fixture follows the existing `o-*.sock` contract; all attempts remain retained.

Retain focused evidence under `target/desktop-gui-corrections/round-01`.
Run one final canonical after focused GREEN and final source binding.
Return the exact delta to the same GUI reviewer.

The same reviewer found a residual original P2 after revision 1's successful canonical.
An explicitly reconciled uncertain safety request could return Accepted without a tracking slot.
Root accepted the finding and authorized this narrow correction.
Revision 1 remains retained with package 43/0/0, canonical 757/0/4, and its unchanged source manifest.

Successful explicit safety reconciliation now restores its existing bounded workflow slot.
It uses the shared request lane and excludes historical OwnerShutdown.
If both safety slots are occupied, admission stops before sending the explicit replay.
The model preserves the uncertain request for another explicit attempt.
No global history polling, duplicate model, public mode, or quit-intent restoration is added.
New controlled-consumer regressions cover Uncertain, explicit reconciliation, Accepted, automatic Applied, exact bytes, and the two-slot bound.

## Queue edits during observation

Root authorized this bounded correction after three preserved native attempts.
Those attempts establish no mutation admission or owner rejection for the missed actions.
The previous canonical passed 759 tests; its source and receipts remain historical.

Queue actions now stay available during background reads when the published queue context matches current State.
One local waiting task captures the command and its owner, session, revision, epochs, and entry identity.
The status view labels this task as waiting and not sent.
Another queue edit is refused while waiting, preflighting, or mutating.
After observation, the existing fresh State preflight validates the captured context before mutation admission.
Changed context refuses the edit without repairing its target.

Disconnect, Close controller, shutdown intent, safety intent, owner change, and failed observation cancel local waiting work.
Mute and Stop remain available during observation and waiting.
The controller retains its single PresentationModel and existing receipt polling behavior.
There is no second intent queue, parallel mutation path, or automatic retargeting.

The private held-read transport forwards real owner replies without changing statuses.
Workflow tests cover single admission, exact captured identity, stale refusal, cleanup, shutdown recovery, owner change, and mute preemption.
A timed-out real read proves cancellation before later observation recovery.
Offscreen egui AccessKit actions prove widget availability, path entry, waiting text, and further-edit exclusion.
These tests do not qualify native accessibility or reinterpret the three earlier native attempts.

One existing safety-capacity test observed Accepted while its replay send remained in flight.
It now waits for that send to finish before testing another request's admission.
The retained failure records the actual bounded dispatcher refusal.

Evidence lives under `target/desktop-queue-edit/round-01`.
Run focused checks, then one canonical bound to final source.
Root owns the new independent review and subsequent native qualification.


## Native Connect and Queue checkpoint

The frozen product passes 768 canonical tests and 54 desktop package tests.
The package count overlaps canonical coverage.
The seventh isolated Linux native attempt passes actual Connect, Queue, Mute, themes, and confirmed shutdown.
Both GUI instances and native consumers exit zero.
Fresh process checks find the engine and every owned process absent.
The engine numeric exit remains unknown because the GUI owns its child handle.
The separate real-owner tests cover numeric exit contracts.
Source, executables, tools, and fixtures match the frozen inputs after native cleanup.
Root inspected the actual Light and Dark window captures.
These captures establish own-window PNG opacity, without compositor or contrast claims.
All six earlier failed attempts remain retained.
See `docs/evidence/desktop/native-connect-queue-linux-20261119.json` for bounded evidence.
Mac windows, output and transport controls, profile workflows, and full desktop parity remain pending.
The dispatch original notice and Apple SDK distribution review remain open release limits.

## Strict T12 client integration

Root approved Unit 1 from `target/desktop-next-contract/round-01/contract.md` and its source map.
The foundation is `b58549bb94c1c86416a5d05e922cd16aebaa0fbb`, with T12 `e2c2f5d` merged in progress.
Root owns metadata, the merge, foreign/native harnesses, and the test-only model cleanup correction.
The projection unit owns strict view types, its consumer tests, this checkpoint, and the narrow README update.
Client, dispatcher, model, and GUI production behavior remain unchanged.
The Start constructor and Output/Safety/Transport GUI are not released in this unit.

State now projects independent local caps and nullable configured profile references.
Capabilities project application restrictions, exact association backends, and directory package limits.
Playback, backend, graph, profile, association, package, and FIR results use typed immutable views.
The decoder retains exact raw replies and required nullable-key presence.
Only explicitly omitted processing observations may be absent.
Unknown or malformed fields remain protocol failures, without an older-shape fallback.
The owner remains the sole source of policy and native identity authority.

Focused tests use actual Core serializers and actual owner/LocalClient replies.
Generated package objects round-trip through real owner processes without applying output authority.
Actual FIR imports provide non-null correction previews and owner-computed references.
The muted live case opens only an isolated ALSA file/null route with generated zero PCM.
It prepares and applies a real saved profile without unmute or submitted output bytes.
Controlled identity and streaming serializer fixtures establish client structure, not native qualification.
Foreign driver extensions and platform qualification remain Root-owned.

The first strict projection RED and all later attempts remain in `target/desktop-projection/round-01`.
One package attempt incorrectly used the old standalone binary before rebuilding it.
Its failed GUI owner startup attempts remain preserved; their numeric exits were not observed.
A later exact cleanup gate found those identified PIDs already absent and sent no requests or signals.
The actual merged owner E2E then exposed the expected outdated unavailable-application assertion.
The assertion now follows the committed capability contract.

Root integrated the separately reviewed backend observation repair after its independent review passed.
Its patch hash is `69924456e9be6e0ff21db13be84ee2e2b75be9e15782912a2e8ace0981d0e229`.
Root releases one final canonical after focused checks and source freeze.
That final source must include the producer repair and Root's test-only cleanup correction.
Do not add earlier standalone GUI and profile counts to claim combined qualification.
Root assigns one independent review of the final combined changes.

## Integrated projection and native checkpoint

The final integrated canonical passes 896 tests, with zero failures and five ignored.
The desktop package passes 69 tests. Rust FFI passes nine tests. These counts overlap canonical.
One independent review approves the strict projection and Root's test cleanup without material findings.
The eighth native run binds the rebuilt owner and unchanged GUI to the complete frozen source.
Connect, queue revisions zero through five, Mute revision six, themes, and separate shutdown confirmation pass.
Both GUI instances and consumers exit zero. Fresh checks find all original process identities absent.
The owner's numeric exit stays unknown because the GUI owns its child handle.
Root and the reviewer inspected both private Light and Dark captures.
All earlier attempts, original fixtures, metadata, and raw native evidence remain retained.
See `docs/evidence/desktop/t12-projection-native-linux-20261119.json` for the bounded result.
Root added only that evidence document and this checkpoint after the successful checks.
Foreign profile workflows, the Start constructor, device discovery, and Output/Transport GUI remain pending.
The normal merge retains both existing parents and the reviewed backend observation repair.
