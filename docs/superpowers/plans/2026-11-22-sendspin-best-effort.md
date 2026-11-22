<!-- ABOUTME: Plans the first Sendspin player slice that plays audio through the Engine with best-effort timing. -->
<!-- ABOUTME: Records the product decision, the ordered units, their done-criteria, and what real Music Assistant use still needs. -->

# Best-effort Sendspin player

## Decision

Doctor Biz chose best-effort Sendspin playback on November 22, 2026.
The Engine plays received Sendspin audio now and labels its timing `unqualified`.
An earlier private design refused all timestamped audio until an output passed physical timing qualification.
No calibrated capture setup exists, so that design could never play.
That earlier work stays untouched in its own worktree and is reference material only.
Doctor Biz builds a separate synthetic capture tool; it later turns `unqualified` into a measured number.

This slice claims no SRC-02, SRC-03, AC-16, or AC-17 completion.

## Success criteria

1. A Linux process test sends a known PCM stream from a test Sendspin server to a real Engine.
   The Engine plays it into an ALSA file-capture device.
   The captured samples equal the sent samples: complete, in order, with no gap or repeat.
   The stream ends cleanly and the Engine shuts down cleanly.
2. Engine state reports the Sendspin source, its transport, and an explicit `unqualified` timing fact.
   It reports no fabricated audible position.

## Architecture

```
earph sendspin (CLI, hosts the owner)
  -> sources.sendspin.start (IPC) -> Core session: source=Sendspin, muted, Preparing
  -> Host Sendspin runtime: CredentialSupervisor.load -> SendspinWorker::start_player
  -> Action::StartSendspin -> playback worker thread
       ActiveSource::Sendspin opens the output (48 kHz, stereo, signed 16-bit) and owns PlayerTimeline
       loop: drain EventLease -> timeline.apply -> schedule -> GainRamp -> Output::write
       declares try_player_state(available=true) after the output opens
  -> observation and events -> Host -> Core -> state.get
```

The Network thread stays unchanged: socket, Noise, clock filter, typed player events.
The playback worker is the only output owner, the only event consumer, and the only timeline owner.
It holds the `SendspinHandle`, so dropping the source cancels the connection.
The Host owns credentials, `start_player`, and retirement order: Network first, then the supervisor.
`earph-sendspin` and `earph-network` need no change.

Use a new `ActiveSource::Sendspin` variant, following `ActiveSource::Dsd`.
Do not add a `Reader` variant: `Playback` assumes a queue entry, a streaming float graph, and end-of-file drain.
A Sendspin `stream/end` must stop and clear; it must not drain a graph tail.

## Scheduling policy

Anchor at stream start, after a clear, and after an underrun.
Map the head chunk's server timestamp to local time with `UsableMapping::port_target`.
Compute `lead = target - (now + reported_delay)`.
If the lead exceeds 5 ms, wait. If it is positive and shorter, prepend that much silence.
If the chunk is late, drop whole chunks first, then frames.
After the anchor, write contiguously and record `error = (now + delay) - target` at each chunk.
This slice applies no drift correction. State reports the measured error and `correction: "none"`.

Call `advance_server_time` every tick with a non-decreasing value.
Never block longer than `Output::wait`; the event queue has no backpressure.
ALSA reports a client delay estimate. Core Audio reports none; treat it as zero and label it unknown.

## Units

| Unit | Goal | Estimated size (source / test lines) | Depends on |
|---|---|---|---|
| U1 | Samples reach ALSA through the real worker and a real socket | 500 / 550 | none |
| U2 | Anchor and error measurement | 250 / 300 | U1 |
| U3 | Core command, Host runtime, real credential bridge | 900 / 700 | U1 |
| U4 | `earph sendspin` and the success-criterion process test | 260 / 300 | U2, U3 |
| U5 | Clears, re-activation, starvation | 300 / 400 | U2 |
| U6 | Pinned Python peer and documents | 0 / 250 | U4 |
| U7 | Credential provisioning and pairing, for real Music Assistant use | 870 / 1130 | U4 |

### U1: thinnest sample path

No Core command and no credential store.
Add `crates/earph-engine/src/worker/sendspin.rs`, `Action::StartSendspin`, `Action::StopSendspin`, `Event::Sendspin`, and `ActiveSource::Sendspin`.
Add observation and event types in `crates/earph-core/src/sendspin_source.rs`.
Add a test peer: a blocking WebSocket server around the existing Snow session fixture.
`start_player` has never run over a real socket; this unit proves it first.

Write contiguously through `GainRamp`. Suspend the output on `stream/end`. No anchoring yet.

Done when the capture equals the sent bytes exactly at 0 dB.
After `stream/end` the source awaits another stream.
After peer close and `StopSendspin` the Network worker has joined and the playback worker is idle.

### U2: scheduler

Add a pure `sendspin_schedule.rs` with wait, pad, drop, and write decisions plus error statistics.
Unit-test hand-computed cases: early wait, pad, late drop across a chunk boundary, delay counted once, a mapping revision change, the monotonic clamp, and a gap filled with silence.
Done when the U1 capture stays exact with chunks stamped 300 ms ahead and state shows a measured start error.

### U3: control surface

Add `sources.sendspin.start { device_id }` and `sources.sendspin.get`.
Start follows the `sessions.start` rules: output selected, approved, matching `device_id`, no other session, no active graph.
The connection target comes from launch configuration (`EngineOwner::with_sendspin`), never from IPC.
Add `SourceKind::Sendspin` and a `sendspin_playback` observation.
The session starts muted. Unmute waits for Prepared. The volume cap applies.
The server may move transport between Buffering and Playing inside the session.
The server cannot create a session, take over, unmute, or change volume.
Done when a start over IPC plays to the capture and a wrong server key gets no audio.

### U4: command line

Add `earph sendspin --server ws://IP:PORT/PATH --server-key KEY --output NAME`.
The status line shows `sync unqualified`.
Done when both success criteria pass in the default test run.

### U5: lifecycle

Handle full and partial clear, re-declaration after re-activation, silence fill and re-anchor on starvation, and budget overflow as a reported source failure.
Until this unit, an ALSA underrun ends the source and revokes output approval.

### U6: independent producer and documents

Add an ignored test with the pinned Python peer. Update the Sendspin documents with the unqualified boundary.

### U7: real Music Assistant use

A real Music Assistant server cannot drive this player before this unit.
The missing parts are:

1. Provisioning. Done in U7a.
2. A pairing flow. Done in U7b and U7c.
3. Pairing and playback cannot share one connection today. U7c and U7d use two connections.
4. Discovery and a listener. The user must supply the server address and a pinned IP.
5. Reconnect. Permit and policy expiry end the session.
6. Player commands. The player advertises none, so server volume and mute do nothing.
7. Formats. Only 48 kHz stereo 16-bit PCM is offered.

Root does not connect to, pair with, or change that server; Doctor Biz runs those steps from written instructions.

Read-only research on November 22, 2026 found no protocol blocker.
It read the pinned spec, aiosendspin 10.2.1, and Music Assistant 2.10.12 source. It ran nothing.

- Music Assistant stable 2.10.12 and 2.10.13 bundle aiosendspin 10.2.1, the peer that Earph pins.
  Beta 2.11 bundles 10.3.0, which Earph has never tested. Test against stable.
- The server listens on port 8927 at path `/sendspin`, so Earph's outbound connection fits.
  U7 needs no listener, no mDNS, and no change to `earph-sendspin` or `earph-network`.
- The player generates the pairing token. The operator pastes it into the server's player setup.
  The source offers that paste step when the player offers only a pairing secret, as Earph does.
  The token never expires; treat it as a password.
- Volume and mute commands are optional. A player that lists only 48 kHz stereo 16-bit PCM receives that format.
- The server address must pass `permitted_address`. A 100.64.0.0/10 address is refused, and so is `wss`.
  Use the server's local network address.

Unverified until run: the player role against a real 10.2.1 server, the setup location in the web interface,
and a roleless pairing followed by a player connection under the same identity.

| Unit | Work | Estimated size (source / test lines) |
|---|---|---|
| U7a | Landed. `earph sendspin init` creates the identity and pairing secret only after the store reports a definite `Missing`, then confirms the load through `CredentialSupervisor`. An existing, locked, unavailable, ambiguous, unreadable, or uncertain store refuses and receives no write. | 180 / 220 |
| U7b | Landed. `earph sendspin token` prints the version-zero token on standard output only, with one warning line on standard error. | 60 / 90 |
| U7c | Landed. `earph sendspin pair --server URL [--timeout SECONDS]` runs one roleless pairing connection with a 10-minute window. It succeeds when the record is stored, read back, and named by the paired identity of the server's next handshake; then it sends `client/goodbye` with reason `restart` and prints the server key. A timeout, a lost acknowledgement, and Control-C leave the store unchanged. An unconfirmed write reports that recovery is required. The pinned Python pairing server passes when run by name with `EARPH_SENDSPIN_PEER`. | 350 / 450 |
| U7d | Landed. `earph sendspin --server URL` without `--server-key` uses the one paired server. No paired server says to pair first; several ask for `--server-key` and list nothing. | 80 / 120 |
| U7e | Reconnect with backoff, with a new permit and fresh credentials for each attempt | 200 / 250 |
| U7f | Owner instructions | documents only |

Pairing and playback use two connections. One connection for both stays deferred.

A source reading on November 22, 2026 found that the pairing connection survives the operator's wait.
The server sends an empty activation at once, which satisfies the 30-second exchange deadline.
After that only keep-alive pings run, and the `pair` window (10 minutes by default) is the one limit.
Music Assistant 2.10.12 acts on the live connection when the operator submits the token.
It stores no token for a later connection, so the operator starts `pair` before pasting.
The web interface location of the setup step, and a submission while the player is disconnected, remain unverified.

The first Mac build of this code passed strict Clippy for Core, Engine, and credentials on November 22, 2026.
No Sendspin test has run on a Mac, and no real keyring or keychain has held a Sendspin identity.

## Known risks

- Event queue overflow ends the connection. Keep `buffer_capacity` small and the worker step non-blocking.
- A state declaration that races a completed clock burst ends the connection with a clock error.
- Cancel or peer close discards queued audio events. Tests confirm submission before end or close.
- Uncorrected drift grows without bound; a slow output eventually exhausts the timeline budget.
- Tests need a private non-loopback interface, as existing network tests do.
- A direct `tungstenite` dev-dependency may need a dependency inventory update.

## Out of scope

Drift correction, resampling, and any synchronization accuracy claim.
Physical and acoustic timing qualification.
TLS, FLAC, Opus, DSD carriage, metadata and controller roles.
A DSP graph on the Sendspin path. Takeover into or out of Sendspin.
Desktop work beyond compile fixes. macOS tests. Network worker backpressure.
