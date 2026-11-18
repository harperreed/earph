# Select a bounded Rubato adapter

Date: 2026-11-18.

Use Rubato 5.1.0 behind the product-owned DSP interface.
Disable its default FFT resampler feature.
Use pinned audioadapter-buffers 5.2.0 for borrowed buffer views.
Keep source rates, emitted frame counts, and padding explicit.

The official tag resolves to `f8711e2252a419a8f61d5dfe378c9f0da6b2ba7e`.
The published crate digest is `6b4da228e06632d025557f798114057cf27a4c6a71d40024c84d6db82fa5ef4a`.
The isolated source audit verified that digest and matched the tagged processing sources.
It exercised actual library processing, drain, and reset without allocation or deallocation.
That proof supports adoption, but does not qualify the production adapter.

Prepend one virtual zero input frame for each nonempty stream.
Preserve this guard across compatible source joins.
After the final padded chunk, flush `ceil(2L/C)+1` zero chunks.
Retain the returned output and declare the conservative padding.
Keep actual output counts separate from nominal rational duration.
Never infer EOF from a zero-output priming call.
The library's truncated output-delay query does not establish exact phase or group delay.

Inspect the public immutable coefficient rows during preparation.
Bound all cubic phases by 1.25 times the largest row L1.
Add the audited numerical error and underflow allowances.
Do not substitute a frequency-response maximum for that arbitrary-signal peak bound.

Charge private tables, aligned scratch, channel history, and preparation overlap.
The audited outer row allocation needs 24 bytes per row on the tested compiler.
Quality construction requested 6,297,600 bytes in the isolated stereo proof.
The earlier 4 MiB graph preparation allowance cannot cover that construction.
Keep requested allocation bytes distinct from allocator overhead and process RSS.

Use product preset names `quality` and `responsive`.
Qualify their actual parameters and rate pairs before exposing them in a graph.
The draft 128-tap responsive filter lost about 7.67 dB at the proposed passband boundary.
Retain that rejected probe and evaluate 256 taps for the responsive configuration.
Keep the quality candidate at 1024 taps.
These candidate measurements do not constitute end-to-end qualification.

The dependency inventory records selection and partial source qualification.
The production adapter, graph scheduler, CPU deadlines, and native integration remain separate gates.

Sources: [tagged manifest](https://github.com/HEnquist/rubato/blob/v5.1.0/Cargo.toml),
[tagged processing source](https://github.com/HEnquist/rubato/blob/v5.1.0/src/asynchro.rs),
[public interpolator](https://github.com/HEnquist/rubato/blob/v5.1.0/src/sinc_interpolator/mod.rs).
