# Linux ALSA spike implementation plan

**Goal:** Complete T05 with explicit routes, exact negotiation, bounded PCM writes, and honest evidence.

**Spec:** `docs/SPEC.md` sections 7, 18, AC-21, AC-22, and AC-23.

**Architecture:** Add `earph-alsa` outside the platform-free core and offline harness.
Keep native pointers in one private module with owned handles and parameter objects.
Expose a JSON CLI for discovery, silent opening, and explicitly approved fixture exercises.

**Execution:** Implement here with TDD and one independent final review.
Use the writing-plans, executing-plans, and test-driven-development skills.

## Scope and authorization

Doctor Biz approved the ALSA recommendation on November 17 with “let's do it.”
Doctor Biz then instructed us to finish without offering to continue.
Routine design decisions and local commits are authorized by the repository instructions.
No physical signal emission or deletion is authorized.
Preserve files and test artifacts throughout this task.
Use the existing linked worktree on `feat/alsa-spike`.

Implement T05 only. Core Audio, decoding, full volume policy, and the player remain later tasks.
The existing specification supplies the design; proceed without redundant design approval gates.

## Contracts and success criteria

- Keep platform handles outside `earph-core` and keep `earph-harness` commands offline.
- Pin `alsa-sys` 0.6.1 and the existing `libc` 0.2.191 on Linux only.
- Link the host ALSA library dynamically and record its runtime version.
- Discover playback hints without opening PCM streams or emitting samples.
- Accept version 1 requests with explicit device, route, format, period, and buffer frames.
- Accept signed 16-bit, packed 24-bit, and signed 32-bit little-endian PCM.
- Reject floats, DSD, malformed requests, excess buffers, and implicit default selection.
- Direct routes require a `hw:` name and an opened `SND_PCM_TYPE_HW` handle.
- Disable ALSA automatic conversion and software volume for direct routes.
- Set rate, channels, format, access, period, and buffer exactly; verify installed parameters.
- Report requested and observed parameters separately, including rational sample rate and PCM type.
- Label compatibility routes as unverified downstream transformations, even when the client format matches.
- Keep opened, exercised, and measured evidence distinct; never claim hardware preservation from an open.
- Separate silent opening from writes; require explicit signal permission before any write.
- Use nonblocking writes, bounded prepared storage, and explicit accepted-frame counts.
- Reject stale epochs, changed formats, invalid spans, and writes after stop or terminal faults.
- Report backend delay as an estimate; never label submitted frames as audible progress.
- Stop on fatal output errors without automatic recovery, route fallback, or service changes.
- Close owned streams on every error and shutdown path.

## Review focus

1. A configured `hw:` alias may actually open a conversion plugin; reject it before configuration.
2. A denied signal request must perform no writes and leave the stream prepared.
3. A short write must advance only accepted frames and preserve the caller's remaining span.
4. A native failure must stop output and reject later writes without reopening another route.
5. Plugin success must never promote physical playback or payload preservation to verified status.

## Tasks

### 1. Define route and request contracts

**Files:** `crates/earph-alsa/{Cargo.toml,src/lib.rs,src/contract.rs,tests/contracts.rs}`, workspace manifest.

- [x] Write tests for version rejection, invalid endpoints, incompatible direct routes, supported PCM, and bounded buffers.
- [x] Run them red before implementing `OpenRequest::validate` and serializable contracts.
- [x] Implement typed errors and explicit output states without changing core contracts.
- [x] Run the contract tests green.

### 2. Implement native ownership and exact negotiation

**Files:** `crates/earph-alsa/src/{native.rs,output.rs}`, `tests/alsa.rs`.

- [x] Add real ALSA plugin tests for exact installed formats and rejected conversion aliases.
- [x] Run them red, then implement owned native objects and exact parameter inspection.
- [x] Add red tests for permission denial, epochs, frame spans, terminal states, cleanup, and file capture.
- [x] Implement nonblocking prepared writes, status, explicit stop, and shutdown cleanup.
- [x] Verify actual file-plugin bytes for integer boundaries and partial blocks.
- [x] Keep generated test files in ignored `target/alsa-tests` directories.

### 3. Expose and exercise the spike

**Files:** `crates/earph-alsa/src/main.rs`, `tests/cli.rs`, harness build provenance and dependency inventory.

- [x] Test executable discovery, silent opening, approval gates, malformed inputs, and actual fixture capture.
- [x] Run them red, then implement the CLI using shared fixture loading and provenance.
- [x] Regenerate dependency inventory and notices; review all added licenses and versions.
- [x] Run a real plugin matrix and silent host discovery/open checks with fresh result reads.
- [x] Record limitations and keep physical playback and PipeWire coexistence unqualified unless actually exercised.

### 4. Verify, review, and record the outcome

**Files:** README, `docs/adr/0002-alsa-output.md`, `docs/alsa-spike.md`, catalog progress, state-machine map.

- [x] Run `scripts/check` and the real plugin scenarios without physical audio.
- [x] Request one independent read-only review of native safety, callers, contracts, and evidence.
- [x] Resolve material findings with regression tests and rerun affected checks.
- [x] Inspect the final diff for secrets and private data; verify the configured destination.
- [x] Commit the complete implementation and update this plan with exact results and remaining qualification gaps.

## Baseline

The canonical check passes at `5e84f21`: 55 tests plus the compile-fail documentation test.
The host reports Rust 1.98.1 and ALSA 1.2.15.3 development metadata.
The runtime library version will be recorded separately.
Raw logs stay under ignored `target/alsa-spike`.

## Outcome

Implementation commit: `ae10b7b`.
The canonical check passed 74 tests plus one compile-fail documentation test, with no warnings.
The independent reviewer passed all 19 default ALSA tests and reported no remaining material issue.
Final direct review fixed one report-validation defect with a demonstrated red-to-green regression.

A fresh clean-build matrix passed all 216 file-plugin captures across 72 fixtures and three block sizes.
Its [portable archive](../../evidence/alsa/README.md) passed `matrix-check` after copying.
Fresh direct and PipeWire opening submitted zero samples and closed successfully.
The separately selected hardware subdevice rejected a second open with `EBUSY` and reopened after close.
The archived foundation evidence still passed its existing validator.

No physical signal emission was performed.
Native short acceptance and `EAGAIN` remain unforced test branches.
Physical playback, unplug, hardware payload capture, endurance, and PipeWire coexistence remain unqualified.
T04 and the remaining M0 decisions still precede the full local-player integration.
The task preserves its branch, linked worktree, and ALSA artifacts.
