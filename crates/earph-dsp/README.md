# Earph graph compiler

Compile versioned graph intent into a bounded, deterministic execution schedule.
This crate currently implements compilation only.
Prepared execution and session activation require separate implementations and qualification.

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
The initial node kinds are input, output, bypass, gain, delay, matrix, sum, and meter.
DSD, fractional delays, implicit conversions, resampling, PEQ, and convolution are unsupported.
Unknown schema versions, node kinds, and fields reject.

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
Resources report node output bytes, combined delay state bytes, and their total.
Delay state bytes include merge compensation.
This budget covers audio buffers; bounded document and schedule metadata use separate ordinary allocations.
Runtime implementations must not allocate additional audio buffers without updating compiler accounting.

Hard admission limits are 128 nodes, 256 edges, and 8192 frames per quantum.
The default audio buffer budget is 256 MiB.
Callers may admit a larger byte budget explicitly or reduce any limit.
Callers declare an optional latency ceiling through `max_latency_frames`.
The default does not impose an additional latency ceiling.
Checked arithmetic and the platform vector-size limit apply before preparation.
The compiler never removes processing to fit a budget.

Strict preservation rejects gain, matrix, sum, nonzero delay, and compensation transformations.
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
Context includes source/output formats, budgets, policy, and quantum.
Activation tokens must bind the resulting hash to source/device epochs and the expected session revision.
The compiler does not introduce another revision authority.

Run `cargo test --locked -p earph-dsp --test compiler` for compiler contract tests.
The tests compile real typed documents and serialized round trips without mocks.
They check topology, format boundaries, deterministic scheduling, compensation, preservation, and resource rejection.
The round-trip test retains its branch fixture and resolved plan under `target/dsp-compiler-fixtures/`.
They do not qualify execution, real-time allocation behavior, hardware playback, or activation transactions.
