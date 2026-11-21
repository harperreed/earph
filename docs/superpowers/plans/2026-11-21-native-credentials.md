# Native credential storage

Doctor Biz authorized this bounded adapter and its isolated qualification.
Root owns integration, commits, and merged canonical checks.

## Contract and assumptions

Keep `CommittedCredentials` and `RecordCatalog` canonical.
Accept generated identities only from callers after definitive absence.
Require explicit native configuration and one exclusive owner lock.
Run blocking native APIs only in a bounded, owned helper process.
Keep secret IPC in anonymous pipes; redact every diagnostic.
Treat write cancellation, helper loss, and failed readback as uncertain.
Keep uncertainty latched until caller-proven terminal completion and a later fresh recovery read.
Never unlock, prompt, delete, repair, or use plaintext fallback.

## Files

Add `crates/earph-credentials` with supervisor, protocol, and platform adapters.
Add focused process tests and isolated native qualification operators.
Change workspace membership and the new crate's dependency closure only.
Keep the eight reviewed codec files as borrowed foundation inputs.

## Success criteria

- [x] Public process tests distinguish missing, locked, unavailable, ambiguous, invalid, and uncertain outcomes.
- [x] Commit acknowledges only exact fresh encoded-byte readback.
- [x] Operation IDs survive uncertainty and replacement requires fresh recovery.
- [x] Cancellation kills and reaps owned children within the operation deadline.
- [x] Linux qualification uses fresh owned D-Bus and Secret Service processes.
- [x] Native startup follows Root's byte review of the private operator.
- [x] macOS compile and isolated keychain gates exist without contacting a real keychain.
- [x] Tests, strict Clippy, formatting, dependency closure, and own-only snapshot remain bound to source hashes.

## Qualification and remaining gates

Root released Linux operator SHA256 `ba05beef7bc6b27e0df84acf3ee873ce3a8f145460b77dbd3f79afb70f248b1c` before startup.
The real isolated Secret Service qualification passed with exact create/update readback and failure-state checks.
Ordinary crate tests, strict Clippy, formatting, and regenerated dependency inventory checks passed.
All native processes joined, and every private fixture remains retained.
macOS compilation and isolated keychain runtime qualification remain unrun hosted gates.
Root owns independent review, combined canonical checks, integration, and publication.

Root integrated the corrected unit after its independent review cleared the directory-sync finding.
The [merged Linux receipt](../../evidence/sendspin/native-credentials-linux-20261121.json) binds the canonical check and explicit native qualifiers.
The canonical run reports 1,662 passes, zero failures, and 15 ignored tests, including child results.
All six session peer tests and three outbound peer groups passed separately.
The actual isolated native test and four directory-sync cases also passed.
A corrected qualification driver selected the real helper instead of its separate test harness.
It verified identical source and passing logs before completing the remaining checks.
The hosted workflow now includes isolated Keychain qualification and an allowlisted evidence reporter.
Actual Mac execution remains pending publication of this checkpoint.

## Directory durability review fix

Root's independent review found a missing parent-directory sync before first native mutation.
Open the marker relative to its retained directory handle.
Sync initialized marker bytes, then sync that directory before allowing operations.
Return uncertainty on directory-sync failure and preserve existing pending-state bytes.
The focused regression failed before this fix because opening still succeeded.
Real syscall checks now prove file/directory/helper ordering and directory-failure refusal.
Power-loss simulation remains unperformed; no privileged filesystem probe belongs to this unit.
Preserve the first complete snapshot and record this fix in a separate complete snapshot.

## Limits

Native stores provide no compare-and-swap across unrelated writers.
The owner lock serializes cooperating Earph owners using the same lock.
Preflight and exact readback detect ordinary races but cannot exclude later external writes.
No Engine startup or Sendspin transport integration belongs to this unit.
