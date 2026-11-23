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
ALSA reports a client delay estimate. Core Audio reports none.
U8 replaced the zero for Core Audio with the accepted frames past the restart cursor.
U9 adds the latency that the Core Audio device reports behind that cursor.

Keep the output running on idle silence while a stream is active and no audio is due (U8).
Anchor only on a running output.
Keep the output running through a full clear, and write scheduled audio only up to 100 ms ahead (U10).
Count a starved output and each chunk that is more than 20 ms late. Act on neither count (U10).

## Units

| Unit | Goal | Estimated size (source / test lines) | Depends on |
|---|---|---|---|
| U1 | Samples reach ALSA through the real worker and a real socket | 500 / 550 | none |
| U2 | Anchor and error measurement | 250 / 300 | U1 |
| U3 | Core command, Host runtime, real credential bridge | 900 / 700 | U1 |
| U4 | `earph sendspin` and the success-criterion process test | 260 / 300 | U2, U3 |
| U5 | Landed. Clears, re-activation, starvation, and underrun recovery | 300 / 400 | U2 |
| U6 | Landed. Pinned Python peer and documents | 0 / 250 | U4 |
| U7 | Credential provisioning and pairing, for real Music Assistant use | 870 / 1130 | U4 |
| U8 | Landed. Keep the output running on idle silence | 200 / 500 | U5 |
| U9 | Landed. Count the Core Audio device's reported latency in the schedule | 150 / 350 | U8 |
| U10 | Landed. Keep the output running through a full clear; count starvation | 190 / 420 | U8 |

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

Landed.
A full clear drops queued and output-held audio, and the next chunk anchors again.
U10 changed that: the output-held audio now plays out.
A partial clear keeps the audio that already left for the output and trims its replacement.
A new activation gets a new available declaration.
A gap up to 100 ms becomes silence; a longer gap anchors again.
An ALSA underrun prepares the same stream again, resets the scheduler, and counts in `timing.underruns`.
Before this unit, an ALSA underrun ended the source and revoked output approval.
Budget overflow ends the source with `RESOURCE_BUDGET_EXCEEDED`.
In this unit the worker wrote no silence while the server sent nothing; an output with a clock then underran and recovered.
U8 changed that.
No test produced a hardware underrun. Tests inject the failure at the output interface and at the ALSA failure path.

### U8: running output

Landed.
The first real Music Assistant test on a Mac played late by the device start time.
The reported start error was 42 ms and 69 ms on built-in speakers, and 293 ms and 426 ms on Bluetooth earphones.
The device started only after the first anchored audio frames, and nothing corrected the delay behind them.
On Core Audio a starved running output also ended the source, because it has no underrun recovery.

The worker now feeds idle silence while a stream is active and no audio is due.
The output runs before the first anchor and after each anchor that follows an output restart.
Idle silence is paced by the monotonic clock and corrected by the output's reported depth.
An output without a clock therefore gets it no faster than real time.
`timing.idle_silence_frames` counts it. `timing.inserted_silence_frames` keeps its meaning.
A `stream/end` stops and releases the output at once. The output does not run between streams.
The worker tests put a clock in front of the ALSA file capture; they show a 150 ms start as 155 ms of lateness without this unit.
No Mac has run this unit.
Unverified on a Mac: the start time, the idle depth margin, and the delay taken from the restart cursor.
Unverified on a Mac: a `stream/end` while an AudioQueue output still awaits its first valid clock.
That suspend needs a validated restart cursor and fails without one.

### U9: device latency

Landed.
A Mac listening test with Music Assistant 2.10.13 used built-in speakers and the AudioQueue route at 48 kHz.
After 14 s state showed a start error of -1220 us, a maximum error of 1503 us, no dropped frame, and no underrun.
The owner still heard Earph behind an iPhone web player.
The delay basis was `audio_queue_sample_time_not_audible_position`: the counted delay ended at the queue clock.
The latency of the device itself was not counted, so audio left late by that amount.

The worker now reads the device latency, the safety offset, and the output stream latency when a Core Audio output opens.
It converts their sum to stream frames and adds it to the scheduler's delay.
The AudioQueue route adds no IO buffer; the direct hardware route adds one.
State reports `timing.device_latency_frames` and `timing.device_latency_applied`, and the delay basis names an applied latency.
An unknown latency counts as zero. A report above one second is shown and not applied.
The player declares a required lead time of 150 ms plus the applied latency.
A server that sends only 150 ms ahead of a 200 ms latency loses every chunk as late.
Timing stays `unqualified`. This unit applies no drift correction and no resampling.

The worker tests give the clocked output a 200 ms device latency.
Without this unit that audio left 200 ms late.
No Mac has compiled or run this unit.
Unverified on a Mac: the property reads and their values, and the safety offset and IO buffer choice on the AudioQueue route.
Unverified: a Music Assistant server that honors a required lead time above 150 ms.
The silent macOS test `native::tests::default_output_latency_reads_start_no_io_and_change_no_setting` in `earph-coreaudio` prints the reported frames.
It is an ignored test; run it by name with `--ignored`.
On November 22, 2026 it passed on the Mac build host: 50 device frames, 50 safety frames, and 0 stream frames at 48 kHz.
That sum is about 2 ms, so a reported latency of this size cannot explain an audible lag on built-in speakers.

### U10: running output through a clear

Landed in two rounds on November 23, 2026. A Mac ran the first round; see "Measured on a Mac" below.
No Mac has compiled or run the second round.
The loopback capture below timed a full clear at 152 ms to 174 ms, because the clear restarted the output.
An anchor on an output that kept running cost nothing measurable.

A full clear now keeps the output running on both backends.
The worker drops the queued audio and the unaccepted rest of the current period.
The audio that the output already holds plays out. Idle silence follows it, and the next chunk anchors on the running output.
That anchor counts the held frames as its delay, as an anchor after a gap does.

A source reading found no way to drop held audio without a restart through today's adapters.
ALSA drops frames only with a stop and a new prepare. `snd_pcm_rewind` could take frames back, but not every plugin supports it.
The Core Audio adapter drops frames only by closing its queue.
`AudioQueueReset` could flush a running queue. The adapter maps the queue clock through each enqueued buffer, and a flush would break that mapping.
No Mac can run here, so the adapters stay unchanged.

The held audio therefore needed a bound.
Before this unit the worker wrote every contiguous chunk at once, as far ahead as the output took it.
From the code, a Core Audio system output takes its staging ring plus an equal queue window: 8192 frames, about 170 ms, with the default sizes.
The worker now writes scheduled audio only while a running output holds less than a scheduled depth.
The first round used the idle depth: three periods and at least 30 ms. The second round uses 100 ms.
The rest waits in the timeline, where a clear still drops it.
So cleared audio plays on for at most about 100 ms, plus the device latency, which plays out in either design.
The bound applies only to an output that counts its queued frames from a cursor, as Core Audio does.
An ALSA delay can count frames that already left the buffer. A bound on that report could starve such a device.
An ALSA output therefore has no bound from the worker. Its buffer is the bound: 256 frames with the default sizes.
The first round bounded every output whose buffer was larger than the depth; the review found that unsafe for ALSA.

State shows the cost. `timing.clear_held_frames` holds the scheduled frames that the output held at the latest full clear.
`timing.max_clear_held_frames` keeps the largest value.

Unchanged: a partial clear, underrun recovery, a stream end, a lost player role, stop, pause, and reconnect.
A stream end still releases the output at once and drops what it holds.
New: `timing.output_discarded_frames` counts the scheduled frames that such a release dropped.

Side finding A is gone with its cause.
The `scheduled` flag chose between a restart and no restart. No clear restarts the output now, so the flag is removed.
The worker instead remembers where the last scheduled frame ended, and counts only scheduled frames as held audio.
A clear after the audio played out reports `clear_held_frames: 0`.

Side finding B is not traced to its cause. See "Tail frames" below.

The worker tests give the clocked output a 150 ms start and a 100 ms head start after a clear on playing audio.
Before this unit that clear lost 6323 frames. Now it loses none, the first wait stays below 20 ms, and the audio leaves on time.
A second test sends 400 ms of audio within 80 ms: the clocked output held 3838 of its 3840 frames before the bound.
It now holds at most 4800 of 7680 frames.
A third test makes the output report a delay, as ALSA does, that counts 960 frames too many. That output fills its buffer.

#### Measured on a Mac

Doctor Biz ran the first round, the build of `a006fe7`, on November 23, 2026.
It used the AudioQueue route, the silent loopback device, and the capture harness. The depth was 30 ms.

- A clear at the 200 ms head start, 4 runs of 10 clears: no missing head frame and a `first_wait_us` of 0.
  The old audio ran 27.9 ms to 29.8 ms past the clear. The new audio left 199 ms to 200.6 ms after it.
  The offsets were unchanged: -64 us to -1297 us.
- A clear at an 80 ms head start, 5 clears: no missing head frame, where the earlier build lost 3437 to 4546 frames.
  The new audio left 79.2 ms to 79.7 ms after the clear.
- One 120 s stream: all 5,760,000 frames left, with no interior gap.
- The analysis reports about 8100 missing tail frames at each 200 ms clear and about 2400 at each 80 ms clear.
  That is the cleared audio. The peer had sent one head start of audio past the clear: 9600 and 3840 frames.
  About 1400 of them had reached the output and played on. The rest is 8200 and 2440 frames.
- Holes: 4 of 44 four-second segments, in 3 of 4 runs, missed interior frames.
  The hole sizes were [128, 4928, 2944], [256, 3008], [128], and [64] frames, all multiples of 64.
  They began 0.6 s to 2.5 s into a segment, not at a clear. The Mac ran other work, with a load average of 3 to 16.
  The earlier build, `adb8d06`, had no interior gap in any run, including one of 300 s.
- The frames were missing, not late. After each hole the offset of the segment stayed constant to under 0.001 us.
- State showed `underruns` 0 and `dropped_frames` 0. `max_abs_error_us` stepped to 65,893 in one such run.

#### The holes

A source reading on November 23, 2026 explains the holes as starvation of the AudioQueue. No Mac ran for it.

- The worker stalled for longer than the output held. With the 30 ms depth the queue held 21 ms to 27 ms.
  `max_abs_error_us` of 65,893 fits: one chunk was decided about 66 ms after its audio was due.
- The worker dropped nothing. While anchored it writes every chunk and skips no frame; see the locked branch of `Scheduler::decide`.
  `dropped_frames` stayed 0, and no idle silence entered, because the timeline still held audio.
- The staging ring cannot overrun. `Producer::write` accepts only what fits.
- The adapter enqueues each buffer without a start time and trusts the start that the queue returns.
  See `pump` in `crates/earph-coreaudio/src/playback/system/native.rs`.
  `Schedule::append` refuses only a start before the predecessor's end. It accepted any start behind the queue clock, and any gap.
- `Schedule::retire` retires every buffer whose end lies behind the queue clock.
  A buffer placed behind the clock retires at the next read, so the held count dropped and nothing signalled a loss.
- Inferred from the measurement, not from code: the queue keeps its sample timeline through a dry spell.
  It places a late buffer right behind its predecessor, in the past, and never plays the part behind its clock.
  That gives missing whole buffers, 64 frames each, and an unchanged offset afterwards. Both were measured.
  AudioQueue is closed source. A queue that placed a late buffer at its present would shift the offset instead; no capture showed that.
- The earlier build held up to 170 ms, so the same stalls starved nothing.

The second round counts both cases. `Schedule` now keeps the latest queue clock.
It counts the frames of each buffer that start behind that clock, and the frames of each gap after a predecessor.
State shows the sum as `timing.starved_frames`.

#### Second round

- The scheduled depth is 100 ms, and at least the idle depth. Idle silence keeps its 30 ms.
  The trade-off: a stalled worker starves the output after that long, and a clear leaves that much old audio.
  The server's head start is at least 200 ms plus the device latency, so 100 ms remain for a clear.
- `timing.starved_frames` counts starved queue frames on Core Audio. It is `null` on ALSA, which fails a call instead.
- `timing.late_chunks` counts contiguous chunks that were more than 20 ms late.
  The count acts on nothing: a late chunk writes at once, keeps the anchor, and records its error, as before this unit.
  The second round first anchored such a chunk again on an output that held half its idle depth. A review removed that rule.
  On Core Audio the held count includes the staging ring, so the rule could fire while the queue still dropped late buffers.
  Both would drop audio, and the stream would then play early for good. A slow device clock would also skip once every 20 ms of drift.
  The measured queue drops late buffers itself and stays on schedule, so Core Audio needs no new anchor.
  Drift is not corrected and not bounded. `late_chunks` and `max_abs_error_us` show it. A designed drift unit is future work.
- With no worker bound on ALSA, a clear plays on for `buffer_frames` / 48000 s: about 5 ms at the default 256 frames.
  Above about 9600 buffer frames the hold exceeds the 200 ms head start, and each clear drops new audio as late.
  No guard exists. A small one would declare a larger required lead for such a buffer, as the device latency already does.
- `clear_held_frames` and `output_discarded_frames` no longer count idle silence that lies before the scheduled frames.
- A clear no longer recovers an underrun by itself. A failed hold query gives `null`, and the next write recovers.
  That removes a branch that no test could reach.
- The slow-start test built its audio after the stream start. Idle silence filled the starting output within 20 ms.
  A chunk that arrives later waits behind a blocked silence period, and its first lead reads low. The test now builds the audio first.
  The product effect stays and is documented: `first_lead_us` and `first_wait_us` read low for such a chunk.

A design for later: keep a deep hold and still clear at once.
The queue could take each buffer with an explicit start time, and a clear could reset the queue and re-enqueue from "now".
That needs a rebuilt clock mapping in the adapter and a Mac to test it. It stays out of this unit.

The worker tests model both kinds of starved output behind the clocked seam, with a worker stall of 250 ms.
An output that drops late frames counts 6000 to 12000 starved frames, at least one late chunk, and keeps its anchor.
An output that plays late frames late counts the same, keeps its anchor, drops nothing, and stays late.
The schedule test counts a buffer behind the clock and a gap after a predecessor.

Unverified on a Mac: the whole second round. Two lines changed in code that only a Mac compiles:
the `starved` field in `clock_diagnostics` of the system route, and the `starved` field in the Engine's Core Audio counters.
Unverified: that the queue clock and the returned buffer start share one timeline closely enough for an exact count.
A count that grows in a run without holes shows that they do not.

The capture runs that verify the second round, with expectations from the code and the first measurement:

- `run NAME --streams 11 --seconds 4` under the same load, several times.
  Expect no interior hole, because a stall must now exceed 100 ms. Expect `starved_frames` flat and `late_chunks` 0.
  If a hole appears, expect `starved_frames` to grow by about the hole's frames, and `late_chunks` to grow for a hole above 20 ms.
  Expect `anchors` to grow only at the clears.
- The clears of that run: no missing head frame, `first_wait_us` near 0, `clear_held_frames` up to about 4800.
  Expect the old audio to run on for up to about 100 ms, and about 4800 missing tail frames at each clear.
- `--lead-us 80000`: the head start is now shorter than the held audio.
  Expect up to about 20 ms of missing head frames, counted in `dropped_frames`. No server gives so short a head start.
- `--streams 1 --seconds 300` under load: no interior hole, `starved_frames` flat, `underruns` 0.
- `--transition end`: compare the missing tail frames with `starved_frames`; see "Tail frames".

The next section records the runs.

#### Second round, measured on a Mac

The Mac build of the second round ran the loopback harness on 2026-11-23. The device was the silent loopback device on the AudioQueue route.

- Four valid runs of eleven 4 s segments, with a clear between segments: no interior hole and no missing head frame.
- Two of those runs had twelve busy processes beside them. One more loaded run was invalid, because the recorder itself reported overlaps.
- At each clear, old audio ran 98.7 to 99.6 ms, and new audio left 199.7 to 200.7 ms after the clear. `first_wait_us` read 0 and `clear_held_frames` read 4736.
- One loaded 180 s stream: no interior hole, and the offset stayed at -52.7 us.
- `starved_frames`, `late_chunks`, `underruns`, and `dropped_frames` stayed 0 in every run. `max_abs_error_us` stayed under 1.9 ms.
- The loaded 180 s stream lost its last 40 frames, and no counter moved. The tail loss stays open.

The build without the late anchor rule ran two more loaded clear runs: no interior hole, no missing head frame, and every counter at 0.
One run of six segments with a stream end between them lost 61 and 46 tail frames in two segments. `starved_frames` and `output_discarded_frames` stayed 0.

No run starved the output, so no run has shown `starved_frames` move on a Mac.
A Bluetooth output and a real Music Assistant skip remain untested with this change.

### U6: independent producer and documents

Add an ignored test with the pinned Python peer. Update the Sendspin documents with the unqualified boundary.

Landed.
The case `play_python` in the `sendspin_cli` test target runs `earph sendspin` against a pinned Python player peer.
That target has no harness and therefore no `#[ignore]`; the case runs only by name and needs `EARPH_SENDSPIN_PEER`.
The peer is `crates/earph-engine/tests/support/sendspin_player_server.py`.
It uses the pinned strict Noise driver and the pinned message models of aiosendspin 10.2.1.
The case checks the handshake, the declared player state, a clear, two stream ends, the exact capture, and the timing fields in status.
See [the runtime notes](../../sendspin-runtime.md#pinned-python-player-peer) for the outcomes and the boundary.

The case exposed one product defect, and this unit fixed it in `earph-network`.
`client/state` carried `available` inside the `player` object. The pinned specification requires it at the top level.
The pinned server model read no `available`. The server tolerates that by default, so Music Assistant played anyway.
The architecture note above says that `earph-network` needs no change; this fix is the exception.

On November 23, 2026 the case passed 14 runs in a row on the Linux development host. The last six ran the final code.

Unverified: the player against the real `SendspinServer` and `PushStream` of aiosendspin.
The pinned environment lacks PyAV, NumPy, and Pillow, so `aiosendspin.server` cannot import there.
The peer's send-ahead and clear timing are test code. The server's policy is known from a source reading only.
Unverified: a server started with `allow_noncompliant_clients=False`. No run used one.
Unverified: Music Assistant with the moved `available` field. The pinned model accepts it; no real server has seen it.
Unverified: the case on a Mac. It needs Linux and ALSA.

### U7: real Music Assistant use

A real Music Assistant server cannot drive this player before this unit.
The missing parts are:

1. Provisioning. Done in U7a.
2. A pairing flow. Done in U7b and U7c.
3. Pairing and playback cannot share one connection today. U7c and U7d use two connections.
4. Discovery and a listener. The user must supply the server address and a pinned IP.
5. Reconnect. Done in U7e. Permit and policy expiry still end the session.
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
| U7e | Landed. The Host runtime connects again after a lost server, with a new permit and fresh credentials for each attempt. See "U7e: reconnect" below. | 200 / 250 |
| U7f | Owner instructions | documents only |

Pairing and playback use two connections. One connection for both stays deferred.

A source reading on November 22, 2026 found that the pairing connection survives the operator's wait.
The server sends an empty activation at once, which satisfies the 30-second exchange deadline.
After that only keep-alive pings run, and the `pair` window (10 minutes by default) is the one limit.
Music Assistant 2.10.12 acts on the live connection when the operator submits the token.
It stores no token for a later connection, so the operator starts `pair` before pasting.
The web interface location of the setup step, and a submission while the player is disconnected, remain unverified.

#### U7e: reconnect

Landed.
Before this unit a lost server ended `earph sendspin`.
The Host runtime now keeps the session and connects again.
It retries only after the server knew this player in the session.
It retries a closed socket, a lost socket, a refused connection, a failed upgrade, and a transport deadline.
It does not retry a refused authentication, a protocol violation, an expired permit, a clock error, or an output fault.
Those keep their exit status. A first connection that fails also ends the command as before.
The wait starts at 250 ms, doubles after each failed attempt, and stops at 10 seconds. It has no random part.
Only a connection that lasted 30 seconds starts the wait at 250 ms again.
A server that accepts the player and drops it at once sees the growing wait.
A worker that reports its closed source late, or never, gets the same reconnect after a healable transport end.
Each attempt gets a new credential supervisor, a new permit, a new admission policy, and a new worker source.
The handshake, the player state, the clock, and the timing anchor are therefore new.
The lost connection stops the output through the existing close path.
State shows the phase `reconnecting`, the transport `buffering`, and a `reconnects` count.
The command line prints `lost the server, retrying`.
A stop, a quit, and Control-C end the session at once during the wait.

Network gained one error, `Lost`, for a socket that fails or ends without a WebSocket Close.
Before, such a socket reported `Protocol`, which a reconnect must not retry.
A server that closes cleanly no longer ends the command once it accepted this player.
Three tests that ended by a peer close now end by a stop.

The command tests use the in-repo test server.
They cover a clean close and a dropped socket, each followed by a new stream that anchors once.
They cover a failed authentication on the reconnect, which ends the command with `AUTH_DENIED`.
They cover Control-C inside a 4-second wait, with a 2-second exit bound.
They cover a server that accepts and drops three times: the waits before the attempts grow.
They cover a clean close before the server accepted the player: the command ends with status 0 and no retry.

Unverified: a restart of a real Music Assistant server. No reconnect has run against one.
Unverified: which transport end a real server restart produces, and how long the server stays away.
Fixed on November 23, 2026: the player pings every 2 seconds and allows 3 seconds for any frame, so it finds a silent server within 5 seconds.
Unverified: a network that really fails without a reset. A local peer that stops reading stands in for it.
Unverified: reconnect on a Mac. No Mac has compiled or run this unit.
Known limits: each attempt opens and closes the output, and a clock error still ends the session.
Known limit: a failed credential load during a reconnect ends the session, for example a store that locked meanwhile.
Known weakness: the runtime matches a worker close to the transport end by error code.
A source fault with the same code, in the same moment as a healable transport end, starts a reconnect.
Fixed on November 23, 2026: a `server/unpair` and an HTTP 401 or 403 on the upgrade now end the session with `AUTH_DENIED`.
A WebSocket Close is still retried, because the pinned server closes with code 1000 when it stops and when it drops a client.
State shows `reconnect_attempts`, `last_end`, and `last_end_code`. See "Refused or gone" in `docs/sendspin-runtime.md`.
Unverified: a refusal by a real Music Assistant server. Its pinned provider unpairs through `server/unpair`.

The first Mac build of this code passed strict Clippy for Core, Engine, and credentials on November 22, 2026.
No Sendspin test has run on a Mac, and no real keyring or keychain has held a Sendspin identity.

## First real server test

Doctor Biz ran the player on a Mac against Music Assistant 2.10.13 on November 22, 2026.
That run supersedes the "unverified" notes above for pairing, the player role, and a Mac keychain file.
No automated Sendspin test has run on a Mac.

- Pairing worked: `init`, `token`, `pair`, then a second connection as a player under the same identity.
- Music Assistant played a library track to Earph through built-in speakers and Bluetooth earphones.
  The server converted 44.1 kHz to 48 kHz, because Earph offers only 48 kHz.
- Before idle silence, the start error was +42 ms and +69 ms on speakers, and +293 ms and +426 ms on Bluetooth.
  With idle silence, a speaker run showed a start error of -1.2 ms and a worst error of 1.5 ms after 14 seconds.
  It dropped nothing and had no underrun.
- Earph still played audibly behind a web player in the same group.
  State names the cause: the counted delay ends at the AudioQueue sample time, not at the speaker.
  The next unit adds the device's reported latency.
- A rebuilt credential helper could not read the stored identity; the store looked locked.
  The cause is unconfirmed. The likely cause is the keychain item's access list, which names the first helper.
  A stable code signature for the helper would fix it.

### Later runs with the device latency

Doctor Biz ran the build of `ce99d1f` on a Mac against Music Assistant on November 22, 2026.

- Built-in speakers: the device reported a latency of 1094 frames, about 23 ms, and the player applied it.
  The start error was 0.3 ms and the maximum error was 1.0 ms. The run had no drop and no underrun.
  By ear, Earph played in sync with a web player in Safari on an iPhone.
  This is no measurement. Sync stays `unqualified`.
- Bluetooth earphones, about ten minutes, with 21 track changes from the Music Assistant page:
  the device reported a latency of 7680 frames, 160 ms, and the player applied it.
  The maximum error was 7.7 ms and did not change from start to end. The run had no underrun.
  By ear, the track changes were clean.
  This was the first real run of the Core Audio discard path.
- Open defect: on the Bluetooth route, `dropped_frames` rose by about 15,400 frames, about 0.32 s, at each stream start.
  The player drops the first part of each track after a skip as late.
  The cause is not traced. The hypothesis: on a skip the server gives less head start than the declared lead.

### Start drop: source reading and instrument

A source reading on November 22, 2026 does not support that hypothesis. It ran nothing against a server.
It read aiosendspin 10.2.1 and the Music Assistant 2.10.12 Sendspin provider. The 2.10.13 provider was not read.
Two runs later showed 360,558 frames dropped over 24 anchors and 165,383 frames over 21 anchors.
That is about 0.31 s and 0.16 s at each start.

What the server does, from the source:

- aiosendspin stamps the first chunk of a new timeline at its clock plus a send-ahead.
  See `commit_audio` and `_min_send_ahead_us` in `aiosendspin/server/push_stream.py`.
- For a buffered source the send-ahead is the larger of the minimum buffer plus a hold-back and the required lead time.
  It is at least 50 ms, and the output delay is added. See `_role_send_ahead_us`.
  For a live source it uses the minimum buffer and ignores the required lead time.
- Both values come from the player state that Earph declares. The server clamps each at 30 seconds.
  See `get_required_lead_time_us` and `get_min_buffer_us` in `aiosendspin/server/roles/player/v1.py`.
- Music Assistant treats a library track as buffered. Radio and plugin sources are live.
  See `_is_live_source` in the provider's `playback.py`.
- A track change keeps the stream. The player receives a full `stream/clear`, no `stream/end`, and no new `stream/start`.
  See `cancel(..., keep_stream=True)` in the provider's `player.py` and `PushStream.clear`.

On the Bluetooth route Earph declared a lead of 310 ms and a minimum buffer of 360 ms.
The first chunk after a skip therefore carries at least 360 ms at the server's clock.
The schedule needs 160 ms of that for the device latency, plus the frames that the output holds.
About 170 ms remain for the server's encode and send, the network, and the player's output.

What the player does, from the code:

- A full clear discards an output that holds scheduled audio. See `halt` in `crates/earph-engine/src/worker/sendspin.rs`.
- The Core Audio discard closes and reopens the output. See `discard` in `crates/earph-engine/src/output/coreaudio/native.rs`.
- Idle silence then starts the output again. No chunk gets a decision before the output runs. See `decide` in the worker.
- A chunk whose target passed during that wait is dropped as late. See `anchor` in `crates/earph-engine/src/sendspin_schedule.rs`.

The code supports this cause: the output restart after each clear takes longer than the head start that remains.
The Bluetooth start errors before idle silence were 293 ms and 426 ms. A restart of that length exceeds 170 ms by 0.12 s to 0.26 s.
The speaker values were 42 ms and 69 ms, which fit inside the margin. That matches a speaker run without drops.
Inferred, not measured: the restart time of the Bluetooth output. No run has timed it.
Unverified: that Music Assistant 2.10.13 behaves like the 2.10.12 source, and the size of the resampler hold-back.

The instrument: `timing.first_lead_us`, `timing.min_first_lead_us`, `timing.first_wait_us`, and `timing.max_first_wait_us`.
The first lead shows the head start that the server gave. The first wait shows how long the output kept the first decision back.
The next Bluetooth run decides the cause. A first lead near 360 ms and a first wait above 200 ms confirm the restart.
A first lead far below 360 ms points at the server instead.

Option 1 landed as U10 on November 23, 2026. The options were:

1. Keep the output running through a full clear. Stop feeding audio, and let idle silence follow.
   Cost: the output plays the audio that it already holds, so the old track sounds for that long after a skip.
   On the AudioQueue route that is the queue depth. The device latency plays out in either design.
   Sync stays exact. Size: about 30 source lines and 60 test lines. This is the recommended fix.
2. Declare a larger lead and minimum buffer by a start allowance for the output.
   Cost: every start and every skip begins later by that allowance, on every route unless the allowance is per route.
   A buffered source keeps no lasting latency. A live source stays behind by the larger minimum buffer.
   Sync stays exact. The allowance is a guess until a run measures the restart. Size: about 10 source lines.
3. Start a late stream with a bounded offset instead of a drop, and hold that offset.
   Cost: this player then plays behind every other player in the group for the whole stream.
   State must show the offset. Size: about 80 source lines and 100 test lines. Not recommended.

### Loopback timing capture

Root ran the first measured timing capture on November 23, 2026, on the Mac build host.
The build of `adb8d06` played streams from the pinned Python peer into the silent phoaudio Loopback device at 48 kHz.
It was also the first automated Sendspin pairing and playback on a Mac.
[The runtime notes](../../sendspin-runtime.md#loopback-timing-capture) describe the harness, the method, and its limits.
Twelve runs gave 73 anchors. Every recording was complete: no callback gap and no driver overrun.
This is one virtual device on one machine. Sync stays `unqualified`.
Each statement below says whether it is measured or inferred.

Clocks, measured:

- The peer's `time.monotonic_ns` (Python 3.13.5) is `mach_absolute_time` scaled by the timebase 125/3.
  In 6000 bracketed readings, each absolute reading lay between its two monotonic readings, at most 292 ns from either.
  The continuous clock differed by 50.04 s, so the check tells the two clocks apart.
- `loopcheck --times` writes the `mHostTime` of each input callback in the same ticks.
  Server time and capture time therefore lie on one clock, and the analysis maps nothing.
- The callback times fit the nominal 48 kHz within 0.001 us. The device reports no latency and no safety offset.

Start offset, measured. The offset is the time a frame left the device minus its server time.

- Inside a segment the offset never moved. Its spread was below 0.001 us in every segment of 3 to 300 seconds.
- Each anchor chose a new offset. Over 72 anchors the mean was -608 us and the standard deviation 408 us.
  The range was -1311 us to -50 us: every frame left early.
- The kind of anchor made no clear difference: -663 us over 20 stream starts, -655 us over 22 clears,
  -429 us over 15 clears after the audio ran out, and -643 us over 15 anchors after a gap.
- One more anchor is left out. It was a stream start with a 40 ms head start, and its gain ramp reached the pattern.
- The player's `start_error_us` ranged from -1115 us to +1176 us, with a mean of +31 us.
  The measured offset lay earlier than `start_error_us` at every anchor: by 7 us to 1323 us, 638 us on average.
- `last_error_us` moved by up to 1.3 ms inside a segment whose measured offset stayed fixed.
- Inferred: `last_error_us` shows when the worker wrote a chunk, not where the audio left.
- Inferred: the difference to `start_error_us` spreads evenly over about 64 frames (1333 us).
  That points at a step of that size in the queue clock that the delay count reads. No code reading confirmed it.
- Unverified: the share of the Python peer in the offset. The peer stamps its clock replies in Python.

Skip behaviour, measured. The peer sent a full `stream/clear` and new audio at once.
The head start was 200 ms, as the pinned server's formula gives for the state that Earph declared here.

- 12 clears on playing audio lost no frame. `dropped_frames` stayed 0.
  The old audio ran 11 ms to 21 ms past the clear. The output was silent for 179 ms to 188 ms.
  The new audio left 199.0 ms to 200.4 ms after the clear, on time.
  `first_lead_us` was 147 ms to 162 ms and `first_wait_us` 58 ms to 65 ms.
- 5 clears after the audio ran out behaved the same: no lost frame, a first lead of 145 ms to 162 ms, a first wait of 58 ms to 66 ms.
  The worker's `scheduled` flag stays set after the scheduled audio played out, so such a clear also restarts the output.
- A shorter head start is no server behaviour. It shows the limit of the restart.
  With 120 ms, 5 clears lost the first 1670 to 2428 frames (35 ms to 51 ms).
  With 80 ms, 10 clears lost 3437 to 4546 frames (72 ms to 95 ms).
  With 40 ms, 5 clears lost 5440 to 6254 frames (113 ms to 130 ms).
  In those 20 clears the new audio left 152 ms to 174 ms after the clear, whatever the head start.
  `dropped_frames` equalled the missing frames in all 20, and the first wait stayed at 57 ms to 69 ms.
- 15 anchors after a gap met a running output: no clear, no restart, only idle silence before them.
  With head starts of 200 ms, 80 ms, and 40 ms they lost no frame, and `first_wait_us` was 0.
  The new audio left on time, 39 ms to 41 ms after the peer sent it in the shortest case.
- 15 stream starts with the 200 ms head start dropped nothing.
  Their first lead minus their first wait put the first decision 117 ms to 132 ms before the audio was due.
  Stream starts with head starts of 120 ms, 80 ms, and 40 ms dropped 327, 1925 to 2328, and 3789 to 4221 frames of their silent preamble.

Drift, measured. One stream ran for 300 seconds.

- All 14,400,000 frames left, in order, with none missing and none added.
- The offset was -1258.0 us at each of 28,126 callbacks. The slope was zero.
- `last_error_us` moved between -1243 us and +26 us over 31 snapshots.
- Not measured: drift between two machines. The peer and the device share one host clock here.
  The run shows only that the AudioQueue path neither gains nor loses a frame in five minutes.

What this says about the start drop:

- Measured: a full clear costs this output 152 ms to 174 ms before new audio can leave. A running output costs nothing measurable.
- Measured parts of that cost: 35 ms to 55 ms pass before the worker first sees the new chunk, and the first wait adds 57 ms to 69 ms.
  Inferred from the code: the first part is the discard, which closes and reopens the output; the rest is the idle silence that the new output holds ahead of the audio.
- Measured: the server's 200 ms head start leaves 26 ms to 48 ms of margin on this device.
  The loopback device has no latency, and the recorder kept it running.
- Inferred: an output that restarts more slowly loses the difference at each clear. That fits the Bluetooth drops of 0.16 s and 0.31 s.
  Not measured: the restart time of the Bluetooth output, and a start drop in any capture with the server's own head start.
- The recommended fix, option 1, is supported. Measured: an anchor on a running output needs no restart time, even with a 40 ms head start.
  Not measured: the fix itself. U10 landed after these runs, and no capture has run its build.
  Not measured: its cost, the old audio that the output still holds. With the discard of that build the old audio ran 11 ms to 21 ms past the clear.
- For option 2: the start allowance would have to cover the whole restart, which this device already puts at up to 174 ms after the clear.

Other findings of these runs:

- Measured: in 8 of 51 segments that ran out, the last 1 to 56 frames never left the device.
  `dropped_frames` did not count them. The cause is not traced; see "Tail frames" below.
- Measured: `earph sendspin init`, `token`, `pair`, and playback work over SSH with a keychain file and its password.
  Each command completed without waiting for a prompt. The keychain search list did not change.
- The pinned peer's source hash checks pass on the Mac with Python 3.13.5 and the packages of `peer-requirements.txt`.

### Tail frames

A source reading on November 23, 2026 looked for the 1 to 56 tail frames. It ran no Mac code and found no proven cause.

What the reading rules out:

- The worker. It hands every frame of a chunk to the output before it writes idle silence.
  The Linux worker tests capture every segment that runs out complete, to the last frame.
- The timeline. It drops queued audio only at a clear, a stream end, or a lost role.
- The gain ramp. It changes samples and removes none, and no ramp runs at a segment's end.
- The scheduler's late drop. It drops at the head of a chunk and counts each frame in `dropped_frames`.

What the reading finds:

- The Core Audio system route stages audio in whole slots of one period, 64 frames with the command's default.
  A tail shorter than one slot stays in the staging ring until more frames arrive.
  See `Slots::fill` in `crates/earph-coreaudio/src/playback/system.rs` and its test `sdk_staging_reads_real_float_payload_and_keeps_short_tail_until_eof`.
  The size fits: the lost tails were all shorter than 64 frames.
- That tail leaves with the next frames, and idle silence supplies them within about 20 ms.
  A close of the queue drops a tail that still waits: a stream end, a stop, and, before U10, a full clear.
- The reading does not explain the loss. In every run the capture driver waited 0.5 s or longer between the last audio and the next close.
  Idle silence should have moved the tail out long before.

The holes of the first U10 build suggest a fourth cause, and the likely one.
The last slot holds the audio tail and the first idle silence. Idle silence starts only 20 ms before the queue runs dry.
A worker that stalls longer hands that slot to the queue behind its clock, and the queue never plays it.
That loses exactly the tail, 1 to 63 frames, and no counter moved. `timing.starved_frames` now counts it.
Not confirmed: no capture has run with that counter.

Before that reading, the cause was one of three, and only a capture can tell them apart:

1. Idle silence did not follow the audio in those segments, and a later close dropped the staged tail.
   Then `timing.output_discarded_frames` grows by the missing frames at a stream end.
   A full clear no longer closes the queue, so the tail then leaves late, as an unmatched run in the analysis.
2. The frames passed the queue clock and were lost behind it. Then the counter stays flat.
3. The frames left and the analysis did not match them. Then `unidentified_frames` or `unmatched_runs` holds them.

No fix landed for this finding. The new counter and the analysis fields narrow it at the next capture.
`--transition end` runs show case 1 best, because each segment there ends with a close.

### Local playback into the loopback device

On 2026-11-23, the Mac build of `adb8d06` played the phoaudio `suite-48000-24-2ch` stimulus into the loopback device.
This is local file playback, not Sendspin. It tests the same AudioQueue output path.

- At 0 dB, the phoaudio `bit-exact` profile passed. The channel map is the identity.
- At -10 dB, phoaudio found a gain change of -10.000 dB and nothing else. The `transparent` profile passed with a residual of -130 dB.
- Both runs report `LEADING_CONTENT_LOST` for the first 288 frames. A frame comparison shows the 960-frame start gain ramp, linear from zero. No frame is missing.
- Each capture read every frame with zero overruns.

Record over SSH through a tmux session that a desktop Terminal started. A plain SSH session records silence, because macOS withholds audio input from it.

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
