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
A socket that fails or ends without a Close reports `Lost`, which is no protocol error.
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

The [pure player timeline](sendspin-timing.md#pure-player-timeline) now provides bounded, generation-bound PCM ownership and lifecycle handling.
The transport advertises that player and delivers its typed events; the Engine's playback worker operates it.
See [Player stream lifecycle](#player-stream-lifecycle) and [Pinned Python player peer](#pinned-python-player-peer).
Timing stays `unqualified`: output-clock mapping, drift correction, and physical synchronization remain unqualified.

The dormant Engine [native services bridge](sendspin-native-services.md) connects explicit credential ownership to these services.
It requires a definitively committed document and explicit activity admission.
It does not start the transport or provide a player.

## Owner control surface

`EngineOwner::with_sendspin` enables the player source for one server.
The launch configuration holds the server URL, one pinned address, the server public key, and the credential store.
IPC never supplies a connection target.
Capabilities list `sendspin` only for an owner launched with this configuration.

`sources.sendspin.start { device_id }` starts a new muted session.
It needs an approved matching output, no existing session, no configured or active pipeline, and a disconnected source.
`sources.sendspin.get {}` reports the phase, the connection generation, the source observation, and the last failure.
State reports the source as `sendspin_playback` with timing qualification `unqualified` and no audible position.

Unmute waits until the server knows that the player is available.
The server moves transport between `buffering` and `playing` inside the session.
It cannot create a session, change epochs, unmute, or change volume.
`sessions.pause` is a safety control and ends the session exactly as `sessions.stop` does.
This source cannot hold a stream, so nothing resumes it; start a new session instead.
Pause keeps its ordinary fences: the owner, the exact session, and the exact expected revision.
Stop alone accepts an older revision for the observed session.
The Host sends the worker its stop before it persists the pause.
Core rejects `sessions.resume` and seeks for this source with `CAPABILITY_UNSUPPORTED`.
Core ignores a source event whose connection generation or epochs are stale.
A source that closes with an error fails the session with that error code.
A clean close, a stop, or a pause clears the session.
A lost server is the exception once the server knew this player: the session stays, and the Host connects again.

## Player stream lifecycle

The playback worker anchors a chunk to its own server timestamp at each stream start.
It anchors again after a full clear, an output underrun, idle silence, and a timestamp gap longer than 100 ms.
A shorter gap becomes silence and keeps the anchor.

While a stream is active and no audio is due, the worker feeds the output idle silence.
Idle silence starts the output at the stream start, so the device runs before the first anchor.
A chunk takes an anchor only on a running output.
A device that still starts would delay every frame behind that anchor.
Idle silence then keeps the output from running dry through an anchor wait, a server gap, and starvation.
It keeps three periods in the output, and at least 30 ms, never more than the buffer once the output runs.
While a Core Audio output still starts, silence can pile up to about two buffers, and audio due inside that backlog drops once as late.
The worker estimates the held frames from the accepted frames and its monotonic clock.
A running output's own report corrects that estimate; an output without a clock reports zero and corrects nothing.
So a clockless output gets idle silence no faster than real time.
Idle silence ends before a waiting chunk's target, so it makes no chunk late.
During contiguous playback it starts only below two thirds of that depth.
It then ends contiguous playback, and the next chunk anchors again.
Idle silence bypasses the gain, so a gain ramp still starts with the first scheduled frame.

The scheduler counts the frames that the output still holds as its delay.
ALSA reports that delay. For Core Audio the worker counts the accepted frames past the restart cursor.
That cursor is callback delivery or the queue clock; it is no audible position.

On Core Audio the scheduler also adds the latency that the output device reports.
The worker reads that latency when the output opens, and again after each reopen.
These reads start no IO and change no setting.
The latency is the sum of three properties: `kAudioDevicePropertyLatency` and `kAudioDevicePropertySafetyOffset` in the output scope, and `kAudioStreamPropertyLatency` of the first output stream.
The device counts them in frames at its nominal rate, and the worker converts them to stream frames.
The AudioQueue route adds no IO buffer. Its cursor is the queue clock, and a frame in the device IO buffer still lies past that clock.
The direct hardware route adds one device IO buffer, because a delivered frame leaves its count and still waits in that buffer.
The scheduler writes each chunk earlier by this latency.
The idle silence depth ignores it, because the latency lies behind the frames that the output holds.

`timing.device_latency_frames` shows the reported latency in stream frames.
An unreadable property counts as zero. With no readable property the value is `null`, and the scheduler adds nothing.
`timing.device_latency_applied` says whether the scheduler adds the latency.
The scheduler refuses a report above one second: state shows the frames, and `device_latency_applied` stays `false`.
While the latency applies, `timing.backend_delay_basis` names it: `audio_queue_sample_time_plus_reported_device_latency_not_audible_position`.
The direct hardware route uses `core_audio_ioproc_delivery_plus_reported_device_latency_not_audible_position`.
The player declares a required lead time and a minimum buffer that both grow by the applied latency.
A server holds a live source at the minimum buffer, so the lead time alone would leave such a source late.
The latency is a device report, not a measurement. Timing stays `unqualified`, and state claims no audible position.
ALSA reports no device latency through this path, so Linux scheduling is unchanged.
Local file playback reads the same properties when it opens a Core Audio output and uses none of them.

The player declares a required lead time of 150 ms plus the applied latency, rounded up to a whole millisecond.
The declaration uses the latency of the first open.
If the server sends a chunk later than that lead, the chunk arrives inside the device latency and drops as late.
With a 200 ms latency and a 150 ms server lead, every chunk drops.

A Mac listening test on November 22, 2026 met its own schedule within 1.5 ms on built-in speakers.
The owner still heard Earph behind another synchronized player, because the schedule ended at the queue clock.
No Mac has run the device latency code.
Unverified on a Mac: the property reads, the choice to add the safety offset and no IO buffer on the AudioQueue route, and a server that honors the larger lead time.

A `stream/clear` without `from_timestamp` drops the queued audio and the audio that the output still holds.
A `stream/clear` with `from_timestamp` drops only the queued audio from that server time on.
Audio that already left for the output stays.
The timeline trims replacement audio that overlaps it, and the anchor stays.
A full clear stops and prepares an output that holds scheduled audio; idle silence then starts it again.
A stream end or a clear closes a Core Audio output without pausing it, because a starting output has no restart position.
An output that holds only idle silence keeps running through a full clear.
A `stream/end` and a lost player role stop the stream without a drain.
Both stop and release the output at once. Nothing feeds the output between streams.
The next `stream/start` starts it again, so audio due before the device runs is dropped as late.
Each new activation gets a new available player state.

Idle silence keeps a running output fed, so only a stalled worker starves it.
A starved ALSA output underruns.
After an underrun the worker prepares the same ALSA stream again and anchors the unwritten audio again.
It reopens no device, and mute and volume stay unchanged.
A second failure before the recovered output accepts a frame ends the source with `OUTPUT_FAULT`.
Every other output failure ends the source, and Core Audio has no underrun recovery.
Audio beyond the timeline budget ends the source with `RESOURCE_BUDGET_EXCEEDED`.

`timing.anchors` counts the anchors and `timing.underruns` counts the recovered underruns since the source started.
`timing.idle_silence_frames` counts the idle silence that the output accepted.
It grows with waiting time, so it stays apart from `timing.inserted_silence_frames`.
That counter holds only the scheduler's bounded silence: the anchor pads and the short gaps.
`submitted_frames` includes both.
`timing.dropped_frames` counts received frames that were due before the player could write them.

`timing.first_lead_us` shows the head start of the first chunk after the latest stream start, clear, underrun, long gap, or idle silence.
It is that chunk's local target time minus the time the player first saw it.
It subtracts no output delay, so it shows what the server gave.
`timing.min_first_lead_us` keeps the smallest first lead since the source started.
`timing.first_wait_us` shows the time from that first sight to the first scheduling decision.
The player decides only on a running output, so this wait includes the output's start time.
`timing.max_first_wait_us` keeps the longest first wait since the source started.
The player drops audio at a start when the first lead is shorter than the first wait plus the backend delay.
A short first lead points at the server. A long first wait points at the output.
No test produced a hardware underrun, because no ALSA plugin without hardware underruns.
The worker tests put a clock in front of the file capture: a start time, a 48 kHz drain, and a final failure when dry.
The worker tests fail one write at the output interface and then prepare the real file-capture stream again.
The ALSA adapter test hands its failure path the `EPIPE` error number of a native underrun.

The Host runtime loads credentials, starts Network, and gives the handle to the playback worker.
Each owner loop iteration polls it, and no step blocks.
After a stop, a pause, a close, or an owner shutdown it joins Network first and then the credential supervisor.
The phase stays `closing` until both joined; a new start waits for `disconnected`.
The joins do not wait for the playback worker to close its output.

A credential operation can end with an unknown outcome.
The supervisor reports one when a native operation outlives its timeout, including a load.
It also reports one for a lost completion or a failed supervisor thread.
Network reports one when it joins with an unresolved commit.
After any of these, every later start in that owner process fails with `AUTH_DENIED`.
`sources.sendspin.get` then reports `credential_recovery_required: true`.
Only a restarted owner clears that fact; the credential store keeps its own pending marker.
The permit and the admission policy last 12 hours. An expired permit ends the session and does not reconnect.

### Reconnect

The Host runtime owns the reconnect. Core, the playback worker, and Network each still handle one connection.
The runtime retries only after the server knew this player in the current session.
It retries these transport ends: `PeerClosed`, `Lost`, `Connect`, `Upgrade`, and `Deadline`.
It never retries `Authentication`, `Admission`, `Expired`, `Protocol`, `Clock`, a cancel, or a budget, service, or storage failure.
It never retries a source that closed for its own reason, such as an output fault.
A stop, a pause, and an owner shutdown release the session, so they end a pending retry at once.

A lost connection closes the worker's source through its ordinary close path, which stops the output.
The runtime turns that close into the phase `reconnecting`, and Core keeps the session, its epochs, and its mute state.
The transport shows `buffering`. The runtime joins Network and the credential supervisor as it does for a stop.
Then it waits: 250 ms at first, doubled after each failed attempt, and 10 seconds at most. The wait has no random part.
Only a connection that the server kept for 30 seconds starts the wait at 250 ms again.
A server that accepts the player and drops it at once therefore sees the same growing wait.
Each attempt spawns a new supervisor, loads the credentials again, and takes a new permit and a new admission policy.
The worker gets a new source, so the handshake, the available declaration, the clock, the timeline, and the anchor are all new.
The phase stays `reconnecting` until the server knows this player again.
`reconnects` in the source observation counts the restored connections of the session.
The worker's source reports zero, and the runtime writes the count into each observation that it forwards.
`submitted_frames` and the `timing` facts count from the latest connection.
A worker that reports no close within two seconds of a healable transport end starts the same reconnect.
A close that such a worker reports later is ignored: it left the observation of the running attempt open.
An attempt whose credential load fails ends the session with that failure.
An attempt that leaves an uncertain credential outcome ends the session with `AUTH_DENIED`.

Each attempt opens the output before the socket connects, as a first start does.
While the server stays away, the output therefore opens and closes once for each attempt.
A dead network without a reset is found only by the transport's ping, after up to 40 seconds.

## Pinned Python player peer

`crates/earph-engine/tests/support/sendspin_player_server.py` is a second producer for the player role.
It uses the pinned aiosendspin 10.2.1 Noise driver in strict mode and the pinned message models.
Those models parse the player's `client/hello` and `client/state`.
They also build `server/activate`, `stream/start`, the audio frames, `stream/clear`, and `stream/end`.
The peer stamps each chunk on its own monotonic clock and answers the clock requests from that clock.
It checks the source hashes of twelve pinned modules before it listens.

The case `play_python` in the `sendspin_cli` test target runs the real `earph sendspin` command against that peer.
The target has no harness, so the case runs only by name, as `pair_python` does.
Set `EARPH_SENDSPIN_PEER` to the pinned Python launcher, then run:

```
cargo test --locked -p earph-engine --test sendspin_cli -- play_python
```

The default test run skips the case, so a machine without the pinned peer runs nothing of it.
The case checks these outcomes:

- The strict handshake accepts the player on a stored long-term record, and the hello offers `player@v1` with 48 kHz stereo 16-bit PCM.
- `client/state` carries `available: true` beside the player object.
  The player object declares an output delay of 0 ms, a required lead time of 150 ms, a minimum buffer of 200 ms, and no commands.
- Audio stamped 2 seconds ahead and then cleared never reaches the capture and takes no anchor.
- The audio after the clear reaches the ALSA file capture complete and in order, with no dropped frame.
- A `stream/end` returns the transport to `buffering`. A second stream on the same connection anchors once and plays complete.
- `earph status --json` shows `unqualified`, `correction: "none"`, one anchor, and a first lead close to the 300 ms that the peer gave.

`client/state` carries `available` at the top level, as the pinned specification requires.
Before this test the player sent it inside the `player` object.
The pinned server tolerates that by default and treats the player as available, so the Music Assistant runs did not show it.

The unqualified boundary of this test:

- The pinned environment lacks the `server` extra of aiosendspin: PyAV, NumPy, and Pillow.
  `aiosendspin.server` cannot import there, so the test runs no `SendspinServer`, no `PushStream`, and no group.
  The peer's send-ahead, pacing, and clear timing are test code, not the pinned server's policy.
- The peer sends PCM only. It resamples nothing and sends no player command.
- The capture is an ALSA file device without a clock. It proves content and order, not timing.
  The start error on that device is about -28 ms with either test server, so the case asserts no error size.
- The case proves no synchronization accuracy. Timing stays `unqualified`.
- No Mac has run the case. It needs Linux and ALSA.
