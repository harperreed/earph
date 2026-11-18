# Earph graph compiler

Compile versioned graph intent into a bounded, deterministic execution schedule.
This crate compiles graphs and executes prepared f64 plans.
The engine owns [session activation, native rendering, and retirement](../../docs/dsp-graphs.md).
Native qualification and whole-worker real-time guarantees remain separate gates.

Call `compile(&GraphDocument, &CompileContext)` before preparing processing state.
The context declares source and output PCM formats, processing quantum, preservation policy, and admitted resource limits.
The compiler does not allocate audio buffers or change session state.

Documents contain exactly one input and one selected output.
Every node must contribute to that output.
Node identifiers follow the core opaque identifier rules.
Node and edge list order does not affect the compiled plan or hash.
The schedule chooses the smallest ready node identifier first.

Each node declares its output format and maximum quantum.
An ordinary node requires one input with the same format.
Matrix nodes declare a separate input channel layout.
Only PCM, mono/stereo layouts, and a single connected rate domain are supported.
The node kinds are input, output, bypass, gain, delay, matrix, sum, meter, PEQ, and convolution.
DSD, fractional delays, implicit conversions, and graph resampling remain unsupported.
Unknown schema versions, node kinds, and fields reject.
See [correction filters](../../docs/correction-filters.md) for parameters, imports, and finite tail contracts.

Use `compile_with_assets` to supply verified immutable correction coefficients.
Use `compile_with_input_bounds` for stream islands with explicit per-channel input bounds.
Explicit bounds are enforced before processing exposes output.
Default structural plans retain their unrestricted processing contract and describe normalized-input peak bounds.
Filter plans enforce normalized input bounds by default because their tail proofs require them.
Compiled plans expose the input, output, and terminal channel bounds.

Edges use source port zero.
Ordinary nodes consume destination port zero.
Sum nodes consume unique contiguous destination ports starting at zero.
Repeated source nodes are permitted on different sum input ports.
Sum inputs require matching rates, precision, and channel layouts.
Matrix coefficients use output rows and input columns in row-major order.
Gain values use a finite linear multiplier.
Matrix coefficients must be finite and match the declared channel counts.

A delay of N frames emits N initial zero frames and shifts its input by N frames.
Its scheduling latency and final tail both equal N frames.
At each merge, the compiler delays shorter input paths to the longest incoming scheduling latency.
Each compiled input records its source schedule index, destination port, and compensation frames.
Compensation uses the same integer delay semantics as a delay node.
The resulting tail includes each path's delay and compensation exactly once.
These delays align scheduling only; they do not correct arbitrary frequency-dependent phase response.

Processing uses interleaved f64 samples.
The quantum bounds frames, while channel counts determine scalar sample counts.
Runtime implementations must allocate one output buffer per scheduled node.
Each output holds `quantum_frames * output_channels` f64 samples.
Each delay owns `delay_frames * output_channels` f64 state samples.
Each compensated input owns `compensation_frames * input_channels` f64 state samples.
Filters also own an input staging buffer and a boxed prepared kernel.
Resources report node buffers, delay state, and the complete resident processing payload.
Delay state bytes include merge compensation.
The total includes filter objects, FFT storage, and unique retained coefficient owners.
Plans expose separate kernel state, asset, and preparation scratch totals.
This budget covers audio buffers; bounded document and schedule metadata use separate ordinary allocations.
Runtime implementations must not allocate additional audio buffers without updating compiler accounting.

Hard admission limits are 128 nodes, 256 edges, and 8192 frames per quantum.
The default audio buffer budget is 256 MiB.
Callers may admit a larger byte budget explicitly or reduce any limit.
Callers declare an optional latency ceiling through `max_latency_frames`.
The default does not impose an additional latency ceiling.
Checked arithmetic and the platform vector-size limit apply before preparation.
The compiler never removes processing to fit a budget.

Strict preservation rejects gain, matrix, sum, PEQ, convolution, nonzero delay, and compensation transformations.
Gain and matrix nodes remain transformations even when their parameters resemble an identity.
Input, output, bypass, meter, and zero-frame delay preserve sample values.
An unchanged integer graph returns `RetainInteger` with zero f64 buffer bytes.
The owner must retain its existing integer payload path for that decision.
An unchanged f64 graph returns `UnchangedFloat64`.
A transformed f64 graph returns `ProcessFloat64`.
Processing policy never authorizes an implicit integer-to-f64 conversion.
Integer retention alone does not implement integer metering; the owner must provide an appropriate observation path.

Compiled plans expose read-only metadata and cannot deserialize from untrusted JSON.
The SHA-256 hash includes canonical intent, the compilation context, and the implementation version.
The hashed context includes source/output formats, policy, and quantum.
Filter hashes also include algorithm identities, coefficient bits or fingerprints, and the selected tail policy.
Admission budgets can change acceptance without changing the hash when both budgets admit the graph.
Activation tokens must bind the resulting hash to source/device epochs and the expected session revision.
The compiler does not introduce another revision authority.

Run `cargo test --locked -p earph-dsp --test compiler` for compiler contract tests.
Set `EARPH_DSP_FIXTURE_DIR` to a writable directory to export graph and plan fixtures.
The default tests serialize and parse in memory without writing into the checkout.
The tests compile real typed documents and serialized round trips without mocks.
They check topology, format boundaries, deterministic scheduling, compensation, preservation, and resource rejection.
Opt-in export retains both files under `EARPH_DSP_FIXTURE_DIR/<pid>-<nonce>/`.
They do not qualify execution, real-time allocation behavior, hardware playback, or activation transactions.

## Prepared execution

Call `PreparedGraph::prepare(plan, epochs)` outside the processing thread.
Preparation allocates the exact audio storage declared by `plan.resources()`.
Each node owns one quantum output buffer.
Delay nodes and compensated inputs own their declared interleaved delay rings.
Preparation allocates filter staging and kernel scratch before processing starts.
Allocation failure returns `ResourceBudgetExceeded` before activation.
`RetainInteger` plans return `CapabilityUnsupported` from f64 preparation.
Keep those plans on the existing integer payload path.

Call `process(input, output)` with finite interleaved f64 samples.
Input and output spans must contain the same number of whole frames.
Output channel counts can differ through explicit matrix nodes.
Each span can contain at most the compiled quantum, including shorter final spans.
The returned count measures frames, not scalar samples.
Zero-frame processing preserves the lifecycle and does not create a tail.
Geometry errors preserve delay state, meters, and lifecycle.
Finite output above full scale passes through without clamping.
Filter plans and explicitly bounded plans reject inputs above their admitted channel peaks.
Non-finite input or any non-finite node output moves the graph to `Failed`.
Failed calls leave the caller's output unchanged.
Resolve `failure_node_index()` through `plan().schedule()[index].id()` for the failing node.
Hot-path errors omit owned node IDs to avoid allocation.

Lifecycle starts at `Prepared` and becomes `Running` after nonempty processing.
Call `finish(output)` only at the final source boundary.
Compatible source joins continue through `process` without resetting state.
Each finish span supplies whole output frames within the compiled quantum.
A pending tail requires positive output capacity.
Finish writes only the returned valid prefix and preserves the remaining output suffix.
The graph becomes `Draining` while tail frames remain, then `Finished`.
Repeated finish calls return zero after completion.
An empty stream finishes immediately without inventing a tail.
Finished graphs reject further source input.

`reset(new_epochs)` accepts a newer `(session, stream)` pair in lexicographic order.
A newer session may restart its stream counter at zero.
Reset clears buffers, rings, meters, and failure location in place.
It returns the graph to `Prepared`, including recovery from a failed prior stream.
`cancel()` moves permanently to `Cancelled` and retains all prepared resources.
Cancellation cannot be reversed by reset.
The owner must dispose of the graph outside the processing thread.

`meter(node_id)` returns an observation only for meter nodes.
`peak_abs` measures the largest absolute scalar sample since preparation or reset.
`sample_count` counts scalar samples, including valid final tail frames.
Meter observations never alter audio or allocate memory.
Process, finish, reset, and cancel perform no allocation, deallocation, locks, logging, or file access.
These methods require exclusive mutable ownership of the prepared graph.

Run `cargo test --locked -p earph-dsp` for compiler, kernel, and execution tests.
Execution tests compile real graph documents and compare actual captures with literal expected samples.
They cover mono/stereo matrix routing, gain headroom, sum order, branch compensation, and exact tail counts.
Split-source execution is compared with an unsplit reference without resetting the graph at joins.
An instrumented global allocator checks success, malformed input, non-finite faults, reset, and cancellation.
These tests qualify offline execution; they do not qualify graph activation or hardware playback.

## Streaming components

`resample::SrcDesign` resolves SRC coefficients and parameter fingerprints outside processing.
Its resource preflight allocates no storage.
`instantiate` creates independent mutable state using the same admitted construction bound.
The standalone `PreparedSrc::prepare` composes those operations.

`stream::PreparedStream` composes prepared same-rate islands and SRC stages through fixed FIFOs.
Its plan checks formats, input peak certificates, and complete preparation resources.
Bounded IIR processing must occupy the final island.
Each step reports accepted source frames, produced output frames, bounded work, and terminal state separately.
Zero output during priming or finishing does not mean EOF.
Compatible source packets share SRC history and one guard per stage.
Finalization forwards every valid upstream tail and declared SRC padding frame.

The caller supplies the worker's shared cancellation token.
Cancellation checks occur between stage steps and before output publication.
Failure preserves the caller's complete output span.
Reset requires newer epochs; cancellation remains permanent.
Processing and lifecycle calls retain prepared storage for off-thread retirement.

These components do not yet expose SRC graph commands or a native streaming bridge.
Native replay, timing observations, and graph admission require separate integration and qualification.
