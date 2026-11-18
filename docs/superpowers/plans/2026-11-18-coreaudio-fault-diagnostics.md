# Core Audio first-fault diagnostics

Preserve the first observed native fault without changing fail-closed playback policy.
This unit does not identify the historical T11 failure retrospectively.
Do not change buffering, startup thresholds, routing, volume, or retry behavior.

## Evidence and authority

Doctor Biz authorizes implementation, parallel work, and scoped hardware evidence.
The single Mac signal for `2396efb` failed and remains consumed.
Never rerun its wrapper or remove its guards.
Its final observation reports OUTPUT_FAULT, 4095 accepted source frames, and 2993 producer-ring writes.
Generated output is 3132 frames; retained output starts at 2560.
Exact native error detail was not retained.
Starvation fits those counters, but multiple native causes share the existing fault byte.
No cause is confirmed by those counters alone.

## Contracts

Expose OutputFaultReason and OutputFaultDetail through the existing Core Audio playback module.
Detail contains the first reason and optional requested/queued frame counts.
Keep OutputProgress.fault and derive its detail and Failed state from one fault-word snapshot.
Other counters remain current observations, not an atomic first-fault cursor snapshot.
Unknown codes remain terminal and report Unknown.

Use one packed AtomicU64 compare_exchange from zero to publish the entire first-fault record.
Later notifications and cleanup cannot replace it.
Use no allocation, logging, spinning, waiting, or panic while recording callback faults.
Represent requested frames through 8192 and queued frames through 65536 exactly.
If a count is unrepresentable, report it unavailable instead of truncating or clamping.
Keep the static 65536-slot arena and never reuse or release listener contexts.
The arena grows from 64 KiB to 512 KiB of bounded lifetime metadata.
Keep that metadata separate from actual ring audio storage and RSS claims.

Classify each existing fault at its actual producer.
Do not add new fault conditions or invent panic containment.
Keep native ownership, shutdown, stale epochs, and late-notification safety unchanged.

At the Engine adapter's non-callback error boundary, write one structured diagnostic containing actual native error and progress detail.
Include operation and actual current counters without device names, UIDs, paths, or source data.
Do not infer native acceptance, acoustic output, or a cause from missing detail.
Keep the returned EngineError and terminal Session authority unchanged.

## Ownership

The native agent owns Core Audio playback.rs, playback/fault.rs, playback/native.rs, and their existing tests.
Root owns Engine output/coreaudio.rs, output/coreaudio/native.rs, and the task note.
Root integrates and dispatches one independent review of the combined unit.
Use isolated worktree `3d48`, branch `feat/coreaudio-fault-diagnostics`, based on `7594910`.
Other qualification worktrees and private evidence remain unchanged.

## Verifiable work

- [x] Reproduce the real Ring/Producer starvation signature without opening a device.
- [x] Retain missing-detail failures before implementing the diagnostic contract.
- [x] Implement exact reason mapping and coherent first-fault publication.
- [x] Verify count bounds, unknown codes, late notifications, concurrent publication, and zero callback allocations.
- [x] Verify structured producer/consumer diagnostics at the Engine boundary.
- [x] Run focused tests, strict Clippy, and the canonical check.
- [x] Run actual Mac SDK checks without starting output.
- [x] Complete one independent review and commit the combined unit.

The deterministic starvation regression publishes 2993 frames into the actual producer/ring.
Five 512-frame callbacks consume 2560 frames.
The next unfinished callback sees 433 queued frames and must emit silence and latch RingUnderrun.
Delivery stays at 2560; accepted input stays at 2993; a subsequent 139-frame write rejects.
This proves the diagnostic mechanism, not the historical physical cause.

## Verified implementation checkpoint

Linux canonical passes 612 tests, with four ignored and no warnings.
Its log SHA-256 is `837c3469b779d3e25651a5d783047dafa5680941b3342cfa49ebf0bc96044c8e`.
Nineteen Core Audio tests exercise the real producer, ring, and coherent fault record.
Two Engine tests serialize and consume structured diagnostics, including a closed Unix stream.
Those Engine progress inputs are constructed fixtures, not native playback observations.
Later supporting native assertions lack separate test-first receipts beyond the retained initial missing-contract failure.

One independent main-model review finds no material defects in the eight-file implementation.
Its receipt SHA-256 is `a1b4269809b41ba19b45841549492372455325edbfdec82866d5501791b6f49e`.
Its reviewed source manifest SHA-256 is `be3a4daa2a0b115bc4cd712a6d427a1fb15dda630fbe5b28e97c81480bc6d9d0`.

Actual Mac compilation and targeted strict Clippy pass without warnings.
The Mac Core Audio library passes 26 tests; the Engine diagnostic tests pass two.
Eleven silent SDK checks overlap that coverage.
Direct reruns of the final test binaries also pass 26 and two tests.
Each directly executed binary remains unchanged during its own execution.
An earlier tooling assertion wrongly compared hashes across controlled Cargo recompilation; its records remain preserved.
The full 1525-file precommit snapshot matches at seven phases and during fresh readback.
Root independently verifies all 60 raw artifacts, command exits, source files, and log hashes.

This SDK evidence deliberately records base `7594910` with `source_dirty=true`.
Its runtime source SHA-256 is `cbb855b17e5448132b87889e1d46d5b4163b03821487c23ab020bc540569c7ea`.
The public precommit SDK receipt identifies this boundary explicitly.
No successful physical output start, hardware signal, or throughput qualification occurs in this unit.
The historical failure remains unclassified; clean release validation and any later hardware run remain separate.

## Qualification boundary

First classify an observed failure or establish the real release execution path with scoped evidence.
Do not claim a playback fix merely because these diagnostic tests pass.
Any later frozen hardware run needs its own reviewed source-bound one-attempt artifacts within existing authority.
Do not modify the consumed `2396efb` signal authorization or retry its scripts.
