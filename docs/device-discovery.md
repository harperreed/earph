# Owner device discovery

Use the shared `devices.list` and `devices.inspect` commands through private owner IPC.
Supply an explicit `backend`; discovery never chooses another backend or a default output.

```json
{"method":"devices.list","backend":"alsa"}
```

```json
{"method":"devices.inspect","backend":"alsa","endpoint":"null"}
```

The payload belongs in the existing versioned command envelope.
Discovery does not require an active session or an expected configuration revision.
Check `capabilities.get` and its `devices.backends` before requesting native work.
Empty backends mean that this owner has no configured discovery implementation.

## Reads and evidence

ALSA returns advertised playback hints, including compatibility plugins.
Inspection matches the exact hint when present; absent hints leave target presence unknown.
ALSA current properties, native formats, periods, and persistent hardware identity remain unknown.

Core Audio returns current native properties and advertised format ranges.
It preserves raw format IDs and flags, nullable properties, and per-object failures.
An incomplete inventory cannot prove a missing UID is absent.
Duplicate display labels do not merge distinct endpoints.
UIDs longer than the current 256-byte output endpoint limit remain unselectable; the owner never truncates them.

Discovery evidence remains advertised, with an explicit `alsa_pcm_hints` or `core_audio_properties` read boundary.
Property observations do not establish that an output opened, ran, preserved samples, or reached a DAC.
Discovery never returns opened-output identity proof or creates device associations.
Raw names and UIDs stay within private IPC; public diagnostics need redaction.

Selection intent contains only a backend and exact endpoint.
It means the target fits the current output contract, not that it can open or play.
Keep output configuration, profile selection, signal consent, arming, gain, and Unmute separate.
Discovery performs no output open, registration, format write, ownership write, or signal test.

## Receipts and limits

The first response is `accepted` with no result.
Replay the same command envelope and request ID until its retained receipt becomes terminal.
Matching replay never dispatches the native query again.
Reusing an ID with changed content rejects with `ASSET_INVALID`.
A fresh refresh requires a fresh ID.

Successful terminal receipts contain `result.kind: devices`.
The value binds the owner, backend, completion time, read boundary, and tagged observation.
Observation kinds are `list`, `inspection`, and `failed`.
Lists and inspections include explicit completeness and per-object failures.
Failed observations preserve typed reasons and report confirmed helper cleanup.
An empty complete list differs from enumeration failure.

One pending query remains pinned outside transport operations, including during ordinary receipt-history rotation.
Completed data remains subject to the existing bounded replay window and result-storage admission.
Owner restart clears old discovery receipts; no durable exactly-once guarantee applies.
Queries do not change playback revisions, epochs, output intent, or saved profiles.

Only one helper may run; another new query returns `SOURCE_BUSY`.
Limits are 256 entries, 4096 text bytes, 256 KiB per result, and 2 MiB of retained device-result capacity.
Core Audio keeps its tighter native array bounds.
Oversize data rejects with a typed resource error rather than silently truncating a complete result.
ALSA bounds Rust hint copies before allocating their strings or growing the result vector.
SDK-owned allocations and total native RSS remain unqualified.

The supervisor enforces a five-second native-child deadline and bounded stdout/stderr pipes.
It terminates and reaps the exact owned helper before publishing completion or releasing its slot.
Owner shutdown requests cancellation while the control loop remains responsive.
A committed shutdown request does not prove process exit; actual owner/helper absence establishes that separately.
Unconfirmed native cleanup cannot be claimed as successful or reused for replacement work.

## Embedding and helper configuration

The production CLI explicitly supplies its own verified executable as the discovery helper.
`EngineOwner::bind` alone advertises no discovery backend.
An embedding host must explicitly provide the actual production engine helper:

```rust
let helper = earph_engine::DeviceDiscoveryHelper::from_executable(engine_executable)?;
let owner = earph_engine::EngineOwner::bind(runtime)?.with_device_discovery(helper);
owner.serve()?;
```

The library never assumes that its current executable understands engine helper arguments.
Configuration pins a canonical regular executable owned by the current user or root.
It rejects world-writable executables and checks executable bytes before each dispatch.
The explicitly configuring host owns trust in that executable and its writable groups.
Executable hashing happens before serving or on the supervisor thread, never in the owner control loop.
Neither device-query payload accepts executable paths or helper configuration.

The child runs through exec and receives only a bounded framed query.
It receives no owner runtime path or token and inherits no owner descriptors across exec.
Its environment contains locale and the validated startup `ALSA_CONFIG_PATH`, when configured.
Relative native configuration paths retain their startup working-directory meaning.
Other inherited environment values, loader overrides, and credentials are excluded.
No fallback native thread or alternate receipt cache exists.

Controlled process fixtures establish supervisor behavior only.
Linux ALSA read-only tests do not establish native Mac properties or completed output qualification.
