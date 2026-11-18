# Local source and owner review

The independent decoder review inspected implementation, callers, requirements, tests, and published Symphonia sources.
It reproduced five defects with real files or executable processes.

| Finding | Resolution | Regression |
|---|---|---|
| DoP markers escaped detection in 32-bit containers. | Check the marker byte at both supported carrier widths. | `dop_in_a_32_bit_container_is_rejected` |
| Embedded FLAC metadata ignored library option limits. | Bound cumulative container metadata before probing. | `oversized_container_metadata_is_rejected_before_probe` |
| A forged short frame count hid remaining audio. | Confirm actual EOF after the declared count. | `a_forged_short_frame_count_cannot_hide_audio` |
| Partial writes produced an incorrect capture hash. | Hash actual captured bytes and report partial-frame bytes. | `a_partial_write_failure_reports_the_actual_capture_hash` |
| FIFO admission could block before file validation. | Check file type and open with nonblocking flags. | `fifo_input_is_rejected_without_waiting_for_a_writer` |

The independent owner review inspected core control, recovery, IPC, runtime storage, and volume safety.
It also inspected the prepared gain ramp without finding a numerical or unity-bypass defect.
It reproduced a directory-alias bug that redirected checkpoint writes outside the owner's held lock.
The owner and client now pin the validated resolved runtime path.
`changing_a_runtime_alias_cannot_redirect_a_locked_owners_checkpoint` failed before the fix and passed afterward.

The owner's final review response was blocked by a tool safety filter after its findings arrived.
The maintainer preserved those findings and completed the corrective review directly.
The review does not claim an uninterrupted independent final approval.
A second independent reviewer inspected the corrective owner changes and the gain and safety modules.
That targeted review found no further defects.

Path validation now runs before IPC commands enter retained history.
This uses the queue's existing path validator rather than duplicating its bounds.
The regression rejects empty, oversized, and NUL-containing paths for both append commands.

The canonical check covers formatting, denied lint warnings, tests, catalog consistency, and locked dependency provenance.
The native macOS canonical check passed 140 test results, including the compile-fail domain test.
The Linux canonical check passed 150 test results, including that domain test.
Both builds report source digest `94ff3c4fa410d36e80af7024483ce81c06735afb29a1b9965b9f01c1c8f9f1b7`.
Both builds report lock digest `7b308b8b7fb1f3e7586c03e19959c9546406b759160a2bb9645990be74c76545`.
These were precommit builds with the modified-source flag set.
Native testing exposed unsupported Darwin socket-creation flags and a peer-close timeout behavior.
Linux uses atomic socket flags; Darwin sets nonblocking and close-on-exec options through its supported APIs.
Framed I/O now uses nonblocking reads and writes with absolute polling deadlines.
The peer-close regression failed on the Mac before the fix and passed afterward.
Physical playback, hardware mutation, DSP graph continuity, and acoustic safety remain outside this review.

Generated notices retain upstream license text, including seven trailing spaces in the MPL copies.
Code and handwritten documentation pass whitespace checks.

## Shared output integration review

The maintainer independently reviewed the integration agent's shared reducer, worker, IPC, and ALSA changes.
The review inspected command admission, consent, gain caps, queue reservations, persistence, cancellation, and native cleanup.
No blocking defect remained in those files.
The integrated Linux canonical check passed 192 test results with one explicit hardware gate ignored.
The four independent executable scenarios passed through real owner and client processes.

The Core Audio adapter received a separate independent ownership and callback review.
The maintainer also reviewed the later SDK timing and scheduled-handoff changes independently.
The approved hardware follow-up showed why callback count cannot establish final-buffer completion.
The SDK clock check remained false at application drain and after two later callbacks.
It became true when the tracked final buffer crossed the sample and host clock boundary.
The committed redacted evidence records that boundary without claiming DAC presentation.

The native engine acceptance test first failed before unmute with `CAPABILITY_UNSUPPORTED`.
The new engine bridge passed independent review and the native owner-and-CLI acceptance.
The native canonical check passed 180 test results with two hardware tests ignored.
The explicit engine hardware test then passed separately.
It submitted 60,000 frames after the seek and preserved unknown physical timing fields.
It reported float32 client data separately from signed24 physical data in eight-byte stereo frames.
The generated source and selected settings remained unchanged.
See [the scoped report](evidence/local-playback/coreaudio-engine-20261118.json).

Fresh review then identified a command-admission gap under worker queue saturation.
The correction reserves bounded FIFO safety capacity and cancels after failed safety delivery.
It reports failure instead of leaving a playing snapshot or claiming an unacknowledged stop.
Three real-channel regressions and the shared-state failure test passed.
The focused independent review found no remaining admission defect.

The first Linux hardware run exposed the nonblocking drain lifecycle.
Native I/O readiness polling reported an error during the final drain.
A zero-valued regression reproduced errno 5 before the correction.
The first pacing correction still timed out because each drain call reinstalled end silence.
The final correction starts hardware drain once and observes native completion state.
Plugins can finish committing buffered input before their slave enters draining.
The guarded regression also rejects a valid next PCM span after draining begins.
The successful trace contains one hardware drain call and no later parameter rewrites.
See the [native drain result](evidence/local-playback/alsa-drain-state-20261118.json) and [trace summary](evidence/local-playback/alsa-drain-trace-20261118.json).

Independent review also caught a diagnostic panic when stderr could not accept writes.
A real `/dev/full` subprocess reproduced it.
Best-effort native diagnostics now preserve the original error when writing fails.
The focused correction review passed.
The final Linux canonical check passed 198 test results with two hardware gates ignored.
The separate [owner-and-CLI signal acceptance](evidence/local-playback/alsa-engine-20261118.json) passed after that check.
It submitted 48,000 frames, reached idle without an error, and preserved source and mixer settings.
Client consumption estimates do not establish DAC preservation or acoustic safety.
Final native macOS checks passed 184 test results with two signal tests ignored.
The final source transfer matched all 1,419 archived input hashes before and after checking.
Its source digest is `76fec754015fec38d50d17cd6fe18d37feba61023cb1fa85cb401a80a4ed5a53`.
No further signal or device-setting changes occurred during that check.
