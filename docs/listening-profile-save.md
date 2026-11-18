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

Offline validation uses declared PCM domains and the existing DSP compiler.
It verifies referenced FIR bytes and geometry through the existing private asset store.
It enforces declared resource limits and rejects unsupported finite aggregate SRC latency constraints.
It does not qualify an actual backend or promise a control-latency deadline.

Automatic resolution and device associations remain unavailable without verified native identity evidence.
A manual choice cannot select a profile targeting another backend, route, or device ID.
An unchanged DAC identity never establishes the attached passive listening setup.
Nonempty correction object references remain unsupported until their import workflow exists.
Correction provenance import, profile application, and package operations remain separate work.

The private store keeps immutable hash-named graph and profile objects.
The existing checkpoint transaction publishes their catalog references only after object durability.
Interrupted saves can leave unreferenced objects, which remain preserved.
Checkpoint persistence failures retire the owner through the existing recovery path.
Restart verifies all reachable objects and refuses missing or corrupt references.
Successful restart remains paused and muted without consent, arming, or prepared tokens.
