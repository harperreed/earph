# Local playback implementation

The approved specification supplies the design for T06 through T09.
Doctor Biz authorized continuous implementation, validation, and routine follow-ups without continuation prompts.
Preserve all source media, branches, worktrees, and existing artifacts.
Ask before deleting anything.
Physical signal emission and live device changes still require explicit approval.

## Success criteria

- Decode qualified WAV, FLAC, and AIFF integer PCM without a float intermediate.
- Verify exact samples, frame counts, seeks, and compatible track concatenation.
- Bound metadata, decoded packets, queue size, and control requests.
- Keep per-file failures visible and preserve source files.
- Reject DSD and detected DoP transport from the PCM path.
- Give one owner authority over queue, transport, receipts, and recovery.
- Restore queues paused and muted without opening a physical output.
- Integrate strict output assessment and explicit signal authorization.
- Verify actual executable flows with generated fixtures and private local acceptance data.
- Run the canonical check and one independent review before committing.

## Ownership

Root owns media decoding, executable flows, host integration, and documentation.
The local_queue agent owns the queue, owner control contract, IPC service, recovery, and their tests.
The agent does not stage or commit shared changes.

## Steps

- [x] T06: Implement bounded queue and read-only lossless decoding with TDD.
- [x] T06: Prove exact seek, frame continuity, failure handling, and private corpus comparisons.
- [x] T07: Add engine owner, local control service, CLI, and safe persisted recovery.
- [ ] T08: Join output negotiation and volume safety contracts.
- [ ] T09: Verify local playback through the real file output adapter.
- [ ] Review, check, commit, and continue ready work until the approved outcome or a real blocker.

## Evidence limits

Offline decoding does not qualify physical playback or acoustic safety.
The private corpus stays outside Git and public reports.
Only independently exercised formats receive qualified status.
DSP continuity and DSD remain separate requirements.

## Current verification

Generated lossless fixtures pass exact decoding, seeks, and executable concatenation checks.
The private corpus passes 51 PCM cases and four DoP rejection cases.
One RF64 case rejects explicitly; 34 other cases remain outside the qualified subset.
The private split-track capture matches its 288,000-frame continuous reference exactly.
The owner passes real IPC, process crash recovery, admission bounds, and persistence failure scenarios on Linux.
The independent review findings and fixes are recorded in `docs/local-playback-review.md`.
The native macOS canonical check passed 140 test results, including the compile-fail domain test.
Its IPC, executable crash recovery, and generated lossless fixtures passed without audio output.
T08 volume policy and prepared software attenuation exist but are not yet wired to a physical playback host.
