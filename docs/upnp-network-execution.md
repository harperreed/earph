# UPnP Network execution

Production startup stays disabled pending the joint owner qualification.
The Network surface binds only when its caller explicitly selects one IPv4 address.
Core owns subscription identity, sequence, snapshots, callback authority, and retirement.
Engine owns the serial owner transaction and supplies fresh authority before each callback attempt.

The device description and HTTP subscription route use one shared service event path.
Each service exposes only its advertised `/upnp/{service}/events` endpoint.
The socket regression downloads the description, parses every eventSubURL, and follows those exact URLs.

`try_recv_incoming` returns SOAP or subscription tickets with the observed socket peer.
Each actual request receives one checked unique ticket number.
The Engine bridge captures its complete Core envelope once.

Subscription responses use `Connection: close`.
A successful `respond` call transfers response bytes to the connection task.
It does not authorize an initial event.
The separate completion channel reports success only after Hyper completes TCP write shutdown.
The Engine then acknowledges the exact binding in Core.

Event delivery uses the listener's owned Tokio runtime.
Each destination attempt requests a fresh Core authority view through a bounded channel.
Cancellation invalidates the exact delivery before its task drops transport resources.
The runtime joins the task before returning its terminal completion.
A failed join retains the exact delivery and its lease.

## Shared capacity

The adapter retains one 8 MiB ledger.
Its resident reservation is 262,144 bytes.
Each HTTP request credit is 2,031,616 bytes.
Event admission reserves ordinary and lifecycle progress credits.
Four sender positions share four 131,072-byte scratch slots.
Each SID owns two allocated final XML slots.
AVTransport slots hold 262,144 bytes each.
RenderingControl slots hold 2,048 bytes each.
ConnectionManager slots hold 1,024 bytes each.

Six maximum AVTransport subscriptions reserve 7,995,392 bytes with the minimum HTTP credits.
A seventh would require 8,519,680 bytes and must fail before Core admission.
Sixteen is the Core record ceiling, rather than an entitlement to sixteen large subscriptions.
Active HTTP reservations can reduce subscription admission further.

Core's writer encodes the final propertyset directly into an allocated event slot.
It does not copy an inner LastChange document or truncate metadata.
SOAP getters use the same Core field table as event snapshots.

Classification stays attached to the connection lease through response completion.
First-event admission also reserves Stop capacity beyond every already admitted ordinary connection.
An unclassified slow connection still has the existing five-second header deadline.
Subscription tickets retain the five-second owner admission deadline.
Their separate response deadline follows the actual connection, capped at fifteen seconds.

## Sender allocation evidence

Each sender position allocates one 81,920-byte buffer inside its 131,072-byte reservation.
The same buffer constructs request headers and receives the bounded response.
Response headers use at most 32,768 bytes.
Response bodies use at most 2,048 bytes.
Chunk framing and trailers share another bounded 32,768 bytes.
URL validation runs before connection construction and releases its parsed URL allocation.
The request writer percent-encodes non-ASCII target bytes directly into its bounded header buffer.
No proxy, DNS resolver, redirect handler, automatic retry, or connection pool exists on this path.

The allocation fixture tracks the runtime thread with a fixed, allocation-free pointer registry.
It includes task creation, authority handshakes, URL parsing, and real pinned TCP delivery.
Its callback URL uses all 8,192 admitted bytes, including a multibyte path.
Its response exercises the header and body limits.
The observed transient peak is 34,560 bytes.
The sender buffer plus transient peak is 116,480 bytes, below the 131,072-byte reservation.
This measurement covers that Linux workload; the explicit allocation bounds govern other inputs.
The four-position fixture separately verifies concurrent cancellation, joining, and complete shared-pool release.

Failed delivery joins retain their exact delivery record and subscription lease.
`SurfaceStats.joined` reports the listener runtime thread's join.
The Engine must also process each delivery completion and its `transport_joined` value.
A listener-thread join does not acknowledge a failed delivery join to Core.

The [combined Linux receipt](evidence/network/renderer-execution-linux-20261121.json) binds this adapter and the explicit Engine bridge.
Named-controller interoperability and default startup remain separate gates.
