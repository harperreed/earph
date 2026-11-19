# Listening profiles implementation plan

> Use `executing-plans` for root implementation and one independent review per bounded implementation unit.

**Goal:** Save named listening intent and apply it through the existing Session transaction.

**Architecture:** Keep immutable profile, graph, report, and FIR objects in private storage.
Publish reachability through one persisted catalog checkpoint.
Use Session and GraphService for application, consent, cancellation, and activation acknowledgments.

**Tech stack:** Existing Rust, serde JSON, private owner storage, CLI, and graph preparation.

**Spec:** `docs/SPEC.md` sections 10–12, 14.1, and 15.
Implement PROF-01/02, DSP-06/07, COR-01, and the bounded PROF-03 package contract.
PROF-03 says M4, while AC-30/35 reference it at M2.
Keep this scheduling mismatch explicit until the complete package evidence passes.

## Constraints and approval

Doctor Biz authorizes autonomous implementation, parallel work, integration, push, and merge.
Keep private media local and unchanged.
Preserve all existing files, worktrees, and artifacts.
Save and import never activate, unmute, consent, arm fixed output, or raise independent volume caps.
Profile application stays muted and binds the exact saved revision.
Never infer passive headphones or speakers from an unchanged DAC identity.
Do not add a second activation authority or restore runtime authorization after restart.
Keep unsupported hardware volume and context rules truthful and unavailable.
Preserve existing output-only configurations until a profile is explicitly selected.
Do not infer saved profiles from existing arbitrary output profile IDs.

## Review focus

- A stale profile revision cannot consume a candidate prepared for another revision.
- An imported cap cannot weaken a persistent local cap or restore fixed arming.
- Two listening setups on one DAC require explicit selection when associations are ambiguous.
- A failed import, missing correction, or uncertain checkpoint cannot publish an applied profile.
- A package cannot export private paths, recordings, authorization, or unreviewed third-party measurement bytes.

## File ownership

Root owns `earph-core/src/profiles.rs`, its public export, and focused tests.
Root owns later profile storage and Session integration until an explicit delegation transfers ownership.
Existing media parsers remain the text and WAV validation authorities.
The T11 CPU, Linux evidence, Mac evidence, and dashboard agents own their separate worktrees or ignored artifacts.

## Task 1: Pure profile contracts and selection

Files: `crates/earph-core/src/profiles.rs`, `src/lib.rs`, `src/ids.rs`, and `tests/profiles.rs`.

Expose strict schema-1 `ProfileDocument` and `read_profile(&[u8])`.
Store stable ProfileId, positive revision, bounded name, graph object hash, source/fidelity policy, output selector, volume intent, latency intent, and correction object references.
Use a fixed-size lowercase SHA-256 type for object references.
Output selectors carry backend, route, DeviceId, and an optional manual setup label.
They do not carry native endpoint paths, consent, or trusted readback.
Keep named latency modes with explicit resident, preparation, and optional frame limits.
Do not treat a saved latency label as successful executable admission.

Use 8 KiB encoded and retained document bounds.
Bound names and setup labels to 128 UTF-8 bytes and correction references to 32.
Bound each retained association to 512 bytes, including spare identifier capacity.
Reject unknown fields, future versions, zero revisions, control characters, invalid hashes, and excess retained capacity.
Add narrow ProfileId and DeviceId capacity accessors for the new retained model.
Do not audit or change unrelated ID retention paths.

Expose `ProfileVolumeIntent::effective_policy(&VolumePolicy)`.
Intersect requested caps with local caps and return a fresh unarmed VolumePolicy.
Keep volume intent separate from output authorization and runtime hardware support.

Expose `resolve_profile` over validated profile documents, exact associations, optional manual choice, and verified device context.
Return selected ID/revision plus manual or exact-device reason.
Return explicit ambiguity or no verified match with bounded candidates.
Exact association uses an owner-supplied native identity digest, never a caller's logical DeviceId alone.
This context cannot deserialize from imported documents.
Manual choice has priority but cannot silently select another output.
Never choose the first of multiple exact-device setups.

- [x] Write failing public tests for strict document roundtrip, limits, policy intersection, and deterministic resolution.
- [x] Retain the expected missing-contract failure before adding production code.
- [x] Implement the smallest pure model and resolver.
- [x] Run focused Core tests and the canonical check.
- [x] Complete one independent review and commit only this unit.

## Task 2: Immutable storage and owner save/import

Task 2a implements owner saves and manual resolution before provenance imports.
Its shared command and recovery contract lives in [listening-profile-save.md](../../listening-profile-save.md).
Root approved the existing synchronous checkpoint callback for this unit.
New immutable-object I/O runs outside the owner loop.
The checkpoint callback retains its existing bounded persistence latency.
No new 100 ms control-latency claim applies.
Task 2b still owns correction provenance imports and their complete workflow evidence.

### Task 2a checkpoint

- [x] Save and update named profiles through actual owner commands and the JSON CLI.
- [x] Provide bounded list/get and explicit offline manual resolution.
- [x] Verify immutable graph/profile objects before catalog reachability.
- [x] Preserve cancellation, retirement admission, safety state, and checkpoint recovery.
- [x] Run corrected Linux canonical checks and one independent main-model review.
- [x] Run the focused zero-device Mac owner/filesystem gate.

The corrected Linux check passes 633 tests, with three ignored and no warnings.
Its log SHA-256 is `f7325a4150b58d169f24d656a115b22caa85b9b56bb5978e45c3d1c0dbe2f5e9`.
The independent review receipt SHA-256 is `a31ba7dee305b69fd7fbc7fc22e5e81f82b629bd0139075ff5a321b5991d667e`.
The Mac gate passes 30 focused tests with zero failed or ignored.
It verifies 1,530 source files across five phases and 62 raw artifacts.
Both platform receipts bind the reviewed dirty precommit snapshot, not a clean release.
The public [precommit evidence](../../evidence/profiles/task2a-precommit-verification-20261118.json) preserves their exact hash bindings.
Post-gate edits add these documentation and evidence files only.
Correction provenance import, verified associations, apply, and packages remain incomplete.

### Remaining Task 2 work

Files: new `earph-engine/src/profiles.rs`, existing `assets.rs`, `runtime.rs`, `host.rs`, `main.rs`, and Core control/session modules.

Store immutable JSON objects by verified byte hash.
Reuse private-directory, create-new, owner, bounded-read, sync, and uncertain-outcome contracts.
Write all objects before a catalog checkpoint makes them reachable.
Preserve interrupted unreferenced objects under the no-deletion instruction.
Load validated documents into immutable shared owners.
Keep checkpoint references separate from loaded executable resources.

Bound the catalog to 32 profiles, 64 exact associations, and 16 results per page.
Keep only one pending import or profile preparation.
Preflight command receipt, response, catalog, checkpoint, and preparation capacities.
Use existing expected revisions and exact owner/session targeting for mutations.
Use `profiles.list/get/create/update/resolve/bind` through the shared owner command path.
Binding takes the owner’s actual selected-device evidence, not a caller-supplied trusted identity digest.
Read large local files through a bounded cancellable import worker.
Revalidate reports with `read_correction_report` and WAVs with the existing strict FIR importer.
Import creates saved intent and provenance without changing playback.

- [ ] Reproduce missing save/import behavior through actual owner/CLI requests.
- [ ] Implement private objects, bounded catalog, and persisted owner commands.
- [ ] Verify real text/FIR imports, stale edits, capacity limits, recovery, and persistence failures.
- [ ] Validate restart as paused/muted without consent, fixed arming, or prepared tokens.
- [ ] Run canonical checks, one independent review, and commit.

### Task 2b implementation checkpoint

Root authorized the bounded import contract after integrating Task 2a at `29a50dd`.
The owner imports real correction reports with their original source and explicit-purpose FIR WAVs.
The legacy FIR CLI now shares the owner worker or the same offline owner lock.
Text request strings remain in exact-content receipt history and count actual retained capacity.
Profile application, device binding, packages, and full DSP-06 qualification remain incomplete.

The same unit corrects reviewed save-path admission and immutable publication bugs.
Local Start waits for prepared-resource facts before profile work begins.
Unloaded catalog references block profile access while unrelated query contracts remain available.
Save and restore compile against the entire prospective catalog, independent of record ordering.
One shared publisher synchronizes complete staged objects and uses exclusive atomic rename.
It preserves all partial stages and existing corrupt objects without replacement.
Root approved Engine's direct Rustix `fs` feature without a version or lock change.

- [x] Exercise real text producer CLI, owner request CLI, retrieved report validator/exporter, and DSP consumers.
- [x] Preserve exact mono/stereo FIR taps, channel maps, preamp, and unknown metadata.
- [x] Verify tampering, stale edits, retained capacities, shared cancellation, and dependency recovery refusal.
- [ ] Finish canonical verification and freeze exact source provenance.
- [ ] Obtain one independent main-model review and the actual scoped Mac owner/filesystem gate.
- [ ] Commit after Root releases those gates.

## Task 3: Bound profile preparation and atomic application

### Approved Task 3a slice

Root approved option A against `296b4c5` on November 18, 2026.
Use the existing `8837` worktree and graph activation authority.
Prepare requires the current consented software output and matching backend, route, device, and fidelity.
Bind the immutable profile reference, effective caps, lower gain, and resource/latency limits to that candidate.
Application stays muted and publishes selection only after acknowledgment and checkpoint persistence.
Require acknowledged Stop before replacing the selected saved revision.
Keep fixed/hardware application, fidelity transitions, and native binding explicitly unavailable.
Keep local caps canonical and independent from effective profile caps.
This bounded slice does not complete Task 3 or qualify physical output.
The ignored `target/profile-application` directory retains its contract, ledger, and verification artifacts.

The Task 3a candidate now uses one profile-bound graph candidate and the existing persisted acknowledgment.
Core scenarios cover stale references, exact Session targeting, independent caps, cancellation, and delayed/failed publication.
GraphService scenarios cover revision/cap hashes, exact memory boundaries, actual source facts, and unsupported finite SRC latency.
The real owner/request scenario captures through ALSA file-to-null, then verifies fresh consent and safe restart.
It preserves capture bytes and request receipts under `target/service-playback`.
Native device binding remains unavailable, and live selected-profile replacement still requires completed Stop.
The only Worker edit initializes the new optional request field in an existing test fixture.
Canonical verification and the single independent review remain the root's integration gates.

Files: Core profile/control/session modules, `session_graph.rs`, and Engine `graph.rs`, `host.rs`, and worker callers.

Bind one existing graph candidate to profile ID, revision, object hash, and current graph/session/device bindings.
Keep idle save and structural preview independent of backend availability.
Prepare live execution using actual source cancellation and current backend facts.
Pass declared profile resource and latency constraints into compiler admission.
Reject unsupported finite aggregate SRC latency constraints explicitly.
Do not silently rewrite quality, remove correction, or change rates.

Recheck selected revision and effective caps before installation.
Reject live output migration with SOURCE_BUSY; require explicit Stop and output selection.
Stage selected revision, graph intent, effective policy, and readiness at the existing activation acknowledgment.
Persist that staged checkpoint before publishing applied.
Keep independent local caps distinct from profile-requested and effective caps.
Mute, disarm, and revoke old profile authorization on a changed saved revision.
Preserve the lower current level and expose blocked policy conditions.
Keep Stop and emergency mute available during preparation and activation.

- [ ] Write failing tests for stale revision, same graph with changed policy, cap escalation, and delayed acknowledgments.
- [ ] Implement one candidate path and the existing persisted acknowledgment transaction.
- [ ] Verify real owner captures, competing revisions, cancellation, Stop, restart, and failure recovery.
- [ ] Run canonical checks, one independent review, and commit.

## Task 4: Self-contained packages and final evidence

Use a strict manifest and fixed hash-named objects in an explicitly selected directory.
No archive or decompression support is required for this bounded first format.
Bound manifests to 64 KiB, packages to 64 objects, and total bytes to 32 MiB.
Verify every object type, size, hash, required reference, and provenance before catalog publication.
Reject symlinks, special files, traversal, arbitrary paths, undeclared assets, and identity collisions.
Never fetch URLs, execute content, auto-apply, or infer redistribution rights.
Never import local exact-device associations or verified-device context from a package.
Require an explicit redistribution choice before exporting third-party or unknown-rights measurement bytes.
Export no secrets, queue recordings, local paths, consent, arming, or runtime readback.

- [ ] Write failing real producer/consumer package and hostile-input tests.
- [ ] Implement bounded export/import through the owner contract.
- [ ] Verify fresh-runtime roundtrips for AutoEq PK/LSC/HSC, supported REW/Camilla text, and FIR correction assets.
- [ ] Verify two setups on one DAC, missing/corrupt assets, no auto-application, and truthful capabilities.
- [ ] Update requirements, state machines, public evidence, and the delegated dashboard.
- [ ] Run canonical and supported-platform checks, one independent review, then push and merge through a PR.

## Historical checkpoints

Base runtime: `2396efb38789038b20c2727afe8bc58409580be9`.
Worktree: `8837`, branch `feat/listening-profiles`.
Task 1 implements pure contracts and selection, without owner commands or application.
Seven public API tests pass after retained missing-contract failures.
The canonical check passes 610 tests, with three ignored and no warnings.
Its log SHA-256 is `bd1312784f51fcec1ec15b84b037d8ae055398ff458441b1aa63c5eb78f44f08`.
One independent main-model review approves the five-file unit without material findings.
Its receipt SHA-256 is `241247137625e5cac6fa52cc772de83d22e3fc699d97305e32678973d75e506e`.
Task 2a now saves and recovers verified immutable objects through the owner.
Its corrected canonical check passes 633 tests, with three ignored and no warnings.
One independent review and the focused Mac owner/filesystem gate pass.
The evidence above records the dirty precommit scope and exact hashes.
Task 2b implements correction provenance imports in the isolated task worktree.
Its verified checkpoint follows below.
Trusted native identity, profile application, and package workflows remain unwired.
T11 qualification continues independently against its frozen source.

## Verified correction import checkpoint

Frozen Task 2b source uses baseline `29a50dd` and runtime digest `6fa2f053b5b8e7d11ba10af22c710f4465595fd211559dac0d4788037aa73005`.
The canonical check passes 675 tests, with four existing ignored tests and no warnings or errors.
Its log SHA-256 is `1aab6636f9bf5ad39b885c302426429d32606886863921c38122279375b78878`.
One independent main-model review approves the exact 33-file source unit without material findings.
See the [precommit verification](../../evidence/profiles/task2b-precommit-verification-20261118.json).

Text imports reconstruct reports from original source bytes before saving intent and provenance.
FIR imports require explicit purpose and preserve rate, layout, taps, channel order, and preamp.
The shared owner serializes import work, immutable publication, and catalog reachability.
Save and restore use the same prospective-catalog admission.
Legacy FIR imports acquire the owner lock or forward to the running owner.
Relative forwarded paths resolve from the caller's working directory.
Interrupted stages remain preserved; exclusive publication never replaces an existing object.

The scoped Mac gate passes 18 overlapping test invocations and strict Core, Media, and Engine Clippy.
Actual CLI checks import, get, validate, export, reimport, relative FIR forwarding, and restart.
Both owner processes exit zero and are absent on fresh lookup.
Root verifies all 331 artifacts and 1550 source files across nine exact ordered checkpoints.
The [Mac candidate](../../evidence/profiles/macos-task2b-precommit-candidate-20261118.json) preserves dirty precommit provenance.
Its [root verification](../../evidence/profiles/macos-task2b-precommit-root-verification-20261118.json) binds actual exits and retained bytes.
The separate [source verification](../../evidence/profiles/macos-task2b-precommit-root-source-verification-20261118.json) checks every checkpoint's file order.

The first CLI helper expected a pre-shutdown revision after restart and failed.
Its corrected helper compares the actual shutdown receipt; both attempts remain preserved.
An initial collector stopped at a retained socket; the corrected archive records socket metadata.
Controlled interruption does not establish physical disk-sync failure or power-loss recovery.
Missing-interface RED does not prove the old newline-only output failure.
No native device opens or signal exercises occur in this gate.

## Resume point

Task3a implements manual application to the current consented software output.
The backend, route, device, and fidelity must match the selected output.
Saved intent supplies no endpoint, consent, arming, or trusted native identity.
The existing graph candidate and acknowledgment transaction apply the exact saved revision.
Application preserves independent caps and lower gain, then mutes, disarms, and revokes consent.
Replacing a selected revision requires acknowledged Stop and clears its selection.

The [Linux precommit receipt](../../evidence/profiles/task3a-linux-precommit-verification-20261118.json) records 696 passed tests and four ignored.
The canonical child exits zero, without warnings or errors.
The [same source review](../../evidence/profiles/task3a-source-review-20261118.json) approves the bounded implementation.
Its one coverage finding is corrected in the same review.
Core and actual owner tests grant consent before update and require revocation afterward.
The executable reads the persisted checkpoint and verifies muted, unconsented, unselected recovery.
Coverage-setup REDs remain classified separately from production regression proof.

This is a source checkpoint before integrated Mac qualification.
Keep the Task3a commit local until PR3 integration and supported-platform checks finish.
Merge the separately qualified OwnerLock and PR3 fixes before the Mac gate.
Then freeze the combined source and run the actual platform scenarios.
Full Task3 native associations and fixed/hardware transitions remain incomplete.
Task4 packages and full desktop workflows also remain incomplete.

### Task 4 pure package contract checkpoint

Root approves the schema-1 declaration and observed-inventory boundary.
The manifest binds one profile, one graph, and typed content hashes.
Bounds cover 64 KiB of manifest bytes, 64 objects, and 32 MiB total.
Owner observations cannot deserialize from imported JSON.
Owner import must verify original bytes, dependency closure, Media provenance, and directory safety.
Owner export still requires explicit exact-content and redistribution review.
This unit implements no filesystem authority, owner command, export, or automatic application.
The retained canonical check passes 705 tests, with four ignored.
One independent main-model review approves the three source files.
The [precommit receipt](../../evidence/profiles/package-contract-precommit-verification-20261118.json) binds source and evidence.
The package owner workflow remains incomplete.
