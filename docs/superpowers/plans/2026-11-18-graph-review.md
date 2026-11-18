# Graph review fixes

Continue PR #2 through fixes, verification, review, and merge.
Keep the historical hardware receipts bound to their original source digest.
Preserve all worktrees, artifacts, and private source media.

## Ownership

- Root owns Core, wire precision, compiler tests, render tests, inventory, and documentation.
- The service fixer owns preparation previews, retirement joining, and publication tests.
- The race fixer owns host acknowledgements, worker terminal handoff, and checkpoint recovery.
- Reviewers inspect the combined change without editing.

## Success criteria

- Stop cannot prepare a graph from the closing source observation.
- Graph commit reserves completion counter capacity before consuming its token.
- Activation failure publishes one terminal event with its request identity.
- Bounded semantic rejections retain request identity.
- Oversized graph frames reject before graph deserialization.
- JSON preserves finite coefficient bits across serialization and parsing.
- Integer graphs reject unsupported metering and report software attenuation.
- Service shutdown cannot wait for a pipeline held by its caller.
- Graph activation publishes its actual playback observation atomically.
- Superseded acknowledgements cannot terminate the owner.
- Mute cannot strand a completed stream in draining.
- Uncertain checkpoint replacement reconciles before restoring graph intent.
- Default compiler tests write no files into the checkout.
- Cancellation allocation tests exercise a current stream packet.
- Canonical checks and independent review pass before push and merge.

## Current evidence

CodeRabbit reviewed head `ced9ae24e4865256058c51e0d04ea6b1ec3e0c90` and requested changes.
The baseline Linux canonical check passed.
Root reproduced four Core state regressions before fixing them.
Core tests pass after those fixes and the atomic playback observation change.
The service fixer reproduced three regressions before fixing them.
Its targeted suite passes 25 tests with strict Clippy.
Compiler tests pass 14 scenarios after making evidence export optional.
Host, worker, and checkpoint fixes pass their focused checks.
Thirty engine unit scenarios, ten real playback scenarios, and twelve owner/storage scenarios pass.
Independent review verified the combined contracts and their focused corrections.
It found no remaining Important or Critical issues in this scope.
The integrated Linux canonical check passes before the final documentation updates.
The frozen runtime commit is `cbc2785bbe8c9d4fbfcf0ccb5988185e822b60ce`.
The final Linux canonical check passes 338 tests, with two ignored.
The Mac canonical check passes 315 tests, with two ignored.
Its separate eleven-test silent SDK recheck overlaps canonical coverage.
One approved attenuated graph exercise passes on each selected route.
Linux submits 48000 source frames plus seven tail frames.
Mac submits 24000 source frames plus three tail frames.
Root verifies the raw counts, log hashes, source hashes, and unchanged recorded settings.
Mac verification covers 62 raw artifacts and 1448 archived source inputs.
Two muted Mac graph swaps report current playback observations without submitting samples.
Redacted review receipts now live under `docs/evidence/dsp/`.
The fixes and receipts are pushed at `42eb803`.
All twenty original review threads are resolved.
A fresh review identified sixteen inline findings and four findings in its summary before merge.
Root and the worker owner are fixing the underlying transition and completion boundaries.
The prior platform receipts remain bound to `cbc2785`; they do not qualify these later changes.

The integrated second-round Linux canonical check passes 357 tests, with two ignored.
Independent review approves the terminal protocol and counter policy without Important or Critical findings.
Proved completion closes the source even when a terminal counter cannot advance.
It never wraps counters and requests owner shutdown before further controls.
A fresh owner reseeds only an exhausted persisted revision.
Ordinary revisions, queue intent, and startup safety retain their existing contracts.
The final diff review found no further in-scope defects.

Two summary findings required additional fixes after the inline review intake.
Activation now publishes the acknowledged active and reserved queue entries atomically.
Bounded semantic graph failures now reach owner receipt history over IPC.
Regression tests reproduced both failures before the fixes.
The claimed same-epoch graph rewind is unreachable through the inspected Session controls.
An explicit mute and volume-cap test proves epoch advancement and repeated-mute stability.
Keep the existing reset guard, which rejects unchanged epochs.
Independent review approves this focused delta without material findings.
The final integrated Linux check passes 361 tests, with two ignored.
All thirty-six inline review threads are resolved on the remote PR.
Platform checks for `f634853` remain historical and do not qualify this final delta.

The final runtime source is frozen at `ad6ef0ff62c49b199225b4314b7e926164f6fafd`.
Clean committed builds pass 361 Linux tests and 331 Mac tests, with two ignored on each platform.
The overlapping eleven-test Mac SDK recheck passes.
One approved final signal per platform completes with zero saturation and unchanged recorded settings.
Linux submits 48007 frames; Mac submits 24003 frames.
Both owners release graph resources and exit cleanly.
Root independently verifies raw counts, digests, all 67 Mac artifacts, and four source-manifest checks.
Final redacted receipts live under `docs/evidence/dsp/final-review-verification-20261118.json`.
The remote summary reply records all four outside-diff findings and the reset-contract rebuttal.
No cleanup or file deletion occurred.

## Second review fixes

Track pending pause, seek, and stop acknowledgements independently from visible transport state.
Retain one completed drain until the owner acknowledges its exact token.
Keep terminal proof across old-entry snapshots and repeated mute generations.
Apply proved completion atomically and preserve consent after normal EOF.
Settle raced graph and repositioning receipts without inventing an output fault.
Read an already-matching recovery baseline without rewriting it.
Reject strict-route gain ramps before graph preparation.
Make failure tests distinguish timeout from disconnection and wait for retirement acknowledgement.

Do not implement automatic removal of failed temporary files.
Doctor Biz explicitly requires approval before deletion.
The documented owner policy preserves partial writes for inspection.
Cleanup remains a separately approved operation.

Closing-source readiness now survives resume attempts and ordinary receipt rotation.
Activation reserves its final event slot across source, queue, resource, and command updates.
Terminal source states cancel graph work even when epochs remain unchanged.
Reserved-slot rejection of a safety command cancels the worker and settles graph failure.
Ended-source pause and seek fail explicitly instead of retaining accepted receipts forever.

The original Linux and Mac hardware receipts qualify the earlier source snapshot only.
Do not copy their success onto the review fixes.
Run new source checks and record their scope before declaring this change qualified.

## Retained source terminal results

Doctor Biz reaffirmed hardware playback and evidence approval on November 18.
Keep the existing quiet-signal limits and selected endpoints.
Preserve all artifacts and private media.

The combined third-round check passes 377 tests, with two ignored.
Core and engine owners independently reviewed each other's changes.
Source failures now retain their proof until successful owner application and exact acknowledgement.
Mute still advances its safety epoch.
Terminal closure never wraps counters and retires at the last available counter.
Fresh owners reserve 64 revision advances near exhaustion.
Ordinary checkpoint revisions stay unchanged.
Graph and transport terminal events retain their separate request identities when capacity permits.
Actual installed graph identity survives older queued settings.
Ramp length contributes to activation identity, and replacements clear old sample peaks.
Private oversized candidates no longer block valid pending checkpoint recovery.
Ownership and permission failures still reject recovery.

Fresh review `5956783637` adds three inline findings and two summary findings.
Investigate pending transport receipt identity, activation readiness, and reservation publication before qualification.
Move graph digest validation into retained semantic admission if the reported path reproduces.
Clarify that Mac submitted frames measure producer-ring writes, with consumption still unknown.
Earlier native receipts remain bound to their original source snapshots.

The retained digest rejection now passes Core integration and real owner/CLI checks.
Both regressions fail on the prior wire-admission path.
The root canonical check passes 379 tests, with two ignored.
Independent caller review approves the digest and evidence-wording changes.
Mac receipt wording now identifies producer-ring writes and derives the tail from graph delay.
Core Audio acceptance and consumption remain unqualified.
The parallel state owner is fixing pending transport identity and activation queue readiness.

## Sixth review checkpoint

Review `5958674967` identifies four reproduced gaps against `5cb9fdc`.
Hardware qualification remains held until the fixes pass review and checks.
Earlier hardware evidence keeps its original source binding.

Bound graph digest retained capacity to 128 bytes before decoding admission or owner history.
Keep bounded malformed digests on retained semantic rejection paths.
Typed commands also check reserved String capacity, including short values with oversized allocations.
Rejected oversized intent does not reserve a request ID.

Stage activation readiness privately until the matching acknowledgement persists.
Publish current-file facts and deferred next-entry facts in the same transition.
Drop deferred readiness when activation fails.
Prune removed entries before staging another fact to preserve the queue bound.
Ordinary readiness updates keep their existing behavior.

Treat preparation cancellation as successful Stop settlement.
Keep genuine source errors even when Stop sets the shared cancellation flag.
Keep requested Stop binding and installed graph identity unchanged.

Give the native write-fault subprocess its own ALSA configuration.
Use its explicit file PCM to reach `/dev/full` under custom ambient configurations.
Preserve logs and partial artifacts without changing host configuration.

Focused regressions reproduce each original gap before its fix.
Real IPC verifies oversized rejection and request-ID reuse.
The typed capacity regression reproduces retained oversized allocation separately.
Core activation tests cover persistence failure, reserved counters, next entries, and queue churn.
Real worker and host tests distinguish cancelled preparation from genuine source errors.
The native subprocess regression passes under a minimal ambient ALSA configuration.
The combined canonical check passes 400 tests, with two ignored and no failures.
Formatting, strict workspace Clippy, requirement checks, and dependency checks pass.
Independent main-model review approves the actual diff without material findings.
The reviewer passes 134 Core tests, 62 engine tests, and the real IPC regression.
Fresh platform qualification remains required for this source snapshot.

## Continuity

PR: https://github.com/harperreed/earph/pull/2
Root worktree: `/home/harper/.codex/worktrees/a804/earph`, branch `feat/dsp-graph`.
T11 worktree: `/home/harper/.codex/worktrees/e49c/earph`, branch `feat/dsp-nodes`.
T11 shared equalizer parameters are saved at `f945a2b`.
The PEQ kernel continues independently while these fixes block merging PR #2.
