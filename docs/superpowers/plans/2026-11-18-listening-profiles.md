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

The correction import commit is integrated with PR3's separately qualified engine fixes.
One independent bounded merge review finds no remaining integration regression.
The [combined precommit receipt](../../evidence/profiles/task2b-integrated-precommit-verification-20261118.json) records 701 passed tests and four ignored.
The actual canonical child exits zero, without warnings or errors.
The stronger shared report writer retains real flush and closed-consumer coverage.
FIR forwarding remains intact, and offline FIR import uses the shared owner-lock guard.
Both parents' test coverage remains; one byte-identical duplicate history test is consolidated.
The first duplicate-symbol build failure and original merge inputs remain preserved.
Earlier platform receipts keep their original source bindings.


## Task3a integration checkpoint

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

This integration has completed the source-bound Linux and Mac qualification recorded below.
The qualified OwnerLock and PR3 c0e27b9 fixes are present.
Keep this integration local until later PR3 review corrections are integrated and verified.
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
## Verified Task3a integration checkpoint

The [combined precommit receipt](../../evidence/profiles/task3a-integrated-precommit-verification-20261118.json) records 722 passed tests and four ignored.
The actual canonical child exits zero without warnings or errors.
The same independent review approves the bounded merge with the qualified PR3 source.
The [Mac candidate](../../evidence/profiles/macos-task3a-integrated-precommit-candidate-20261118.json) records scoped contracts and real muted owner control.
Root [verifies](../../evidence/profiles/macos-task3a-integrated-precommit-root-verification-20261118.json) all 503 artifacts and 1566 source files across ten phases.
All three final owners exit zero and remain absent on fresh lookup.
All observed states stay muted, with nineteen observed zero producer-write counters.
Requested caps remain separate from independent caps, and lower gain survives profile changes.
Stopped revision replacement revokes fresh consent and clears persisted selection before the next restart.
Selected device settings, source bytes, and executable hashes stay unchanged.

The first helper omitted the actual correction CLI build prerequisite.
Its fifteen passing profile cases remain retained; the one failed case passes after the explicit build.
The first muted helper expected exit zero for a correct rejected command.
Its owner shuts down cleanly; the corrected helper passes the full workflow in a fresh runtime.
Both helper failures and their artifacts remain preserved.
No signal, unmute, callback, private arming, DAC, or acoustic claim follows from this muted gate.

These receipts bind the dirty precommit integration of source24e9970 with parentc0e27b9.
Keep this integration local until PR3 merges and its later review corrections are integrated and verified.
The broader Task3 and Task4 work remains open.

### Task 4 Owner implementation scope

Doctor Biz approved the concrete Owner proposal v2 and its four shared commands.
Root approved bounded package results/diagnostics and new-ID imports preserving positive revisions.
Exact-content export review requires explicit export, content, and redistribution acknowledgments.
Known-content checks never claim automatic privacy proof.
The approved proposal remains under `target/profile-package-owner/approved-contract.md`.

Worktree 7ba8 uses branch `feat/profile-package-owner`.
Composition merge `a4595bf` joins pure schema commit `6f703e7` and qualified integration `dea1e08`.
The listening profile plan conflict preserves both checkpoints as a union.
Root seed `67b895385589a2129ea23196666386f474ac321a4afca28017f8c87b14440676` applies cleanly.
Approved publication patch `0aacb4a60c3866a1da05314fc8ecd88e5ee79239d480a81dbf27d1918dd14393` also applies cleanly.
Later Root Worker, CLI, and native corrections remain separate integration work.
No implementation commit or push occurs before the independent review.

Owner package work shares ProfileJobs, prospective catalog admission, prepared original bytes/banks, and immutable publication.
Import preserves exact JSON/text/WAV hashes and revalidates normal consumers before catalog checkpoint publication.
Export carries only required saved intent and provenance objects, with local source review outside the package.
The descriptor reader checks actual reachability and rejects unused declarations and hostile inventory.
The staged producer preserves every partial and reports post-install uncertainty truthfully.
Real owner/request executables and Media consumers supply local evidence.
Final canonical verification, independent review, and supported-platform gates must bind the final frozen source.
No Mac, native-output, physical-failure, full Task4 release, or automatic privacy claim follows from local source work.

### Task 4 same-review corrections

Root accepts two concrete P2 findings from the independent review of frozen revision-v2.
The review receipt is `9fae60dd7d8667d8c8a9b254d927f4bcd463833a37af56dbdd62a9966bd4a2a7`.
The same reviewer will reconcile the corrected revision3; no new panel is required.

FIR admission must count declared geometry, decoder scratch, original source buffers, and accumulated banks before reads or allocations.
The correction reuses GraphAssets geometry and deduplicates shared references.
It intersects profile and owner allowances before FIR reads, then replaces estimates with actual retained capacities.
The large-bank regression requires Budget when documents fit but the declared bank cannot fit preparation.
Cumulative-bank, owner-boundary, and actual shared-coefficient checks cover that accounting.

Package I/O must preserve missing, EOF, changed bytes, type, link, and traversal refusals.
Unexpected read, metadata, open, and enumeration failures must report InternalFault/io_failure.
A real write-only File tests a kernel read failure.
A private controlled reader injects EIO after an actual prefix and verifies retained uncertainty diagnostics.
That injection establishes the code path, not a physical filesystem or disk failure.
All earlier frozen source, runtime, logs, and partial artifacts remain preserved.
Revision3 requires focused and canonical verification before reconciliation.
Later Root CLI, Worker, native, short-path, Mac, and shipping gates remain separate.

### Verified Owner package local checkpoint

The [Owner package precommit receipt](../../evidence/profiles/owner-package-precommit-verification-20261118.json) binds the reviewed dirty Linux source.
Its canonical child exits zero: 761 passed, zero failed, four existing ignored.
Focused checks exit zero: 292 passed, zero failed, zero ignored.
Formatting, strict workspace Clippy, requirements, and dependency checks pass without compiler warnings or errors.

The reviewed source contains 1576 files and uses merge `a4595bf3cba59d3086dce376f76feeacc5696375`.
That merge combines pure package commit `6f703e739b9093805a21b8a23ad02e6bb9549cc1` and qualified profile integration `dea1e0878aa7fb966c3187bd196d4ccde2bc6a58`.
The approved Root seed and publication patch retain their exact separate hashes in the receipt.
The source manifest is `dceea5c4d7bea0ddf715d434a8cbf2baeff1493604d3064c4b69744ed713b37d`.
The runtime input digest is `5b464e2a9f5d750b53225506ada4333d25ae40de1dec19c23a0fb3d91b8cc919`.
The lockfile remains unchanged.

One independent review requests two bounded corrections, then the same reviewer approves their reconciliation.
FIR geometry, decoder scratch, source buffers, and cumulative banks now preflight before allocation or byte reads.
Unexpected storage errors now report InternalFault/io_failure while content refusals retain their meaning.
The reconciliation receipt is `0c30c8ae03553c26ae6c0c7367ad7210c63e5d82ddfb5275097cabe70873d10a`.
Its evidence verifies all 1576 source hashes and 55 bound artifact entries, with no material finding remaining.
Behavioral REDs, corrected GREENs, compilation attempts, source snapshots, and partial artifacts remain preserved.
Only this checkpoint and its public-safe receipt change after the reviewed source gate.

This is a local Linux checkpoint, with no new Mac owner-package qualification.
Later Root round2 CLI, Worker, native, and short-path integrations remain pending.
Supported-platform and shipping gates remain open.
Full Task4, completed T12, and public release are not claimed.

### Owner package and PR3 round2 integration in progress

Root authorizes merging reviewed round2 commit `2d10d01909d37c5fb0922116fd46593e453e69b9` into Owner package commit `7377b9bd6e9fb5ff76b29fa3e1ee9d2e9fb83a2d`.
The existing task branch retains both histories; the merge remains uncommitted for coupling review.
All earlier package and round2 receipts describe their own historical source snapshots.
They do not qualify this combined candidate.

Conflict resolution preserves exact package bytes, cumulative FIR admission, storage classification, and the shared directory-descriptor publisher.
It retains the round2 UTF-8 forwarding refusal before source I/O and the package-specific prepared bank lookup.
Both additive Core test groups remain present.
Linux-only native non-UTF-8 file coverage and the portable argument-refusal case retain round2 scope.
The prepared-FIR fixtures use round2 short runtime roots and the package-aware source access.
No public wire or security-control change is required by these resolutions.

Combined scoped checks pass 327 tests: Core 42, Media 41, and Engine 244, with no failure or ignored case.
The actual CLIs build and real package producer/consumer and owner/request executable cases pass.
The combined canonical check, independent coupling review, and supported-platform qualification remain pending.
Use the installed Rust toolchain directly and disable Cargo cache auto-cleaning for these checks.
Freeze exact combined source, dependency lock, runtime, logs, actual exits, and merge delta before one independent coupling review.
Root assigns that main-model review and performs any later normal commit.
No combined Mac, current-source physical playback, full Task4, completed T12, PR3 merge, or public release claim follows here.

### Owner package and PR3 round2 local merge checkpoint

Root accepts the independent coupling review for this local merge checkpoint.
The package parent is `7377b9bd6e9fb5ff76b29fa3e1ee9d2e9fb83a2d`.
The round2 parent is `2d10d01909d37c5fb0922116fd46593e453e69b9`.
The reviewed staged tree is `b5de71d6098fd7add65f058dd8d052bfdc419c27`.
The review binds 1580 source files and 42 artifact entries, with no open material finding.
Its receipt hash is `c9a715d1bccc8fe6a1b7319bbd54baafe7920c4002f263e16350246c29d167a2`.
Its evidence hash is `91d3b8eb9f7878d151cb8fa12fbba88deaca4c6a8da644583d658164df79146e`.

Combined scoped checks pass 327 tests: Core 42, Media 41, and Engine 244.
The one combined canonical check passes 783 tests, with zero failures and four existing ignored tests.
Each check records its actual exit as zero.
Scoped tests overlap the canonical suite; do not add their counts as separate coverage.
Strict Clippy, formatting, catalog validation, dependency validation, and actual CLI builds pass.
Real package producers, consumers, owner/request executable roundtrip, restart, and file-to-null playback cases pass.
The runtime truthfully records the package parent, dirty combined source, Linux x86_64, and debug profile.

The public receipt is [the integration precommit verification](../../evidence/profiles/owner-package-round2-integration-precommit-20261118.json).
Its SHA256 is `135ce015f5254076b15982b52e2732caee948d1f796c97b98734b9f3e163eaa2`.
It binds exact source, parent deltas, manual resolutions, runtime, lock, logs, actual exits, and review hashes.
The original zero-count receipt and corrected receipt remain preserved with their parser and logs.
The correction reads numeric Cargo columns instead of labels and changes no actual result.
Historical receipts keep their original source and platform scope.

Only this appended checkpoint and its public receipt change after the reviewed source gate.
No runtime source change or suite rerun occurs in this documentation checkpoint.
Root performs any later normal merge commit after verifying the final staged tree.
Combined Mac, physical output, clean release, full Task4, completed T12, and release qualification remain pending.

### Approved observed-device association unit

Root approves the continuation contract `2c8d4b810a3c204dfb55053dc414b06028492c042f53d9911ac3fbb6cb6cf485`.
Its source base is local merge checkpoint `d449f93eb0aed26ce0332dcbc2d43232162dc19e` in worktree `7ba8`.
The uncommitted merge adds reviewed diagnostic prerequisite `98244f0d913204917fe1b66e3b974f4e6c99d801`.
Only its two known files enter that prerequisite merge.

Implement Core Audio native identity publication, bounded persisted associations, existing resolver wiring, and typed receipts/capabilities.
Bind observed identity to the existing profile candidate/hash and persisted activation acknowledgment.
Keep one owner, checkpoint, candidate store, and activation authority.
Retain current consent, fixed arming, lower gain, independent caps, and exact-byte package privacy.
ALSA associations remain unsupported without a stable native identity producer.
Do not implement fixed-purpose preparation, hardware volume, or device/fidelity migration in this unit.

Use TDD with retained RED/GREEN logs and actual numeric exits.
After the final source freeze, run scoped checks and one canonical verification.
Root assigns one independent main-model review before any normal local commit.
The positive native owner E2E needs a later Root-released Mac gate.
This local unit makes no new Mac, physical-output, full T12/AC-35, or release qualification claim.

### Observed association local checkpoint

Root accepts one independent review of the frozen observed-association unit.
Canonical exits zero: 796 passed, zero failed, five ignored, with no compiler diagnostics.
Scoped checks pass 357 tests and overlap canonical coverage.
The added ignored stderr helper runs explicitly through its parent.
The [local receipt](../../evidence/profiles/observed-associations-linux-precommit-20261119.json) binds the exact tested dirty tree and runtime.
Only this checkpoint and its public-safe receipt change after that verification.
Positive native owner E2E and combined Mac qualification remain pending.
ALSA stable identity, fixed/hardware application, full T12/AC-35 and release remain unqualified.
