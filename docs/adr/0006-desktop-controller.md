# Desktop controller and presentation boundary

Date: November 18, 2026.
Status: The shared controller is integrated. Native macOS rendering is being replaced and remains under qualification.

## Decision

Use pinned eframe/egui 0.37.0 for the Linux desktop shell.
Use SwiftUI and AppKit system controls for macOS.
The Mac app uses standard windows, menus, forms, text editing, tables, and file dialogs.
It defaults to the system appearance without changing OS settings.
The Linux renderer paints matching opaque panel and clear colors.
Its Light/Dark choices stay inside the application.
Root's retained X11, macOS, and nested Wayland proofs establish the scoped native-window and accessibility behavior.
They do not establish screen-reader user sessions, native dialogs, packaging, or product workflows.
The Mac app statically links the shared controller through generated UniFFI bindings.
Both renderers reuse the same request and workflow implementation.

Attach through the existing `earph-ffi::LocalClient` on both platforms.
Keep the standalone Engine executable as the authoritative owner.
Use existing Core command types and Engine's shared control-frame limit.
Never implement a second Session or audible policy in the desktop client.

## Client contract

Keep immutable request bytes for exact reconciliation.
Project version-one replies strictly and retain their original bytes.
Keep required null observations distinct from absent fields.
Keep opaque playback, graph, and profile bodies unqualified until typed views are added.

Maintain separate bounded ordinary and safety worker lanes.
Bound queued requests, completion retention, one UI drain, and presentation history.
Reserve two history slots for safety work so ordinary uncertainty cannot block its admission.
Keep accepted and uncertain records until reconciled or explicitly handled by a later operator workflow.
Do not silently evict unresolved intent to admit more work.

An accepted receipt represents pending work.
Applied represents that operation's owner contract; it never proves physical audio presentation.
A lost reply or top-level owner uncertainty error can follow an applied mutation.
Reconcile only with the exact retained envelope.
Do not repair stale targets or revisions.

Stop retains the exact observed owner and non-null session.
Core accepts that Stop when a supplied revision becomes stale within the same session.
Changed owners or sessions remain rejected. Ordinary mutations still require the current revision.
The controller sends Stop once and retains its receipt until output retirement completes.

`volume.set_guarded` carries the facts captured by one software-volume gesture.
Core checks its exact owner, session, source, active entry, epochs, output, caps, mute, and silence state.
It requires output consent and a supplied revision no newer than the current revision.
An unchanged context permits the gesture across playback revision changes.
The command shares existing gain, ramp, headroom, and native-effect checks.
The controller retains the original request and never retries a refused volume mutation.
The ordinary `volume.set` command keeps its strict revision check.
Fixed arming stays owner-local; the software observation carries only serializable caps.
The guarded command also retains the original gesture's `observed_revision` through its later preflight.
If exact silence was already active, that original revision must still be current.
This prevents an older gesture from clearing a newer repeated Silence command.
After any revision change, restoring finite volume from exact silence requires a fresh gesture.

Only observed state replies update the current snapshot.
Keep mutation receipts separate from observed owner state.
After an owner change, preserve old records and block old-target reconciliation.
Keep replayed historical reads from replacing newer observations.

Disconnect never stops the owner.
Quit is a different action: it stops playback and shuts down the owner.
See [the quit decision](0007-quit-stops-playback.md).
Stop admission, discard queued work, and let existing IPC calls reach their bounded outcome.
Return worker ownership so the view can join it off the UI thread.
RAII drop remains a joining fallback for resource safety.
No request-cancellation or serial-server preemption promise follows from the two lanes.

## Integration and evidence

The package uses the shared workspace and authoritative dependency lock.
Build provenance includes the desktop manifest and production source.
The canonical check builds the real owner before desktop process tests run.
The desktop package tests actual Core replies and real owner IPC.
Transport-fault tests forward to the actual owner before losing or delaying a reply.
A standalone-process E2E uses generated queue media and profile saves without selecting output or starting a source.
The established Linux canonical suite retains its separately authorized file-to-null playback coverage.

Explicit owner launch, queue, output, profile, and diagnostic workflows use that shared controller.
The Mac renderer preserves those workflows through captured presentation tokens.
An expired token fails instead of silently selecting a newer State.
Native rendering and packaging require their own real Mac checks.
See the [native Mac plan](../superpowers/plans/2026-11-21-native-macos-controller.md).
