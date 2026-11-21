# Outbound Sendspin transport

`earph_network::sendspin` owns one explicitly started, roleless outbound connection.
It drives the existing `ClientSession` through an unsplit WebSocket.
It creates one current-thread Tokio runtime and one operating-system thread.
It adds no application startup path, listener, discovery, source, or output owner.

Call `SendspinConnectPermit::new` with an opaque owner, nonzero connection and grant generations, expiry, endpoint, and pins.
The endpoint must use explicit `ws`, a nonzero port, and an absolute path.
The conservative syntax rejects userinfo, queries, fragments, whitespace, controls, backslashes, and percent escapes.
Pins use the unchanged Network address predicate.
Literal endpoint addresses must match every supplied pin.
Hostnames never invoke DNS.
The client opens pinned addresses sequentially within one absolute connect deadline.
`wss` returns `TlsUnsupported`; it never downgrades.
No ambient proxy, cookie, Origin, authentication header, redirect, or TLS root enters this transport.
Mandatory Noise authentication remains inside `ClientSession`.

Construct `SessionConfig` with injected credentials and an authoritative catalog.
Pass one explicit `ClockConfiguration` with a caller-owned `MonotonicDomain` and `TimePolicy`.
The domain uses a private monotonic origin and a nonzero generation.
Clones sample that same domain.
Choose a fresh generation when replacing the clock domain.
Transport admission grants no clock or output authority.
Set `supported_roles` to an empty array.
Role-specific `_support` fields are rejected, including player format offers.
Use `roleless_session_limits()` or tighter values.
The catalog must have capacity five and at most five records and superseded identifiers.
This bound places its canonical encoded payload below 65536 bytes.
The driver retains the session's authoritative catalog and never maintains a second copy.
Protected credentials remain subject to the existing catalog planner.
`start` compares the supplied permit with the caller's exact current permit.
It also fences the session's connection generation.
Retire the existing owner before replacement.

Implement `SendspinServices` for admission, candidate generation, and catalog commit.
Each method must return promptly with an owned `Operation`.
Polling that operation must never block or prompt.
Service methods and futures must not panic.
Dropping admission or candidate work must cancel that work and release its resources.
Keep synchronous keyring calls outside this runtime.
Acknowledge commit success only after the caller's required durable write and fresh readback.
Return `Unchanged` only when the previous document definitely remains unchanged.
Return `Uncertain` when the exact storage outcome needs recovery.
Never retry an uncertain commit inside the service.
An uncertain result blocks another `start` on that worker after retirement.
Call `acknowledge_recovery` only after fresh caller-owned storage recovery resolves the prior document.
The prior observer keeps its uncertainty visible after that acknowledgment.
The driver applies each result with its captured original session ID.
It stores at most one operation of each kind without a completion queue.
These three owned slots remain below the four-completion ceiling.

Pairing and empty activities still require affirmative admission.
The driver rejects every active role and every activity except pairing.
Application sends are limited to explicit core JSON `client/goodbye` traffic.
The dedicated clock driver owns every `client/time`; generic producer APIs reject caller-stamped probes.
The caller must validate each core payload before submitting it.
Rejected sends return their original producer message.
Rotation suppression returns producers through bounded `Unsent` event leases.
No internal queue retains an unbounded rejected-producer list.

`SendspinHandle` controls commands and cancellation.
Dropping the handle requests cancellation.
`SendspinObserver` only reads diagnostics and drains events.
Dropping an observer grants or revokes no authority.
An `EventLease` retains byte and entry credits until it drops.
Holding a consumer lease therefore still consumes queue capacity.
Producer admission reserves byte and entry credits before any JSON parsing.
Pending enqueues share those credits with queued commands.
Dequeued commands keep their credits through session validation and ownership transfer.
Parsing holds no state lock, so cancellation and commit completion can proceed.
Concurrent decoded producer payloads share an outer allowance bounded by the aggregate producer limit.
Invalid or cancelled input releases its reservation and returns the original producer.
Cancellation and acknowledged commit completion bypass ordinary queue capacity.
Queue exhaustion fails closed without cancelling acknowledged durable work.

The transport retains each `WriteId` through complete WebSocket flush.
Successful buffering alone never acknowledges a session write.
A local complete flush proves no remote delivery or processing.
Observed close, partial failure, expiry, or cancellation poisons any outstanding write as uncertain.
For a valid peer Close, the original socket drains its existing buffer and queued Close reply within `close_ms`.
Draining an already-issued frame creates no Complete receipt for the poisoned session write.
Successful reply flush reports `PeerClosed`; an expired flush reports `Deadline`.
If the transport stays unwritable, the deadline ends the reply attempt and the socket closes.
The transport never re-encrypts, retransmits, or resumes that write on another socket.
RFC fragmentation and Ping/Pong remain below the complete-message session boundary.
The worker samples checked monotonic microseconds when WebSocket delivery returns a complete message.
Its single `ReceiveStamp` survives ciphertext queues, plaintext queues, and final-fragment reassembly.
The session derives milliseconds from that stamp for its existing deadlines.
It never reconstructs microseconds from truncated milliseconds.

The existing Network thread also owns one roleless `TimeSync` driver.
A probe stays an opaque, bounded offer while earlier output or socket readiness blocks it.
After socket readiness, the worker samples T1 immediately before serialization and encryption.
Only accepted `start_send` records an emitted probe.
Probe writes use existing control ownership and leave the ordinary producer slot available.
Probe fragments finish before a queued producer starts.
The original `WriteId` still waits for complete flush and retains uncertain-write shutdown.
Authenticated `server/time` replies feed the clock before ordinary event publication or timeout polling.
The worker consumes these protocol replies internally, without charging consumer event credits.
Integer, echo, binding, and domain checks reject unauthorized samples without extending a deadline.
Suppression or blocked ordered session delivery abandons the incomplete burst before later clock advancement.
A released held reply retains its original stamp and cannot update an abandoned probe.
Re-handshake preserves the established filter; activation permits fresh probes.
A fresh connection, server binding, or domain creates a fresh filter.
The existing reducer owns every numeric, burst, timeout, age, and cadence policy.

Transport and service deadlines use absolute monotonic instants.
Control traffic never extends a partially flushed write or a handshake exchange.
Admission and candidate timeout cancel their owned operation and close the socket.
Commit timeout closes the socket while retaining its acknowledged operation.
`AwaitCommit` remains observable until the exact callback reaches the original closed session.
`retire` performs one bounded wait and returns `CleanupTimeout` while retaining its thread handle.
A later `retire` joins that same owner.
`Drop` requests cancellation and joins owned work; only explicit retirement promises a bounded wait.
A caller service that never resolves its acknowledged commit therefore prevents successful retirement.

`Observation` exposes redacted phases, generations, deadlines, queue usage, operation ownership, terminal reason, and join state.
The reducer owns its phase deadline and exposes no getter.
`session_deadline_ms` explicitly remains unavailable.
Root accepted this diagnostic limitation without changing reducer deadline enforcement.
Transport and service deadlines report only the actual deadlines this module owns.
Message counters and `last_arrival` expose the single checked microsecond delivery stamp.
Receive stamps and mapping facts use the supplied clock domain.
Transport and service deadline diagnostics retain their existing worker-relative millisecond origin.
`Observation.clock` copies the bound mapping snapshot, revision, age, status, and diagnostics.
Debug output omits mapping estimates and authenticated identity bytes.
A mapped snapshot grants neither player availability nor output readiness.
Errors and Debug output omit URLs, identities, pins, credentials, tokens, and raw messages.

The default ceilings are 8192 endpoint bytes, eight pins, and one socket.
Connect and upgrade each allow 3000 ms.
Complete writes and caller services each allow 5000 ms.
Cleartext and WebSocket messages allow 100000 bytes; encrypted messages allow 65535 bytes.
Frame payloads allow 100000 bytes.
WebSocket read, write target, and maximum write buffers are 4096, zero, and 131072 bytes.
Session input, output, and reassembled bodies each allow 1048576 bytes.
Session metadata allows 64 pending messages and 4096 fragments.
Exchange and assembly deadlines each allow 30000 ms.
Ordinary commands allow 16 entries; events allow 32 outstanding entries, including leases.
Producer and event bytes each share a 1048576-byte aggregate ceiling.
The conservative outer payload allowance is 8388608 bytes.
This bound describes owned payload capacity, rather than allocator RSS or kernel buffers.
Ping, Pong, close, and retirement ceilings are 30000, 10000, 1000, and 1000 ms.
Callers may tighten these ceilings; construction rejects zero or excessive allowances.
The explicit zero WebSocket write target is intentional.

The selected WebSocket dependency is exact `tokio-tungstenite 0.30.0` with only `handshake`.
It uses exact Tokio 1.54.1 and futures-util 0.3.34.
Only futures `sink` and `std` are requested directly.
The existing workspace already enables its other shared futures features.
The exact locked closure and actual license hashes appear in `docs/dependency-lock.json`.
`THIRD_PARTY_NOTICES.txt` retains their actual license texts.
MIT terms permit this use while preserving their notices.
No upstream implementation was copied into Earph.
The pinned Python peer remains a separately installed Apache-2.0 test tool.
Its distribution must preserve its own license and any applicable notices.

Run focused tests with `cargo test --locked -p earph-network` and `cargo test --locked -p earph-sendspin`.
Set `EARPH_SENDSPIN_PEER` to the pinned existing interpreter launcher for the ignored `sendspin_python` test.
Set `EARPH_PAIRING_FIXTURE_ROOT` to private retained storage before that test.
The peer verifies the installed Noise, wire, handshake, and pairing module hashes.
It also checks aiohttp source hashes against the exact wheel pinned by the existing lockfile.
The fixture uses an admitted private interface and ephemeral ports.
Its filesystem store contains synthetic credentials and is development-only.
Each scenario restarts a separate process to read and verify its committed catalog.
Do not use it as a production persistence fallback.

Full player support, Music Assistant interoperability, output-clock mapping, and physical synchronization remain unqualified.

The dormant Engine [native services bridge](sendspin-native-services.md) connects explicit credential ownership to these services.
It requires a definitively committed document and explicit activity admission.
It does not start the transport or provide a player.
