# Desktop controller and presentation boundary

Date: November 18, 2026.
Status: Root accepts the direction and reviewed client/model unit. Shared-workspace integration remains pending.

## Decision

Use pinned eframe/egui 0.37.0 for the later native desktop shell.
Paint an opaque central panel and return a matching opaque clear color.
Keep Light/Dark choices inside the application without changing OS appearance settings.
Root's retained X11, macOS, and nested Wayland proofs establish the scoped native-window and accessibility behavior.
They do not establish screen-reader user sessions, native dialogs, packaging, or product workflows.
This first lasting unit adds no toolkit dependency.

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

Only observed state replies update the current snapshot.
Keep mutation receipts separate from observed owner state.
After an owner change, preserve old records and block old-target reconciliation.
Keep replayed historical reads from replacing newer observations.

Disconnect never stops the owner.
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

Owner launching, native views, dialogs, packaging, and full transport equivalence remain later units.
