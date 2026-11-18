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
Push, remote feedback resolution, and merge remain pending.

Closing-source readiness now survives resume attempts and ordinary receipt rotation.
Activation reserves its final event slot across source, queue, resource, and command updates.
Terminal source states cancel graph work even when epochs remain unchanged.
Reserved-slot rejection of a safety command cancels the worker and settles graph failure.
Ended-source pause and seek fail explicitly instead of retaining accepted receipts forever.

The original Linux and Mac hardware receipts qualify the earlier source snapshot only.
Do not copy their success onto the review fixes.
Run new source checks and record their scope before declaring this change qualified.

## Continuity

PR: https://github.com/harperreed/earph/pull/2
Root worktree: `/home/harper/.codex/worktrees/a804/earph`, branch `feat/dsp-graph`.
T11 worktree: `/home/harper/.codex/worktrees/e49c/earph`, branch `feat/dsp-nodes`.
T11 shared equalizer parameters are saved at `f945a2b`.
The PEQ kernel continues independently while these fixes block merging PR #2.
