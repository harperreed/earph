# Prepared PCM graphs

Earph compiles and activates bounded PCM graphs through the existing engine owner.
The Linux checks exercise real CLI commands and native file-to-null captures.
Mac canonical checks and one selected hardware exercise on each platform also pass.
See the [review receipts](evidence/dsp/review-verification-20261118.json) for current source and qualification boundaries.
The [earlier receipts](evidence/dsp/verification-20261118.json) retain their original source scope.
This work does not complete M2 or release qualification.

## Control flow

Read `state.get` with a fresh request ID.
Use its owner, session, and revision for graph commands.
Select the output and grant consent for that output before starting a local source muted.
An accepted start does not establish graph readiness.
Wait for `state.get` to report buffering and a playback format before preparing or validating a graph.
Use the owner, session, and revision from that readiness response to build the graph command.
Its observed format and selected output supply the preparation context.
Controllers cannot supply device facts, relax fidelity, or enlarge allocation budgets.

| Method | Result |
|---|---|
| `graph.inspect` | `active`, `configured`, `prepared`, `pending_stage`, `recovery_required`, and `service` storage observation. |
| `graph.validate` | Asynchronous preview against the captured session; creates no activation token. |
| `graph.prepare` | Asynchronous preview and one ready token; keeps the active graph intact. |
| `graph.commit` | Consumes the token and starts a muted rebuffer transition. |

Validation and preparation return `accepted` before background work finishes.
Replay the identical command to read its evolving receipt.
Use a fresh request ID for another logical operation or inspection.
Read-only polling does not advance the configuration revision.
Ready tokens expire after 30 monotonic seconds.
They bind the owner, session, source epochs, device generation, revision, and resolved graph hash.
One job or candidate owns the preparation slot.

Commit uses the ready token and its `preview.graph_hash`:

```json
{
  "method": "graph.commit",
  "token": "TOKEN_FROM_PREPARATION",
  "graph_hash": "HASH_FROM_PREPARATION"
}
```

Place this payload inside the existing version 1 command envelope.
Refresh its expected revision and targets from the current owner.
A stale, expired, consumed, or mismatched token rejects.
Acceptance does not mean the graph is active.
The worker installs it muted and reports an engine frame boundary.
Session persists the intent before reporting `applied`.
Activation never grants consent or unmutes output.

The preview's binding records its original preparation context.
Session epochs describe the current playback generation.
`graph_epoch` advances once per applied graph activation.
Pause and seek preserve the chosen graph.
Streaming holds preserve source DSP history and replay under separate output epochs.
True seek resets decoder, streaming DSP, and replay together.

## Intent and processing

Core owns the graph document and checked finite scalars.
DSP owns immutable compiled plans and prepared kernels.
Ports declare domain, rate, mono/stereo layout, precision, and maximum quantum.
The implemented nodes include input, output, bypass, gain, integer delay, matrix, sum, meter, PEQ, convolution, and SRC.
The stream compiler supports explicit serial rate changes around same-rate graph islands.
Unsupported cycles, cross-rate reconvergence, fields, and implicit conversions reject explicitly.
Live streaming graphs must return to the already-opened native output format.
Named profile saves retain intent without applying it or granting output permission.
Profile application, correction provenance imports through the owner, and DSD processing remain separate work.
See [correction filters](correction-filters.md) for PEQ parameters and the private correction import workflow.

Floating processing requires this explicit request:

```json
{
  "kind": "float64",
  "quantization": "round_ties_away_saturate_no_dither"
}
```

The path converts native integers to normalized f64, executes the graph, then applies shared safety volume.
One declared quantization rounds ties away and saturates into the native integer period.
No dither or hidden attenuation is inserted.
The ledger also records SDK conversions, including the existing Core Audio Float32 route.
Native client and physical format observations retain their separate meanings.

`integer_bypass` requires an unchanged integer graph and keeps the existing integer path.
Strict preservation refuses processing nodes even when their parameters resemble an identity.
Neither bypass nor compiler preservation proves DAC preservation.

The DSP compiler owns conservative channel bounds for routing, PEQ, and convolution.
The engine uses those same bounds for previews and volume admission.
For bound B, admission requires `max(1, B) * volume_linear <= software_cap_linear`.
Graph attenuation cannot grant more volume authority.
Preparation, commit, volume changes, and unmute enforce this rule.
Fixed unity output refuses positive graph headroom requirements.

## Ownership and recovery

Preparation and retirement run outside the control actor and prepared processing boundary.
The service permits two pipeline owners and one preparation or candidate slot.
Retirement acknowledgement releases storage before another candidate can be admitted.
Cancelled, rejected, stopped, and failed pipelines use the same off-thread retirement path.

The aggregate graph admission ceiling is 256 MiB.
It counts kernels, conversion spans, native periods, retained owners, metadata allowance, and preparation scratch.
The metadata allowance is 2 MiB plus measured intent capacity.
Preparation scratch reserves another 4 MiB.
Correction resolution reserves another 8 MiB while reading and decoding one bounded asset.
The compiler adds each kernel's separate construction scratch bound.
Resolved coefficient storage counts once per unique asset within each pipeline.
These are conservative payload allowances, not allocator or process-RSS measurements.
Existing decoder and backend storage retain their separate bounds.

Graph commands admit at most 128 nodes, 256 edges, and four coefficients per matrix.
The original graph command frame has a 128 KiB ceiling, including whitespace.
Retained typed intent also has a 128 KiB capacity ceiling, including spare storage.
The ordinary request window remains 1024 entries.
Live graph records and one shared terminal record preserve asynchronous reconciliation across history rotation.
Control history and previews remain separate from the graph audio budget.

Compatible queue joins preserve graph history.
Final EOF drains the declared graph tail once, then drains the native backend.
Tail frames increase output counts and never advance source positions.
Structural graphs subtract scheduling latency when backend consumption is known.
PEQ and convolution keep audible source positions unknown because scheduling latency does not establish filter phase.
Filter previews expose their tail policy, per-node horizons, and terminal channel bounds.
The worker retains completion proof until the owner acknowledges its exact token.
Old queue-entry snapshots and repeated mute generations cannot consume that proof.
Normal completion settles raced graph or positioning requests without inventing an output fault.
Graph preparation and resume wait for pending pause or seek acknowledgements.

If terminal counters exhaust, proved completion still closes the source muted and requests owner shutdown.
Counters never wrap.
A fresh owner resets only an exhausted persisted revision and restores intent without granting output consent.

Same-rate structural rebuffering resets graph history and discards the old tail at an explicit discontinuity.
Streaming pause and mute holds preserve DSP history and replay.
A true seek resets streaming history instead of reconstructing earlier filter state.
These controls do not establish physical pause continuity.
Allocation-free tests cover conversion, kernels, gain, quantization, finish, reset, and cancellation.
The whole playback worker still allocates, locks, decodes, and performs I/O.

Preparation failure preserves the active graph.
Activation failure requests output stop and exposes `recovery_required`.
It publishes one terminal event correlated with the graph request.
Stale acknowledgements cannot terminate the owner or replace the current observation.
Renewed consent alone cannot clear that gate.
If checkpoint persistence stops the owner, restart it and let startup reconcile storage first.
Then read fresh state before granting consent and starting a muted source.
Renew consent for the selected output before restarting a failed source.
If the failed session still owns the source, use `takeover: true` when calling `sessions.start`.
Keep output muted until a valid replacement graph activates.
Restart restores saved intent muted, without active resources or ready tokens.
It never silently substitutes bypass for saved processing.
Checkpoint replacement uses [bounded recovery](operational-storage.md) before restoring graph intent.
An uncertain storage outcome promises neither the old nor replacement checkpoint until startup reconciles it.
