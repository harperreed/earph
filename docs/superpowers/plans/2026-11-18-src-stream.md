# Bounded SRC stream implementation

Root owns Core graph admission, the compiler, native replay, and engine integration.
This task owns the SRC design split and a bounded scheduler over real DSP stages.
Do not expose an accepted SRC graph node before the whole product path exists.

## Contracts

Keep the reviewed SRC output and coefficient parameters unchanged.
Resolve immutable peak, resource, and fingerprint facts before instantiating a kernel.
Compose prepared same-rate islands and SRC stages without duplicating their algorithms.
Require explicit compiled input bounds for every island.
Require PEQ to follow the last SRC until a stronger composition proof exists.
Keep original source acceptance separate from internal stage input and final output.
Drain each stage only after its predecessor and FIFO finish.
Preserve one SRC guard across compatible source joins.
Retain all storage through reset, cancellation, and failure.
Keep caller output unchanged on failure.

## Work

- [x] Run the unchanged public SRC release baseline.
- [x] Prove missing design admission APIs with a failing compile.
- [x] Split resource preflight, coefficient analysis, and instantiation.
- [x] Pass sixteen release scenarios and regenerate numerical receipts.
- [x] Verify unchanged rendered output and response CSV hashes.
- [x] Add real streaming composition, backpressure, priming, lifecycle, and failure scenarios.
- [x] Implement bounded FIFO scheduling and exact stage counters.
- [x] Integrate the filter owner's bounded island compiler dependency.
- [x] Exercise PEQ/FIR after actual SRC amplification with compiled input bounds.
- [x] Run allocation and canonical checks.
- [x] Complete independent stream review.
- [x] Commit only this task's reviewed files.

## Shared dependency

The filter owner supplies `compile_with_input_bounds(document, context, assets, input_bounds)`.
Its plans expose admitted input bounds, output bounds, preparation scratch, and bounded IIR truncation.
The stream scheduler validates those facts before creating runtime stages.
The scheduler does not copy compiler peak formulas or assume a normalized bound after SRC.

The reviewed dependency is integrated through canonical commit `0375fcf`.
Sixteen real streaming scenarios pass through that compiler and the actual kernels.
The stream constructor accepts the worker's shared cancellation token.
It checks that token between stage work units and before public output publication.
Native replay and history-preserving epoch rebind remain outside this unit.
Two further regressions fail before their corrections and pass afterward.
IIR must occupy the final island because truncation bounds do not compose across separate stages.
Finished streams still reject partial-frame output geometry.
The canonical check found one test-style Clippy error.
The corrected canonical check passes 456 tests, with three ignored and no warnings or errors.
Independent root review approves the SRC design split.
That two-file split is saved in commit `f9942f0`.
Independent root review approves the stream implementation without material findings.
Public-module export and renewed public-import verification remain parent-owned.
Retiring an unactivated candidate must not cancel a token shared with active playback.

## Evidence

The baseline release suite passes fourteen scenarios.
The design split passes sixteen release scenarios.
The numerical writer passes thirty cases and ten drift traces.
Rendered output retains SHA-256 `1d32af744c39629341c4e8c1b6254fce0cc3da5a8cfacfed6bdf898b4c438c45`.
Response CSV retains SHA-256 `04f914b313cb3e6af69c82413e2361cf77630b37323579853a2f82475ab0e114`.
Logs and receipts remain under this worktree's ignored `target` directory.
The canonical log is `target/src-stream-canonical-final.log`.
Its SHA-256 is `29af6e2bb36b8f95f82a42a4aead3997668cfa550e7f6c70a36eceb6890f3e1e`.
No hardware, private media, publication, or deletion belongs to this task.
