# Earph Foundation Implementation Plan

> **For agentic workers:** Use `superpowers:executing-plans` for native execution or `superpowers:subagent-driven-development` if Doctor Biz selects delegation. Follow the checked steps in order.

**Goal:** Implement T01–T03: checked contracts, deterministic offline rendering, virtual output, fault injection, and honest execution evidence.

**Architecture:** Start with two Rust crates. `earph-core` owns domain types and the session model. `earph-harness` owns fixtures, virtual output, filesystem access, qualification tools, and an offline executable. The harness consumes the same core contracts that later source and output adapters will use.

**Tech Stack:** Rust 2024; the installed Rust 1.98.1 toolchain; Cargo; Serde JSON; SHA-256; Bash for `scripts/check`. Resolve and audit exact dependency releases during Task 1. Commit `Cargo.lock`. Manage the toolchain with `mise.toml` without changing global settings.

**Spec:** [Product specification](../../SPEC.md), especially §§3–5, 12, 16–19 and Appendices B, C, E, F. [First implementation target](../../README.md) explicitly selects T01–T03.

**Implementation owner:** Bassquake 9000. **Review partner:** Doctor Biztron. One owner maintains the shared schemas.

## Authority and current state

Doctor Biz requested implementation of the supplied spec using Superpowers on November 16, 2026. Treat the supplied spec as the design baseline. Do not invent the missing wording of decision D25.

The initial checkout contained only the spec bundle, `.gitignore`, and `.kata.toml`. Baseline commit: `e08c518`. There was no executable or existing check command.

Doctor Biz approved this plan and native execution with independent review by saying “lets go”. Execute all six tasks. Complete the foundation before planning its dependent playback slice.

## Global constraints

- “First-party engine, client and tooling code uses MIT.”
- “Begin with a small Cargo workspace.”
- “Unimplemented settings MUST reject with a capability error rather than silently do nothing.”
- “Preserve integer PCM payloads without routing them through f32.”
- “DSD does not pass through ordinary PCM DSP.”
- “Bound every queue.”
- “Always invalidate old epochs after seek, takeover, discontinuity and output replacement.”
- “On first use, begin muted.”
- The proposed software-volume default is −40 dB, not a claim about acoustic safety.
- The baseline audio/graph budget is 256 MiB, excluding GUI memory.
- “A simulated device does not satisfy a real hardware gate.”
- “User approval is required before tests emit signals.” This plan only writes offline captures.
- “A future implementation agent should start with T01–T03, not generate every future subsystem.”
- Keep source and fixture originals unchanged. Generate artifacts under ignored `target/` directories.
- Add two `ABOUTME:` comments to handwritten comment-capable source and scripts.
- Use TDD for behavior. Run unit, integration, and real executable tests.
- Use one independent review for shared-contract and safety changes. Reviewers remain read-only.

## Deliverable and limits

The executable generates fixtures, renders to virtual capture, injects deterministic faults, and validates evidence. It cannot play to a physical output or receive network audio.

This completes the T01–T03 foundation, not all M0. AC-01 still requires candidate adapters and macOS/Linux builds. REL-02 remains partial until relevant dependencies and protocol revisions are selected and exercised. Linux GUI, operational storage, and IPC framing decisions remain in the rest of M0.

Do not install decoder, DSP, GUI, network, or hardware libraries solely to fill future inventory rows. Record candidates as unselected and unsupported. Their eventual implementation must pin and audit them before use.

## Success criteria

1. `scripts/check` passes formatting, lint, unit, integration, executable, catalog, and dependency checks.
2. Offline signed 16/24/32-bit captures preserve every numeric sample and frame across block sizes and short writes.
3. Repeating a fixture and fault schedule yields identical capture hashes and semantic events.
4. Invalid schemas, unsafe domain combinations, stale epochs, and resource overflows produce typed failures without partial mutation.
5. Receipts separate acceptance from application. Startup stays muted. Discovery alone never takes source ownership.
6. Generated evidence identifies the actual build, environment, fixtures, commands, measurement boundary, results, and artifact hashes.
7. Unsupported platforms and hardware remain not run. Partial scenario coverage never becomes a full pass.

## Review focus

1. Integer overflow in frame counts and 24-bit sign extension must fail or preserve values exactly. Task 2 owns these tests.
2. A backend accepting only part of a block must neither repeat nor omit frames. Task 5 owns these tests.
3. Old packets arriving after seek or takeover must never enter the new capture. Tasks 3 and 5 own these tests.
4. A malformed command or reused request ID must not partly mutate state or execute twice. Task 3 owns these tests.
5. A failed or truncated evidence write must never produce a passing report. Task 6 owns these tests.

## File boundaries

| Path | Responsibility |
|---|---|
| `Cargo.toml`, `Cargo.lock`, `mise.toml` | Workspace and exact build inputs |
| `LICENSE`, `THIRD_PARTY_NOTICES.txt` | First-party license and actual dependency notices |
| `scripts/check` | Canonical local verification |
| `crates/earph-core/src/{lib,ids,audio,control,session}.rs` | Product types and the in-memory session contract |
| `crates/earph-core/tests/{audio_contracts,schema_contracts,session_contracts}.rs` | Public-contract integration tests |
| `crates/earph-harness/src/{lib,main,fixture,virtual_output,render,catalog,evidence}.rs` | Offline tools and adapters |
| `crates/earph-harness/tests/{catalog,offline,cli,evidence}.rs` | Integration and executable tests |
| `fixtures/contracts/` | Versioned command examples, including rejected inputs |
| `fixtures/audio/manifest.json` | Fixture generation rules, seed, dimensions, and expected hashes |
| `docs/adr/0001-foundation-contracts.md` | Schema, resource, lifecycle, and dependency decisions |
| `docs/dependency-lock.json` | Exact active dependency/features/licenses and unselected candidates |
| `docs/reference-environment.json` | Observed environment and unqualified platform matrix |
| `docs/evidence/foundation/` | Selected portable reports from actual runs |
| `README.md` | Build, offline commands, checks, limits, and next gate |

## Task 1: Establish the executable contract checker and build baseline

**Spec work:** T01. **Requirements:** ARCH-01, REL-01, REL-02. **Acceptance:** AC-01 and AC-49, foundation subsets only.

**Files:** Workspace files, licenses, `scripts/check`, harness `lib.rs`, `main.rs`, `catalog.rs`, `tests/catalog.rs`, ADR, dependency inventory, and reference environment.

**Interfaces:**

- `catalog::validate_catalog(bytes: &[u8]) -> Result<CatalogSummary, CatalogError>`.
- `CatalogSummary` reports requirement, scenario, and deferred counts.
- `earph-harness catalog-check --catalog PATH` exits 0 on valid input and nonzero with structured diagnostics otherwise.
- `earph-harness dependency-check --inventory PATH` compares the inventory with actual locked Cargo metadata and enabled features.

- [x] Add tests against the supplied catalog and malformed copies. Require 69 unique requirements and 50 unique scenarios. Check dangling links, dependency cycles, duplicate IDs, and unknown status values. Check that all D01–D24 decisions remain referenced and D25 remains unresolved.
- [x] Run `cargo test -p earph-harness --test catalog`. Save the expected failure showing missing validation behavior.
- [x] Implement the smallest workspace and checker. Preserve the original catalog. Select exact active Serde/JSON/hash releases from upstream metadata. Audit actual license files and enabled features, including transitive/build dependencies. Commit notices and the lock. Record Ubuntu 26.04.1, Linux x86-64, and Rust 1.98.1 as observed here. Record macOS/device qualification as not run.
- [x] Add `scripts/check`: `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets --locked -- -D warnings`, `cargo test --workspace --locked`, and the executable catalog/dependency checks. Verify inventory agreement with `cargo metadata --locked --format-version 1`. Define status values in the ADR: implementation `not_started|in_progress|implemented`; verification/scenario `not_run|partial|pass|fail|blocked`.
- [x] Run the canonical check and review the diff. Inspect for secrets. Commit with `chore: establish checked Rust foundation`.

## Task 2: Define checked identities, formats, and audio blocks

**Spec work:** T02. **Requirements:** ARCH-04, ARCH-05, ARCH-07. **Acceptance:** AC-04 and AC-07 subsets.

**Files:** Core manifest, `lib.rs`, `ids.rs`, `audio.rs`, `tests/audio_contracts.rs`, and ADR.

**Interfaces:**

- Opaque string newtypes: `ZoneId`, `SessionId`, `DeviceId`, `ProfileId`, `GraphId`, `RequestId`.
- Reject empty IDs, control characters, and IDs longer than 128 UTF-8 bytes. Never derive identity from display names.
- `Epochs { session: u64, stream: u64 }`, using checked increments.
- `PcmFormat::new(sample_rate_hz: u32, channels: ChannelLayout, representation: PcmRepresentation) -> Result<Self, EngineError>`.
- Initial layouts are mono and stereo. PCM representations are signed 16/24/32-bit and f64. Positive rates describe formats; they do not advertise hardware support.
- `PcmBlock::new(header: BlockHeader, samples: PcmSamples) -> Result<Self, EngineError>`. Typed storage separates integer variants from f64.
- `DsdBlock` is a distinct payload type. No implicit conversion to `PcmBlock` exists.
- Headers contain epochs, sequence, frame position/count, format, and a time-domain tag. Check scalar counts, ranges, finite values, and size arithmetic.
- `MemoryBudget::admit(required_bytes: usize) -> Result<(), EngineError>` uses a default of 268435456 bytes.

- [x] Write boundary tests for signed extrema and low bits, especially 24-bit sign extension. Cover zero rate, invalid layout/counts, NaN/Inf, arithmetic overflow, rejected zero-frame blocks, and budget exhaustion. Include unit tests for checked arithmetic and packing. Include a compile-fail doctest preventing DSD submission to a PCM-only consumer.
- [x] Run `cargo test -p earph-core --test audio_contracts` and doctests. Confirm the new tests fail because validation or types are absent.
- [x] Implement checked constructors and distinct storage variants. Add product error codes from Appendix E. Keep dependencies out of public signatures and keep float conversion explicit.
- [x] Run core tests and `scripts/check`. Verify 16/24/32-bit numeric equality across serialize/deserialize and pack/unpack boundaries. Do not count raw JSON equality as audio qualification.
- [x] Review the changed contract and commit with `feat: define checked audio domain contracts`.

## Task 3: Implement versioned commands and the session reducer

**Spec work:** T02. **Requirements:** ARCH-07, CTL-01, CTL-02, PLAY-05, SAFE-01, OBS-01. **Acceptance:** AC-07, AC-12, AC-33, AC-36 subsets.

**Files:** `control.rs`, `session.rs`, core contract tests, `fixtures/contracts/`, and ADR.

**Interfaces:**

- `decode_command(bytes: &[u8], limits: &ControlLimits) -> Result<CommandEnvelope, EngineError>`.
- Version 1 envelopes carry `request_id`, target IDs, expected revision where required, and typed payloads.
- Methods implemented here: capability/state queries, session start/stop/pause/resume/seek, and mute. Seek requires source capability. Everything else returns `CAPABILITY_UNSUPPORTED`.
- `Session::new(zone: ZoneId, limits: ControlLimits) -> Self`.
- `Session::submit(&mut self, command: CommandEnvelope) -> Receipt`.
- `Session::advance(&mut self, event: EngineEvent) -> Result<(), EngineError>`.
- `Session::snapshot(&self) -> SessionSnapshot` and `Session::events_since(&self, cursor: u64) -> EventBatch`.
- `Receipt` carries `accepted|applied|rejected|cancelled|failed`, reason, request/target IDs, revisions, and optional activation position.
- Inject monotonic timestamps through events. Do not read wall time in the reducer.

- [x] Write version round-trip and rejection tests. Reject missing/future/older versions, unknown required fields/methods, malformed IDs, and oversized messages before mutation. Version 1 is the only supported version. There is no historical migration or compatibility path to invent.
- [x] Write transition tests for §5, explicit takeover, stale target/session/revision, seek flushing, pause retention, final drain once, failure, and cancellation. Require startup mute and −40 dB software intent. A volume command must not unmute. Fixed output stays unavailable in this slice.
- [x] Write bounded log/deduplication tests, including unit tests for eviction and cursor recovery. Use 1024 retained receipts and 1024 state events per zone. Retained identical requests return their existing receipt. Reusing an ID for different content fails without mutation. Eviction ends the deduplication guarantee. A lagging cursor receives a gap plus a current snapshot.
- [x] Run the contract tests red. Implement the reducer with atomic validation-before-mutation. Advance epochs on seek, takeover, discontinuity, and output replacement. Separate start acceptance from readiness/application. Keep muted state orthogonal to transport. Do not claim physical silence from model state alone.
- [x] Run core tests and `scripts/check`. Review schema fixtures against Appendix E. Commit with `feat: add versioned control and session contracts`.

## Task 4: Generate independent, deterministic PCM fixtures

**Spec work:** T03. **Requirements:** ARCH-06. **Acceptance:** AC-01, AC-04, AC-06 foundation subsets.

**Files:** Harness `fixture.rs`, offline tests, `fixtures/audio/manifest.json`, and README.

**Interfaces:**

- `FixtureSpec { seed: u64, frames: u64, format: PcmFormat, pattern: FixturePattern }`.
- `FixtureSource::new(spec: FixtureSpec) -> Result<Self, EngineError>`.
- `FixtureSource::read_into(&mut self, destination: &mut PcmBlock) -> Result<usize, EngineError>` reuses prepared storage.
- `earph-harness fixtures --output DIR` writes canonical little-endian integer payloads and a manifest. Refuse existing outputs unless explicitly replaced by the caller.
- Pattern variants: silence, channel-tagged impulse, signed boundary cycle, and seeded noise. Use documented SplitMix64 for seeded noise. Pin a short independently calculated golden vector.
- Qualification dimensions: 16/24/32-bit, mono/stereo, 44100/48000/96000 Hz, 1031 frames, seed 2389.

- [x] Test deterministic bytes/hashes, channel order, signed endpoints, exact frame counts, empty input, invalid dimensions, and checked limits before allocation. An empty fixture ends without producing a zero-frame block. Use hand-authored expected samples independent of the renderer and packer.
- [x] Run `cargo test -p earph-harness --test offline` red.
- [x] Implement bounded generation and canonical payload output. Generate fixture manifests from actual bytes. Hash payload and manifest separately. Never hash nondeterministic run metadata as audio identity.
- [x] Regenerate fixtures twice in separate temporary directories. Compare payloads/manifests and verify the independent expected vectors. Run the canonical check.
- [x] Review and commit with `test: add deterministic PCM fixture generation`.

## Task 5: Render through bounded virtual output and injected faults

**Spec work:** T03. **Requirements:** ARCH-06; partial ARCH-04, ARCH-05, PLAY-05, OUT-04, OBS-01. **Acceptance:** AC-06; engine-boundary AC-04 and AC-12 subsets.

**Files:** Harness `virtual_output.rs`, `render.rs`, `tests/offline.rs`, and README.

**Interfaces:**

- `VirtualClock::advance_frames(&mut self, frames: u64, sample_rate_hz: u32) -> Result<(), EngineError>` retains rational time without per-block rounding drift.
- `VirtualOutput::prepare(format: PcmFormat, capacity_frames: usize, epochs: Epochs) -> Result<Self, EngineError>`.
- `VirtualOutput::write(&mut self, block: &PcmBlock, offset_frames: usize) -> Result<usize, EngineError>` returns accepted frames. Only output consumption advances audible position.
- Advertise only integer PCM in this harness. Reject f64 and DSD output until their paths exist.
- `RenderRequest` selects fixture, block size, admitted budget, expected epochs, and ordered `FaultSchedule`.
- `render(request: RenderRequest) -> Result<RenderReport, RenderFailure>` preserves the partial capture and events on failure.
- `earph-harness render --fixture PATH --block-frames N --output DIR` runs the actual renderer. `--faults PATH` supplies a versioned schedule.

- [x] Test clean preservation at block sizes 1, 64, 256, and 1024. Compare captured numeric samples against independent source expectations, including the final partial block. Require exact counts and distinct requested/resolved/observed fields. Add clock unit tests proving block partitioning cannot accumulate timestamp drift.
- [x] Test scheduled short writes, zero-progress writes, underruns, disconnects, malformed blocks, and stale epochs. Inject faults at first frame, block joins, and final frame. Reject invalid schedules before rendering. Preserve partial output after failures.
- [x] Test repeated seek/takeover with delayed old blocks. Require no stale samples in capture. Verify output consumption, rather than decode progress, drives the virtual audible position.
- [x] Run tests red. Implement the bounded renderer and virtual clock. On short writes, resume from the first unaccepted frame. On zero progress, underrun, malformed input, or output failure, stop safely with a typed error. Stale blocks are rejected before capture. No automatic output replacement occurs.
- [x] Run each fault schedule twice. Require identical semantic events and capture hashes. Instrument prepared processing for zero allocation/deallocation; perform artifact I/O and destruction outside that region. Run `scripts/check`, review, and commit with `feat: add deterministic virtual rendering and fault injection`.

## Task 6: Qualify the real executable and record scoped evidence

**Spec work:** Finish T01–T03 integration. **Requirements:** ARCH-01, ARCH-06, ARCH-07; scoped REL-01/REL-02 and OBS-04 foundations. **Acceptance:** AC-06; partial AC-01, AC-04, AC-05, AC-07, AC-12, AC-39, AC-49. AC-07's profile cases remain for the profile implementation.

**Files:** Harness `evidence.rs`, `tests/{cli,evidence}.rs`, `scripts/check`, catalog progress entries, selected evidence, and README.

**Interfaces:**

- `earph-harness qualify --output DIR` executes the committed virtual fixture/fault matrix through the real producer and consumer.
- `evidence::validate(record: &EvidenceRecord, root: &Path) -> Result<(), EvidenceError>` checks required fields, outcome, IDs, and artifact hashes.
- Emit evidence compatible with `docs/evidence-template.json`. Record `measurement_boundary: engine_output` and virtual backend/device identity.
- Include build/commit, dirty-state flag, lock SHA-256, exact command, harness version, OS/architecture, fixture hashes, timestamps, results, artifacts, and limitations. Graph hash is null with a stated reason because no DSP graph exists yet.
- Stable semantic reports exclude run timestamps and temporary absolute paths. Those belong in the evidence wrapper.

- [ ] Add executable tests that invoke the built binary in isolated directories with real fixture files. Cover generation, render, fault exit status, malformed JSON, missing assets, unwritable destinations, corrupt manifests, and pre-existing output directories. Parse actual output and compare actual capture files.
- [ ] Add evidence tests rejecting tampered artifacts, truncated writes, missing build fields, unknown requirement/scenario IDs, out-of-root paths, and reports marking failed runs passed. Preserve interrupted-run artifacts with failure status. Never overwrite an earlier successful run during a failed retry.
- [ ] Run the new tests red. Implement qualification and write evidence only after collecting actual results. Use temporary files plus rename for final reports. Redact private absolute paths and avoid device serials. Never promote a partial scenario or virtual result into hardware evidence.
- [ ] Run `scripts/check`, then `cargo run --locked -p earph-harness -- qualify --output target/evidence/foundation`. Validate the emitted evidence with a fresh read. Check `cargo tree` and inventory agreement. Record the source commit and dirty state accurately; evidence commits naturally follow the source commit they describe.
- [ ] Request one independent read-only review of the actual diff, public contracts, safety model, callers, and test evidence. Use the session's main model. Fix or rebut material findings. Run failed checks after fixes. Perform the final direct sanity review and secret scan.
- [ ] Copy only portable, validated reports to `docs/evidence/foundation/`. Link scoped evidence from the catalog. Update its summary status to reflect actual progress. Keep AC-01, AC-07, and REL-02 partial. Keep all untested hardware/protocol/UI requirements not run. Update this checklist and README. Commit with `feat: qualify the offline foundation with traceable evidence`.

## Following plans

Keep the remaining specification as the roadmap. Do not mark it complete with this plan.

| Next work | Spec tasks | Entry condition | Exit evidence |
|---|---|---|---|
| Finish M0 risk decisions | T04–T05 and remaining M0 decisions | Foundation contracts pass | Backend spikes; GUI/storage/IPC decisions; pinned candidate checks; actual platform matrix |
| Local playback | T06–T09 and early T13 shell | Backend/safety decisions exist | WAV/FLAC/AIFF, ownership, IPC/CLI, queue, strict negotiation, mute/unplug, and local GUI checks |
| Useful DSP | T10–T12 | Local playback slice passes | Graph transactions, numerical EQ/FIR/SRC, latency, headroom, imports, and profiles |
| Desktop and FFI | T13 | Shared control contract and profiles exist | macOS/Linux workflows, Swift integration, Kotlin smoke tests, and lifetimes |
| Network and DSD | T14–T17 | Playback/DSP prerequisites pass | Real Music Assistant/controller traces, pinned Sendspin timing, qualified DSD safety, and arbitration |
| Release qualification | T18 | Integrated features pass | Packages, notices, privacy/security checks, stress/endurance, platform evidence, and recovery |

## Plan review

Checked against T01–T03, the five M0 requirements, AC-01/06/07, and Appendix E. The plan deliberately leaves hardware, candidate filters, and later subsystems to their dependency-ordered plans. Their absence blocks broader milestone claims.

No historical schema exists to migrate. Reject unsupported older versions while preserving the submitted bytes. Do not add a speculative legacy interface.

The native execution method is recommended because these tasks share contracts and build on each other. Keep one implementation owner. Use the independent reviewer at the shared-contract completion boundary.
