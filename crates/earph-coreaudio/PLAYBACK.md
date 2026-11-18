# Core Audio playback

`CoreAudioOutput` opens one explicit device UID and registers one owned IOProc.
Opening never starts the device.
`start(SignalPermission::Granted)` and `resume(SignalPermission::Granted)` can emit audio.
`Denied` fails before either native start call.
The application must apply its shared volume and admission policy before granting permission.

`OutputRequest` fixes the sample rate, channel layout, integer representation, callback size, ring capacity, and route.
The adapter never changes device properties, gain, hog ownership, or the system route.
The ring holds at most 65536 frames.
Each callback handles at most 8192 mono or stereo frames.
Writes accept complete frames and may return a partial count or `WouldBlock`.
Retry from the accepted offset.
Epochs and contiguous frame positions must match the opened stream.

`StrictInteger` requires the current physical and virtual ASBDs to match the exact integer representation.
Packed signed 24-bit data therefore conflicts with a physical 24-in-32 stream or virtual float32 stream.
The adapter packs integer bytes without floating-point conversion.
Matching settings and callback delivery do not prove DAC preservation.
The report keeps payload preservation unverified.

`Float32Compatibility` requires an observed interleaved float32 virtual ASBD.
Physical and virtual sample rates and channel counts must match the request.
The producer converts integer samples to normalized float32 before publishing complete frames.
The report marks this payload modified and downstream transformations unknown.
No resampling, channel remapping, device reconfiguration, or DSD route exists here.

`progress()` counts producer submissions and frames delivered through the IOProc separately.
Callback delivery does not establish hardware consumption or acoustic presentation.
Backend delay and audible position remain unknown.
No SDK timestamp establishes a qualified physical timing estimate in this adapter.
`callback_timing()` separately reports valid SDK output host ticks and sample time when available.
Native start and stop invalidate prior timestamp generations.
These observations describe SDK scheduling and do not prove DAC presentation.
The tested device briefly reported different device-clock and IOProc sample epochs after start and resume.
Do not infer presentation from their numerical order before establishing their clock relationship.
`scheduled_output_complete()` returns an optional SDK hardware-handoff result after explicit EOF.
It requires matching submitted and delivered totals and the current generation's last payload-bearing buffer.
It translates sample times through the running device and checks paired sample and host clocks.
Inconsistent epochs, unsupported clocks, missing timestamps, and inactive output return unknown.
Unexpected native clock errors fault the stream.
Later silence callbacks do not advance the tracked payload boundary.
The check concerns the SDK buffer handoff and does not establish DAC or acoustic presentation.

`finish()` closes producer admission and lets callbacks drain a final partial buffer with silence padding.
This drains the application ring, not a measured hardware queue.
The IOProc stays running until an explicit `stop()` or `close()`.
End-of-track hardware presentation remains unqualified.
An unmarked underflow produces silence and a terminal fault.
Device liveness, rate, buffer, stream-list, or stream-format notifications also fault the stream.
There is no fallback or automatic restart.
`pause()` stops the owned IOProc and retains frames not yet delivered through the callback.
`resume()` requires permission and rechecks current settings.
Hardware may discard frames already delivered before a pause; gapless pause is unqualified.
`stop()` and `close()` cancel queued frames and permanently reject further writes.
Open a fresh output for a new epoch or route.

Every IOProc registration owns one unique stable callback context.
Property listeners reference only a separate static fault flag.
The process reserves 65536 flags in a fixed 64 KiB pool and never reuses them.
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
