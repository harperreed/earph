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
