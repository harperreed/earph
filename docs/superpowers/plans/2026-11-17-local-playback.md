# Local playback implementation

The approved specification supplies the design for T06 through T09.
Doctor Biz authorized continuous implementation, validation, and routine follow-ups without continuation prompts.
Preserve all source media, branches, worktrees, and existing artifacts.
Ask before deleting anything.
Doctor Biz approved hardware playback and evidence during this task.
The hardware acceptance scope below records that approval.

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
- [x] T08: Join output negotiation and volume safety contracts.
- [x] T09: Verify local playback through the real file output adapter.
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
T08 volume policy and software attenuation now run through the shared playback worker.
The integrated Linux canonical check passed 192 test results and one ignored hardware gate.
Real owner and CLI tests verify exact joins, pause, seek, gain bounds, and missing-source failures.
Worker tests also verify reservation conflicts, output faults, running stop, and seek to EOF.
The native Core Audio adapter passed the approved lifecycle and scheduled-handoff hardware runs.
Adapter evidence does not qualify the unfinished native engine integration.
The native engine acceptance test first failed with `CAPABILITY_UNSUPPORTED` while muted.
The Core Audio engine bridge passed independent review and native acceptance.
Native canonical checks passed 180 test results with two hardware tests ignored.
The explicit owner-and-CLI hardware acceptance then passed separately.
It verified muted preparation, pause, seek, resume, and natural drain through the selected route.
Its redacted report is `docs/evidence/local-playback/coreaudio-engine-20261118.json`.
The tested precommit source digest is `1ccaed900a06399f95ea3e7f396d39334552c14f80e0ef3f837a3baa869c6516`.
The test kept settings and its generated source unchanged.
Audible position, consumed position, backend delay, and DAC preservation remain unverified.
Fresh review found that ordinary command saturation could reject stop or mute.
The worker now reserves bounded safety capacity and preserves FIFO command order.
Exhausted or disconnected safety delivery cancels playback and reports failure without claiming native closure.
Real-channel regressions and shared-state tests passed after the correction.
The final Linux canonical check passed 198 test results with two hardware gates ignored.

The first approved Linux signal submitted 48,000 frames but reported a terminal wait error.
Zero-valued PCM reproduced the mismatch with native errno 5.
The first pacing correction timed out because repeated drain calls reinstalled hardware end padding.
The final correction initiates hardware drain once, then observes its native state.
It permits plugin commit work while the native stream remains prepared or running.
The gated zero-valued regression passed with one hardware drain call and no repeated parameter writes.
The final real owner-and-CLI signal then passed with 48,000 submitted frames and matching client estimates.
Source and mixer settings stayed unchanged.
Both failed attempts and successful results remain in `docs/evidence/local-playback/`.
Final native macOS checks passed 184 test results with both hardware-signal gates ignored.
The source digest is `76fec754015fec38d50d17cd6fe18d37feba61023cb1fa85cb401a80a4ed5a53`.
The final source transfer preserved and verified all 1,419 input files.
This final check emitted no signal and changed no device settings.

## Output integration

Foundation commit: `754086c`.
The Linux integration agent now owns the shared session commands, owner worker, and ALSA transport integration.
Root owns independent executable acceptance and the final integration review.
The Core Audio agent owns its adapter in a separate worktree on `feat/coreaudio-playback`.
These implementation owners do not edit each other's files.
Root reviewed the shared reducer, worker, IPC, reservation gate, and ALSA extensions without blocking findings.
The Core Audio agent reviews root's native engine bridge and executable acceptance independently.

Software onboarding uses explicit attenuation under compatible-conversion policy.
Strict preservation requires an unchanged integer path and compatible volume policy.
Fixed unity requires arming for the current device and profile.
Consent and arming never restore after an owner restart.
The initial adapter checks ran without physical signal writes or device mutations.

The implementation must exercise real ALSA file-to-null capture before requesting physical acceptance.
Native Core Audio acceptance now includes the approved scoped playback run below.
The shared host must report unknown hardware timing without substituting a fabricated value.

## Hardware acceptance approval

Doctor Biz said, “ok. i approve hardware playback and evidence,” on November 17, 2026.
This approval covers Earph hardware playback and scoped test evidence on the selected Linux and macOS outputs.
Refresh each explicit device identity and current format before starting.
Use short generated signals with software attenuation of −40 dB or lower and bounded ramps.
Keep raw device reports private and redact shared evidence.
Preserve hardware gain and unrelated audio settings.
Do not use microphones or capture unrelated audio.
Approval does not authorize deletion or private music transfer.
Keep advertised, opened, exercised, and independently measured claims separate.

## Live progress dashboard

Doctor Biz requested a dedicated dashboard maintainer and parallel execution.
The progress_dashboard agent owns the page, source, publication, browser checks, and ongoing updates.
The private dashboard is `https://artifacts.folk-fujita.ts.net/p/earph-progress/`.
Root continues playback integration and independent acceptance.
