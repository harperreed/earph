# Offline graph capture

Build the explicit `earph-engine render-graph` consumer without hardware output or owner activation.
The parent approved this bounded API and owns native integration.

## Contract

Require graph, new output directory, integer format, gain, ramp, quantum, and one through 256 sources.
Accept an explicit private runtime only to resolve immutable FIR assets.
Join only identical decoded source formats.
Compile every graph through `compile_stream` and render through `PreparedStreamRender`.
Keep source acceptance, DSP output, acknowledged output, capture bytes, and partial frames distinct.
Observe actual source EOF and stream terminal state.
Keep SRC stage timing honest and report unknown aggregate audible timing.
Create a private directory and new private capture/report files.
Preserve all sources and partial artifacts on every failure.
Emit one typed JSON report and use a nonzero failure exit status.

## Ownership

Own only new engine offline_render.rs, tests/offline_graph.rs, main dispatch/help, public export, and task documentation.
The reviewed render wrapper is an uncommitted dependency patch in this worktree.
Keep every dependency change outside the final owned commit and patch.
The post-dependency library baseline is retained under ignored target/offline-graph.
The parent will integrate the wrapper before this consumer.
No hardware, publishing, credentials, deletion, reset, or hook bypass belongs to this unit.

## Progress

- [x] Create isolated worktree from integrated compiler and Q-shelf base `5be7848`.
- [x] Verify the eight-file wrapper dependency and focused existing baseline.
- [x] Obtain parent approval for the concrete public API and capture lifecycle.
- [x] Prove missing real CLI behavior red before implementation.
- [x] Pass owned WAV to actual CLI boundary cases against standalone SRC captures.
- [x] Prove joins, chained SRC, filters, packed formats, gain/ramp, and failure artifacts.
- [x] Finish resource preflight and actual partial-write evidence.
- [x] Freeze owned changes and run canonical checks.
- [x] Obtain independent review and commit only the owned unit.

The first capture scenarios pass lengths zero, one, C-1, C, and C+1 at 48 kHz to 44.1 kHz.
They compare exact signed24 capture bytes and preserve original WAV bytes.
Expected red and green logs remain under ignored target/offline-graph.
The real shell file-size limit leaves a partial signed24 frame and a readable exact-prefix report.
The failed period remains unacknowledged while readback records every actual byte.
Unknown capture observations remain null rather than claiming an empty file.
Asset geometry receives a payload admission check before private-store resolution.
The shared compiler and wrapper remain the only DSP and staging resource authorities.

The root committed the unchanged eight-file wrapper dependency as `c6dd76e`.
Its public API and source hashes match this worktree's dependency copy.
Exclude those changes from the owned commit.
Stage only the offline module export hunk in engine lib.rs.
Use the verified index for that commit so a whole-file commit cannot include the dependency exports.

The frozen canonical check passes 524 tests with three ignored scenarios and no warnings or errors.
The ten offline scenarios pass, and strict engine/all-target Clippy passes.
The independent reviewer passes all ten real CLI scenarios and independently hashes the partial capture.
The parent accepts that review receipt.
Final fresh-eyes review finds no additional material issues.
The owned commit includes only this consumer, its export, tests, and documentation.
The dependency files remain separate and preserved in this worktree.
