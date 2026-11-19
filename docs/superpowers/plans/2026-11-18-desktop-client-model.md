# Desktop client and presentation model

Doctor Biz authorized this bounded unit after accepting the native toolkit and opaque-panel proofs.
Root owns integration, independent review, commits, and shipping.
Worktree `1bab` starts from `043025bdb61bf8c0ba3e4314efbc9aef54817000` on `feat/desktop-client-model`.
Preserve worktree `8766` and every earlier probe.

## Scope and assumptions

Add only `crates/earph-desktop/**`, this plan, and the desktop ADR.
Use the actual attach-only `earph-ffi::LocalClient`.
Use existing Core command types and Engine's shared frame-limit accessor.
Do not instantiate an owner in production code.
Do not add GUI dependencies, owner launching, dialogs, packaging, or audio operations.
Root integrates the final package into the shared workspace and updates its lock and inventory.
The package-local workspace and lock isolate local qualification until that integration.

## Contract

Retain immutable command bytes and exact reply bytes.
Reject missing consumed fields, invalid types, unsupported result kinds, and mismatched receipt identities.
Require fields even when their valid value is null.
Retain opaque graph/profile result bodies without inventing observations.
Do not duplicate engine policy or repair stale targets.

Use one bounded ordinary lane and one bounded safety lane.
Bound completion retention and presentation history too.
Synchronous IPC stays on owned worker threads.
Disconnect stops admission and drops queued work; it never stops the owner.
Explicit worker joining belongs off the UI thread.
Do not claim request cancellation or preemption of the owner's serial IPC server.

Keep accepted, applied, rejected, cancelled, failed, owner admission errors, and uncertain outcomes distinct.
An IPC error after submission can mean the mutation applied despite the missing reply.
The owner can also return a top-level uncertainty error rather than a receipt.
Reconciliation resends only the exact retained envelope.
An observed owner change never retargets retained commands.
Fresh queries use fresh identities; replayed queries return retained owner observations.

## Verification and checkpoints

- [x] Read the prior desktop proposal, actual owner contracts, and existing FFI tests.
- [x] Create the dedicated managed worktree and task branch.
- [x] Run the existing FFI baseline: eight real-owner tests pass.
- [x] Add projection RED tests using actual Core reply producers.
- [x] Implement strict reply and state projections with explicit null preservation.
- [x] Add and pass real-owner adapter and bounded-dispatch tests.
- [x] Add and pass model tests for pending, terminal, stale, and uncertain outcomes.
- [x] Exercise dropped replies through a test-only transport proxy forwarding to the real owner.
- [x] Run a real owner-process E2E with generated local queue fixtures and no source start.
- [x] Run package formatting, strict Clippy, tests, and canonical workspace checks.
- [x] Freeze the concrete view API and source inputs for Root's independent review.

Tests retain generated fixtures, runtime directories, logs, and failed runs.
No devices or output starts enter any scenario.
No commit or push runs before Root's independent review.

## Canonical verification authority

Root explicitly authorizes the existing Linux canonical file-to-null tests for this verification tail.
Those tests use their existing isolated ALSA configurations and generated fixtures.
This approval does not authorize physical playback, microphones, hardware endpoint overrides, or Mac actions.
Keep the new desktop package E2E free of Start, output, and device commands.
Run the existing canonical suite and the separately isolated package checks.

## Verified implementation checkpoint

The new package passes 21 tests, formatting, and strict Clippy.
The real-process E2E keeps generated media unchanged, output absent, and the owner muted.
Controller disconnect leaves the process alive until explicit test-owned shutdown.
Actual process exit is zero and its stderr remains empty.

The unchanged shared workspace passes 651 tests with four existing ignores.
Its formatting, strict Clippy, catalog, and dependency checks pass.
The temporary desktop package is outside that shared workspace and is checked separately.
The package-local dependency resolution adds only `earph-desktop` compared with the shared lock.
No new third-party package or GUI dependency enters this unit.

Initial public API tests fail before the new modules exist.
The parser's first executable RED fails on actual Core state and owner-error documents.
Later RED tests expose missing nested null fields, contradictory snapshot metadata, and exhausted safety history.
The safety fix reserves two of sixteen history slots from ordinary work.
A Core-generated top-level serialization failure also has a retained RED before its uncertainty mapping is fixed.
Additional real-owner scenarios verify exact replay, pending saves, safety progress, stale reads, and disconnect cleanup.
Their first execution passes against the already implemented contract; they are not reported as independent RED cycles.

Retained logs live under `target/desktop-model` in this worktree.
The final package logs are `final-package-tests-v4.log`, `final-package-clippy-v4.log`, and `final-package-format-v4.log`.
The canonical log is `canonical-01.log`.
The source manifest and qualification receipt are produced there for Root's review.

No GUI, native dialog, package installation, or new macOS client build is claimed.
Playback, graph, and profile result bodies remain opaque until later typed view work.
The README documents bounded admission, exact reconciliation, and worker joining outside the UI loop.
Root's independent review, shared-workspace integration, and commit remain pending.

## Reviewed local checkpoint

One independent main-model review approves the bounded client/model source.
Root fixed cross-zone state-query coalescing after a real owner regression exposed it.
The corrected package passes 22 tests, strict Clippy, and formatting.
The regression retries only explicit pre-admission queue pressure with the same request.
The [precommit receipt](../../evidence/desktop/client-model-precommit-verification-20261118.json) binds the corrected source and retained evidence.
Shared-workspace integration, typed views, native GUI, and macOS product qualification remain incomplete.

## Shared workspace checkpoint

The desktop package now uses the authoritative shared workspace and lock.
The lock adds only the local desktop package; third-party packages and notices remain unchanged.
Build provenance covers its manifest and production source.
The canonical check builds the real owner before the desktop process test.
That test locates the owner from its own Cargo profile directory.
The combined check passes 732 tests, with four ignored and no warnings.
The same independent desktop review approves the ten integration paths.
The [integration receipt](../../evidence/desktop/workspace-integration-precommit-verification-20261118.json) binds the exact precommit source.
Native GUI and macOS desktop qualification remain incomplete.
Later PR3 corrections require a new combined qualification.
