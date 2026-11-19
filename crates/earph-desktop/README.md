# Desktop client and presentation model

This package attaches to the existing local owner through `earph-ffi::LocalClient`.
It creates no engine, source, output, or owner process.
Its current scope is the control adapter, worker lanes, projections, and presentation history.
Native views, owner launching, dialogs, and packaging remain later units.

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
No GUI toolkit dependency enters this unit.

Use the installed Rust toolchain directly and run:

```sh
crates/earph-desktop/scripts/check /absolute/new/artifact-directory
```

The checker builds the real standalone owner, then runs formatting, strict Clippy, tests, and owner-process E2E.
The E2E finds the real owner beside the Cargo profile's test directory.
It fails visibly if that binary is absent.
Test owners, local transport proxies, generated WAV bytes, and all runtime files stay retained.
The desktop package scenarios never select output or start a source.
The proxy only drops or delays real owner traffic; it implements no engine behavior.

The root `scripts/check` builds that owner and includes the desktop package.
Use the package check for a narrower run with separate retained logs.
