# Native desktop controller

This package controls the existing owner through `earph-ffi::LocalClient`.
The native window provides Connect, Queue, actual state, safety actions, and receipt details.
Its explicit Start local engine action launches the existing standalone executable.
It never hosts a second engine or duplicates owner policy.
Native dialogs, packaging, output setup, playback, profiles, and detailed signal-path views remain later units.

## Launch and use

Build the owner and controller from the shared workspace:

```sh
cargo build --locked -p earph-engine -p earph-desktop
target/debug/earph-desktop
```

In Connect, enter an absolute private runtime path with at most 60 UTF-8 bytes.
Its parent must already exist and satisfy the owner's private-directory checks.
Use Attach for an existing owner or Start local engine for the displayed absolute executable path.
The initial executable path names the controller's sibling `earph-engine`.
The controller does not launch an owner merely because its window opens.
Launch logs remain in a new private directory beside the selected runtime.
Startup error details identify those logs when launch fails.
The UI reads bounded startup prefixes; it does not impose a disk quota on owner logs.

In Queue, enter literal file or folder paths and use the explicit Add buttons.
Queue edits do not decode media or start playback.
Select, move, and remove use stable entry identities and a captured owner/revision context.
An external edit requires refreshing and reviewing the draft before another submission.
Path entry works without a native dialog or shell expansion.
Unprepared is a real preparation state, not a playable-media claim.

The status strip shows actual state and local observation freshness.
Mute and Stop use the existing safety lane independently of ordinary reads.
There is no Unmute, Start, output-selection, or fixed-arming control in this unit.
Light and Dark paint an opaque central panel with a matching clear color.
No appearance choice changes OS settings.

Closing the controller leaves the local engine running.
Disconnect retires client workers without cancelling commands already sent.
Stop engine and quit requires an explicit confirmation and sends the existing owner command.
The controller verifies actual exit only when it owns that same child and owner identity.
It never kills a process to acquire a runtime or to resolve an uncertain shutdown.
Rejected or uncertain shutdown ends the active quit workflow and keeps its exact historical receipt.
Refresh, Disconnect, and reattachment remain available.
An uncertain shutdown requires explicit reconciliation before another shutdown intent.
Reconciling that historical request does not automatically quit the controller.

Receipt details preserve Accepted, Applied, Rejected, Cancelled, Failed, owner errors, and Outcome unknown.
Reconcile resends exact retained bytes, including after disconnect and reattachment to the same owner.
Owner changes block old-target replay.
One presentation model holds sixteen records across disconnect and reconnect.
It preserves request order, latest observation, exact envelopes, and owner-change flags.
Admission retains fourteen ordinary slots and two safety reservations.
Completed records may retire; unresolved outcomes are never silently discarded.
Reconnection waits for old request workers to finish without blocking the UI.
Accepted work shares one replay cadence across ordinary, safety, and shutdown workflows.
The scheduler admits at most one exact replay per 100 ms and rotates across fixed workflow slots.
Permanent refusal stops automatic polling while preserving the retained record.
Uncertain outcomes still require explicit reconciliation.
Explicit safety reconciliation restores its bounded polling slot after admission succeeds.
Historical shutdown reconciliation never restores quit intent.
Raw paths and replies remain local details, not a redacted support bundle.

The widget-tree tests supplement native validation.
Controlled receipt-producer tests exercise the real Unix client and Core receipt serialization.
Their scripted Accepted safety statuses prove consumer scheduling, not Stop during playback.
They do not establish AT-SPI, NSAccessibility, screen-reader sessions, packaging, or dialog support.
The retained toolkit proofs qualify only their documented native controls and environments.

## View API

Create `DesktopClient::attach(runtime_directory)` on the control side.
Attachment retains the path and starts request workers; it does not prove that an owner exists.
Use fresh `RequestId` values for every logical query or mutation.
Use `Request::query` with an existing Core query and explicit zone.
Call `submit` without waiting for IPC.
Drain `poll` from the UI update loop.
Read immutable observations from `model()`.

```rust,no_run
use earph_core::{control::Command, ids::{RequestId, ZoneId}};
use earph_desktop::{DesktopClient, Request};

# fn example() -> Result<(), Box<dyn std::error::Error>> {
let mut client = DesktopClient::attach("/absolute/private/runtime".into()).unwrap();
let read = Request::query(
    RequestId::new("fresh-query-identity")?,
    ZoneId::new("main")?,
    Command::State,
).unwrap();
client.submit(read).unwrap();
client.poll(); // The event loop repeats this; a single poll need not have a reply.
if let Some(observed) = client.model().latest_state() {
    assert!(observed.state.owner_id.is_some());
}
let (_history, shutdown) = client.disconnect();
let cleanup = std::thread::spawn(move || shutdown.join());
// Retain this handle; wait for it only outside the UI loop.
# cleanup.join().unwrap().unwrap();
# Ok(())
# }
```

Before a mutation, obtain a fresh state query result.
Pass that `StateView` and the existing Core command to `Request::mutation`.
The builder captures owner, zone, session, and expected revision unchanged.
A stale receipt remains rejected; neither the adapter nor model retries with repaired targets.
Refresh and reconcile the intended edit before submitting a new mutation identity.

`submit` returns `Submission::Admitted` or a coalesced state-query identity.
Coalescing applies only while another state query is in flight.
Reusing an identity retained by this model fails explicitly.
History eviction does not extend the owner's deduplication guarantee.
Callers remain responsible for fresh identities across client instances and history rotation.

## Pending and uncertain results

`RequestRecord.presentation` distinguishes queued, accepted, applied, rejected, cancelled, failed, owner error, and uncertain states.
`in_flight` reports an active client delivery or reconciliation separately from an asynchronous accepted operation.
`outcome` retains the latest exact owner reply or delivery failure.
`request.bytes()` retains the immutable envelope for the record.

Accepted means pending owner work.
For a profile save, applied means durable saved intent; it does not mean profile activation.
A missing transport reply can follow an applied mutation.
The owner can also return a top-level `DEVICE_UNAVAILABLE` uncertainty error.
Its top-level `INTERNAL_FAULT` can report receipt serialization failure after a command took effect.
These failures never become rejected receipts or fabricated success.

Call `reconcile(request_id)` only for accepted or uncertain records.
It resends the identical bytes, with the same identity, targets, and revision.
It never allocates a replacement mutation identity.
After observing another owner identity, the model marks retained old-target records `owner_changed`.
Those records cannot reconcile through this client.
They remain visible with their actual prior outcome.

Only state-query results update `latest_state()`.
Mutation receipts do not optimistically change observed mute, queue, output, gain, or revision.
Late replayed snapshots cannot replace a later logical observation.
The current snapshot shares immutable storage with its retained receipt.
`received_at` is the local receipt-observation time, not an audio presentation timestamp.

## Bounds and worker ownership

Each lane has one queued request and one worker.
Ordinary and safety lanes have separate queues and threads.
The safety lane covers Stop, Pause, mute-on, caps, and explicit owner shutdown.
That classification selects client scheduling only; the owner still enforces every command and policy.
It does not preempt the owner's serial IPC server or promise a control-latency deadline.

The completion channel holds at most four results.
One `poll` drains at most four results.
The presentation history holds at most sixteen records.
Ordinary records use at most fourteen slots, leaving two slots for safety work.
Unresolved ordinary history cannot consume that safety reservation.
Completed records may retire; accepted, uncertain, and in-flight records are not silently evicted.
If unresolved records fill the history, admission fails explicitly.
Each admitted frame remains within the Engine's shared one-MiB limit.
These count and frame limits do not claim a process-RSS or allocator-overhead bound.

Synchronous `OwnerClient::request` belongs on a worker, never the UI thread.
`disconnect` stops admission and drops queued work.
It returns the retained model plus stopped-worker ownership without waiting.
Move `WorkerShutdown` off the UI thread before `join` or drop.
Directly dropping `DesktopClient`, `Dispatcher`, or `WorkerShutdown` joins for resource safety and can wait for an in-flight bounded IPC call.
Views must use the explicit disconnect path while the UI is responsive.
No release path sends Stop, mute, or owner shutdown.
Discarding a reply never cancels an already-sent operation.

## Projection boundary

The parser requires version-one receipt identities, status, revisions, epochs, and all consumed fields.
A required nullable field must exist even when its value is null.
Unknown capability versions and result kinds fail explicitly.
Snapshot revision, zone, and epochs must agree with its applied receipt.
Malformed or mismatched replies retain their original bytes in `ProjectionError`.

State, queue, capabilities, and volume caps have typed projections.
Playback internals and graph/profile result bodies remain opaque JSON objects in this unit.
Their bytes are retained without inventing transformations, metrics, or application state.
Later views must add tested typed projections before using those opaque fields as controls or qualified observations.
Raw local replies are not redacted support bundles.

## Local qualification

The package belongs to the shared workspace and uses its authoritative root lock.
Build provenance includes the desktop manifest and production source.
The native shell uses the pinned workspace eframe dependency and approved feature set.

Use the installed Rust toolchain directly and run:

```sh
crates/earph-desktop/scripts/check /absolute/new/artifact-directory
```

The checker builds the real standalone owner, then runs formatting, strict Clippy, tests, and owner-process E2E.
The E2E finds the real owner beside the Cargo profile's test directory.
It fails visibly if that binary is absent.
Test owners, local transport proxies, generated WAV bytes, and all runtime files stay retained.
The Connect/Queue scenarios never select output or start a source.
The strict projection E2E uses generated zero PCM and an isolated ALSA file/null configuration.
It observes real muted profile preparation and application without unmute or physical output.
Other projection scenarios exercise actual saved profiles, correction imports, packages, and typed failures.
The decoder requires the integrated owner's complete nullable fields and retains exact raw reply bytes.
Its typed observations keep requested output, prepared bindings, actual backend formats, and unknown values separate.
These API checks do not add output, transport, profile, or signal-path controls to the current window.
Foreign-platform and native-window qualification retain their separate evidence gates.
The proxy only drops or delays real owner traffic; it implements no engine behavior.

The root `scripts/check` builds that owner and includes the desktop package.
Use the package check for a narrower run with separate retained logs.

Playback views preserve exact output-activity counters, their scope, and confirmed-close samples.
Unsupported activity stays explicit null. These SDK diagnostics do not prove OS calls, callbacks, or audible delivery.
See `../../docs/local-playback.md` for the owner-defined measurement boundary.
