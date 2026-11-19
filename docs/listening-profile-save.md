# Saving listening profiles

Use the existing owner and JSON request CLI for all profile commands.
Saving works while idle and never opens output.
Saving preserves mute, current volume, independent caps, consent, and fixed-output arming.
It does not apply the saved graph or select another output.
Replacing the selected saved revision requires a completed Stop while its source is live.
That replacement clears its application claim, mutes, disarms, and revokes consent in the saved checkpoint.

Read `state.get` before a mutation.
Send its owner ID, current Session ID, and revision in the command envelope.
Use a null Session ID while idle.
Use the current Session ID during playback.

| Method | Payload fields | Result |
|---|---|---|
| `profiles.create` | `id`, `draft`, `graph` | A saved revision starting at 1 |
| `profiles.update` | `id`, `expected_profile_revision`, `draft`, `graph` | The next saved revision |
| `profiles.import_correction` | `id`, `expected_profile_revision`, `draft`, `input` | Saved correction intent and provenance |
| `assets.import_fir` | `path`, `purpose` | A verified immutable FIR reference |
| `profiles.list` | `offset`, `limit` | Up to 16 summaries and the save operation state |
| `profiles.get` | `id` | The validated profile, graph intent, and profile object hash |
| `profiles.resolve` | `manual_profile_id`, `output` | A manual choice or an explicit selection requirement |
| `profiles.prepare` | `id`, `expected_profile_revision`, `profile_sha256` | A short-lived shared graph candidate |
| `profiles.apply` | `id`, `expected_profile_revision`, `profile_sha256`, `token`, `graph_hash` | A muted activation acknowledgment |
| `profiles.bind` | `id`, `expected_profile_revision`, `profile_sha256`, `expected_device_epoch` | `CAPABILITY_UNSUPPORTED` until native identity is available |

The draft contains `name`, `source_policy`, `fidelity`, `output`, `volume`, `latency`, and `corrections`.
Its output selector contains backend, route, device ID, and an optional manual setup label.
Its volume fields use integer `software_cap_millidecibels` and optional `hardware_cap_millidecibels` beside `mode`.
The owner converts these caps into the saved document's dB values.
The graph payload uses `GraphIntent`: `graph` and `processing`.
The owner computes hashes from the exact immutable bytes it stores.
Clients do not supply graph hashes beside graph payloads.

A save first returns `accepted`.
Resend the identical command envelope to read its terminal receipt.
`applied` means the immutable objects and catalog checkpoint committed durably.
It describes saved intent, rather than graph activation or backend qualification.
Updates require both current Session revision and current profile revision.
Creating an existing ID fails without replacing it.

The list result includes `operation.stage`, request ID, and deadline.
The stage is `saving`, `retiring`, or null.
The deadline bounds saving and becomes null during retirement.
It does not promise a cleanup completion time after the receipt settles.
Cancellation can settle a receipt before worker resources retire.
During retirement, another save, source preparation, or graph preparation returns `SOURCE_BUSY`.
Stop, emergency mute, and owner shutdown remain available.
The worker releases its allocation reservation only after matching completion.

The catalog holds at most 32 profiles.
Profile documents allow 8 KiB, graphs allow 128 KiB, and checkpoints allow 2 MiB.
Profile retention also has a 64 MiB allowance, including command and query history.
Admission conservatively counts repeated shared owners and staging peaks.
That allowance excludes allocator overhead and opaque native allocations.
Live source and graph admission include the retained profile allowance.
Compilation reserves history growth inside that allowance until its resources retire.
An unused profile catalog adds no profile allowance to existing graph admission.
A local Start must report prepared-resource facts before profile work can begin.
Save and restore validate the whole prospective catalog with the same admission rule.
The worker compiles each graph sequentially without opening a native output.

Offline validation uses declared PCM domains and the existing DSP compiler.
It verifies referenced FIR bytes and geometry through the existing private asset store.
It enforces declared resource limits and rejects unsupported finite aggregate SRC latency constraints.
It does not qualify an actual backend or promise a control-latency deadline.

Automatic resolution and device associations remain unavailable without verified native identity evidence.
A manual choice cannot select a profile targeting another backend, route, or device ID.
An unchanged DAC identity never establishes the attached passive listening setup.
Direct create/update commands require empty correction references.
The correction import command creates those references from verified source facts.
Profile packages and broader application modes remain separate work.

Profile preparation requires a live local source with actual opened backend observations and current output consent.
The selected backend, route, device ID, software volume mode, and fidelity must match the saved profile.
Preparation never supplies an endpoint, opens another device, grants consent, arms fixed output, or assumes hardware readback.
Fixed/hardware application and fidelity transitions return `CAPABILITY_UNSUPPORTED` in this slice.
A different live output returns `SOURCE_BUSY`; Stop and select the output explicitly.

Use the exact immutable profile reference returned by `profiles.get` or the save receipt.
Preparation first returns `accepted`; replay the identical envelope for its `GraphPrepared` result.
That result's `preview.profile_application` binds the profile revision/hash, graph object hash, effective caps, gain, and limits.
It shares the existing graph candidate slot, five-second preparation deadline, and 30-second token lifetime.
A profile token cannot be consumed by `graph.commit`, or an ordinary token by `profiles.apply`.
Changes to Session revision, device/source epochs, profile reference, effective caps, or gain invalidate preparation.

Application first returns `accepted` and stays muted while the worker installs the graph.
It revokes consent and fixed arming without raising the current level.
The configured profile appears only after the matching worker acknowledgment and successful checkpoint.
Its terminal `applied` receipt reports the activation frame, rather than claiming an acoustic output boundary.
Consent the exact acknowledged output/profile explicitly before requesting unmute.
Stop and emergency mute remain available throughout preparation and activation.
An installation followed by storage failure requires recovery; the owner never claims physical rollback.

`local_volume_policy` stores the independent caps changed by `volume.caps.set`.
`volume_policy` reports the effective intersection with the selected profile's requested caps.
Applying a different profile never treats the preceding effective caps as the local source.
The current gain stays at the lower of its prior value and the prospective software cap.
Insufficient headroom or unknown required hardware readback remains blocked.
Restart restores verified configured intent paused/muted, without consent, arming, active graph evidence, or prepared tokens.
A checkpoint with selected profile intent must retain its separate local caps; missing or inconsistent caps refuse recovery.

Live resident limits cover the candidate pipeline, bridge, backend audio, metadata, and source/native period storage.
Preparation limits cover the known aggregate peak, including old resources, reopen overlap, assets, scratch, and catalog retention.
Both intersect existing engine admission and are passed to the existing compilers.
These bounds exclude opaque native allocations and whole-process RSS.
Offline save success does not qualify actual live resource admission.
Finite aggregate SRC latency remains `CAPABILITY_UNSUPPORTED`; named latency modes do not establish qualification.

Capabilities expose `profiles.application` with the supported software mode and current-output, consent, and matching-fidelity requirements.
They report `live_selected_profile_update: false` and `finite_aggregate_src_latency: false`.
Exact associations remain false until the native owner supplies verified device identity.
The bind request accepts no identity hash, native endpoint, or imported trust assertion.

For text imports, run `earph-correction-import` with explicit format, rate, channels, and quantum.
Send `input` with `kind: "text_report"`, its `report_json`, and the original UTF-8 `source_text`.
The owner reimports that text and requires exact agreement with the report.
It preserves supported REW Generic, AutoEq PK/LSC/HSC, and ordered CamillaDSP controls.
Unsupported controls fail rather than produce a partial correction.

For FIR imports, send `kind: "fir"`, `path`, `purpose: "correction"`, `context`, `metadata_json`, and `preamp_db`.
The context contains `sample_rate_hz`, `channels`, and `quantum_frames`.
The owner requires the declared rate and mono/stereo layout to match the WAV.
It preserves exact authored taps and ordered channels without normalization, trimming, broadcasting, or resampling.
The saved graph contains the explicit preamp and convolution chain.
The existing `earph-engine import-fir --runtime DIRECTORY FILE` command keeps its syntax and result shape.
With an active owner, that command forwards through the same worker.
It resolves relative source paths in the calling CLI's directory before forwarding.
Offline imports take the same owner lock and use the same private writer.
Neither path bypasses a held owner lock.

For a new profile, set `expected_profile_revision` to null.
For an edit, send its current positive revision.
The result includes correction documents and an offline compiler preview with channel bounds and required headroom.
Feed retrieved text reports into the existing `--validate-report` and `--export` commands.
The preview describes saved processing intent and never grants playback capability.

Metadata preserves model, measurement provider, target, parameter set, revision, and redistribution license.
Missing metadata stays null.
A license value records a caller assertion and does not establish redistribution rights.
The owner stores original text beside its verified report and stores FIR facts in a strict manifest.
Source paths do not enter these documents.
Restart verifies every source dependency and reconstructs the exact imported graph.

Text allows 64 KiB, reports allow 256 KiB, metadata JSON allows 4 KiB, and FIR manifests allow 8 KiB.
FIR WAVs allow 5 MiB and 262,144 taps per channel.
Paths retain the existing 1,024-byte command bound.
Combined cached correction JSON allows 256 KiB per profile.
Report and source request strings remain in the existing exact-content receipt history.
Their actual retained capacities count inside the 64 MiB allowance.

The private store keeps immutable hash-named graph and profile objects.
The existing checkpoint transaction publishes their catalog references only after object durability.
The writer synchronizes complete staged bytes before exclusive atomic publication at the hash name.
It never replaces an existing hash name and verifies existing objects before reuse.
Interrupted saves can leave partial stages or unreferenced objects, which remain preserved.
Checkpoint persistence failures retire the owner through the existing recovery path.
Restart verifies all reachable objects and refuses missing or corrupt references.
Successful restart remains paused and muted without consent, arming, or prepared tokens.

## Directory packages

Use `profiles.package_preview` with an exact `ProfileCatalogReference` in `profile`.
The reference contains `id`, positive `revision`, and `profile_sha256`.
The result shows the complete manifest, manifest hash, bounds, and local original source locations.
Read those original sources and review their exact content before exporting.
Local review paths stay in the authenticated response; they never enter package objects.

Use `profiles.export` with `profile`, an absent `directory`, and the preview's `expected_manifest_sha256`.
Send `export_review: "reviewed_exact_candidate"` and `content_review: "reviewed_no_private_data"` explicitly.
Send `redistribution: "refuse_measurement_bytes"` or `"reviewed_required_measurements"` explicitly.
The refusal choice rejects a package requiring original text or FIR bytes; it does not omit dependencies.
License strings never grant redistribution rights automatically.
The owner rejects known private content and unsupported WAV metadata instead of rewriting original hashes.
These fixed checks do not prove arbitrary bytes contain no private data.
A concrete export still requires the caller's exact-content and redistribution acknowledgments.

Select an absolute UTF-8 directory path without dot or parent components.
Export requires a private current-owner parent and a nonexistent destination.
It refuses nested packages and overlap with owner storage.
The owner writes a private sibling stage through the shared immutable publisher.
It publishes the manifest last, validates the completed directory, and synchronizes files and directories.
It installs the directory with exclusive rename and synchronizes the parent.
It never replaces or merges an existing destination.

Use `profiles.package_inspect` with `directory` to validate a selected package.
Use `profiles.import_package` with that directory, exact `profile`, and inspection's `expected_manifest_sha256`.
Import rereads and validates the actual original bytes, complete reachable closure, types, and provenance.
It creates a new catalog ID only and preserves the package's positive revision and hashes.
An occupied ID fails, including an equal reference; replay the same request ID for idempotency.
Ordinary later updates increment the imported revision through the existing checked update path.
Import never applies intent, selects output, changes consent/arming, or trusts native device context.

The format is schema-1 `manifest.json` beside fixed typed hash filenames.
It contains exactly one profile and graph, plus their required correction/source/WAV objects.
Limits are 64 KiB of manifest bytes, 64 objects, and 32 MiB including original manifest bytes.
Existing per-type and profile resource limits still apply.
One correction chain remains supported per profile; plain graphs may reference multiple supported FIR banks.
The consumer rejects symlinks, hard links, special/executable files, extra entries, and unused declarations.
The consumer reconstructs text reports from their original source and verifies FIR geometry and byte identity.
It never fetches URLs, executes content, extracts archives, or decompresses packages.

All four operations use the existing pending/retiring profile worker and standard receipt polling.
Inspect and preview also advance the Session control revision when their terminal receipt checkpoints.
No saved or active intent changes during those reads.
Cancellation releases the pending request before the actual worker may retire.
Blocking kernel I/O and individual decoder/compiler calls have no forced interruption guarantee.
Every partial stage and complete unreachable object remains preserved.
A sync, cancellation, or checkpoint failure can leave a complete final export directory.
A failed receipt never proves filesystem rollback; inspect the selected destination before choosing a fresh retry path.
`profile_package_issue` distinguishes missing, invalid, inventory, collision, review, resource, and I/O failures.
Its `filesystem_state: "inspect_required"` reports possible retained filesystem effects.
Capabilities report directory bounds, new-ID-only import, review requirements, and absent archive/automatic-application support.
Supported-platform qualification remains a separate evidence gate.
