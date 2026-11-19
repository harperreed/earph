# Shared diagnostic reads

`signal_path.get` and `metrics.get` use the existing version-one command envelope and receipt path.
Send them through `earph-engine request --runtime DIRECTORY`, LocalClient, or OwnerClient.
They do not open an output, start a source, discover devices, or change product state.
They also work while the owner has no session.
A supplied owner ID must match the current owner.
A fresh request ID reads current state.
An exact retained request replay returns its original historical result.

Each result separates `binding` from `report`.
The binding contains exact local target identities and the owner cut.
Treat that wrapper and the full control reply as private.
Only `report` follows the default shareable allowlist.
It contains no free-form identities, paths, endpoints, source metadata, authored names, error messages, or logs.
Numeric node and stage aliases belong only to that report.
The report omits graph identities because existing activation hashes include private route inputs.
This policy is not a proof against inference from numeric settings.

The signal path distinguishes requested configuration, resolved active compilation, observed playback, and unknown segments.
A configured graph restored from disk is requested intent.
It does not establish an active compiled graph or any playback observation.
Resolved transformations describe the active preview's preparation facts.
Current software gain, gain ramp, mute, and caps remain separate requested values.
Preservation and unknown downstream processing remain independent facts.

Same-rate nodes come from the actual compiler schedule.
Their latency is a path latency in their declared sample-rate domain.
The bypass flag describes an explicit compiled bypass node.
These are compiler facts, not a per-node execution trace.
Per-node implementation versions remain null because the producer does not supply them.
Streaming reports use actual stage formats and stage-local timing.
They supply no scalar aggregate latency or tail across rate domains.
A library's truncated output delay does not establish exact SRC group delay.

Playback observations are the last values retained by Session.
Session does not retain their original epoch or timestamp envelope.
The report therefore leaves sample origin and sample time null.
Its cut describes the owner state that retained those values.
It does not establish a fresh hardware sample or current physical presentation.
Backend format reports retain their actual measurement boundary.
Unknown boundary strings become unavailable; arbitrary strings do not enter the report.
DAC internals and unobserved output-route processing remain unknown.

Underruns, deadline misses, CPU load, and queue depth remain null with `unsupported` reasons.
They have no shared aggregate producer in this unit.
Available peaks and backend delay retain their declared playback-observation scope.
Saturation count belongs to graph processing since its last reset.
Its saturating maximum is a lower bound, not proof of an exact count.
An observed zero differs from a null value with an unavailable reason.
Software statistics do not measure analog jitter, distortion, or audibility.

Core owns the DTOs, allowlist producer, and semantic validators.
Desktop projection reuses those DTOs and validators.
FFI continues forwarding the same bytes.
Strict readers require nullable fields to exist and reject unknown fields or unsupported versions.
Capabilities advertise the implemented reads and their 64 KiB result bound.
Existing bounded receipt history retains query results; no event service or new dispatcher is added.

These reads do not implement event cursors, diagnostic preview/export, cancellation, build provenance, or release-evidence aggregation.
They do not complete T17, OBS-03, AC-34, AC-38, or AC-39.
Use exact producer, consumer, integration, and platform evidence before extending those claims.

## Linux qualification

The integrated source passes 1071 canonical tests, with zero failures and five existing skips.
Formatting, strict workspace Clippy, catalog validation and dependency validation pass.
Real owner cases cover CLI, Rust FFI, the shared client and standalone exact replay with output unopened.
Real compiler and Session cases prove private route inputs cannot change the shareable report.
The shared decoder rejects four independent source/native format corruptions.
Valid hardware caps survive reads and checkpoint restoration across the existing policy domain.
All source hashes remain unchanged throughout that run.
See [the Linux receipt](evidence/diagnostics/reads-linux-20261119.json) for exact products, checks and limits.
