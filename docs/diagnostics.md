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
Current finite software gain, exact zero, gain ramp, mute, and caps remain separate requested values.
The exact-zero bit selects effective software silence while retaining the finite setting.
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
Capabilities advertise each implemented method and its bounds.
Signal-path and metrics reads retain their 64 KiB limit.
Existing receipt history retains results; no event service or new dispatcher is added.

## Event recovery and preview

`events.get` requires a `cursor` field, which may be null.
A null cursor returns current public recovery facts and the latest resume sequence, without history.
A cursor contains the exact local `owner_id` and `after_sequence`.
A replacement owner rejects the old cursor.
Pages contain at most 128 retained state events in ascending order.
Sequence gaps can occur because some state updates do not append events.
The history is not a complete audit log.

`gap` reports expired or future history using Session's existing gap rule.
Adopt the current recovery snapshot after a gap, then resume at its event sequence.
Historical entries are context; do not apply them over that recovery snapshot.
Recovery covers public transport, mute, gain, error-category, and cut facts.
It is not a full private Session snapshot.
Each page has its own cut; multiple pages do not form a frozen snapshot.
An events result has a 256 KiB limit.
Event request IDs are omitted.
Event clocks retain Session's state clock; they do not timestamp receipt arrival or a fresh hardware sample.

`diagnostics.preview` captures one owner cut without a source or device query.
Its report retains an immutable UTF-8 JSON string in `bytes`.
The SHA256 and byte length describe those exact decoded string bytes.
Do not hash the outer JSON string's escape syntax or the private binding.
The bundle includes signal-path, metrics, public recovery, and up to 1024 retained state events.
The same typed validators check the producer and desktop consumer.
Sample origins remain unavailable where Session never recorded them.

Preview payloads have a 512 KiB limit, with a 64 KiB metadata allowance.
The final control receipt must also fit the existing 1 MiB frame limit.
One current candidate expires after 30 seconds on the owner's monotonic clock.
Expiry is checked on the next owner command; no background timer or queue is added.
An exact retained replay returns the original bytes after expiry.
That replay does not restore live candidate authority.
A fresh request captures a fresh cut; its revision can remain unchanged.
Failed admission preserves the previously live candidate.

All diagnostic receipts and candidates share an 8 MiB retained-payload budget.
Admission conservatively charges result bounds, metadata, shared Arcs, and staged copies.
The budget can reject new reads before the 1024-receipt count limit.
It excludes allocator and OS overhead and does not measure total process memory.
No unresolved operation is evicted to make room.

The Engine attaches its compiled-package record after restore and before publishing its endpoint.
The context accepts one Engine-validated record before diagnostic observations.
It remains outside checkpoints and State snapshots.
Without an attached producer, `running_build` explicitly reports `not_provided`.
Compiled package facts differ from runtime executable identity and execution-checkout facts.
The shared build helper declares separate Engine and harness input sets.
Both retain component sorting and u64 little-endian hash framing.
The record includes the dependency lock and selected source inputs, rather than every checkout file or build setting.
Unknown compiler, target, profile, and Git facts stay explicitly unavailable.
The preview runs no Git or build command.
Test inventory remains `not_attached`; reproduction remains `incomplete`.

## Explicit artifact publication

Review `diagnostics.preview`, then send `diagnostics.export` with its SHA256 and an explicit absolute `directory`.
Export requires the current owner and revision.
The worker writes the retained bytes; it never gathers another preview.
The active export pins that candidate and refuses new previews until the worker retires.
Profile and diagnostic artifacts share one bounded worker slot.
Cancellation and expiry preserve that slot until actual completion.

The destination must not exist.
Its parent must be private and owned by the current account.
Every path component uses a nofollow directory descriptor.
The worker stages exactly `manifest.json` and `diagnostics.json` in a private sibling directory.
It checks regular files, private permissions, single links, exact inventory, lengths and digests before installation.
It rechecks named parent and stage identities before the exclusive no-replace rename.
It syncs the files and directories, then validates the installed directory through bounded readback.
The aggregate file bound is 589824 bytes.

Artifact results are local and contain private paths and request identities.
Their binding retains the admission cut, even if product state changes during work.
The completion clock does not claim a current product snapshot.
Effects distinguish `unchanged`, `stage_retained`, `installed_verified`, `installed_sync_uncertain`, and `inspect_required`.
An accepted export uses `inspect_required` because filesystem work can begin before its reply arrives.
Only a verified installed export reports successful publication.
Failed and cancelled jobs retain their known effect phase and original staging locator.
Renamed paths can make that locator uncertain; inspect the explicit destination before deciding what happened.
The worker never deletes stages or published directories.

Send `diagnostics.inspect` with the explicit destination and expected bundle digest to reconcile uncertain outcomes.
Inspection uses the same bounded worker, reads without repair, and reports `missing`, `matching`, `conflicting`, or `unreadable`.
A missing destination says nothing about retained stages elsewhere.
Repeat the original retained envelope to obtain its pending or terminal receipt without repeating publication.
An owner replacement requires a fresh inspection request bound to the replacement owner.

Send `diagnostics.cancel` with the target request ID to request cancellation from the current owner.
The cancellation result distinguishes `requested`, `already_terminal`, and `unknown_operation`.
It does not claim that filesystem effects were undone.
An already verified installation can win the cancellation race.
The five-second deadline is cooperative; a blocked filesystem syscall can delay cancellation and normal thread joining.
Shutdown requests cancellation and drains worker completions before joining outside the UI thread.

The native export workflow still needs platform qualification.
This backend does not complete T17, OBS-03, AC-34, AC-38, or AC-39 by itself.

## Desktop workflow and Linux software checkpoint

The desktop presents current signal-path, metrics, event and preview observations with their owner cuts.
Successful diagnostic reads retire their receipt rows after adoption into those current views.
Read the visible observation identity rather than assuming its receipt remains in the history panel.
Artifact publication and inspection retain their original local receipts for reconciliation.
Raw local receipts contain private identities and paths; only the reviewed redacted payload is shareable.

Preview acknowledgment binds its request identity and explicit destination.
Destination edits, another capture and owner changes clear that acknowledgment.
The owner decides expiry and candidate replacement when export reaches admission.
An accepted operation keeps its exact polling identity while freeing foreground delivery.
Ordinary delivery pressure preserves captured intent and reconciliation in the existing waiting slot.
Mute and Stop have two independent bounded safety admissions.

The integrated Linux source passes 1157 canonical tests with zero failures and five existing skips.
Actual owners, files, CLI, Rust foreign-client and desktop workflows exercise export and uncertain delivery recovery.
The canonical test command collects failures from every test target before reporting failure.
See [the software receipt](evidence/diagnostics/artifacts-linux-software-20261119.json).

The first native diagnostic run stopped because its harness expected retired read receipts.
It does not establish diagnostic action delivery or native diagnostic acceptance.
Native verification must use current observation identities and distinguish reconstructed reads from original wire bytes.
Observer tools must not replay preview commands; an evicted receipt identity can gather another candidate.
Current-source Mac software checks and Mac window qualification remain pending.

## D1 Linux qualification

The published D1 baseline passes 1071 canonical tests, with zero failures and five existing skips.
Formatting, strict workspace Clippy, catalog validation and dependency validation pass.
Real owner cases cover CLI, Rust FFI, the shared client and standalone exact replay with output unopened.
Real compiler and Session cases prove private route inputs cannot change the shareable report.
The shared decoder rejects four independent source/native format corruptions.
Valid hardware caps survive reads and checkpoint restoration across the existing policy domain.
All source hashes remain unchanged throughout that run.
See [the Linux receipt](evidence/diagnostics/reads-linux-20261119.json) for exact products, checks and limits.

## Preview and provenance Linux qualification

The integrated source passes 1109 canonical tests, with zero failures and five existing skips.
Formatting, strict Clippy, catalog validation, dependency validation and product builds pass.
All 1835 source hashes remain unchanged during qualification.
Focused actual CLI and Rust FFI checks verify event recovery, exact preview replay and the compiled Engine record.
The standalone shared desktop client decodes the same owner results and joins the Engine with exit zero.
Real build producers verify the shared input order, length framing and explicit unavailable facts.
The compiled record stays unchanged after a real hashed input changes.
New provenance fixtures remain available for inspection.
One independent review covers the shared producer, consumers and final test changes.

Mac Clippy found Linux-only test support visible on macOS.
The source now gates that support consistently with its existing Linux-only tests.
The next committed-source Mac build must verify that fix.
No current Mac compile or window pass follows from the Linux result.
See [the preview receipt](evidence/diagnostics/preview-linux-20261119.json) for exact hashes and remaining limits.

The [active zero GUI receipt](evidence/desktop/native-zero-linux-20261120.json) binds the later frozen Linux software-output build.
Twenty-four native actions verify active graph diagnostics, exact zero, finite exit, mute independence, and FactoryDefaults.
All 109,975 exported bytes match the exact native preview text, and inspection reports Matching.
The owner, GUI, and capture reader exit normally.
This proof covers private X11 and an ALSA file-to-null PCM boundary; physical output and other platforms remain separate.
