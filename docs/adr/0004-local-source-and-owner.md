# Local source and owner decisions

Date: November 17, 2026.
Status: Implemented foundation; output integration continues.

## Decoder boundary

Use Symphonia 0.6.1 with only PCM, WAV, AIFF, and FLAC features enabled.
Keep its types inside `earph-media`.
Expose the existing `PcmFormat`, `PcmBlock`, and epoch contracts to the engine.
Read local files without changing tags, paths, or contents.

The first qualified subset uses signed 16, 24, and 32-bit mono or stereo PCM.
Recover original integer values directly from each decoder representation.
Do not normalize integer preservation paths through floating point.
Decode and allocate on a source worker.
Never run file reads or decoder allocation inside an output callback.

The library's metadata limits do not cover every embedded FLAC reader.
Apply a cumulative container budget before probing.
Verify actual EOF rather than trusting a declared frame count.
Reject unsupported sample domains and detected DoP carriers explicitly.
Keep RF64 and other extended containers unqualified until separately implemented and tested.

## Control and storage boundary

Use one `Session` for queue state, revisions, transport intent, events, and receipts.
Use stable queue identities rather than indices as command targets.
Page queue reads so valid escaped paths cannot exceed the reply limit.
Resolve directory imports through a bounded adapter after command admission.

Use versioned JSON checkpoints for the current bounded queue.
A database adds no needed behavior for this initial operational state.
Profiles and asset storage remain separate later requirements.
Validate restoration through the same domain bounds as ordinary mutations.
Discard saved preparation claims because files may have changed.
Restore paused and muted without opening an output.

## Output observation boundary

Keep requested source format separate from observed client and physical stream formats.
Report unknown consumed position and backend delay as null.
Use the callback-delivered cursor only for Core Audio source recovery.
Never present that cursor as an audible or DAC position.
Mac pause continuity remains unqualified for frames already handed to hardware.

Prepare two callback periods before starting the Core Audio stream.
For shorter streams, mark EOF before starting the partial final buffer.
Drain through the last payload buffer's SDK sample and host clock boundary.
An empty application ring does not prove that boundary has passed.
If the boundary stays unknown, fail explicitly instead of reporting successful completion.
These clocks do not prove DAC preservation or acoustic presentation.

Represent ALSA draining explicitly.
After hardware drain starts, observe its native state instead of restarting the drain operation.
Repeated hardware drain calls can reinstall the library's end-silence padding.
Keep plugin commit work separate until its native slave enters draining.
Use bounded pacing while waiting for drain completion.
Do not use ordinary I/O readiness as the completion signal.
The [pinned hardware callback](https://github.com/alsa-project/alsa-lib/blob/v1.2.15.3/src/pcm/pcm_hw.c#L754) installs end silence.
The [pinned rate callback](https://github.com/alsa-project/alsa-lib/blob/v1.2.15.3/src/pcm/pcm_rate.c#L1134) can require additional buffered commits.
The [generic wrapper](https://github.com/alsa-project/alsa-lib/blob/v1.2.15.3/src/pcm/pcm.c#L1358) treats native setup as completed drain.

Stage each mutation, then sync and atomically publish its checkpoint.
Publish an applied receipt only after the durable commit succeeds.
Stop the owner after storage failures because the persisted outcome may be uncertain.
Retain bounded receipts in memory without promising durable exactly-once execution.

## IPC boundary

Use private per-user Unix sockets on Linux and macOS.
Each connection carries one length-prefixed JSON command and one reply.
Bound each frame to one MiB.
Apply an absolute deadline to connection, frame read, and frame write.
Use nonblocking I/O and polling so buffered replies survive peer closure on macOS.

Pin one validated resolved runtime directory for the lock, socket, endpoint, and checkpoint.
Use an OS advisory file lock for one owner per runtime.
Expose a fresh owner identity after restart and reject old-owner mutations.
Keep the same command envelope for Rust, IPC, and CLI callers.
Do not open a TCP listener.

Preserve old sockets and interrupted temporary files under Doctor Biz's no-deletion instruction.
Their presence does not select an active owner; `endpoint.json` identifies the current socket.
Cleanup requires a separate explicit approval.

## Qualification limits

The implementation passed Linux and native macOS canonical checks.
The generated fixtures and private corpus establish scoped decoder behavior.
They do not establish physical output preservation, acoustic safety, or DSP continuity.
See the [review](../local-playback-review.md) and [implementation plan](../superpowers/plans/2026-11-17-local-playback.md).
