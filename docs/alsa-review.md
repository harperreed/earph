# ALSA spike review and verification

Reviewed the T05 working diff against `5e84f21` on November 17, 2026.

The independent reviewer inspected native safety, ownership, negotiation, signal approval, lifecycle, evidence, callers, and dependencies.
The reviewer found no material defect and independently passed all 19 default ALSA tests.
The host-specific hardware test remained ignored in that review run.

The final direct review found one evidence-validation defect.
A changed installed buffer size passed when its report hash was updated.
The regression reproduced that failure before the fix.
The validator now checks installed buffer, period, rational rate, significant bits, format, and access.
The same independent reviewer inspected the fix and found no remaining material issue.

The final canonical check passed 74 tests and one compile-fail documentation test.
Formatting, Clippy with denied warnings, catalog validation, and dependency validation passed.
The plugin matrix captured all 72 fixtures at three block sizes: 216 exact payload comparisons.
Corruption tests reject altered captures, escaping paths, exaggerated scope, and mismatched installed parameters.

Separate silent checks opened the host hardware and PipeWire routes without submitting samples.
The hardware ownership test observed `EBUSY`, closed the first stream, and reopened the selected subdevice.
The binary links dynamically to the host `libasound.so.2`.
Cargo versions, features, manifest hashes, licenses, and notices were refreshed and inspected.
The source and docs scan found no credentials or private data to publish.
The configured repository remains the intentionally public `harperreed/earph`.

The tests do not force native short acceptance or `EAGAIN`.
Physical playback, unplug, endurance, hardware payload measurement, and PipeWire coexistence remain unqualified.
These are qualification limits, not passing results.

Raw local verification logs remain under ignored `target/alsa-spike`.
The [portable archive](evidence/alsa/README.md) retains a fresh matrix and silent checks from clean commit `ae10b7b`.
The archived matrix passed a fresh read through `matrix-check`.
The four top-level records passed their checksum check.
The existing foundation archive still passed `evidence-check`.
Earlier test fixtures and captures remain under ignored `target/alsa-tests`.
No source files, branches, or worktrees were removed.
