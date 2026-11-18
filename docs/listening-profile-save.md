# Saving listening profiles

Use the existing owner and JSON request CLI for all profile commands.
Saving works while idle and never opens output.
Saving preserves mute, current volume, independent caps, consent, and fixed-output arming.
It does not apply the saved graph or select another output.

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
Profile application and package operations remain separate work.

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
