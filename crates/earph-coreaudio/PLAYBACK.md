# Core Audio playback

`CoreAudioOutput` opens one explicit device UID.
DirectHardware owns one HAL IOProc; System owns one AudioQueue.
Opening never starts the device.
`start(SignalPermission::Granted)` and `resume(SignalPermission::Granted)` can emit audio.
`Denied` fails before either native start call.
The application must apply its shared volume and admission policy before granting permission.

`OutputRequest` fixes the source rate, channel layout, integer representation, application period, ring capacity, and route.
The adapter never changes device properties, gain, hog ownership, or the system route.
The ring holds at most 65536 frames.
Each HAL callback handles at most 8192 mono or stereo frames.
System uses fixed SDK slots partitioned by the application period, including a final fractional slot.
Controlled audio storage includes both the Rust staging ring and those SDK slot capacities.
It excludes metadata, allocator overhead, and opaque OS converter memory.
Writes accept complete frames and may return a partial count or `WouldBlock`.
Retry from the accepted offset.
Epochs and contiguous frame positions must match the opened stream.

`StrictInteger` requires the current physical and virtual ASBDs to match the exact integer representation.
Packed signed 24-bit data therefore conflicts with a physical 24-in-32 stream or virtual float32 stream.
The adapter packs integer bytes without floating-point conversion.
Matching settings and callback delivery do not prove DAC preservation.
The report keeps payload preservation unverified.

`Float32Compatibility` uses AudioQueue with interleaved Float32 at the source rate.
The producer converts integer samples to normalized Float32 before publishing complete frames.
AudioQueue performs any further OS conversion to the selected device.
Its client ASBD is freshly read and verified before use.
The report preserves that client ASBD separately from observed physical and virtual hardware formats.
The exact selected UID is set and read back before enqueue and start.
Unsupported binding or different readback fails without choosing another device.
The report marks payload preservation modified and downstream transformations unknown.
Hardware rate, buffer, stream formats, hog ownership, and system volume are never set.
Application geometry64/256 can prepare against observed hardware48k/512 without requesting hardware changes.
Actual driver conversion and unchanged settings still require Mac qualification.
No DSD route exists here.

`progress()` counts producer submissions and route-specific acquisition separately.
HAL keeps its existing IOProc delivery counter.
System keeps that counter zero and publishes AudioQueue acquisition in `queue_acquired_frames`.
Buffer release permits slot reuse; it does not prove playback.
`restart_cursor_frames()` returns HAL delivery or a conservative System payload prefix.
System maps queue time through each buffer’s actual scheduled interval.
It retires whole buffers, with no promise of sub-buffer precision.
Fractional schedules retire at `ceil(actual_start) + frames`, without rounding an endpoint down.
Queue time and payload frames have separate origins.
Queue time can advance through gaps without advancing payload retirement.
System validates timestamp flags, finite range, precision, monotonicity, and timeline continuity.
Pause captures a fresh mapped prefix while running.
A successful native pause freezes that prefix before disposal and epoch replacement.
A post-pause clock cannot advance the frozen prefix.
Unknown timing never becomes an acquisition-based restart position.
Callback delivery does not establish hardware consumption or acoustic presentation.
Backend delay and audible position remain unknown.
No SDK timestamp establishes a qualified physical timing estimate in this adapter.
For HAL, `callback_timing()` reports valid SDK output host ticks and sample time when available.
System leaves that HAL timing query unknown.
Native start and stop invalidate prior timestamp generations.
These observations describe SDK scheduling and do not prove DAC presentation.
The tested device briefly reported different device-clock and IOProc sample epochs after start and resume.
Do not infer presentation from their numerical order before establishing their clock relationship.
For HAL, `scheduled_output_complete()` returns an optional SDK hardware-handoff result after explicit EOF.
It requires matching submitted and delivered totals and the current generation's last payload-bearing buffer.
It translates sample times through the running device and checks paired sample and host clocks.
Inconsistent epochs, unsupported clocks, missing timestamps, and inactive output return unknown.
Unexpected native clock errors fault the stream.
Later silence callbacks do not advance the tracked payload boundary.
The check concerns the SDK buffer handoff and does not establish DAC or acoustic presentation.

For System, EOF enqueues the exact short tail and requests `AudioQueueStop(false)` once.
Completion requires the current generation's observed running-to-stopped transition after that request.
Stop resets the queue timeline, so no clock read advances retirement after the drain request.
While draining, the last proven payload prefix remains held.
Observed natural completion retires the remaining enqueued payload before final counters and disposal.
Initial stopped state, returned slots, and immediate cancellation never prove natural EOF.
The Engine retains its bounded drain deadline.
For HAL, `finish()` closes admission and lets callbacks drain a partial buffer with silence padding.
This drains the application ring, not a measured hardware queue.
The IOProc stays running until an explicit `stop()` or `close()`.
End-of-track hardware presentation remains unqualified.
An unmarked underflow produces silence and a terminal fault.
Device liveness, rate, buffer, stream-list, or stream-format notifications also fault the stream.
There is no fallback or automatic restart.
HAL `pause()` stops the owned IOProc and retains frames not yet delivered through the callback.
System `pause()` pauses AudioQueue and freezes its validated payload prefix.
`resume()` requires permission and rechecks current settings.
Hardware may discard frames already delivered before a pause; gapless pause is unqualified.
`stop()` and `close()` cancel queued frames and permanently reject further writes.
Open a fresh output for a new epoch or route.

Every IOProc registration owns one unique stable callback context.
Property listeners reference only a separate static fault flag.
The process reserves 65536 eight-byte flags in a fixed 512 KiB pool and never reuses them.
Late notifications can fault only their retired generation, without retaining an audio ring.
After the pool exhausts, output preparation returns `ResourceBudgetExceeded` and requires a process restart.
The pool also counts valid preparation attempts that later fail registration.
Apple documents listener removal as unregistration without an in-flight callback quiescence promise.
See [the SDK contract](https://developer.apple.com/documentation/coreaudio/audioobjectremovepropertylistener(_:_:_:_:)).
Checked shutdown stops the IOProc, removes listeners, and destroys the registration.
An uncertain SDK registration, start, stop, listener removal, or destruction outcome retains that context until process exit.
The adapter reports the failure and never retries an uncertain lifecycle operation.

The callback uses preallocated atomic byte storage and scalar atomics.
It performs no allocation, deallocation, logging, file access, property queries, locks, or UI calls.
Native unit tests invoke the real callback ABI with allocated SDK `AudioBufferList` objects.
A test allocator checks callback allocations and frees.
Native integration tests register callbacks and queue owned test samples without starting devices.
They verify permission denial, ownership, cleanup, and unchanged device settings.
These silent tests do not qualify signal emission, real-time deadlines, pause timing, or DAC preservation.
The separately gated [hardware acceptance](HARDWARE-ACCEPTANCE.md) requires explicit playback approval.

The System buffer callback only validates its fixed slot and publishes acquisition and availability.
It performs no PCM rendering, decoding, allocation, logging, locks, or waits.
Its running listener reads only the queue running property and publishes transition bits.
The worker owns all copying, enqueueing, timing queries, pause, drain, cancellation, and synchronous disposal.
Successful `AudioQueueDispose(true)` quiesces callbacks before callback storage is freed.
Queue disposal also disposes its associated timeline under Apple's ownership contract.
An uncertain disposal retains callback storage and prevents another disposal attempt.
CFString property readbacks use Create-rule ownership.

System's client-frame enqueue window stays within one ring capacity ahead of its retired payload prefix.
`AudioQueueEnqueueBufferWithParameters` supplies the actual start timestamp for every buffer.
Zero trims, zero parameters, and a null requested start preserve ordinary FIFO playback.
Schedule intervals must be nonnegative, precise, nonoverlapping, and consistent with contiguous payload and output epochs.
The worker reserves metadata space before staging another SDK buffer.
The prepared ledger holds at most one record per ring frame.
Each record represents at least one outstanding payload frame, so the existing enqueue window proves this bound.
Returned PCM slots do not release schedule records.
Whole-buffer retirement releases those records independently.
Ledger metadata does not count as PCM storage and allocates only during preparation.
Slot release alone never expands that window.
`queued_frames` counts controlled staging and SDK slots; it excludes PCM already acquired into opaque OS storage.
The existing activity tracker counts adapter write and start entries across native reopens.
Its public `sdk_write_attempts` name does not mean individual `AudioQueueEnqueueBuffer` calls.

AudioQueue start and resume requests can precede the actual running transition.
The adapter distinguishes that transition from the first valid client clock for each request.
Before that clock, only `kAudioQueueErr_InvalidRunState` may remain pending within the five-second progress deadline.
An unknown clock returns no restart cursor and grants no clock-derived enqueue credit.
Repeated pending reads never reset the deadline.
A validated advancing payload prefix replenishes the bounded window.
Queue-time movement through gaps never refreshes the five-second payload-progress deadline.
Other native errors, post-ready clock loss, invalid timestamps, and unexpected stops remain faults.
Same-queue resume preserves the schedule origin and starts a new bounded readiness episode.
It validates raw queue continuity against the last running clock, without rebasing or taking a hidden maximum.
Pause still requires a validated capture before freezing its frontier.
Diagnostics retain optional last-observed running state, native clock status, and clock readiness.
HAL leaves those queue-only fields unknown.

Clock diagnostics preserve the raw sample-time bits, finite numeric value, timestamp flags, and discontinuity indication.
They also preserve the enqueued count observed at that read and the last accepted whole queue frame.
`queue_retired_frames` separately reports the mapped payload prefix.
`queue_clock_observed_enqueued_frames` records context, not a bound on queue time.
`queue_schedule` retains the latest returned actual-start bits, flags, payload span, and output epochs before validation.
Invalid returned schedule timestamps therefore retain their exact raw evidence.
The native enqueued count includes every successful SDK acceptance, including a buffer with unusable schedule evidence.
Such a schedule faults the queue without advancing retirement or replacing older schedule records.
The submitted and acquired counters retain their existing separate boundaries.
An unsuccessful clock call clears the raw-read fields instead of presenting initialized storage as a returned timestamp.
NaN and infinity retain exact IEEE754 bits while their finite-value field remains unknown.
Non-finite, negative, precision-range, backward, and discontinuous active clocks remain faults.
A queue clock beyond the number of enqueued payload frames is valid when its scheduled intervals explain the mapping.

The System registration metadata uses one boxed allocation during preparation, outside all callbacks.
Its separately boxed callback context keeps the same stable address and checked disposal lifetime.
This metadata allocation does not change reported PCM storage capacity.

A finite negative sample time may precede the queue's zero origin during a bounded start or resume episode.
With a valid sample-time flag and no discontinuity, it remains unknown before that episode's first accepted clock.
It does not set a restart cursor, grant enqueue credit, advance the stored frontier, or refresh the five-second deadline.
After readiness, a negative sample remains a fault.
Pause capture remains strict, and same-queue resume keeps the previous validated schedule origin.
This readiness handling does not establish the exact physical scheduled start time.
