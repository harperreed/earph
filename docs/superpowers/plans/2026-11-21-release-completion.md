# Finish the first Earph release

Doctor Biz authorized continued implementation of the whole first release.
Use the definition of done in [SPEC.md section 19](../../SPEC.md#19-release-evidence-and-definition-of-done).
Its M0–M4 requirements define this release. Requirements marked LATER remain future work.
Keep [requirements.json](../../requirements.json) as the canonical requirement catalog.
Keep exact execution evidence separate from intended behavior and catalog progress labels.

## Completion rule

A passing unit, review, commit, or package is a checkpoint.
Continue through dependent work after each checkpoint.
Finish only when the first-release gates pass or a real external blocker prevents further authorized progress.
If one path needs physical evidence or approval, continue independent implementation and verification.
Ask Doctor Biz before deletion, credential expansion, or a new irreversible action.
Preserve all earlier builds, worktrees, evidence, user files, and unsaved windows.

## Verified starting point

The Play fix is published in `2c6ab56`; its evidence and usage notes are published in `4d8f6d5`.
The full repository check and six native Mac control checks pass for their recorded source snapshots.
The selected local track submitted all frames and closed output without an Engine error.
Hearing remains unconfirmed.

The same unsigned Mac development app now occupies the user's `Applications/Earph.app` path.
All 27 bundle files match the verified source package.
The copied app launches, while the existing Engine and window remain running.
This verifies one same-user delivery, not the complete AC-50 installation and rollback matrix.
See [delivery evidence](../../evidence/packaging/macos-user-install-20261121.json).

## Integration order and ownership

1. Implement the [pure Sendspin player timeline](2026-11-21-sendspin-player.md).
   Its isolated owner changes only Sendspin protocol, mapping, timeline, and focused tests.
2. Implement [typed DSD ownership](2026-11-21-dsd-engine-playback.md) in a separate worktree.
   Its owner holds the Core, Engine, and DSD projection files until Root integrates the result.
3. Resolve UPnP controller interoperability from actual producer and consumer evidence.
   Keep strict SOAP argument validation unless the inspected contract proves our behavior wrong.
4. Integrate Sendspin's paired admission, single source lease, scheduling, latency ledger, and live correction.
   Root starts shared Core/Engine edits only after DSD integration releases those files.
5. Run real Music Assistant and independent-controller scenarios against the resulting owner.
   Keep software, native, independent-clock, and acoustic evidence boundaries explicit.
6. Reconcile the remaining first-release requirements with their actual implemented entry points and evidence.
   Repair missing behavior before changing its status to complete.
7. Build and install the final named Mac/Linux candidates, then run the remaining release gates.
   Complete notices, source obligations, performance, failure, upgrade, rollback, and signing work within approved authority.

Root owns canonical integration, full checks, independent review, commits, and publication.
Workers own disjoint implementation files and retain failing-baseline and passing-test evidence.
The dashboard maintainer owns the existing progress page and its status source.
Do not create another output owner, credential store, transport, requirement catalog, or progress page.

## Gates that implementation cannot manufacture

DSD requires endpoint-specific qualification before physical submission.
PCM device negotiation cannot establish that qualification.
Sendspin readiness requires current admission, clock mapping, source ownership, and honest output timing.
A timestamp queue alone does not meet that contract.
Independent-clock and acoustic results require the named real setup and capture boundary.
Signing requires authorized signing material; an unsigned build remains unsigned.
Do not claim any of these gates from source inspection or a software file/null capture.

## Current work

The pure Sendspin timeline is integrated and passes the full repository check.
Its independent review cleared both findings after focused fixes and failing-baseline regressions.
See the [scoped evidence](../../evidence/network/sendspin-player-timeline-20261121.json).
Operational Network admission and Engine scheduling remain the next Sendspin work.
DSD Engine implementation continues in its separate worktree from `4d8f6d5`.
Native output timing adapters passed source review; Mac compilation remains pending.
UPnP's independent SDK passed its native API probe; current-owner controller qualification remains pending.
Root continues release integration while the dashboard records verified milestones.
Git commits and scoped evidence record completed work; this document defines the continuing execution boundary.
