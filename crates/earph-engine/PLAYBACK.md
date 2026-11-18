# Local playback integration

The owner keeps `Session` authoritative for commands, queue configuration, revisions, receipts, epochs, and volume admission.
The decoder and native output live on one worker thread.
The IPC parser lives on a separate thread.
A slow frame read cannot hold the playback control actor.

The worker has 64 bounded command slots and 1024 bounded state-event slots.
Ordinary controls can use only the first 32 command slots.
The remaining slots reserve admission for mute, caps, pause, stop, and shutdown.
All controls retain FIFO order so older settings cannot overtake a later mute.
If safety traffic exhausts the reserve, cancellation requests native closure and the owner reports failure.
The failed receipt does not claim confirmed native closure.
Progress uses one coalesced observation slot.
Preparation and compatible transitions can emit at most three events per admitted queue entry before control catches up.
The queue admits at most 256 entries.
The worker holds the current decoder and one prefetched decoder.
Decoded packets remain subject to the media adapter's fixed packet and frame budgets.
Output uses one reusable period block and the adapter's prepared scratch buffer.
No decoder work runs in an output callback.

## Transport and queue behavior

`sessions.start` prepares the selected entry, or the first entry if none is selected.
It retains playing intent but begins muted.
The prepared state is `buffering` until explicit unmute permits output and the backend starts.
Core Audio can accept prebuffered frames before native start.
The start receipt remains accepted until native start, or short EOF drain completion.
A native-start observation records stream activation frame zero.
Drain completion without that observation leaves the receipt's activation frame unknown.

Pause preserves paused intent during preparation.
After preparation, pause returns accepted until the worker confirms its backend transition.
Seek also returns accepted until the worker confirms its new position.
Local stop returns accepted until the worker confirms native closure.
Wait for the operation to settle before treating either transition as applied.
Unmute does not resume a paused session.

The ALSA pause path reads the backend consumption estimate, drops pending output, and reprepares the same handle.
It seeks the source to the estimated consumed cursor.
The next stream epoch starts at output position zero.
This may introduce a pause discontinuity on physical hardware.
It does not promise hardware-native pause or a measured DAC cursor.
The file-to-null acceptance path proves exact byte continuity for this fallback.

Source positions refer to frames within a queue entry.
Output positions refer to the current output stream epoch.
The worker maintains spans across compatible joins to map output consumption to an entry and source position.
Decoded position can lead submitted position.
Submitted position can lead the backend consumption estimate.
The snapshot identifies the measurement boundary and preserves unknown consumed frames and backend delay as null.
The top-level audible position is also null when the backend cannot measure it.
The source format remains separate from the actual backend client format.
The typed backend observation records client and device formats, its measurement boundary, and preservation state.
ALSA system routes do not imply a known device format.
The modified flag accounts for both gain processing and known backend conversion.
No adapter claims DAC preservation from successful negotiation alone.

The internal restart cursor identifies a safe software reprepare boundary.
For ALSA it uses the client consumption estimate.
For Core Audio it uses callback delivery.
Callback delivery does not establish DAC presentation.
Frames delivered before a Core Audio reprepare can be dropped before physical presentation.
Physical pause continuity and gapless playback remain unqualified for that path.
Repreparing from that boundary never replaces unknown audible timing with a fabricated number.

Appending entries is allowed during playback.
The worker reserves one next entry before preparing its decoder.
State exposes `active_entry_id` and `reserved_next_entry_id`.
Removing or moving those entries, or earlier entries, requires stopping first.
Reorder destinations must also remain beyond the reservation.
Selecting another entry requires stopping first.
Farther future entries remain editable.
The worker observes committed future queue order before reserving subsequent entries.

Queue commits recheck the worker's actual reservation after durable staging.
If a track became protected during the write, the owner restores the previous checkpoint and fails the edit.
A failed rollback stops the owner.
The reservation gate contains only bounded memory work.
It never contains socket reads, decoding, checkpoint writes, or directory syncs.
An append that arrives after end-of-queue draining begins remains queued for a later start.

Natural EOS drains the backend once.
ALSA hardware draining begins once, then native state determines completion.
Repeating hardware drain calls can reinstall end-silence padding.
Ordinary I/O readiness does not establish successful drain completion.
Stop, cancellation, and failure drop pending output instead.
Fault reports retain the final submitted count and clear unavailable clock measurements.
A worker fault mutes the session and revokes signal consent and fixed-output arming.
Recovery requires an explicit control action.
The worker never performs automatic xrun recovery, route fallback, SRC, or implicit format conversion.
Explicit Core Audio float compatibility reports modified processing and its actual float client format.

## Safety and persistence

Output selection does not open a device or grant consent.
Output consent targets the current device and profile.
Starting playback can open that approved route but cannot emit before explicit unmute.
Changing the route or profile loses consent and fixed-output arming.
Hardware volume remains unsupported until a backend implements set and verified readback.

Software volume begins at minus 40 dB and is visibly modified processing under compatible conversion.
Strict unmute requires unity gain without a software ramp.
Fixed output additionally requires explicit arming for the current device and profile.
Caps remain independent of profile configuration.
Checkpoint restoration preserves caps and desired output configuration but drops consent and arming.
It restores paused and muted without opening an output.

## Hardware approval

Doctor Biz explicitly approved hardware playback and evidence during this T08/T09 task.
The approval covers short generated signals on the previously inspected explicit ALSA and selected Core Audio routes.
Refresh its identity before opening or writing.
Use a bounded software ramp at minus 40 dB or lower.
Preserve hardware and mixer gain.
Do not sweep formats or rates.
Keep raw device identifiers and reports in private artifacts.
Publish only redacted results for that route.
An opened or exercised route does not prove digital preservation or acoustic level.
The permission does not extend beyond those selected outputs or their approved signal procedures.

Complete the code and fault checks before this physical run.

The final selected Linux run passed with 48,000 submitted frames and matching client consumption estimates.
It preserved the generated source and mixer settings.
The selected Mac engine run passed muted preparation, pause, seek, resume, and natural drain.
It preserved the generated source and device settings.
See the scoped redacted results under `docs/evidence/local-playback/`.
Earlier failures remain alongside successful evidence.

The Linux physical run refreshed the approved identity and submitted one second at minus 40 dB.
All 48,000 frames were accepted before a terminal output fault.
The owner muted and revoked consent.
Consumed position and backend delay remained unknown after the fault.
The mixer snapshot and source digest remained unchanged.
No fallback or signal retry occurred.
This result establishes an exercised route, not successful completion or digital preservation.
Private raw reports and the redacted result remain under `target/local-playback/private-hardware.eyy6js`.
