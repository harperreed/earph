# Core Audio precision spike implementation plan

**Goal:** Complete T04 by proving SDK access, device inspection, strict format assessment, and silent callback ownership.

**Spec:** `docs/SPEC.md` sections 7 and 18; scoped AC-21 and AC-23 foundations.

**Architecture:** Add `earph-coreaudio` beside the ALSA adapter.
Keep native SDK calls private and platform-free requests and assessments testable on both hosts.
Expose discovery, inspection, precision assessment, and silent callback-registration checks through a JSON CLI.

**Execution:** Use Superpowers planning, TDD, inline implementation, and one independent final review.
The existing specification supplies the design.
Doctor Biz instructed us to finish without redundant approval or continuation prompts.

## Scope and authorization

Doctor Biz requested the next task after T05 on November 17, 2026.
The next risk spike is T04, Core Audio.
Doctor Biz approved a separate Mac for this task's isolated build and silent tests.
The private task note records the selected SSH alias and task folder.
The host reports arm64, macOS 27.2.1, and Xcode 27.2.
Use a new private task folder and preserve existing files and settings.
Never read private keys or credentials.
No physical sample emission or file deletion is authorized.

Use branch `feat/core-audio-spike`, stacked on the completed T05 branch.
Preserve `feat/alsa-spike` and its evidence.

T04 is a backend risk spike, not the full M1 player.
Inspect nominal rate, buffer size, physical and virtual stream formats, and hog-mode availability.
Register and destroy a silent IOProc without starting it.
Keep advertised modes, current settings, registration, exercise, and measurement separate.
Device format mutation, exclusive ownership changes, signal writing, and live playback remain T08–T09 integration work.
Do not implement a partial playback command or label read-only assessment as a negotiated open.

## Contracts and success criteria

- Add `earph-coreaudio` without changing shared core contracts or existing commands.
- Pin `objc2-core-audio`, `objc2-core-audio-types`, and `objc2-core-foundation` 0.3.2 on macOS only.
- Disable default framework features and enable only the C hardware and string surfaces used.
- Keep unsafe code inside the native boundary.
- Require one explicit device UID; never choose a default or fall back.
- Bound property arrays, stream counts, requests, and response artifacts.
- Check native status and returned property sizes before reading values.
- Release owned Core Foundation strings and IOProc registrations on normal and error paths.
- Preserve raw stream descriptors and derive their PCM interpretation without duplicated state.
- Compare integer PCM rate, channels, bit depth, byte layout, and buffer size exactly.
- Report physical and virtual mismatches separately.
- Reject floats and DSD for strict integer preservation assessment.
- Treat hog availability as advertised capability, not acquired ownership or preservation evidence.
- Never call device-start or property-setting APIs in this spike.
- Keep registration evidence explicitly `registered_not_started` and require zero callbacks and samples.
- Surface unavailable properties as unknown where the SDK marks them optional.
- Report inspection failures instead of omitting devices silently.
- Record Linux checks, native Mac checks, build provenance, and limitations separately.

## Review focus

1. Read-only device inspection must not alter nominal rate, stream formats, volume, or hog mode.
2. A float virtual format must fail an integer precision request even when physical bit depth matches.
3. Native variable-size properties may change between size and data queries; bound and reject inconsistent sizes.
4. Callback registration must never start hardware and must destroy exactly the registration it owns.
5. Public artifacts must not expose personal device names or persistent device UIDs.

## Tasks

### 1. Define contracts and exact precision assessment

**Files:** `crates/earph-coreaudio/{Cargo.toml,src/lib.rs,src/contract.rs,tests/contracts.rs}`, workspace manifest.

- [x] Write failing tests for explicit UID selection, schema, integer PCM, and bounded buffer requests.
- [x] Define raw stream observations with physical and virtual modes kept separate.
- [x] Test exact matches, mismatched rates, byte layouts, channels, float formats, unknown ownership, and missing streams.
- [x] Implement the smallest pure assessment API and verify all contract tests.

### 2. Implement native inspection and registration

**Files:** `src/native.rs`, native integration tests, and dependency inventory.

- [x] Prepare a repeatable remote check script using tracked source files and a private task folder.
- [x] Install or select the pinned Rust toolchain without removing existing versions.
- [x] Run native tests red against the real Core Audio SDK before implementing native operations.
- [x] Implement bounded property reads, UID matching, exact descriptor reporting, and optional property states.
- [x] Implement owned, never-started IOProc registration and explicit checked close.
- [x] Run real Mac discovery, wrong-UID, inspection, and registration/close/reopen tests.
- [x] Review selected dependency versions, features, native frameworks, and license texts.

### 3. Add executable flows and evidence

**Files:** `src/main.rs`, CLI integration tests, harness provenance, and spike docs.

- [x] Test JSON discovery, explicit inspection, precision assessment, missing UID errors, and silent registration.
- [x] Run executable tests against real Mac devices without emitted samples or changed settings.
- [x] Record actual supported formats and precision blockers without promoting them to hardware qualification.
- [ ] Add portable, redacted native evidence with a reproducible source identity.

### 4. Verify and finish

- [x] Run the Linux canonical check and the native Mac checks.
- [x] Confirm existing offline and ALSA archives still validate.
- [x] Obtain one independent read-only review of native ownership, assessment, callers, tests, and evidence.
- [x] Fix material findings and rerun affected checks.
- [x] Scan the implementation diff for secrets and private device identifiers; scan the final public archive before committing it.
- [ ] Commit implementation and clean-build evidence, preserving branches, worktrees, and artifacts.

## Initial findings

The Linux host has only its native Rust target installed.
The Mac's default Rust is 1.92.0, below the repository's pinned Rust 1.98.1.
`mise where rust@1.98.1` reports that version absent on the Mac.
Resolve the toolchain before claiming native compilation or checks.

## Execution notes

Mise installed Rust 1.98.1 without changing the older default toolchain.
Its Rust path points to rustup proxies; use `rustup which` to select the actual compiler.
A real Git checkout is required for the existing qualification provenance tests.
The remote runner now checks inside a marked private task folder and preserves the earlier source copies.

Real HAL tests reproduced a collision between registrations sharing a null callback context.
Unique owned contexts fixed both concurrent registration and cleanup tests.
Raw SDK flags remain authoritative after a regression rejected contradictory normalized encoding labels.
The runner rejects SSH options supplied as host aliases before creating a source archive or connecting.

The binding crates omit license files; their published VCS records identify one upstream license declaration.
The inventory verifies exact names, version, repository, revision, and declaration hash.
The publisher's Apple SDK distribution question remains a release gate.
