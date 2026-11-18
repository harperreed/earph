# SRC native bridge

Keep source acceptance separate from native output submission.
Use the reviewed stream scheduler and the forthcoming serial-cut compiler.
Keep Session authoritative for graph activation, consent, and transport.

## Material assumptions

Native graphs return to the already-open output rate.
ALSA requires its negotiated queue geometry to equal the request.
Core Audio retains an explicitly bounded producer ring.
Both adapters expose restart cursors with non-acoustic meanings.
Replay retains pre-volume f64 output for the native queue plus one staged period.
Reject a restart cursor outside retained history.
Keep one global output ledger across backend counter resets.
Pause and mute preserve the DSP frontier.
True seek resets every stage and replay span.
Graph replacement declares its source boundary and discarded held output.
Never infer an audible source cursor through SRC.

## Verifiable work

- [x] Implement a bounded replay ledger with checked counters and no processing allocations.
- [x] Verify wrapping, partial submissions, repeated holds, invalid cursors, epochs, cancellation, and reset.
- [x] Compose real SRC output with replay and compare against uninterrupted output.
- [x] Add shared gain and single quantization after replay selection.
- [x] Add unequal-rate offline file rendering through the compiled stream plan.
- [ ] Integrate live preparation resources and actual worker cancellation ownership.
- [ ] Integrate native source/output counters and terminal stream completion.
- [ ] Test actual owner/native flows, queue joins, holds, seek, replacement, and retirement.
- [ ] Run canonical checks and independent main-model review.
- [ ] Freeze the runtime and run the approved platform qualification.

Doctor Biz authorizes parallel implementation and hardware evidence.
Use only owned generated quiet signals within the retained approval.
Preserve private media, existing endpoints, settings, worktrees, and artifacts.

## Replay checkpoint

The isolated output ledger passes six focused scenarios.
A real PreparedStream output matches standalone SRC output after partial submissions and a held restart.
Counter exhaustion preserves retained samples and never wraps.
An instrumented shared test allocator confirms zero processing allocations and frees.
The existing thirteen render bridge scenarios pass after extracting that shared allocator helper.
Focused strict Clippy passes.
The new ledger is not wired into production playback yet.
Full canonical verification and independent review remain required before integration.

During a hold, preserve decoder packet and kernel history epochs.
Advance only the wrapper's output/control binding.
True seek updates source admission and uses the normal newer-epoch stream reset.
Never call PreparedStream.cancel while retiring a candidate that shares the live source's cancellation token.
Drop retired candidates off the processing thread after revoking their ownership.
Charge replay, native periods, conversion spans, staged output, and metadata beside stream preparation peaks.

## Stream render checkpoint

The public wrapper starts from retained failing API tests.
Actual source acceptance and native writes use separate counts.
Gain and the existing shared quantizer operate after selecting retained pre-volume samples.
Two prepared native periods preserve the last published period across cancellation.
The focused publication-cancellation test was added after its final check; focused test-first proof is absent.
It now exercises the real prepare/publish seam without a synthetic backend or app mock mode.
An invalid held restart reproduces a nonterminal output fault before the retention fix.
The corrected path blocks output until a true reset.
Real 48k-to-96k-to-48k output matches standalone stages through partial signed24 submissions.
Instrumented processing, staging, and submissions allocate and free nothing in that scenario.
The wrapper is a library component; offline CLI and native worker integration remain unfinished.

The frozen library canonical check passes 469 tests, with three ignored and no warnings.
Independent main-model review finds no material issues in the declared library scope.
The reviewer verifies all eight frozen file hashes and reruns the meaningful component scenarios.
The review does not qualify worker, CLI, or hardware integration.

## Live graph contracts

Use one Core definition for immutable SRC parameters, resources, and compiled stage facts.
Keep ordinary DSP reexports and serialized fact bytes unchanged.
Share stage facts through one Arc array between compilation and graph inspection.
The isolated shared-facts unit owns those moves before root changes GraphPreview.

Represent same-rate and streaming timing as explicit variants.
Keep existing same-rate numeric timing fields in serialized observations.
Expose streaming stage facts without inventing an aggregate latency or exact tail.
Keep audible source position and entry unknown through SRC.
Observe truncated library delay only after actual preparation.

Capture the worker's actual source cancellation token when submitting graph preparation.
Reject preparation after that source retires.
Drop unused stream candidates off-thread without cancelling their shared live source.
Use the shared bridge resource preflight before compiler coefficient allocation.
Charge active pipelines, candidate construction, assets, metadata, native buffers, and replay together.

Advance decoder offsets only by accepted source frames.
Advance native periods only after actual output acceptance.
Keep decoder packets and DSP history across source-bound holds.
Use the accepted source frontier for a declared muted graph replacement.
Report discarded held output at replacement.
Finish the stream only after the real final source EOF.
Start native drain only after the stream is terminal and all valid output was submitted.

## Bridge admission checkpoint

StreamRenderConfig now exposes allocation-free bridge geometry admission before coefficient compilation.
The prepared wrapper uses the same implementation for its complete resource check.
It charges input conversion, output staging, replay, both native periods, and wrapper ownership.
Native backend storage remains separate.
Public API tests fail before the extraction and pass with it.
The complete canonical check passes 525 tests, with three ignored and no warnings.
Independent main-model review approves the two-file extraction without material findings.
The reviewer reruns five wrapper scenarios and ten real offline CLI scenarios.
Live GraphService admission still needs this helper and actual worker cancellation ownership.
