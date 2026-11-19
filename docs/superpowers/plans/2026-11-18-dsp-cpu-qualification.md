# Prepared DSP CPU qualification

Root authorizes this fixed workload contract and its implementation.
The runtime baseline is `2396efb38789038b20c2727afe8bc58409580be9`.
Keep production runtime code unchanged.
Preserve every run, including failed and incomplete artifacts.
Use no physical output or private media.

## Fixed workloads

Use stereo signed24 input and native periods at 48000 Hz.
Compile f64 graphs with the existing 256 MiB default budget.
Set software gain to -6.020599913279624 dB without a ramp.
Measure each workload at 64 and 256 frames.

| Name | Authored chain |
|---|---|
| `same_rate_gain` | Gain 0.5 |
| `same_rate_peq10` | Ten peaking filters |
| `same_rate_fir4096` | Stereo 4096-tap convolution |
| `responsive_roundtrip_correction` | Responsive SRC 48k -> 44.1k -> 48k, FIR1024, four peaking filters |
| `quality_roundtrip_correction` | Quality SRC 48k -> 44.1k -> 48k, FIR1024, four peaking filters |

Set each SRC chunk to the case's quantum.
The PEQ centers are 80, 125, 200, 315, 500, 800, 1250, 2000, 3150, and 5000 Hz.
Each peaking filter uses Q 0.707 and gain -3 dB.
The roundtrip workloads use the final four centers after the final SRC.
PEQ cannot precede later SRC stages under the current bounded-tail contract.

Generate FIR taps with alternating signs and magnitude `0.5 / tap_count`.
Invert the right channel signs.
These exact power-of-two coefficients have L1 0.5 per channel.
Their reference hash covers interleaved little-endian f64 coefficients, not an imported WAV.
Use the committed deterministic integer generator for the repeated source packet.
Its peak does not exceed 1/64 full scale.
Retain the exact packet byte hash, authored graph, compiler hash, and resolved metadata.

## Operations and limits

For same-rate graphs, measure the actual `PreparedRender.process` call.
The test includes the production source through the existing render-bridge test convention.
Do not expose a new production API only for qualification.

For SRC graphs, measure demand for exactly one new native period.
Stage pending output before pumping source.
Advance source spans only by actual acceptance.
Use actual replay, gain, quantization, acknowledgment, and observation methods.
Retain staged suffixes between transactions.
Bound each demand transaction to 4096 scheduler iterations.
The queue contains four native periods.

Software acknowledgments are harness ledger actions.
They do not prove native backend acceptance or physical playback.
Timing includes two output witnesses and their rolling checksum per staged prefix.
It excludes fixture creation, preparation, logging, serialization, sorting, and file I/O.
It excludes decoding, worker scheduling, native I/O, callbacks, driver queues, and acoustic output.
Wall-clock timing includes process preemption.

Measure cold priming separately.
Warm the same prepared instance for 4096 output-demand periods.
Retain 100000 measured full-period transactions without resets or EOF.
Then finish the real graph or stream to its terminal state.
Report tail calls, total duration, and actual counters separately.
Do not mix variable tail work into full-period percentiles.

Use nearest-rank p50, p95, p99, and p99.9.
Report minimum, maximum, total, and actual deadline misses.
Evaluate the strict gate with exact integer arithmetic:
`p999_ns * 48000 * 10 < quantum * 1000000000 * 7`.
Count a deadline miss only when duration exceeds the full period.
Do not subtract measured timer overhead.
Never weaken graphs or thresholds after viewing results.

## Producer and receipts

Use the ignored `release_cpu_qualification_receipts` test explicitly in release.
Set `EARPH_CPU_QUALIFICATION_OUTPUT` to a new private directory.
Run with one test thread and no simultaneous controlled builds or benchmarks.
Do not change clocks, priority, affinity, or power policy.
The ordinary smoke producer uses eight warmup and sixteen measured periods.
Smoke receipts carry no qualification claim.

Write ten raw CSV files and one validated JSON manifest.
Use create-new files and a new private destination directory.
Each successful qualification CSV contains exactly 100000 measured rows.
Retain file lengths, SHA-256 hashes, source bindings, hardware facts, and count frontiers.
Validate rows and recompute statistics through the real receipt consumer.
Bound total CSV output to 128 MiB.
Preserve completed files and a failed status after errors.

## Progress

- [x] Retain missing-API test-first failure.
- [x] Verify exact statistics and strict boundary behavior.
- [x] Run all ten real workloads and allocation checks in smoke tests.
- [x] Reproduce inconsistent terminal-source receipt acceptance before fixing validation.
- [x] Finish receipt count validation and canonical checks.
- [x] Complete one independent review.
- [ ] Freeze a clean release build and coordinate the Linux qualification run.
- [ ] Run the same producer on disaster under root coordination.

This unit does not qualify native deadlines, two-hour endurance, or physical output.
Root owns publishing, platform integration, and any later runtime changes.

The canonical check passes 607 tests, with four ignored and no warnings.
All ten short workload cases pass through the real producer and receipt consumer.
An actual terminal-source count mismatch fails before its receipt-validation fix.
Independent review approves the frozen harness without material findings.
Its receipt SHA-256 is `32126ddb3b580083440cd4d526952b88d68ce44c95fa62f37bba7bd3bc9879fa`.
Commit the approved harness before preparing the release qualification build.
Full release measurements remain pending.
