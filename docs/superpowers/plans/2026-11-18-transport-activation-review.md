# Transport and activation review fixes

Resolve review 5956783637 against source commit `5b22d0e`.
Preserve every prior worktree and test artifact.
Root owns digest admission, its tests, and evidence wording.
This task owns transport receipt identity and activation source facts.

## Success criteria

- Replay the exact pending transport command after ordinary receipt rotation.
- Reject changed content under that reserved request ID.
- Settle only the retained transport receipt and its exact history record.
- Preserve cancellation, replacement, failure, and staged commit behavior.
- Defer activation reservation changes until the matching playback acknowledgement.
- Publish actual open-file format and total frames with activation readiness.
- Keep seeks valid when an older readiness event was dropped.
- Preserve mute epochs and terminal source ownership.
- Pass focused regressions, canonical checks, and independent review before integration.

## Decisions

Keep one retained transport record outside the ordinary receipt window.
Retain its terminal result until another transport command is accepted.
Update a history slot only when it shares the retained record's exact Arc.
Queries and rejected commands cannot replace this record.

Add `source_total_frames` to the activation acknowledgement.
Read this count from the current open `LocalFile`.
Read source format from that file's playback observation.
Validate frame bounds, expected source format, and queue membership.
Publish readiness and playback in the same staged transition before persistence.
Do not consume another event slot to repair readiness.

While activation is pending, defer `LocalReserved` without advancing counters.
The matching acknowledgement publishes active and reserved entries together.
Keep ordinary join behavior unchanged.

## Evidence

All three focused reducer regressions failed before the implementation.
Receipt replay returned `StaleRevision` after its history record rotated out.
Reservation changed the active entry while playback still named the prior entry.
Activation persistence observed `Unprepared` after the old readiness event was rejected.

The native regression uses an owned WAV and ALSA NULL output.
It drops old-epoch readiness through the actual owner handler.
Activation reuses the open file and publishes its actual 65-frame count.
The atomic acknowledgement restores readiness and permits a valid seek.

Root's five-file digest and evidence patch received independent review.
Its decode, owner validation, and producer-ring counter callers match the intended changes.
No Important or Critical defect was found in that patch.

The combined canonical check passes 386 tests, with two ignored and no failures.
It includes root's reviewed digest and evidence changes as dependency copies.
Strict Clippy, formatting, requirement checks, and dependency checks pass.
The scoped final diff review found no further defect.
Root independently reviewed this task's actual diff and touched callers.
No Important or Critical finding remains in this patch.
The root combined canonical check also passes 386 tests, with two ignored.

Fresh review `5958051547` identifies two further Stop fault races and one wording issue.
Keep hardware qualification on hold while those source-terminal paths are checked.

## Authorization and limits

Doctor Biz authorized these source fixes and their validation.
Root will integrate, review, commit, and publish the combined result.
Do not delete files, use hardware playback, push, reset, or bypass hooks.
Historical platform evidence does not qualify these source changes.
