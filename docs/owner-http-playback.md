# Owner HTTP-FLAC control

The existing `earph-engine request --runtime PATH` command accepts one JSON envelope on standard input.
The owner uses the already selected output and requires explicit output consent.
Use software capture when following this guide without physical output authorization.
See [local playback](../crates/earph-engine/PLAYBACK.md) for output selection, consent, and volume controls.

Every envelope contains these fields:

```json
{"schema_version":1,"request_id":"unique-request","zone_id":"main","owner_id":null,"session_id":null,"expected_revision":null,"payload":{"method":"state.get"}}
```

For each mutation, first read fresh `state.get`.
Copy its owner, current session, and revision into the mutation envelope.
Give each new operation a unique request ID.
Retain the complete envelope until its outcome is certain.
Replay those exact bytes after uncertain delivery; never construct another operation to discover its outcome.

Read capabilities and grant generation with this query payload:

```json
{"method":"sources.network.get"}
```

Install exact origins with explicit IP pins and DER certificate bytes:

```json
{"method":"sources.grants.replace","expected_generation":0,"grants":{"origins":[{"origin":"http://media.lan:53270","pins":["192.168.1.20"],"roots_der":[]}]},"lifetime_ms":60000}
```

HTTPS requires explicit DER roots and retains certificate chain and hostname checks.
The origin cannot contain a resource path or query.
The initial address policy rejects loopback, link-local, multicast, and other special ranges.
Grants expire after the requested lifetime, which must be between one millisecond and one hour.
Granting alone performs no fetch.

Stage a URI and read its exact identity and staging generation:

```json
{"method":"sources.stage_uri","uri":"http://media.lan:53270/recording.flac","metadata":"controller-owned text"}
```

```json
{"method":"sources.get"}
```

Staging performs no fetch and leaves current playback intact.
Use the returned `staged.media_id` and top-level `generation` for the start payload.
Use the current grant generation from `sources.network.get`.
Set the envelope's `session_id` to an explicit fresh session identity for this start:

```json
{"method":"sources.start_http_flac","media_id":"staged-1","staging_generation":1,"grant_generation":1,"device_id":"selected-device","takeover":false}
```

Set `takeover` only when replacing an existing session.
Acceptance means the owner reserved and committed preparation; it does not mean playback began.
Read `state.get` until `network_playback` contains received format and a `ready` or `paused` lifecycle.
Before that measurement, `sources.network.get` reports `preparing` even when transport intent is paused.
The observation contains received format and `total_frames`, which can remain `null` after completion.
Network observations have no queue entry or invented presentation clock.

Prepare the intended graph explicitly.
This example is an identity graph for received stereo PCM at 48 kHz:

```json
{"method":"graph.prepare","graph":{"schema_version":1,"output":"out","nodes":[{"id":"in","kind":{"kind":"input"},"format":{"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"float64","max_quantum_frames":64}},{"id":"out","kind":{"kind":"output"},"format":{"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"float64","max_quantum_frames":64}}],"edges":[{"from":"in","to":"out","from_port":0,"to_port":0}]},"processing":{"kind":"float64","quantization":"round_ties_away_saturate_no_dither"}}
```

Match the received sample rate, channels, and selected output period.
Use the authored correction graph when correction is intended.
Identity processing requires explicit controller intent; the owner never installs silent bypass.
Reconcile the preparation envelope until it returns an applied graph token and preview hash.
Read fresh state, then commit those exact preparation facts:

```json
{"method":"graph.commit","token":"returned-token","graph_hash":"returned-preview-hash"}
```

Reconcile the commit until its installation receipt becomes applied.
For replacement, inspect an optional `network_playback.replacement_boundary` in the resulting State.
Its source position marks accepted HTTP source frames at the transition.
Its discard count describes held frames in the previous client PCM format and output epoch.
The record names the new transition epoch and preserves source identity across later Pause or progress.
Preparation and graph activation submit no source PCM while muted.
Read fresh state, then unmute:

```json
{"method":"mute.set","muted":false}
```

Reconcile the original start envelope until it becomes applied or failed.
An actual native-start receipt records frame zero in the current native output epoch.
This follows the existing local start contract; it does not report an HTTP source frame or acoustic clock.
The accepted write frontier remains separately observable as `submitted_frames`.
Drain completion without a native-start observation leaves the activation frame unknown.
Use the same source and safety controls during network playback:

```json
{"method":"sessions.pause"}
```

```json
{"method":"sessions.resume"}
```

```json
{"method":"mute.set","muted":true}
```

```json
{"method":"sessions.stop"}
```

Pause and Mute preserve bounded replay without seeking or fetching again.
Resume preserves the current mute setting.
Reconcile accepted Pause, Resume, and Stop operations using their original envelopes.

Revoke the current grant generation explicitly:

```json
{"method":"sources.grants.revoke","expected_generation":1}
```

Grant replacement also retires the old network source and never resumes it automatically.
Revoke and Stop settle only after the exact worker acknowledges submission retirement.
Already accepted native samples cannot be retracted.
Idle and paused expiry remove authority without another controller command.
Failures require explicit recovery; replaying an old successful start never starts another fetch.
Owner restart restores durable local intent with no network grant, staged URI, admitted source, or output consent.

Use the existing shutdown payload to retire the owner:

```json
{"method":"owner.shutdown"}
```

Shutdown can return accepted while source and graph retirement continue.
The owner exits after retirement; its volatile terminal receipt may no longer be queryable afterward.
An observed process exit and peer closure establish a different boundary from initial command acceptance.

The initial lane supports 16/24-bit mono/stereo FLAC with floating streaming processing.
Network seek, queue next/previous, network profile application, and strict integer preservation remain unavailable.
The retained local queue stays visible and can start through the existing local start command.
