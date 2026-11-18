# SRC native bridge

Keep source acceptance separate from native output submission.
Use the reviewed stream scheduler and serial-cut compiler.
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

## Source cancellation checkpoint

Graph preparation now requires the worker's actual source cancellation Arc.
The owner captures that token when submitting each graph job.
Replacement Start and source cancellation invalidate the same shared token.
Admission rejects retired sources without reserving a candidate slot.
Preparation checks both job cancellation and source cancellation at its existing checkpoints.
Graph job cancellation and candidate retirement do not cancel the live source token.
Existing same-rate renderers still own their private processing cancellation state.
The stream pipeline will share the captured source token when it is installed.

The public service API regression fails before the required source parameter exists.
Thirty service scenarios pass with explicit test-owned source tokens.
The worker identity scenario verifies the actual Start producer, replacement, and cancellation path.
That supporting accessor has no separate retained test-first baseline.
The combined canonical check passes 566 tests, with three ignored and no warnings.
Independent main-model review approves the source-token foundation without material findings.
It reruns thirty service tests, the actual worker identity scenario, and a real owner/client graph flow.
GraphService stream mode and native worker integration remain unfinished.

## SRC validation and storage checkpoint

GraphValidate now compiles real serial SRC cuts and returns shared stage facts.
Validation creates no runtime pipeline or prepared token.
Actual owner tests preserve revision, epochs, playback, and empty capture.
GraphPrepare with SRC still rejects explicitly until the native worker can consume it.
No SRC graph can bypass the existing renderer.

Native observations capture actual Rust-owned adapter audio storage.
Admission charges that storage beside active pipelines, candidate construction, assets, metadata, and the existing source period.
Core Audio charges two simultaneous rings during close/reopen replacement.
ALSA charges its one retained packing scratch.
Opaque SDK, driver, kernel, allocator overhead, and RSS remain excluded.
Storage observations affect admission and stay outside activation identity.

Stream admission uses the shared bridge preflight before coefficient construction.
Compiler preparation peaks remain the SRC and island authority.
Reported stream components sum to candidate pipeline storage.
Shared preview construction preserves existing transformations, headroom checks, and same-rate values.
The full canonical check passes 579 tests, with three ignored and no warnings.
Independent main-model review approves the nine-file source/test unit without material findings.
Its thirty-three service scenarios and two real owner scenarios pass independently.
Live preparation, native holds, seek, replacement, completion, and platform qualification remain unfinished.
