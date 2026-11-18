# Strict serial SRC graph compilation

Root owns native playback, Core session control, signal-path timing, and the offline CLI.
This unit owns strict SRC intent, shared graph validation, rate-island compilation, and resolved metadata.
Keep the unit isolated until the product consumer exists.

## Contracts

Add explicit SRC input rate, output port rate, preset, and chunk parameters.
Require every SRC to dominate the selected output.
Retain arbitrary same-rate branches inside each island.
Reject SRC bypass paths, unsupported rate merges, and non-final PEQ islands.
Require f64 PCM and reject strict preservation, including unity SRC.
Honor every declared port quantum, including the SRC's maximum input and output spans.
Resolve resources before coefficient analysis.
Finish island bounds using actual upstream SRC peak bounds.
Use one same-rate compiler implementation and one stream resource implementation.
Keep synthetic IDs internal and remap diagnostics to original nodes.
Reject finite latency ceilings while aggregate SRC latency lacks a proof.
Report per-domain timing and guard/padding facts without an invented exact latency.

## Work

- [x] Run the unchanged compiler, filter, and stream baseline.
- [x] Add failing strict SRC intent and pure preflight scenarios.
- [x] Share SRC geometry, parameter facts, and stream resource preflight.
- [x] Extract shared graph validation and two-phase same-rate resolution.
- [x] Implement serial-cut partitioning and compiled stream metadata.
- [x] Prove real graph-to-stream processing, hash identity, resource rejection, and diagnostic mapping.
- [x] Run canonical checks and independent root review.
- [x] Commit only reviewed owned files.

## Ownership

The approved base is public stream checkpoint `4db816e`.
Core graph intent and transformation types belong to this unit.
DSP compiler, model, rate compiler, tests, and required exports belong to this unit.
The DSP runtime change is one explicit SRC rejection arm.
The stream change is a shared pure resource preflight only.
The resampler change is shared allocation-free geometry and parameter facts only.
Do not change session/control logic, media imports, native adapters, or engine playback here.
No hardware, publishing, credentials, private corpus, deletion, or hook bypass belongs to this unit.

## Evidence

The public entry is `compile_stream(document, context, assets, StreamBudget)`.
The immutable result exposes resolved stages, original IDs, bounds, resources, transformations, and content identity.
Consume it through `into_stream_plan()` and the existing `PreparedStream` renderer.
The compiler adds one conservative 2 MiB metadata reserve through the stream resource authority.
Each same-rate island keeps its existing metadata reserve.
Asset coefficients remain shared through immutable owned references.
Repeated assets can receive conservative per-island resource charges.

Release verification passes 13 compiler scenarios, 17 resampler scenarios, and 17 stream scenarios.
The numeric writer passes 30 fixed cases and 10 drift histories.
Rendered audio and response CSV hashes match the reviewed parent baseline.
The source and test hashes bind the regenerated receipt under ignored `target/src-numeric`.
Focused logs and expected-red logs remain under ignored `target`.
Canonical verification passes 472 tests with three ignored scenarios and no warnings or errors.
Formatting, strict Clippy, catalog validation, and dependency provenance also pass.
Independent component review approves the initial source/test freeze.
A focused correction now removes admitted port ceilings from semantic hashes.
Its regression fails before the correction and passes afterward.
The original declared ceilings still validate before internal normalization.
Replacement canonical verification passes 472 tests with three ignored scenarios and no warnings or errors.
The independent reviewer approves the focused delta, and the parent accepts that receipt.
Final fresh-eyes review finds no additional material issues.
The named commit contains only this reviewed unit and task note.

SRC timing exposes a rational guard and bounded adapter finish output.
The library delay stays unknown until runtime instantiation.
Aggregate exact audible timing has no proof in this unit.
Finite latency ceilings therefore reject explicitly when SRC exists.
Native playback, Session activation, and the offline CLI still belong to the parent task.
