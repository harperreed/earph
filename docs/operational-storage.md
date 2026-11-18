# Operational checkpoint recovery

The local owner stores its queue and configured graph in `queue.json`.
The owner lock permits one writer for a runtime directory.
Each checkpoint contains at most 2 MiB.

Every checkpoint write uses one `checkpoint-recovery.json` record.
Its schema version is 1.
Its byte limit is twelve MiB plus 512 bytes, including JSON escaping.
Both files keep the existing private ownership and permission checks.

The record has two states:

- `pending` stores the previous checkpoint as a JSON string, or `null` for initial absence.
- `settled` stores the SHA-256 digest of the committed checkpoint.

Before replacing the checkpoint, the owner writes and syncs the pending record.
The owner then writes, syncs, and renames the checkpoint.
After validating the current queue reservation, the owner writes and syncs the settled record.
Only then does it publish the queue and report success.
The reservation guard spans the final record write and sync.
This path does not establish whole-worker realtime performance.

A failure before checkpoint replacement preserves the previous bytes.
A failed directory sync after rename leaves the replacement outcome uncertain.
The writer reads back the exact destination before retrying directory sync once.
If that retry fails, it reports an explicit uncertain-outcome error.
It never reports that rename was undone.

Startup reconciles recovery before restoring a session.
It syncs the directory before interpreting an existing recovery record.
For `pending` with previous bytes, it restores and syncs those bytes, then settles that result.
If the validated checkpoint already matches those bytes, startup reads it without another write.
That read-only recovery can leave the pending record until the next transaction.
For `pending` with initial absence, it restores an empty logical state and retains the pending record.
The uncertain candidate stays on disk until a later successful transaction replaces it.
No recovery path deletes a file.
Failed replacement temporaries remain available for inspection under the task's explicit retention policy.
Cleanup requires separate approval.
For `settled`, it checks the checkpoint digest before loading any graph intent.
Invalid records, digest mismatches, or failed recovery refuse startup.

An uncertain settled-record write can recover the replacement checkpoint.
An uncertain pending transaction recovers the previous checkpoint.
The uncertain error therefore promises neither outcome before recovery.
Recovered graph intent always starts muted and unprepared.
Filesystem and storage faults beyond successful sync semantics remain outside this guarantee.
