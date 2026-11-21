# UPnP renderer integration

Doctor Biz authorized the complete build and parallel implementation.
Keep renderer startup disabled until its execution and event contracts pass qualification.
The [wire surface](../../upnp-protocol-surface.md) and [Core ownership](../../renderer-control.md) already exist.
Their receipts describe their exact qualification limits.

## Shared state

Keep one Core Session, one playback worker, and one output owner.
Keep the local queue separate from the staged renderer descriptor.
Route every mutation through the existing owner transaction and replay ledger.
Never derive authority from a caller-supplied peer address.
The transport supplies the observed peer separately.

Controller grants and media-fetch grants remain separate.
Callback grants must also remain separate from media-fetch grants.
IP grants restrict LAN ingress; they do not establish cryptographic controller identities.
The renderer does not expose general administration.

Use the existing 16 MiB Core authority ledger.
Use the existing 8 MiB adapter reservation ledger.
Charge shared immutable allocations once across retained owners and transactions.
Preserve closing ownership until the exact worker acknowledgement arrives.

## Current units

U1 defines controller grants, staged associations, and active and closing source ownership.
Fourteen worker scenarios cover queued cancellation and cancellation during real HTTP preparation.
They do not establish cancellation after the replacement source unmutes.

U2 adds exact software zero and atomic FactoryDefaults through the common gain path.
U2 is integrated and locally qualified; production renderer ingress remains disabled.
Zero remains independent of mute and the finite dB setting.
It changes only the unaccepted suffix of cached native periods.
Positive finite control clears zero without unmuting or regenerating cached graph output.
The accepted wire range is Volume 0..30700 and VolumeDB -32767..0.
The minimum VolumeDB represents exact zero.
Both wire conversions use Core's shared grid and quieter ties.
The earlier 0..100 proposal is superseded before renderer startup ships.
New rendering mutations require the explicit rendering-control right.
Existing transport grants do not acquire that right.

U3 supplies bounded descriptions, SOAP, HTTP, and SSDP.
Its listeners do not execute Core commands or grant output authority.
Production startup remains disabled.
The Linux constructor restricts multicast to the selected address.
Mac acquisition remains unsupported until its address/index contract and native isolation pass.

## U4 execution and events

Implement bounded callback authority before enabling subscription endpoints.
Core owns subscriptions, expiry, sequence, and controller scope.
The adapter owns delivery mechanics and joins its transport tasks.
Use at most sixteen subscriptions and four active deliveries.
Retain each initial event before coalescing later complete snapshots.
Initial SEQ is zero; later sequence wraps from u32::MAX to one.
Renewal never extends controller or callback authority.
Recheck destination pins, authority, and expiry before each NOTIFY connection.
Do not create a media GET permit for callbacks.
Reject redirects, proxies, automatic DNS fallback, and disallowed destinations.
Retire a subscription on an invalid-SID 412 response.
Preserve bounded initial delivery after unsubscribe only while its independent authority remains valid.

Derive encoded event limits before accepting renderer mutations.
URI and metadata can expand through two XML escaping layers.
An admitted mutation must not lose a required event because encoding later exceeds its budget.
Keep callback payloads inside the shared adapter reservation.
Keep raw URLs and metadata out of general queries, checkpoints, diagnostics, and Debug.

Pin the locally authorized graph intent, processing request, and selected output in controller authority.
Do not inherit a graph selected after remote Play.
Do not reuse a short-lived preparation token as a controller grant.
After measured readiness, create a fresh preparation for that exact source and authorized intent.
Recheck controller, source, output, device, graph, and media authority before activation.
Revocation cancels the continuation; a late result cannot restore its authority.
Play never unmutes.
An early refused Unmute does not queue a future Unmute.

Capture each SOAP request's Core envelope once.
Retain that exact envelope for internal replay.
Treat separate SOAP requests as separate requests.
Return success only from the actual scoped owner result.
Project transport, volume, mute, connection state, and events from the same Core facts.
Preserve unknown duration and position instead of inventing measurements.

## U5 qualification

Exercise the real owner, CLI, SOAP, events, generated FLAC, and captured software output together.
Cover explicit Unmute, callbacks before Play and after Stop, revocation, expiry, restart, and pressure.
Verify no second output owner or replacement authority survives stale callbacks.
Run actual Music Assistant and a second controller without patching either controller.
Check advertised volume normalization against their real runtime behavior.
Keep native Mac discovery, Swift, physical output, and release qualification explicit and separate.
Run the canonical check and one independent review before enabling the renderer.

## U4a Core contract

The [Core execution and event contract](../../upnp-core-contract.md) defines U4a types and consumer boundaries.
It pins authored graph intent, owns separate callback grants, and retains response-gated initial/latest deliveries.
The shared writer enforces the encoded event cap before renderer staging commits.
U4a is integrated, independently reviewed, and qualified by the [Linux receipt](../../evidence/network/upnp-core-linux-20261120.json).
The review fixed exact accepted graph replay after preparation state changes, while preserving live authority checks.
U4b implements the actual adapter ledger, response-FIN completion, bounded sender, and callback permit.
U4c implements fresh measured graph preparation through the existing owner loop and common output worker.
Both units passed their scoped software checks and independent review.
The replacement regression also corrected Core's waiting-authority lifetime without relaxing execution readiness.
The [combined Linux receipt](../../evidence/network/renderer-execution-linux-20261121.json) qualifies the merged software boundary.
Joint U5 remains pending.
Production startup remains disabled through those units and joint U5 qualification.
