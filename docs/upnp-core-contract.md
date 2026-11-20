# Renderer execution and event contract

Core owns renderer graph authorization, callbacks, subscriptions, and delivery identity.
The Network sender and Engine bridge consume this contract in separate implementation units.
Production renderer startup remains disabled until their joint qualification passes.
The [Linux receipt](evidence/network/upnp-core-linux-20261120.json) binds the integrated Core tests to their exact source.
It includes the repaired graph-replay case and the completed independent review.

## Playback authorization

Each controller grant requires a nullable `playback` field.
A non-null `RendererPlaybackAuthorization` contains immutable `GraphIntent`, exact `OutputConfiguration`, and `expected_device_epoch`.
Grant admission checks graph storage and structure, processing, current output selection, device epoch, and consent.
A null authorization permits applicable non-Play rights.

Core hashes the authored intent with the `earph.renderer.authored-intent.v1` domain.
This authored identity differs from the Engine's compiled activation hash.
Core compares retained intent content as well as its digest.
The output selection remains a separate exact pin.

Accepted Play captures `RendererGraphBinding` with owner, listener epoch, unique attempt generation, controller generation, and source binding.
It also captures source epochs, authored identity, and immutable local authorization.
`renderer_graph_continuation()` returns that captured binding.
`renderer_graph_binding_valid()` rechecks its complete live authority.

After measured network readiness, call `submit_renderer_graph_with_effects` with that exact binding.
It admits only the pinned GraphPrepare or its exact fresh GraphCommit token and activation hash.
`GraphJobRequest.renderer_application` and `GraphActivationRequest.renderer_application` carry the binding.
Profile application and renderer application remain exclusive.
Preparation completion and activation acknowledgement recheck renderer authority.
The existing five-second preparation deadline and thirty-second token lifetime remain intact.

Stop, revocation, expiry, replacement, and competing local graph work invalidate the continuation.
Late results cannot recreate it.
An explicit later Play may capture another attempt for the same muted source without refetching.
Its unique attempt generation prevents the earlier binding from regaining authority.
Play remains muted; an early refused Unmute leaves no deferred intent.
Scoped SetMute, Pause, and Resume reuse canonical commands and exact active source fences.

## Callback grants

The `events` right permits controller-scoped subscriptions.
Use `renderer.callbacks.replace` and `renderer.callbacks.revoke` through local owner control.
Supply `controller_id`, `expected_controller_generation`, and `expected_generation`.
Replacement also supplies `lifetime_ms` and a separate `RendererCallbackGrantDocument`.
Inspect the registry generation through `renderer_callback_generation()` for local replacement.
Use `renderer_callback_generation_for()` to capture a controller's exact callback generation for protocol ingress.

Each callback origin requires HTTP, an explicit port, and at most eight address pins.
Pins must belong to the controller's locally allowed peers.
The document permits at most eight origins.
Callback URLs may use HTTP's default port; Core preserves every URL whole.
The adapter must apply its shared URL and destination policy before constructing transport.
Callback authority never creates media-fetch authority or a GET permit.

`renderer_controller_for_peer()` rejects absent or ambiguous controller ownership.
The transport supplies the observed peer separately from serialized command content.
Ordinary IPC cannot submit scoped renderer subscription requests.
Callback grants, URLs, SIDs, snapshots, and continuations remain volatile and redacted.

## Subscription admission and response completion

Capture one complete CommandEnvelope for each actual protocol request.
Retain that envelope for internal replay.
Use the scoped `RendererSubscriptionAction` in `renderer.subscription`.
Call `submit_renderer_subscription_with_effects` with observed ingress and an actual adapter lease.
Its `RendererSubscriptionResult` carries the canonical receipt and private subscription response information.

The adapter implements `RendererSubscriptionLease` using its existing shared eight-MiB ledger.
The lease owns two final event slots and retains the adapter's shared admission obligations.
Keep ordinary progress, lifecycle progress, and bounded sender scratch inside that ledger.
Core checks service, declared slot capacities, and unique lease identity.
These checks do not establish concrete sender allocation or concurrency measurements.

Core retains at most sixteen records, including orphan initials and closing deliveries.
Admission reserves one of four delivery positions and a unique initial delivery identity before creating a SID.
It captures the complete initial snapshot within the successful owner transaction.
Later mutations replace one complete latest snapshot without changing initial or in-flight ownership.

A subscription expires at the minimum of requested duration, 1800 seconds, and both authority expiries.
Return TIMEOUT rounded down to whole seconds.
Reject admission when no positive second remains.
Renewal preserves callback URLs, scope, initial ownership, and sequence.
It cannot extend controller or callback authority.
Unknown, expired, and wrong-scope SIDs return the Core precondition failure for wire 412 mapping.

Call `acknowledge_renderer_subscription_response()` only after successful response completion and TCP write shutdown.
The admitted connection supplies its actual deadline, bounded by Core to fifteen seconds.
Failure or deadline expiry retires an unconfirmed subscription.
Successful completion authorizes the reserved initial attempt with a fixed thirty-second deadline.

Unsubscribe removes renewal and latest ownership.
It preserves one undelivered initial attempt under independent live callback authority.
The response-pending phase retains its reserved position until response completion or deadline.
Every orphan retains its record, authority ownership, and adapter lease.

## Delivery and retirement

Call `begin_renderer_delivery()` with the exact subscription binding.
Core permits one in-flight delivery per SID and four globally.
Initial SEQ is zero; later SEQ wraps from u32::MAX to one.
Every failover attempt retains the same delivery, snapshot, sequence, URLs, and absolute deadline.
AVTransport and RenderingControl use a five-second earliest-next-send boundary.
ConnectionManager uses its unmoderated event fields.

Before each connection attempt, obtain `renderer_delivery_authority()` from current Core state.
Invalidate the host's live view before cancelling transport after a committed revocation.
Use `renderer_deliveries_needing_cancellation()` to find exact outstanding deliveries.
Join their tasks before reporting `transport_joined=true` to `complete_renderer_delivery()`.
A failed join retains closing ownership and its reservation.

A NOTIFY 412 retires only the exact SID.
Connect, timeout, malformed-response, overflow, and cancellation outcomes abandon that event.
A live SID remains eligible for future events after ordinary delivery failure.
Core never resends a failed logical event indefinitely.

`retire_renderer_epoch()` invalidates the listener's controller and callback grants before transport cancellation.
It advances the listener epoch and retains exact closing deliveries until their acknowledgements.
A replacement listener requires fresh local grants.
Restart restores no grant, consent, SID, continuation, or callback authority.

## Shared projection and bounds

Use `renderer_projection()` for live SOAP getters.
Use the immutable `RendererEventSnapshot.projection` for delivery bodies.
Both paths use one state-variable field table.
Debug output redacts shared field values as well as their containing snapshots.
Position-only observations do not create LastChange events.
Duration and position remain unknown until Core has the corresponding measurements.
Unsupported counters use the AVTransport i4 maximum sentinel.
No-media duration and unsupported next fields follow the service template.
Transport actions and preset availability derive from current Core facts.
Local media paths and metadata never become renderer descriptors.

`encoded_size()` performs an allocation-free exact pass over the complete propertyset.
`encode_into()` writes both XML layers directly into the reserved final String.
It creates no complete intermediate LastChange document.
Attribute whitespace uses character references so two XML parses preserve opaque metadata.
Invalid XML characters reject staging before mutation.

Stage admission uses that same writer with the largest legal fixed scalar widths.
It counts exact URI and metadata contents in both transport and current-track fields.
Reject oversized staging before changing descriptor, association, or revision, even without subscribers.
The complete encoded AVTransport cap remains 262144 bytes.
RenderingControl reserves 2048 bytes per slot; ConnectionManager reserves 1024 bytes per slot.
Their fixed field tables and scalar bounds fit those slots.

Core extends the existing sixteen-MiB authority ledger.
It deduplicates shared grant documents, graph intent, descriptors, snapshots, receipts, and binding Arcs.
Candidate admission counts old and proposed ownership together.
Each SID reserves two fixed Core snapshot records and one delivery record before later event changes.
Dispatch transfers that reservation to the in-flight delivery.
The adapter remains responsible for its separate existing eight-MiB transport ledger.

The [AVTransport:1 template](https://upnp.org/specs/av/UPnP-av-AVTransport-v1-Service.pdf) defines the time and counter sentinels.
Its sections 2.2.14–2.2.26 also define no-media duration, unsupported next values, and current transport actions.
