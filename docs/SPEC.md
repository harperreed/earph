# Rust Audiophile Player
## Product and engineering specification

**Version:** 0.1-draft  
**Date:** November 16, 2026 — America/Chicago  
**Product name:** Not selected; “Rust Audiophile Player” is a descriptor.  
**First-party license:** MIT.  
**Status:** Design baseline, not an implementation report. No software, hardware, acoustic or performance tests have been performed for this document.

## 0. Authority, scope and terminology

This document converts the product-design conversation into requirements, implementation boundaries, a phased release plan and testable acceptance criteria.

**Confirmed direction** comes from the decisions in Appendix A. **Proposed engineering defaults** make those decisions executable; they are recommendations, not additional answers attributed to the user. **External constraints** are supported by the primary-source register in Appendix D. **Targets** are intended qualification criteria, not measured performance claims.

**Unresolved decision record:** Question 25 was answered **D**, but the option wording is absent from the visible transcript and was not recovered from prior context. Preserve that selection without inventing its meaning. The first-release boundary below is an explicit **proposed baseline**, assembled from the preceding decisions. This does not block implementation of the agreed foundations.

“MUST” identifies a requirement for its assigned release gate. “SHOULD” permits a documented, tested exception. “LATER” preserves product intent without making that feature an initial-release dependency. Unimplemented settings MUST reject with a capability error rather than silently do nothing.

### Product statement

Build an **open-source, local-first, inspectable audio renderer and DSP engine**, implemented in Rust, that can play local files or act as a Music Assistant endpoint. Provide GUI, CLI and API control over the same engine. Start on macOS and Linux; retain clean boundaries for native mobile clients, headless processing and remote audio endpoints.

**The product owns audio reproduction, processing, device policy and evidence. Music Assistant owns the main music-library and discovery experience.**

### Reading guide

Sections 1–2 define the product and proposed release. Sections 3–10 cover the engine, sources, output, DSP and safety. Sections 11–16 cover profiles, control, observability, correction and licensing. Sections 17–20 contain qualification targets, implementation order, evidence and remaining risks. Appendix A preserves decisions; B catalogs requirements; C specifies tests; D records sources; E defines control contracts; F defines evidence records.

## 1. Product goals and boundaries

### 1.1 Core jobs

The user should be able to play an album without Music Assistant, send music from Music Assistant to a particular DAC, apply headphone or room correction, inspect every known transformation, and reproduce a listening configuration from an exported profile. An application or agent should be able to perform the same operations safely through documented commands.

A successful system makes three questions easy to answer: **What is playing? What is happening to the samples? What will reach the selected output?** Where the answer is unknown, it says so.

### 1.2 Non-goals for the first release

Do not build a competing streaming subscription service, metadata knowledge graph, recommendation system, persistent music catalog, dedicated operating system or DAW. Do not implement direct commercial-service integrations while Music Assistant can supply audio. Do not require Python, a plugin host or a separate DSP process for ordinary playback.

Do not promise universal DAC compatibility, all-platform native DSD, seamless rate switching on every device, equivalent sound to HQPlayer, equivalent correction to Dirac, or audibility benefits from larger buffers, high rates or floating-point precision alone.

Do not infer support for Signalyst's NAA protocol merely from adopting a similar architectural separation. NAA interoperability is not an initial requirement.

### 1.3 Architectural reference

Signalyst documents a controller/server division for HQPlayer Desktop and an Embedded engine with a control API, UPnP renderer and network-audio endpoint support. We borrow the **separation of controller, processing engine and output**, not proprietary algorithms or an assumed internal design. [S01, S02]

## 2. Proposed first-release contract

Call the initial integrated release **v0.1** until a release naming policy exists. It is built through independently useful gates, not as one large implementation effort.

| Required for v0.1 | Reserved for later |
|---|---|
| Native installable macOS and Linux applications | Windows, iOS and Android playback applications |
| Rust engine, CLI, local API and UniFFI boundary | Remote headless deployment and separate network output endpoints |
| Local files, UPnP renderer and Sendspin player | Broader source/provider ecosystem |
| Stereo local output and qualified USB DACs | Multichannel hardware, independent zones and multiple DACs |
| PCM preservation and qualified DSD passthrough | PCM-to-DSD modulation and advanced DSD DSP |
| Typed configurable graph, EQ, convolution and resampling | Third-party plugin hosting and external engine backends |
| Imported headphone/room profiles | Native phone/microphone measurement and automatic calibration |
| Manual profiles, device associations, gain safety | Context rules and a richer local library |
| Truthful signal path and structured diagnostics | Advanced analyzers and a polished visual graph editor |

A simple graph configuration interface is sufficient; an elaborate node-canvas editor is not. A thin early playback milestone is not evidence that the whole v0.1 contract has shipped.

**Proposed launch qualification matrix:** Apple Silicon macOS and x86-64 Linux are the first hardware test targets. Additional architectures can compile before being marked qualified. M0 records exact OS, toolchain and device versions instead of inventing an unsupported universal compatibility range.

## 3. System architecture

### 3.1 Logical composition

```text
GUI / CLI / external controller
              |
    typed control service + events
              |
  session owner / policy / profiles / graph compiler
              |
local files | UPnP renderer | Sendspin player
              |
       demux / decode / timeline
              |
      +-------+-------------------+
      |                           |
PCM preservation path       typed PCM DSP graph
      |                     EQ / FIR / SRC / routing
      +-------------+-------------+
                    |
           format packing / output
                    |
          Core Audio / ALSA / shared route
                    |
                chosen DAC

DSD source -> DSD payload lane -> native DSD or DoP packing -> qualified output

Observability spans all layers. DSD does not pass through ordinary PCM DSP.
```

### 3.2 Component responsibilities

| Component | Owns | Must not own |
|---|---|---|
| Domain/model | IDs, formats, commands, events, graph/profile schemas | GUI, OS handles, third-party public types |
| Session service | Source ownership, transport, queue and state transitions | DSP arithmetic or provider catalog |
| Source adapters | File/network lifecycle, decoding input, metadata and clock contract | DAC policy or duplicate user settings |
| Graph compiler | Type/rate/channel checks, scheduling, resources and latency | Network traffic or on-thread allocation |
| DSP nodes | Prepared sample processing with explicit capabilities | Device discovery or UI callbacks |
| Output adapters | Device ownership, format negotiation, clock position and packing | Music-library management |
| Control adapters | UniFFI, local RPC and CLI serialization | Independent business rules |
| Asset/state store | Profiles, correction assets, evidence and operational state | Altering the user's music files |
| Clients | Interaction and presentation | Hidden DSP policy or private state authority |

Begin with a small Cargo workspace. These boundaries may be modules; do not manufacture a crate for every noun. Split crates where dependency isolation, platform compilation, public API or real-time validation benefits from doing so.

### 3.3 Local deployment and ownership

The core is an embeddable Rust library. A desktop application may host it directly and expose local IPC to the CLI. The standalone executable may host the same core without a GUI. Only one host owns a zone/output at a time; other processes attach or receive a clear conflict.

Use a per-user owner lock and local IPC endpoint. Desktop closure behavior must be explicit: quitting the owner stops playback; closing a window may keep the host alive according to the application's visible preference. Do not accidentally start two engines when the CLI discovers an existing desktop host.

The local host is not a requirement to introduce remote/distributed processing in v0.1. Later deployment changes where this service runs, not the meaning of its commands.

### 3.4 UniFFI boundary

UniFFI generates Rust-library bindings, including supported Swift and Kotlin bindings; it does not supply a GUI toolkit or application packaging. [S03]

Expose handles, immutable snapshots, bounded commands and asynchronous operation results. Do not call Swift/Kotlin from the audio callback or ferry per-buffer audio through the UI boundary. Bindings are versioned with the Rust API; test object lifetime, cancellation, error mapping and subscriber disconnection.

Proposed UI direction: SwiftUI plus UniFFI for macOS, Swift for iOS and Kotlin for Android later. The Linux toolkit remains an M0 implementation ADR. A Rust-native Linux client may call the same Rust service directly; it must not introduce a separate behavioral contract. No mobile UI is required to validate the initial FFI boundary.

## 4. Audio execution model

### 4.1 Separate control and audio work

Use a control/runtime domain for file access, network I/O, decoding preparation, graph compilation, storage and notifications. Use bounded worker queues and a deadline-sensitive audio domain for prepared DSP and output submission.

After preparation, the audio path MUST NOT allocate or free heap objects, perform file/network I/O, wait on unbounded locks, log synchronously or invoke foreign UI callbacks. Preallocate buffers and scratch storage. Retire old graphs and reference-counted resources off the audio thread; an atomic pointer swap alone does not make destruction safe.

The backend clock drives local output consumption. Network timestamps describe presentation intent, not an alternative permission to overrun the DAC's pace. Decode ahead within a declared bounded budget. A “memory playback” option is later; whole-library RAM loading is not necessary.

### 4.2 Typed audio blocks

Every block carries a session epoch, stream/format epoch, sequence/frame position, channel layout, frame count, valid sample representation and a time-domain tag where relevant. A schema-level sample rate is not interchangeable with a measured hardware clock rate.

Preserve integer PCM payloads without routing them through f32. Use a separate prepared f64 PCM graph as the proposed initial processing representation. Float conversion, quantization, dither and channel remapping are explicit operations. Packet/header bytes are not the definition of audio fidelity: lossless container unpacking and zero padding may preserve numeric sample values.

DSD has its own representation, packing order, framing and output path. No ordinary gain, EQ, resampler or limiter may accept it accidentally. Unsupported conversions are errors, not reinterpret casts.

### 4.3 Failure behavior

Bound every queue. A slow client may lose/coalesce telemetry but cannot block playback. An underrun produces backend-appropriate safe output or stops the stream; subsequent recovery follows source timing and policy. A malformed graph, non-finite sample or protective-processing failure must not silently bypass into full-scale output.

Always invalidate old epochs after seek, takeover, discontinuity and output replacement. Never allow buffered audio from a previous session to become audible in a new session.

## 5. Playback sessions and source arbitration

Use an explicit state machine:

```text
Idle -> Preparing -> Buffering -> Playing
                     |             |
                     +-> Failed    +-> Paused -> Playing
                                   +-> Reconfiguring -> Buffering
                                   +-> Draining -> Idle
                                   +-> Suspended / Failed
```

`Muted` is orthogonal to transport state. State snapshots include the source owner, current operation, session and graph epochs, output identity and error/recovery reason.

Only one source owns the v0.1 zone. A valid explicit play/takeover request stops the prior session, invalidates its data, activates the new source and applies existing safety policy. Merely discovering a controller, connecting Sendspin or setting UPnP metadata must not steal playback. Remote clients cannot bypass local admission or volume restrictions.

Local queue state remains local. Music Assistant remains authoritative for its queue; our clients forward supported next/previous/seek commands upstream and report unsupported ones. Network track metadata may be cached for display but is not a second editable streaming library.

A stop normally stops audible playback promptly. Natural end-of-queue drains decoder/DSP tails exactly once. Pause/resume must document what is retained, what is drained and how the reported audible position is derived.

## 6. Input contract

### 6.1 Local files

Required first: WAV, FLAC and AIFF; opening files/folders, drag-and-drop, queue reorder/remove, seeking, metadata/artwork display when available, and restart recovery. Do not alter tags, move files or require catalog indexing.

Qualify ALAC, MP3, AAC and Vorbis independently as decoding extensions. Decoding support is not proof of gapless support: Symphonia's published matrix distinguishes demuxer and decoder gapless capabilities. A codec/container combination must pass its own trim, seek and boundary fixtures before being advertised as gapless. [S10]

DSF and uncompressed DFF passthrough are part of the integrated release gate. DST-compressed DFF and SACD ISO parsing are deferred. Add other decoders behind the source interface rather than forcing every format through one library.

For compatible lossless tracks, gapless means zero inserted, omitted or repeated frames. Preserve DSP state through the join. A physical device reopening for a rate change can cause a discontinuity; report it rather than changing rate silently to preserve a marketing claim.

### 6.2 UPnP/DLNA renderer

Implement the renderer role, not a media server. Cover SSDP discovery, device/service descriptions, AVTransport, RenderingControl, ConnectionManager, subscriptions/events and supported transport actions. Test with Music Assistant and an independent controller. Music Assistant documents a DLNA player-provider path. [S05]

Advertise protocol/format capabilities truthfully. Implement next-URI support where necessary for the chosen controller's continuous-play path; otherwise advertise its absence and test the resulting behavior. UPnP interoperability does not imply formal DLNA certification.

Remote media URLs are input, not trusted instructions. Apply HTTP(S) scheme restrictions, origin policy, redirect revalidation, response/body limits and credential redaction. Authorizing a LAN Music Assistant origin must work without granting access to arbitrary local files, loopback administration services or link-local metadata endpoints. Event callback destinations need equivalent scrutiny.

### 6.3 Sendspin

Implement the player role against a **pinned specification revision and a named Music Assistant version**. Music Assistant documents Sendspin support. The Rust SDK is a candidate, not an assumed complete implementation; its repository still labels it work-in-progress. [S04, S06]

Pin security/handshake, discovery, role negotiation, message framing and clock behavior together. Do not freeze a remembered older protocol layout into our public types. Maintain interoperability fixtures whenever the adapter is upgraded.

The current player format contract offers PCM, FLAC and Opus. Advertise only formats actually implemented; prefer lossless transport for this product. Source metadata, received codec/rate/depth and emitted format must remain separate. Do not advertise native DSD carriage through a PCM-only role. [S07]

**Timing design:** map server time into local monotonic time; measure output progress; maintain a latency ledger from the scheduling point to physical presentation; account for each delay exactly once. Separate startup lead time, network buffering, algorithm latency, backend buffering and downstream delay. Recompute the plan after graph or output changes. [S08]

Drift correction is an explicit processing stage. A synchronized profile may use qualified ASRC; a strict sample-preserving policy that cannot satisfy the timing contract must reject that combination or require an explicit mode change. It must not silently desynchronize, resample or relabel altered samples as bit-perfect.

Group membership does not make the player a group coordinator. Do not promise sample alignment across untested endpoints or treat software timestamp estimates as an acoustic measurement.

## 7. Output devices, capability evidence and DSD

### 7.1 Backend policy

On macOS, implement Core Audio device enumeration, rate/format negotiation, ownership and route-change handling. Apple exposes nominal-rate and hog-mode properties; obtaining ownership is not, by itself, evidence of sample preservation. [S16]

On Linux, use a direct ALSA hardware path for qualified preservation mode. ALSA documents `hw` as direct access and `plug` as a conversion layer. A shared/system route, including the qualified PipeWire desktop path, is a separate compatibility mode with its own observed state. Do not kill the user's audio server to gain access. [S15]

CPAL is a candidate compatibility adapter, not an architectural commitment. Qualify its exact backend controls against this specification before using it for precision output. Use platform-specific code where a generic abstraction cannot expose the required control or evidence.

### 7.2 Evidence levels

Store separate capability records:

| Level | Meaning |
|---|---|
| Advertised | Reported by an API, descriptor or user-entered profile |
| Opened | Device/backend accepted the requested configuration |
| Exercised | Audio ran through that configuration without the tested failure |
| Measured | A defined independent capture or device test verified the claimed property |

Key evidence to device identity, transport, backend version, OS/driver environment, format and test method. Store timestamps and hashes of evidence. Firmware changes, changed driver/backend behavior and uncertain identity invalidate prior verification as appropriate.

A device accepting a mode does not prove its analog performance or digital payload. User approval is required before tests emit signals. Do not probe every rate at startup with audible material.

### 7.3 Format negotiation

Profiles choose a policy: **strict preservation**, **explicit compatible conversion**, or **expert override**. Resolution produces a preview containing requested format, selected hardware mode, inserted transformations, policy conflicts and evidence confidence.

Unknown capabilities remain unknown. Unsupported manual settings fail with an explanation. Failure to open an exclusive route must not silently switch to a shared mixer or the laptop speakers.

### 7.4 DSD safety

DoP transports DSD using PCM framing and marker bytes; it is not DSD-to-PCM conversion. [S17] Treat the packed transport as opaque after packing. PCM attenuation can damage the marker/data structure; do not apply it. [S18]

Validate DSD payload round-trips, channel order, marker sequencing, packing widths, supported rates and transitions on the actual backend/device matrix. Implement DSD-aware silence/mute behavior; zero-filled generic PCM buffers are not a universal DSD mute policy.

Fixed output or verified hardware volume can be used where supported. Software volume, EQ or room correction on DSD requires a separately implemented conversion/DSD-processing path; until then, reject the incompatible profile. Never silently downconvert to fulfill a strict policy.

## 8. DSP graph and processing contract

### 8.1 Graph structure

Use a **typed feed-forward graph** for the initial implementation. Support reordered nodes, branches, merges, channel splits and matrix routing. Arbitrary composition does not mean unconstrained feedback: reject cycles initially. Feedback nodes and independently clocked domains are later capabilities requiring explicit scheduling semantics.

Ports declare sample domain, precision, channel layout, processing rate and block constraints. Nodes declare resource requirements, latency, tail behavior, parameter schema, determinism, bypass semantics and whether they preserve samples. A generic node interface does not establish support for every possible graph. Quantum/block adapters must account for every consumed/produced frame and their buffering delay.

Begin with one hardware clock and a limited, explicit set of rate-domain boundaries. Parallel same-rate branches are mandatory. Graphs needing unsupported multirate merges fail validation rather than receiving implicit converters.

### 8.2 Compilation and activation

Compile in this order: validate schema and assets; resolve capabilities; type-check edges; insert only policy-approved conversions; derive rate domains and schedule; compute latency/resource budgets; prepare buffers and kernels; produce a plan preview; atomically activate against an expected revision.

A plan carries a content hash, implementation versions, resolved device format and safety constraints. Persist intent separately from this environment-specific resolved plan. Roll back to the last working configuration on preparation failure.

Same-latency parameter changes may use tested smoothing. Large or topology-changing updates may require a mute/rebuffer cycle. Do not promise click-free live switching for every arbitrary graph. Dual-graph crossfades require an explicit CPU/memory budget and are themselves transformations.

Latency compensation aligns scheduling delays at branch merges. It does not automatically correct frequency-dependent phase response. Do not align a room impulse's largest peak and call the result a complete phase correction.

### 8.3 Initial nodes

Provide gain/preamp, parametric EQ, convolution, explicit resampling, channel matrix, delay, metering taps and bypass. Implement PEQ with documented types, Q/slope definitions, units and supported parameter ranges. Reject non-finite/invalid coefficients.

Use partitioned convolution with per-channel assets, known normalization and explicit tail/latency. Distinguish **a measured room impulse response** from **a correction filter impulse response**. The former is not ready-made correction.

Resampler presets expose actual parameters and measured tradeoffs rather than invented equivalence to proprietary filter names. Provide at least a qualified quality-oriented and responsive configuration. No automatic rate increase solely because a DAC accepts a high number.

At PCM output, quantization and any dithering are explicit. Do not add dither to an unchanged integer-preserving path. DSP-to-integer reduction uses a documented policy and deterministic test fixtures; any noise shaping is declared rather than hidden.

FIR assets are rate-specific. Prefer a matching asset or explicit graph-rate choice. Deriving a filter at another rate requires a deliberate, tested conversion preserving the intended gain/response; no silent “fix the WAV header” behavior.

### 8.4 Reuse policy

Rubato supplies synchronous/asynchronous Rust resampling and documents real-time-friendly processing APIs. RustFFT supplies FFT primitives. They are candidates for direct reuse behind our interfaces. [S11, S12]

CamillaDSP exposes reusable DSP components, but its documentation says library use is secondary and patch releases may break the library API. Pin an exact revision and isolate it behind an adapter. Reuse selected components before considering a second complete runtime. [S09]

Do not expose upstream configuration objects as the product's stable public schema. Do not fork large engines merely to gain one filter unless licensing, maintenance and numerical qualification have been assessed.

## 9. Fidelity modes and claims

Represent **intent**, **actual transformations** and **verification scope** separately.

A strict profile requests sample preservation and rejects required modifications. A processed profile permits configured DSP. Automatic bypass eliminates only transformations proven unnecessary under the plan; identity-looking parameter values are not sufficient evidence of a no-op implementation.

Proposed state fields:

```text
requested_policy: strict | processed | automatic
source_encoding: original metadata, with provenance or unknown
received_format: actual decoded transport/file representation
transformations: ordered operations with parameters and reasons
payload_preservation: preserved | modified | unknown
observation_boundary: decoder | engine_output | backend | external_capture
proof_basis: planned | instrumented | captured
proof_scope: build + environment + fixture/stream interval
hardware_path: verified-for-scope | unverified | stale
```

Do not display an unqualified “verified bit-perfect” badge merely because DSP is off. A digital comparison at the engine boundary does not verify the OS, DAC or analog output. Lossy source decoding cannot reconstruct a pre-encoding original; preservation claims must identify the decoded baseline.

Sample-rate correction, sample insertion/removal, software attenuation, normalization, limiter action and fades count as modifications. Hardware volume is a downstream control: report it separately and do not infer whether it is analog or digital without device evidence.

## 10. Volume and safety policy

Volume is not an optional GUI affordance; it is a core invariant enforced for GUI, CLI, API, UPnP and Sendspin alike.

Maintain a gain ledger: source gain already applied upstream when known; correction preamp; user software attenuation; matrix/mix gains; optional protective processing; hardware setting/readback. Protocol percentages must map explicitly into our selected volume mode without double attenuation.

Support fixed, hardware and software modes. On first use, begin muted. A proposed default for software-volume onboarding is **−40 dBFS attenuation**, which is a conservative software default, **not a guarantee of safe acoustic level**. The physical amplifier, loudspeaker/headphone sensitivity and source content are not known.

Fixed/unity output requires explicit arming for the current device/profile. Never import or automatically select a profile that unexpectedly arms full-scale output. Known software/hardware caps persist independently of profile content. Unknown hardware volume is not assumed safe; output changes must not restore a larger level accidentally.

Do not unmute merely because a remote volume command arrives. Apply bounded ramps where the sample domain permits them; in strict/DSD cases use tested mute/stop transitions rather than concealed PCM processing.

Headroom planning and runtime peak monitoring are mandatory. Clipping prevention must not imply an invisible always-on limiter. In strict mode, report/refuse incompatible gain requests or stop safely; in DSP mode, any active limiter or gain reduction is part of the visible graph/policy. Never bypass a failed crossover or protective correction and continue at full scale.

## 11. Profiles, assets and persistence

A **profile** describes listening intent; a **resolved plan** is the executable interpretation for one environment. Never collapse them into the same saved object.

| Object | Required information |
|---|---|
| Profile | Stable ID, name, schema/revision, graph reference, source/fidelity policy, output selector, volume mode/caps, latency mode |
| Graph | Typed nodes/edges, implementation requirements, parameters and asset references |
| Correction asset | Content hash, kind, channel map, sample rate, normalization, preamp, provenance and redistribution metadata |
| Device record | Stable identity, environment fingerprint, capabilities, test evidence and quirks |
| Session snapshot | Owner, epochs, queue reference, transport state and selected profile; no secrets |
| Measurement project | Later: stimulus, raw captures, capture route, calibration, timing fit, quality results, target and generated correction |

Manual named profiles and exact-device associations ship first. A manual setup selection can distinguish headphones and speakers connected to the same DAC; the DAC's identity cannot establish which passive device is attached.

Selection priority is explicit: current manual choice, exact verified binding, broader configured match, safe default. Ambiguous matches require selection rather than unstable first-match behavior. Later context rules add preview, bounded evaluation and an explanation; arbitrary scripting is not required.

Use versioned JSON for portable profile documents and a content-addressed local asset store. A small transactional operational store may hold queue/session/history state; it is not a music catalog. Choose its implementation in M0 and test migrations, backup and recovery. Persist desired configuration only after validation; preserve a last-known-good snapshot.

Profile packages carry a manifest and asset hashes. Validate paths and sizes, disallow traversal/executable content, distinguish missing assets from invalid data and do not auto-fetch or auto-apply imported packages. Export cannot silently include copyrighted recordings, secrets or third-party measurements whose redistribution rights have not been reviewed.

## 12. Control API, CLI and native applications

### 12.1 One semantic interface

Required API groups: capabilities and schemas; devices and capability evidence; source/session/queue control; graph validate/prepare/commit; profiles and associations; volume/mute; signal path and diagnostics; asset import/export; operational backup/recovery.

The same command definitions drive all interfaces. CLI commands support machine-readable output and stable nonzero failure codes. Human output must not be the automation contract. Local API transport is a proposed versioned JSON RPC-style contract over per-user IPC; remote access is not enabled by default. The exact framing is an M0 ADR, not a reason to duplicate services.

### 12.2 Commands and receipts

A mutating command includes `request_id`, target session/zone/device as applicable, an expected revision for configuration edits, and a typed payload. Dangerous operations support validation/preview before application. Names are presentation; stable IDs are targets.

A receipt distinguishes `accepted`, `applied`, `rejected`, `cancelled` and `failed`, with a reason code, current revisions and affected IDs. Returning from an asynchronous API is not proof that samples have reached the DAC. Report the activation epoch or sample boundary when known.

Use a documented bounded deduplication window for request IDs. Do not promise durable exactly-once execution across arbitrary crashes. Clients can reconcile an uncertain result using the receipt/status and state snapshot rather than blindly retrying unsafe operations.

### 12.3 Events and concurrency

Events carry a monotonic sequence, revision, origin/correlation ID and timestamp. Subscribers can request a snapshot, then consume events. Coalesce high-frequency metering; expose a gap marker when a consumer falls behind. Preserve important state transitions in a bounded recoverable log.

Graph changes use expected-revision checks. A stale controller cannot overwrite a newer correction profile. Emergency mute remains available independently of pending graph preparation. UI, CLI and remote actions receive identical safety enforcement.

### 12.4 Minimum GUI

Provide Now Playing/queue, source selection, output selection and capability details, profiles, editable DSP chain/parameters, correction import preview, signal-path inspection and diagnostics/recovery. Display missing/unsupported features honestly.

A graph can be fully configurable via structured editing before a visual patch-cable editor exists. The first GUI should make the common chain easy and still expose the underlying graph. Avoid a GUI-only preference that changes audible behavior without appearing in the API/profile.

## 13. Observability and reproducibility

Always expose actual source and output formats, active graph hash/revision, node implementations, gain ledger, output identity, known latency, underruns, deadline misses and policy/fallback reasons. Optional metrics may be unavailable; do not substitute a fabricated zero.

Separate requested configuration from resolved configuration and observed backend state. Treat upstream transcoding and DAC internals as unknown unless there is specific evidence. A complete signal-path model can contain explicit unknown segments.

Export a diagnostic bundle containing redacted configuration, dependency/build identifiers, environment, recent events, device evidence, performance summaries and reproduction steps. Audio capture is off by default and requires deliberate consent. Remove credentials, device serials and private file paths from the default shareable bundle.

Later analyzers include FFT/spectrogram, impulse response, filter response and level-matched A/B. Distinguish a calculated DSP response from an acoustic measurement. Software underrun or timestamp statistics are not a measurement of analog DAC jitter or distortion.

## 14. Room and headphone correction

### 14.1 First release: import and apply

Support a documented subset of REW text EQ exports, REW/CamillaDSP filter definitions, AutoEq parametric settings and mono/stereo correction FIR WAVs. REW documents text EQ and filter-impulse WAV exports; we do not need to embed its application or parse every native project format. [S19]

AutoEq produces equalization settings and provides optimization tools; our engine applies the resulting filters. Capture headphone model, measurement provider, target, parameter set, preamp and revision. Selecting a manufacturer/model must not conceal differing measurement sources or targets. [S20]

Import previews show gain requirements, unsupported filters, rate/channel assumptions and provenance. Do not silently normalize FIRs or discard preamp values. Preserve user edits as a new local profile revision.

### 14.2 Later: phone-assisted calibration

The phone is a **measurement client**, not an assumed laboratory microphone. Apple offers a measurement mode intended to minimize system processing; Android's `UNPROCESSED` source is conditional and may behave like its default source when unsupported. Neither API establishes that every phone/route is calibrated. [S21, S22]

Use this proposed workflow:

1. Pair the phone with the local engine, select the actual capture route and obtain explicit microphone permission.
2. Establish a controlled listening setup, conservative test level and an immediately available stop control.
3. Generate a known stimulus on the selected output. Record locally on the phone, including acoustic timing references and sufficient lead/tail data.
4. Upload the lossless capture with hashes and capture metadata. Network arrival times are not the acoustic time reference.
5. Estimate timing offset and clock-rate drift; align/de-drift against the known stimulus. Inspect clipping, noise, repeatability and evidence of unwanted processing.
6. Restrict correction to the frequency/time regions supported by the capture quality and calibration evidence. Generate a preview, not an automatically activated aggressive filter.
7. Perform a new physical validation measurement and save the resulting profile with confidence and provenance.

Use local recording plus upload first, rather than making unreliable network timing part of the measurement itself. A later live measurement UI may visualize progress without making its display timing authoritative.

Calibration profiles must identify the phone model, selected microphone/route, relevant OS/capture mode and measurement conditions. A generic model curve is not equivalent to an individually calibrated capsule. Do not label uncalibrated readings absolute SPL or offer confident phase correction without a valid timing reference.

### 14.3 Later: precision mode

Support calibrated USB/interface microphones, calibration files and multi-position measurement projects using the same analysis/filter engine. Distinguish frequency-response calibration from absolute-level calibration. Store raw captures and processing parameters so analysis can be repeated.

Constrain boost, bandwidth, filter energy, latency and speaker protection. Do not solve a narrow deep null with unrestricted gain. Multi-position optimization needs a documented objective and held-out validation positions; a better predicted curve is not proof of a better measured room.

“Dirac-like” is a workflow aspiration: guided measurements, targets, filter generation and verification. It is not a claim of algorithmic, patent, branding or performance equivalence. No native calibration is a v0.1 release blocker.

## 15. Latency modes and future extensibility

A latency mode is a set of constraints, not a subjective quality badge.

| Mode | Proposed policy |
|---|---|
| Quality-focused | Permit long filters and additional buffering within explicit CPU/memory/latency budgets |
| Balanced | Use a qualified default processing/buffering configuration and responsive control behavior |
| Low latency | Reduce buffer/block delays where compatible with the selected graph; refuse unsupported combinations |

Larger buffers alone are not a sound-quality feature. Do not secretly remove convolution, reduce filter quality or change rate to meet a latency target. Show the actual resolved latency and the constraint that failed.

For future headless use, preserve source, control and output boundaries and authentication identity. Separate processing-host and DAC-endpoint clocks explicitly. A later network output transport must negotiate formats, buffering, clock behavior, reconnect policy and confidentiality; it is not merely sending raw samples over a socket.

For plugins, start with native trusted Rust components. Later evaluate CLAP, VST3 and LV2 as independent integrations. CLAP and the current VST3 SDK publish MIT licenses; individual plugins and wrappers still require their own review. [S23, S24]

Out-of-process scanning/processing can provide failure containment at a latency cost, but does not automatically constitute a security sandbox. Define privileges, shared-memory bounds, crash policy and state restoration before claiming isolation. Never bypass a crashed protective stage to keep sound playing.

## 16. Licensing, dependencies and security

### 16.1 MIT project, not mislabeled dependencies

First-party engine, client and tooling code uses MIT. Retain the licenses of incorporated components. “Our code is MIT” does not mean “the entire binary contains only MIT code.”

| Candidate | Intended use | License/qualification disposition |
|---|---|---|
| Rubato | Resampling and rate tracking | MIT/Apache-2.0 options; choose and record the actual release terms [S11] |
| RustFFT | FFT/convolution primitives | MIT/Apache-2.0 options; pin and retain notices [S12] |
| Symphonia | PCM-format demux/decode | MPL-2.0; review enabled codecs and compliance [S10] |
| CamillaDSP components | Selected EQ/convolution/routing implementation | MPL-2.0 or GPL option documented; choose the audited feature set and adapter [S09, S13] |
| Sendspin Rust SDK | Protocol adapter | Apache-2.0 repository; compatibility and maturity require testing [S06] |
| AutoEq | Profile/optimizer interoperability | MIT code; review separately sourced measurement/profile redistribution [S20] |

CamillaDSP specifically documents its ASIO feature as GPL-only for resulting binaries. Do not enable it casually in a supposedly permissive distribution; Windows/ASIO is a later decision. Its MPL option is not permission to relabel copied source MIT. [S13]

Mozilla describes file-level MPL obligations and distribution of larger works with differently licensed files. Use an actual release audit: exact versions, enabled features, build dependencies, modified covered files, notices and required source availability. [S14]

Do not embed a GPL room-correction implementation or a proprietary SDK without a separate compatibility decision. Running a tool in another process is not automatically proof that every distribution obligation disappears. Native algorithm selection and patent/licensing review are later gates for advanced correction/modulation.

### 16.2 Security model

Local IPC is user-restricted. Remote administration requires opt-in pairing, authenticated/encrypted communication and explicit capabilities. LAN renderer discovery/control has a different trust model; make enabling it a visible onboarding decision and follow the protocol's actual security mechanisms rather than assuming LAN equals trusted.

Treat media, artwork, URLs, XML, profile archives and plugin state as untrusted. Bound sizes, duration, channels, decompression, recursion and graph complexity. Enforce URL-origin rules after DNS resolution and redirects, without breaking explicitly authorized local Music Assistant sources.

Do not expose local file browsing through the renderer protocol. Store secrets outside exported profiles. Keep local playback account-free and offline-capable. Updates and support uploads must not occur invisibly during a listening session.

## 17. Proposed qualification targets

These are **engineering targets to validate**, not existing benchmark results. M0 records reference hardware and exact test graphs. Any change to a threshold requires an explicit ADR, not a retrospective edit to make a failing run pass.

| Area | Proposed gate and measurement |
|---|---|
| PCM preservation | Zero differing numeric samples and zero count differences across deterministic signed 16/24/32-bit fixtures on the qualified lane; distinguish engine-boundary from hardware proof |
| Gapless | Split/unsplit lossless signal yields equivalent continuous output and sample count under bypass and qualified DSP, including final tail handling |
| DSP realtime discipline | Zero allocations/deallocations and forbidden blocking operations on instrumented processing/callback paths after preparation |
| CPU margin | On named reference graphs/hardware, p99.9 processing duration below 70% of available block period; report maxima and deadline misses too |
| Endurance | Two-hour local playback on each launch OS with zero unexpected underruns under the committed workload; a separate fault-injection suite tests recovery |
| Memory | Hard per-plan budgets and bounded queues; default baseline budget of 256 MiB for engine-owned audio/graph buffers, excluding GUI; larger graphs require an explicitly increased admitted budget |
| PEQ | Response within 0.01 dB of an independent analytical reference at declared nonsingular test frequencies; finite/stable output across parameter-boundary fixtures |
| Convolution | f64 reference fixtures: relative L2 error at most 1e-10 and max absolute error at most 1e-9 for unit-bounded inputs; declare fixture sizes and treatment of near-zero references |
| Quality SRC preset | Target passband deviation at most 0.01 dB through 90% of the lower Nyquist frequency, and at least 100 dB alias/image rejection in the declared stopband; define test signals/windowing and each rate pair before measurement |
| Local control | p95 command acceptance under 100 ms and UI state propagation under 250 ms on reference hardware; this is not seek-to-audible latency |
| Sendspin | Meet the pinned protocol's own output-relative timing and recovery requirements; qualify simulated drift and physical independent-clock playback separately [S07, S08] |
| Graph swap | No stale-revision commit, no realtime destruction, no unsafe level jump; a documented mute/rebuffer is allowed when seamless activation is unsupported |
| Safety | Every in-scope startup, unplug, gain, malformed-input and DSD-mode transition scenario passes on required targets |

For Sendspin, the retrieved specification defines a steady-state ±1 ms floor and ±0.5 ms target **relative to its time-filter prediction at the client output**, not a guaranteed bound on arbitrary room-to-room acoustic offsets. Record the exact revision and measurement definition. [S07]

Numerical equality across all CPU architectures is not a default DSP promise. Use error tolerances and versioned golden/reference fixtures where floating-point evaluation differs; preserve exactness where the payload path requires it.

## 18. Implementation gates and task ordering

| Gate | Deliverable | Exit evidence |
|---|---|---|
| **M0 — Contracts and risk spikes** | Decision ledger; domain/schema/control model; dependency/license audit; virtual output; backend/SDK spikes; reference matrix | Reproducible offline render, architecture boundary checks, recorded dependency decisions and identified blockers |
| **M1 — Local playback foundation** | Local lossless playback, safe stereo output on both OSes, basic CLI/API/GUI shell, queues, strict policy and telemetry | PCM preservation, gapless, source/session lifecycle, output negotiation, mute/unplug and IPC tests |
| **M2 — Useful DSP player** | Typed graph, PEQ, convolution, resampling, profiles, imports, headroom and signal-path inspector | Numerical, graph transaction, correction-import, latency and profile tests |
| **M3 — Integrated audio endpoint** | UPnP, Sendspin timing/control integration, qualified DoP/DSD support and source arbitration | Real Music Assistant/controller traces, drift and latency tests, DSD and volume qualification |
| **M4 — v0.1 qualification** | Complete desktop workflows, Linux compatibility route, distributable artifacts, docs, fuzz/stress and evidence review | All v0.1 requirements passing on the published matrix; release artifacts, notices and recovery evidence |
| **Later** | Mobile/headless/endpoints, native measurements, plugin hosting, advanced modulation and richer local library | Separate specifications and gates based on this model |

Dependencies are chiefly **M0 → M1 → M2 → M3 → M4**, but independent work may overlap after its contracts are stable. A scenario used in more than one gate runs its relevant cases early and its complete cross-platform/integration matrix at M4; early partial coverage is not marked final pass.

### Dependency-ordered work packages

| Task | Scope | Prerequisite | Contract/file ownership boundary |
|---|---|---|---|
| T01 | Confirm traceability, schema versions, license policy and reference environment | None | Spec/catalog/ADRs |
| T02 | Domain IDs, format types, command/events and state machine | T01 | Shared model; one integrator owns schema changes |
| T03 | Offline renderer, virtual clock/output, test fixture generator | T02 | Test harness and fixture manifests |
| T04 | Core Audio precision-output spike | T02, T03 | macOS output adapter |
| T05 | ALSA precision/shared-route spike | T02, T03 | Linux output adapters |
| T06 | Decoder/file queue and frame continuity | T02, T03 | Local source and decoder adapters |
| T07 | Owner lifecycle, local IPC, CLI, receipt semantics | T02 | Control service/adapters |
| T08 | Volume policy, negotiation and device evidence | T04, T05, T07 | Shared safety/device service |
| T09 | Join foundation into local playback milestone | T06, T07, T08 | Integration owner; AC foundation subset |
| T10 | Graph compiler, prepared execution and latency model | T03, T09 | Graph compiler/runtime |
| T11 | EQ/FIR/SRC adapters and numerical tests | T10 | Separate node modules and their fixtures |
| T12 | Correction imports, profiles and asset store | T08, T10, T11 | Import/profile model; no duplicate safety policy |
| T13 | UniFFI and desktop workflows | T07; DSP portions after T12 | Bindings and platform UI; shared API remains owned by T02/T07 |
| T14 | UPnP adapter and controller interoperability | T09, T12 | UPnP protocol adapter and fixture captures |
| T15 | Sendspin adapter, clocks and timing qualification | T09, T10, T11 | Sendspin/timing adapter and protocol lock |
| T16 | DSD container/packing/output safety | T04, T05, T08, T09 | DSD lane and device qualification |
| T17 | Source arbitration, failure injection and integrated observability | T14, T15, T16 | Integration tests; no new source authority |
| T18 | Packaging, security/license audit, performance and release evidence | T13, T17 | Release pipeline and evidence manifest |

Do not assign parallel agents to modify the same shared schema or control semantics without an integration owner. Each task must carry requirement IDs, prerequisite contracts, allowed files/modules, acceptance commands, expected artifacts and a review owner. Changes to public schemas, safety or timing require updating affected fixtures in the same change.

## 19. Release evidence and definition of done

A requirement is not complete because a screenshot looks correct or a unit test exists. Link it to exact passing evidence for the claimed environment and behavior.

Each evidence record MUST contain requirement/scenario IDs, commit/build identity, dependency lock hash, OS/architecture/backend/device identity, fixture and graph hashes, test command/harness version, expected result, observed result, status, timestamps, artifact paths and hashes. Hardware-specific evidence must identify the measurement boundary and procedure.

A release gate fails for missing required evidence, not-run safety tests, stale device qualification, undocumented dependencies or a capability advertised beyond tested support. A future requirement is deferred, not falsely passed. A simulated device does not satisfy a real hardware gate.

First release definition of done: install on the named macOS and Linux systems, play local files without accounts, receive both network input protocols, apply/import correction and resampling, inspect the actual pipeline, use all control surfaces consistently, survive the specified failures, and distribute accurate notices and reproducible qualification evidence.

## 20. Open implementation decisions and risk register

| Item | Treatment |
|---|---|
| Q25 option D wording | Unresolved product-record item; release plan explicitly proposed, not attributed to the user |
| Product name | Use descriptive working title; does not block code |
| Linux GUI toolkit and operational store | Choose in M0 with a small proof, license review and API-boundary check |
| Precise launch OS/device matrix | Record real available hardware in M0; never fabricate qualification |
| Decoder gapless coverage | Qualify codec/container pairs; limit the claim rather than assuming a library table guarantees end-to-end behavior |
| CamillaDSP integration stability | Exact-version adapter; no upstream types in public schemas |
| Sendspin protocol/SDK drift | Version lock plus interoperability suite; implement missing behavior or defer support honestly |
| DSD device/driver behavior | Separate safety gate, narrow verified matrix and explicit unsupported modes |
| Real-time graph swaps | Prepared plans, off-thread retirement and tested fallback rebuffering |
| Shared-output fidelity | Distinct compatibility route and explicit unknown/modified status |
| Phone capture variability | Confidence-qualified results, per-route calibration and precision-microphone alternative |
| AutoEq/measurement redistribution | User-import capability first; bundle only assets with reviewed provenance/rights |
| Full modularity turning into scope expansion | Require working vertical slices; stable boundaries do not require every future implementation |

## Appendix A. Product decision ledger

The ledger preserves accepted direction. Where earlier answers were recorded by a letter, the conversational decision summaries supply the meaning; Q25 remains the explicit exception.

| ID | Confirmed direction |
|---|---|
| D01 | Focus on audiophile playback/DSP; leave the main music-library/discovery experience to Music Assistant |
| D02 | Rust first; UniFFI for native client bindings; desktop/iOS/Android direction |
| D03 | Local processing first, eventual headless/central processing and endpoint separation |
| D04 | Local files, UPnP/DLNA renderer and Sendspin inputs |
| D05 | Minimal local files/queue first; optional lightweight library later |
| D06 | Incremental delivery with architecture for the full DSP ambition |
| D07 | macOS and Linux first; other desktop/mobile platforms later |
| D08 | Stereo/local DACs first; extensible outputs for multichannel/multiple DACs/network later |
| D09 | GUI, CLI and API using one shared control model |
| D10 | Resampling/selectable filters plus EQ/convolution; reuse open DSP and support headphone profiles |
| D11 | Imported correction first, native measurement/calibration incrementally |
| D12 | Fully configurable DSP graph; phone-assisted measurement is desired |
| D13 | Both phone convenience and calibrated-microphone precision modes |
| D14 | Open/modular native components first; plugins/external engines later |
| D15 | Manual profiles and device association first; context rules later |
| D16 | Full observability incrementally, with structured signal-path model from the start |
| D17 | Explicit fidelity modes, automatic bypass and per-profile policy, with truthful status |
| D18 | Automatic capability negotiation, overrides, fallbacks and verified DAC profiles |
| D19 | Fixed, hardware and software volume modes, configurable per profile |
| D20 | Exclusive source sessions; switching stops the prior source |
| D21 | Configurable quality-focused, balanced and low-latency modes |
| D22 | Gapless, seek, queue and robust pause/resume first; advanced transitions later |
| D23 | Native apps plus standalone engine/CLI/API distribution |
| D24 | MIT for first-party project code |
| D25 | Selected D; option wording unavailable; no inferred meaning recorded as confirmed |


## Appendix B. Normative requirements catalog

Phase names refer to implementation gates, not calendar commitments. All implementation statuses are **not started** and all test statuses are **not run**. The machine-readable companion carries dependency and decision links.

| ID | Gate | Requirement | Acceptance |
|---|---|---|---|
| `ARCH-01` | M0 | Own the product model, session orchestration and DSP graph in Rust; keep UI and transport libraries outside the real-time engine. | AC-01 |
| `ARCH-02` | M1 | Expose one semantic command/query/event contract to GUI, CLI, local API and UniFFI clients. | AC-02 |
| `ARCH-03` | M1 | Run locally first with one authoritative engine owner per output; support embedding and a local standalone host without requiring a remote server. | AC-03 |
| `ARCH-04` | M1 | Maintain separate typed PCM-preserving, floating-point DSP and DSD payload paths. Never process DoP transport words as PCM. | AC-04, AC-13 |
| `ARCH-05` | M1 | Use bounded preallocated audio buffers and keep allocation, deallocation, blocking I/O, locks with unbounded waits and foreign callbacks out of the audio processing path. | AC-05 |
| `ARCH-06` | M0 | Provide deterministic offline rendering, a virtual output and fault-injection harnesses before qualifying real hardware. | AC-01, AC-06 |
| `ARCH-07` | M0 | Define schema and control compatibility rules; reject unsupported features rather than accepting inert settings. | AC-07 |
| `PLAY-01` | M1 | Play local WAV, FLAC and AIFF with open-file/open-folder and a basic ordered queue; preserve originals without retagging or reorganizing. | AC-08 |
| `PLAY-02` | M1 | Implement play, pause, resume, stop and capability-aware seek with audible position distinct from decode position. | AC-09 |
| `PLAY-03` | M1 | Deliver sample-continuous gapless local playback for qualified compatible lossless tracks, with no inserted, duplicated or dropped frames. | AC-10 |
| `PLAY-04` | M2 | Maintain DSP state across compatible track boundaries and explicitly negotiate or interrupt incompatible rate/format transitions. | AC-10, AC-11 |
| `PLAY-05` | M1 | Allow only one active source per local zone. A source handoff stops the prior session and invalidates its buffered audio and commands. | AC-12 |
| `PLAY-06` | M1 | Persist the local queue and recover into a paused/muted state after a crash; never autonomously resume into an unverified output. | AC-09, AC-14 |
| `PLAY-07` | M3 | Support qualified DSF and uncompressed DFF playback through DoP; native DSD is enabled only on specifically tested backend/device combinations. | AC-13 |
| `PLAY-08` | LATER | Add optional fades, crossfade, ReplayGain, shuffle and repeat with transformations visible and strict preservation policies enforced. | AC-40 |
| `SRC-01` | M3 | Implement an interoperable UPnP AV renderer with discovery, transport, connection capabilities, rendering control and truthful event/state reporting. | AC-15 |
| `SRC-02` | M3 | Integrate the Sendspin player role against a pinned protocol/SDK revision and a named Music Assistant test version; advertise only usable formats and controls. | AC-16 |
| `SRC-03` | M3 | Schedule Sendspin audio against the protocol timeline, account for processing/output delay exactly once and qualify drift correction on independent clocks. | AC-17 |
| `SRC-04` | M3 | Keep original source metadata, received transport format and actual output format separate. Report upstream processing as unknown when it is not evidenced. | AC-18 |
| `SRC-05` | M3 | Treat Music Assistant as authoritative for its queue and library; route supported control actions upstream without creating a competing queue. | AC-16, AC-19 |
| `SRC-06` | M3 | Apply bounded parsing, trusted-origin fetch rules, session epochs and pairing/authorization policies to all network sources. | AC-20 |
| `OUT-01` | M1 | Implement tested macOS Core Audio and Linux direct-ALSA output adapters with device discovery and exact negotiated-format reporting. | AC-21 |
| `OUT-02` | M4 | Offer a Linux system/shared playback route compatible with the qualified PipeWire desktop setup; do not label an unverified shared route bit-perfect. | AC-22 |
| `OUT-03` | M1 | Track advertised, opened, exercised and independently measured device capabilities separately, keyed to a device/backend/OS environment. | AC-21, AC-23 |
| `OUT-04` | M1 | Expose output clock position, buffering and latency with measurement basis and unknown values; invalidate stale timing after route changes. | AC-09, AC-17 |
| `OUT-05` | M1 | Never silently reroute to a different physical output after unplug, busy-device or format failure. Pause/mute and report recovery choices. | AC-14 |
| `OUT-06` | M1 | Apply explicit strict, compatible and manual negotiation policies; conversions and rejected overrides must be explained. | AC-04, AC-21 |
| `DSP-01` | M2 | Compile a validated typed feed-forward graph with reorderable nodes, branches, merges and channel matrices; reject unsupported cycles and incompatible domains. | AC-24 |
| `DSP-02` | M2 | Provide f64 PCM gain, parametric EQ, convolution and selectable resampling configurations behind interchangeable node implementations. | AC-25, AC-26, AC-27 |
| `DSP-03` | M2 | Keep sample-rate changes explicit and validate filter coefficients, channel layouts and processing-rate domains before activation. | AC-24, AC-25 |
| `DSP-04` | M2 | Track algorithmic latency and align parallel branches at merges where required; do not equate alignment with arbitrary phase correction. | AC-28 |
| `DSP-05` | M2 | Prepare graph changes off the audio thread, commit atomically against a revision, retire old resources off-thread and preserve the last working graph on failure. | AC-29 |
| `DSP-06` | M2 | Import a documented REW text/CamillaDSP-filter subset, AutoEq parametric presets and correction FIR WAVs with validation and provenance. | AC-30 |
| `DSP-07` | M2 | Reject unapproved correction-IR rate changes, unidentified measurement IRs, non-finite coefficients and resource-exhausting graphs. | AC-30, AC-31 |
| `DSP-08` | M2 | Implement quality-focused, balanced and low-latency policies as declared constraints; never silently remove correction or change sound to meet a latency target. | AC-32 |
| `DSP-09` | LATER | Add PCM-to-DSD, DSD processing, modulators, advanced noise shaping and DAC-specific correction only behind dedicated validation gates. | AC-41 |
| `SAFE-01` | M1 | Centralize mute, startup level, volume limits and route-change protection below all control surfaces; fixed output requires explicit arming. | AC-14, AC-33 |
| `SAFE-02` | M2 | Validate headroom and monitor non-finite samples and clipping; any protective sample alteration must change the reported fidelity status. | AC-31, AC-33 |
| `SAFE-03` | M1 | Support fixed, hardware and software volume as separate per-profile modes, with known gain stages and no double attenuation or assumed hardware readback. | AC-33 |
| `SAFE-04` | M3 | Refuse PCM gain/EQ/limiting on DSD payloads; use verified DSD-aware mute/transition behavior and never reinterpret DSD as audible PCM. | AC-13, AC-33 |
| `SAFE-05` | M1 | Use local-user authorization for local IPC; make remote administration opt-in, paired and encrypted; redact secrets in diagnostics. | AC-20, AC-34 |
| `SAFE-06` | M4 | Bound media/import/API resources, fuzz untrusted parsers and enforce safe failure without automatic protective-filter bypass. | AC-31, AC-34 |
| `PROF-01` | M2 | Provide versioned named profiles referencing graph, source policy, output policy, volume policy and latency mode. | AC-35 |
| `PROF-02` | M2 | Support deterministic exact-device associations and manual listening-setup selection; do not infer headphones attached to an unchanged DAC. | AC-35 |
| `PROF-03` | M4 | Export/import self-contained profile packages with content hashes, source/license metadata and no auto-application of dangerous gain changes. | AC-30, AC-35 |
| `PROF-04` | LATER | Add context rules with explicit priority, bounded evaluation, preview and an explanation of why a profile was selected. | AC-42 |
| `CTL-01` | M1 | Expose typed commands, snapshots, capability queries and sequenced events; commands return receipts rather than pretending accepted means applied. | AC-02, AC-36 |
| `CTL-02` | M2 | Use expected revisions and explicit session/device targets for configuration changes; reject stale mutations and deduplicate retried commands. | AC-29, AC-36 |
| `CTL-03` | M4 | Ship macOS and Linux GUIs covering source, playback, device, profile, DSP configuration, signal path and diagnostics without requiring a visual node editor. | AC-37 |
| `CTL-04` | M1 | Ship scriptable CLI controls with structured output, stable errors and bounded waits through the same service contract. | AC-02 |
| `CTL-05` | M4 | Ship versioned UniFFI bindings with Swift integration tests and Kotlin smoke tests; do not transport per-buffer audio through foreign UI callbacks. | AC-02, AC-05 |
| `CTL-06` | M4 | Make slow/disconnected controllers harmless to playback, with bounded event queues, gap markers and snapshot recovery. | AC-36 |
| `OBS-01` | M1 | Report requested, resolved and observed source/graph/device state separately; model preservation, transformations and verification scope as independent fields. | AC-18, AC-38 |
| `OBS-02` | M2 | Provide a signal-path inspector with node identity, versions, formats, gain, latency, bypass state and conversion reasons. | AC-38 |
| `OBS-03` | M2 | Record underruns, deadline misses, queue depths, CPU/latency summaries, clipping and source/graph epochs without blocking audio. | AC-05, AC-32 |
| `OBS-04` | M4 | Export a redacted reproducible diagnostic/evidence bundle tied to build, environment, fixtures, graph and exact test outcomes. | AC-34, AC-39 |
| `OBS-05` | LATER | Add FFT/spectrogram, impulse-response inspection, level-matched A/B and distortion analysis with digital versus acoustic measurement provenance. | AC-43 |
| `COR-01` | M2 | Treat headphone/room corrections as graph assets with model, measurement source, target, preamp, sample rate and provenance. | AC-30, AC-35 |
| `COR-02` | LATER | Implement a guided phone measurement workflow using local capture, authenticated transfer, quality checks and acoustic timing/drift alignment. | AC-44 |
| `COR-03` | LATER | Support calibrated measurement microphones, calibration files and multi-position capture in the same measurement project model. | AC-45 |
| `COR-04` | LATER | Generate bounded correction filters with explicit confidence, boost/energy limits and validation measurements; never equate uncalibrated phone capture with reference measurement. | AC-44, AC-45 |
| `EXT-01` | LATER | Reuse the same engine/control model in a remote headless host and lightweight output endpoints; define transport and clocks before promising synchronized multi-output. | AC-46 |
| `EXT-02` | LATER | Add CLAP, VST3 and LV2 hosting incrementally with plugin identity/state, latency budgets, scanner isolation and tested crash behavior. | AC-47 |
| `EXT-03` | LATER | Add Windows, iOS and Android playback clients, a lightweight local library and multichannel hardware only through platform-specific qualification. | AC-48 |
| `REL-01` | M0 | License first-party code MIT and maintain an audited dependency/license/feature inventory; preserve third-party notices and applicable source obligations. | AC-49 |
| `REL-02` | M0 | Pin candidate dependencies and protocol revisions; isolate CamillaDSP library API changes and treat Sendspin Rust SDK maturity as a qualification question. | AC-01, AC-49 |
| `REL-03` | M4 | Deliver installable desktop apps plus Rust library, standalone local host, CLI and documented control API from one versioned workspace. | AC-50 |
| `REL-04` | M4 | Gate release on traceable acceptance evidence, real macOS/Linux hardware coverage, bounded-resource stress tests and all in-scope safety cases. | AC-39, AC-50 |
| `REL-05` | M4 | Keep basic local playback offline and account-free; make telemetry upload and asset redistribution explicit opt-ins. | AC-34, AC-50 |


## Appendix C. Acceptance scenarios and required evidence

These are executable test specifications, not results. Hardware cases require a recorded device/OS/backend matrix. A missing fixture, unavailable microphone or untested DAC yields **blocked/not run**, never a passing result.

### AC-01 — Architecture and dependency spike (M0)

**Setup:** Virtual output; selected crate/protocol revisions recorded.
**Action:** Render a known PCM fixture offline; prepare one filter through each candidate adapter; build the workspace on macOS and Linux.
**Pass condition:** Stable product types do not expose upstream types; feature/license inventory and candidate decisions are committed; no unqualified backend is marked supported.
**Covers:** ARCH-01, ARCH-06, REL-02.
**Evidence:** `dependency-lock.json`, `architecture-check.txt`, `offline-output.sha256`.

### AC-02 — Control-surface equivalence (M4)

**Setup:** Same engine version, fixture, output and starting state for Rust, CLI, API and Swift clients; Kotlin binding smoke harness.
**Action:** Run equivalent play/pause/seek/profile/graph commands and inspect normalized state and receipts.
**Pass condition:** Identical semantic results and policy enforcement; unsupported operations give the same typed errors; FFI lifetimes/cancellation pass.
**Covers:** ARCH-02, CTL-01, CTL-04, CTL-05.
**Evidence:** `control-transcripts.json`, `ffi-test-results.json`.

### AC-03 — Single output owner (M1)

**Setup:** GUI owner and standalone-host executable on the same machine.
**Action:** Start a second owner, attach multiple controllers, disconnect the GUI and deliberately stop the host.
**Pass condition:** Second instance attaches or fails clearly; no competing device opens; documented host lifecycle and clean shutdown.
**Covers:** ARCH-03.
**Evidence:** `owner-lifecycle.json`, `output-open-trace.json`.

### AC-04 — PCM preservation and strict refusal (M1)

**Setup:** Integer fixtures covering signed limits, low-order bits and supported rates/widths; virtual capture and qualified real route.
**Action:** Decode, bypass processing, pack/unpack output and compare numeric sample payloads; request an unsupported format and enable software gain.
**Pass condition:** Zero sample/frame differences on preservation paths; strict mode rejects required alteration; lossless container padding is distinguished from quantization.
**Covers:** ARCH-04, OUT-06.
**Evidence:** `pcm-comparison.json`, `negotiation-events.json`.

### AC-05 — Real-time discipline (M4)

**Setup:** Instrumented allocator, callback/DSP thread markers and slow foreign observers.
**Action:** Warm the engine, then play, seek, replace graphs, disconnect observers and induce diagnostic pressure.
**Pass condition:** No real-time allocation/deallocation, forbidden blocking calls or FFI callback work; no playback dependence on observer speed.
**Covers:** ARCH-05, CTL-05, OBS-03.
**Evidence:** `allocation-report.json`, `realtime-trace.json`.

### AC-06 — Repeatable fault harness (M0)

**Setup:** Deterministic virtual clock, bounded input source and controllable output.
**Action:** Inject short writes, underruns, backend failure, malformed chunks and stale-generation packets using fixed seeds.
**Pass condition:** Repeatable event sequences and output hashes; failures terminate or recover according to documented policy.
**Covers:** ARCH-06.
**Evidence:** `fault-seeds.json`, `event-log.json`, `capture.sha256`.

### AC-07 — Schema compatibility (M0)

**Setup:** Current, older supported and future-version profile/command fixtures.
**Action:** Load and round-trip supported data; submit unknown required nodes/fields and malformed versions.
**Pass condition:** Supported migration preserves meaning; unsupported required features fail explicitly; original data remains recoverable.
**Covers:** ARCH-07.
**Evidence:** `schema-tests.json`, `migration-diffs.json`.

### AC-08 — Minimal local playback (M1)

**Setup:** Owned/generated WAV, FLAC and AIFF fixtures, a large directory and a missing file.
**Action:** Open one file, append a directory, reorder the queue and compare source-file hashes before/after.
**Pass condition:** Correct tracks and order without modifying source files; bounded metadata work; missing files produce visible per-item errors.
**Covers:** PLAY-01.
**Evidence:** `queue-tests.json`, `source-hashes.json`.

### AC-09 — Transport semantics and recovery (M1)

**Setup:** Known-duration fixture, variable output buffering and seek-capable/streaming inputs.
**Action:** Pause/resume/seek repeatedly; inspect audible vs decoded position; restart after abrupt termination.
**Pass condition:** Position follows output accounting; seeks flush stale data; unsupported seeks reject; restart restores queue paused/muted, not audibly playing.
**Covers:** PLAY-02, PLAY-06, OUT-04.
**Evidence:** `transport-timeline.json`, `recovery-state.json`.

### AC-10 — Gapless continuity with DSP state (M2)

**Setup:** One generated signal split losslessly into multiple files, plus the unsplit reference.
**Action:** Play both through bypass, EQ and convolution; compare aligned captures including the join and final filter tail.
**Pass condition:** Equal sample count and no artificial discontinuity; state is not reset at compatible joins; final tail is drained exactly once.
**Covers:** PLAY-03, PLAY-04.
**Evidence:** `gapless-comparison.json`, `boundary-captures.wav`.

### AC-11 — Incompatible track transition (M2)

**Setup:** Queue containing changed rates, channel layouts and PCM/DSD modes.
**Action:** Play transitions under strict and compatible policies with backend reopen delays.
**Pass condition:** Documented muted reconfiguration or explicit conversion, truthful discontinuity reporting and no false universal-gapless claim.
**Covers:** PLAY-04.
**Evidence:** `transition-events.json`, `output-format-trace.json`.

### AC-12 — Exclusive source ownership (M3)

**Setup:** Local, UPnP and Sendspin adapters with tagged test audio and delayed packets.
**Action:** Start each source over another, then deliver old-source chunks and commands after the new session starts.
**Pass condition:** One owner; prior source stopped/flushed; old generations rejected; setting metadata alone does not steal ownership.
**Covers:** PLAY-05.
**Evidence:** `handoff-events.json`, `tagged-output-analysis.json`.

### AC-13 — DSD transport and safety (M3)

**Setup:** Generated DSF/uncompressed DFF payloads, software DoP parser and at least one qualified hardware route on each launch OS.
**Action:** Pack/unpack DoP; switch PCM/DSD; pause/stop; inject underrun; request software attenuation and DSP on the passthrough path.
**Pass condition:** DSD payload preserved; markers and mute transitions validated; incompatible processing refused; no DSD bytes emitted as ordinary PCM. Native DSD advertised only where separately exercised.
**Covers:** ARCH-04, PLAY-07, SAFE-04.
**Evidence:** `dsd-roundtrip.json`, `dop-transition-capture.bin`, `hardware-check.json`.

### AC-14 — Output change and crash safety (M1)

**Setup:** Two physical outputs, one fixed-output profile and one software-volume profile.
**Action:** Unplug active DAC, change defaults, fail reopen, restart process and switch profiles with different saved levels.
**Pass condition:** No silent reroute or unexpected level increase; fixed output remains unarmed until explicit approval; state and corrective action are visible.
**Covers:** PLAY-06, OUT-05, SAFE-01.
**Evidence:** `safety-event-log.json`, `device-trace.json`.

### AC-15 — UPnP interoperability (M3)

**Setup:** Pinned Music Assistant version, another independent controller and network fault proxy.
**Action:** Discover; set URI; play/pause/seek/stop; change volume; subscribe/unsubscribe; test next-URI and renderer restart.
**Pass condition:** Correct advertised services and events; implemented next-URI behaves correctly or is clearly unsupported; no claim of third-party certification.
**Covers:** SRC-01.
**Evidence:** `upnp-conformance.json`, `controller-results.json`.

### AC-16 — Sendspin interoperability (M3)

**Setup:** Pinned spec, SDK and Music Assistant revisions; PCM and FLAC fixtures.
**Action:** Pair/discover, negotiate roles/formats, play, change formats, clear/end streams, reconnect and control upstream playback.
**Pass condition:** Protocol-conformant lifecycle; only supported formats advertised; no stale buffered replay; queue stays authoritative on server.
**Covers:** SRC-02, SRC-05.
**Evidence:** `sendspin-transcript.json`, `protocol-version-lock.json`.

### AC-17 — Clock drift and latency accounting (M3)

**Setup:** Two independent output clocks; instrumented simultaneous capture; declared jitter/drift and DSP-latency cases.
**Action:** Run long synchronized playback; change latency mode/FIR; inject jitter, a late chunk and an underrun; quantify output offsets.
**Pass condition:** Meet pinned protocol limits in the declared operating envelope; delay counted once; recovery is conformant; evidence separates simulated, digital-output and acoustic results.
**Covers:** SRC-03, OUT-04.
**Evidence:** `clock-test.json`, `paired-capture.wav`, `sync-error.csv`.

### AC-18 — Truthful source/output fidelity (M3)

**Setup:** Original high-rate source deliberately transcoded upstream; local bypass and DSP cases.
**Action:** Inspect GUI/API signal path and preservation status before and after sync adjustment or gain changes.
**Pass condition:** Original metadata never substitutes for received format; transformations visible; unknown upstream/hardware facts remain unknown.
**Covers:** SRC-04, OBS-01.
**Evidence:** `fidelity-snapshots.json`.

### AC-19 — Queue authority (M3)

**Setup:** Active Music Assistant stream and an existing local queue.
**Action:** Issue next/seek from GUI and CLI; leave and rejoin network playback; inspect both queue identities.
**Pass condition:** Supported commands routed to server; local queue is retained separately; no divergent shadow streaming queue.
**Covers:** SRC-05.
**Evidence:** `queue-authority-events.json`.

### AC-20 — Network source abuse and authorization (M4)

**Setup:** Malformed protocol corpus, disallowed URL origins and unpaired peers.
**Action:** Attempt unauthorized controls, forbidden URI schemes, redirects to forbidden destinations, oversized messages and stale session replay.
**Pass condition:** Blocked with bounded resources and no secret/file disclosure; explicitly authorized LAN Music Assistant URLs still function.
**Covers:** SRC-06, SAFE-05.
**Evidence:** `network-security-tests.json`, `parser-fuzz-summary.json`.

### AC-21 — Backend negotiation (M1)

**Setup:** Named macOS/Core Audio and Linux/ALSA machines and DACs; supported/unsupported formats.
**Action:** Enumerate, open, inspect actual mode, play, close and reopen; test busy-device, unplug and stale saved-profile conditions.
**Pass condition:** Requested vs actual format and evidence recorded; strict failures do not silently use mixers/converters; advertised capability is not marked measured.
**Covers:** OUT-01, OUT-03, OUT-06.
**Evidence:** `hardware-matrix.json`, `backend-negotiation.json`.

### AC-22 — Shared Linux compatibility route (M4)

**Setup:** Qualified PipeWire desktop configuration with another application playing.
**Action:** Play through the system route, change server rate and switch to direct output.
**Pass condition:** Shared playback works with truthful transformations/unknowns; direct mode failure is explained; no automatic system-service termination.
**Covers:** OUT-02.
**Evidence:** `linux-route-test.json`.

### AC-23 — Capability evidence invalidation (M1)

**Setup:** Device record with test evidence and changed OS/backend/device fingerprint fixtures.
**Action:** Change relevant environment inputs and reload the profile; initiate a user-approved capability test.
**Pass condition:** Old verification becomes stale; non-invasive checks are distinguished from emitted-signal tests; no audible automatic probe.
**Covers:** OUT-03.
**Evidence:** `capability-provenance.json`, `invalidation-tests.json`.

### AC-24 — Graph compiler validity (M2)

**Setup:** Valid branched/matrix graphs plus cycles, invalid domains, excessive graphs and unsupported nodes.
**Action:** Validate and compile each; inspect plan, resource budget, latency and required conversions.
**Pass condition:** Valid graph scheduled deterministically; invalid graphs rejected before activation with actionable node/edge errors.
**Covers:** DSP-01, DSP-03.
**Evidence:** `graph-fixture-results.json`, `compiled-plan.json`.

### AC-25 — PEQ and gain numerics (M2)

**Setup:** Independent analytical biquad reference; impulse, sweep, DC and limit-value fixtures.
**Action:** Render filters at qualified rates, compare frequency response and finite output, and update coefficients during playback.
**Pass condition:** Meets stated numerical tolerances; invalid frequencies/Q/gains rejected; coefficient changes follow the tested transition policy.
**Covers:** DSP-02, DSP-03.
**Evidence:** `peq-response.csv`, `numeric-test-results.json`.

### AC-26 — Convolution correctness (M2)

**Setup:** Impulse identity, short direct-convolution reference, long stereo correction filters and rate-mismatch fixture.
**Action:** Compare partitioned output, latency and tail to reference; process silence; import incompatible-rate and measurement-IR files.
**Pass condition:** Matches stated tolerances and channel order; no hidden normalization; incompatible/ambiguous assets require rejection or explicit conversion.
**Covers:** DSP-02.
**Evidence:** `convolution-comparison.json`, `fir-provenance.json`.

### AC-27 — Resampling qualification (M2)

**Setup:** Fixed and asynchronous rate pairs, impulses, sweeps, out-of-band tones and deterministic drift.
**Action:** Measure gain, passband, alias rejection, sample accounting, latency and ratio control for every exposed preset.
**Pass condition:** Each preset meets its declared measured envelope; no unsupported claims of algorithm equivalence or audible superiority.
**Covers:** DSP-02.
**Evidence:** `resampler-benchmarks.json`, `response-data.csv`.

### AC-28 — Branch latency alignment (M2)

**Setup:** Parallel paths with known integer and supported fractional latency differences.
**Action:** Inject an impulse, merge branches and change one path latency under the allowed reconfiguration policy.
**Pass condition:** Correct alignment within declared precision; unsupported alignment rejected; phase response is not mislabeled as a scheduling offset.
**Covers:** DSP-04.
**Evidence:** `branch-alignment.json`, `impulse-capture.wav`.

### AC-29 — Atomic graph/profile changes (M2)

**Setup:** Running graph; competing controllers; a replacement needing more resources than available.
**Action:** Prepare/commit two revisions concurrently, inject failure during preparation and observe transition behavior.
**Pass condition:** At most one valid revision commits; stale command rejected; old working graph retained; no real-time resource destruction or unsafe gain jump.
**Covers:** DSP-05, CTL-02.
**Evidence:** `graph-transaction-events.json`, `resource-lifecycle-trace.json`.

### AC-30 — Correction import and provenance (M2)

**Setup:** Known REW/AutoEq text subsets and FIR WAV fixtures, unknown fields and hostile file paths.
**Action:** Import, preview, export and reimport; compare coefficients, preamp, units, sample rate, metadata and hashes.
**Pass condition:** Supported meaning preserved; unsupported content reported; no execution/network fetch from profiles; measured room IR not assumed to be correction.
**Covers:** DSP-06, DSP-07, PROF-03, COR-01.
**Evidence:** `import-roundtrip.json`, `asset-manifest.json`.

### AC-31 — Unsafe DSP rejection (M4)

**Setup:** NaN/Inf and unstable coefficients, excessive gain/resource demands and malformed assets.
**Action:** Attempt activation and inject non-finite runtime output from a test node.
**Pass condition:** Invalid plans rejected; runtime faults cause safe stop/mute; protective filters are never silently bypassed; no unbounded resource growth.
**Covers:** DSP-07, SAFE-02, SAFE-06.
**Evidence:** `dsp-safety-tests.json`, `fault-events.json`.

### AC-32 — Latency modes and sustained playback (M4)

**Setup:** Committed reference hardware, baseline graphs, stress workloads and memory budgets.
**Action:** Run stated endurance/performance suites, switch modes and request a graph outside the mode budget.
**Pass condition:** Meets declared performance gates; reports latency and underruns; refuses incompatible choices instead of changing DSP invisibly.
**Covers:** DSP-08, OBS-03.
**Evidence:** `performance.json`, `endurance.json`, `memory-trace.csv`.

### AC-33 — Volume policy across every controller (M3)

**Setup:** Fixed/hardware/software profiles; UPnP/Sendspin mapping fixtures; DSD and PCM paths.
**Action:** Apply volume/mute concurrently, reconnect hardware, attempt profile-import escalation and issue maximum remote volume.
**Pass condition:** Consistent caps and mute semantics; no duplicate gain; readback uncertainty visible; incompatible DSD gain refused; no fixed-output escalation.
**Covers:** SAFE-01, SAFE-02, SAFE-03, SAFE-04.
**Evidence:** `volume-policy-tests.json`, `gain-ledger.json`.

### AC-34 — Privacy and local-first operation (M4)

**Setup:** Network blocked; secrets and private paths in test state.
**Action:** Play local files offline, inspect outbound connections, export default diagnostics and try cross-user local IPC.
**Pass condition:** Local playback needs no account; no unsolicited upload; diagnostics redact secrets; other users cannot control the engine by default.
**Covers:** SAFE-05, SAFE-06, OBS-04, REL-05.
**Evidence:** `privacy-tests.json`, `redaction-report.json`.

### AC-35 — Profiles and deterministic association (M2)

**Setup:** Several DAC identities and headphone/speaker setups sharing one DAC.
**Action:** Switch devices/setups, resolve overlapping bindings and round-trip a profile with correction assets.
**Pass condition:** Predictable selection with explanation; ambiguous hardware requires manual setup selection; gain policy survives export/import.
**Covers:** PROF-01, PROF-02, PROF-03, COR-01.
**Evidence:** `profile-selection.json`, `profile-roundtrip.json`.

### AC-36 — Receipts, concurrency and slow clients (M4)

**Setup:** Two controllers, duplicate request IDs, disconnects and backpressured event consumers.
**Action:** Issue commands, replay retries, overflow telemetry queues and reconnect using cursors/snapshots.
**Pass condition:** At-most-once mutation within documented dedupe window; applied/failure state distinct from acceptance; event gaps recover without blocking playback.
**Covers:** CTL-01, CTL-02, CTL-06.
**Evidence:** `api-concurrency.json`, `event-recovery.json`.

### AC-37 — Desktop usability parity (M4)

**Setup:** Packaged macOS and Linux applications on qualified systems.
**Action:** Complete first-run device selection, local/network playback, correction import, profile switch, signal-path inspection and safe recovery without CLI.
**Pass condition:** Both GUIs cover release features with accessible controls and actual state; no unavailable future feature masquerades as functional.
**Covers:** CTL-03.
**Evidence:** `gui-acceptance.json`, `screenshots-manifest.json`.

### AC-38 — Signal path accuracy (M2)

**Setup:** Bypass, EQ, resampling, software/hardware gain and shared-output cases.
**Action:** Compare inspector/API reports against compiled graph and backend instrumentation.
**Pass condition:** Requested/resolved/observed are distinct; all active transformations and evidence limits shown; no unsupported end-to-end verification claim.
**Covers:** OBS-01, OBS-02.
**Evidence:** `signal-path-comparison.json`.

### AC-39 — Evidence completeness (M4)

**Setup:** Requirements catalog and all release test runs.
**Action:** Validate references, hashes, environments, actual results and required platform coverage.
**Pass condition:** Every in-scope requirement has passing exact evidence; blocked/not-run does not count as pass; future requirements remain clearly deferred.
**Covers:** OBS-04, REL-04.
**Evidence:** `release-traceability.json`, `evidence-validation.json`.

### AC-40 — Advanced transitions (LATER)

**Setup:** Lossless/lossy, rate-change and strict-mode fixtures.
**Action:** Exercise fades/crossfades/ReplayGain and queue modes with incompatible strict policies.
**Pass condition:** Transformations disclosed, strict conflicts rejected and rate-specific gapless claims tested.
**Covers:** PLAY-08.
**Evidence:** `transition-feature-tests.json`.

### AC-41 — Advanced DSD/DAC processing (LATER)

**Setup:** Validated reference vectors, ultrasonic/noise measurements and qualified hardware.
**Action:** Qualify each new modulator/correction algorithm and all mute/start/stop/failure transitions.
**Pass condition:** Declared numerical, spectral, stability and hardware limits met; no generic HQPlayer/Dirac equivalence claim.
**Covers:** DSP-09.
**Evidence:** `advanced-dsp-qualification.json`.

### AC-42 — Context-aware profiles (LATER)

**Setup:** Conflicting context rules, changing stream format and manual overrides.
**Action:** Evaluate, preview and apply rules during playback with rapid context changes.
**Pass condition:** Deterministic priorities, bounded evaluation, no oscillation and preserved safety constraints.
**Covers:** PROF-04.
**Evidence:** `context-rule-tests.json`.

### AC-43 — Advanced audio analysis (LATER)

**Setup:** Synthetic known responses and calibrated loopback data.
**Action:** Compare spectral/impulse results and run latency-aligned, gain-matched A/B with output provenance.
**Pass condition:** Known inputs reproduced within declared tolerances; software estimates never presented as measured DAC/room performance.
**Covers:** OBS-05.
**Evidence:** `analysis-reference-tests.json`.

### AC-44 — Phone measurement confidence (LATER)

**Setup:** Supported phones/routes, injected AGC/clipping/drift, acoustic-reference recordings and a comparison microphone.
**Action:** Capture locally, upload with hashes, align/de-drift, assess confidence and generate bounded correction.
**Pass condition:** Bad captures rejected; no unsupported absolute SPL/phase claim; confidence-qualified regions only; physical before/after validation recorded.
**Covers:** COR-02, COR-04.
**Evidence:** `phone-measurement-project.json`, `validation-captures.wav`.

### AC-45 — Precision room correction (LATER)

**Setup:** Calibrated microphone/interface and multi-position listening-area project.
**Action:** Calibrate, capture positions, create target/constraints, generate filters and measure corrected results.
**Pass condition:** Calibration provenance retained; boost/energy limits respected; before/after distinguished from prediction and reversible profile created.
**Covers:** COR-03, COR-04.
**Evidence:** `precision-measurement-project.json`, `before-after-results.json`.

### AC-46 — Remote host and endpoint separation (LATER)

**Setup:** One host, two endpoint clocks, network fault proxy and authenticated controllers.
**Action:** Use the same control contract remotely, negotiate endpoint formats and exercise disconnect/reconnect and grouped playback.
**Pass condition:** No UI requirement on host; preserved safety/identity; declared timing and quality envelope met without assuming NAA interoperability.
**Covers:** EXT-01.
**Evidence:** `remote-endpoint-qualification.json`.

### AC-47 — Plugin hosting failure containment (LATER)

**Setup:** Test plugins with hangs, crashes, latency changes and malicious state.
**Action:** Scan, instantiate, restore, process and crash plugins under the isolation policy.
**Pass condition:** Engine survives where isolation is claimed; bounded overhead; safe stop rather than unsafe correction bypass; no blanket sandbox claim.
**Covers:** EXT-02.
**Evidence:** `plugin-host-tests.json`.

### AC-48 — Additional clients/platforms (LATER)

**Setup:** Named Windows/mobile/multichannel targets and library migration fixtures.
**Action:** Repeat applicable control, output, interruption, permission, route-change and persistence suites.
**Pass condition:** Support published only for exercised features/platforms; mobile limits and multichannel routing independently validated.
**Covers:** EXT-03.
**Evidence:** `platform-expansion-matrix.json`.

### AC-49 — License and feature audit (M4)

**Setup:** Locked dependency graph including platform/build features and bundled correction assets.
**Action:** Review SPDX declarations, actual licenses, notices, modifications, source-delivery obligations and asset provenance.
**Pass condition:** First-party MIT declaration accurate; no unreviewed restrictive feature in release; all distributed third-party obligations documented.
**Covers:** REL-01, REL-02.
**Evidence:** `sbom.json`, `THIRD_PARTY_NOTICES.txt`, `license-audit.json`.

### AC-50 — Release installation and rollback (M4)

**Setup:** Fresh supported macOS/Linux environments and an older supported installation.
**Action:** Install packaged GUI/CLI/host, run offline, upgrade with migration, restore backup and uninstall.
**Pass condition:** Working binaries/docs and authenticated artifacts; no accidental duplicate host; user music untouched; rollback path documented and tested.
**Covers:** REL-03, REL-04, REL-05.
**Evidence:** `installation-matrix.json`, `artifact-hashes.json`, `migration-results.json`.



## Appendix D. Primary-source register

Sources were inspected for this specification on November 16, 2026 (America/Chicago). These are verification references, **not dependency locks**. Implementation must record the exact selected release/commit and enabled features. Retrieved documentation is not evidence that our product has implemented or passed anything.

**S01 — Signalyst, HQPlayer quickstart.** Documents Desktop server/client separation.  
`https://signalyst.com/quickstart-guide/`

**S02 — Signalyst, HQPlayer Embedded.** Documents processing engine, control API, UPnP renderer and network-adapter capabilities.  
`https://signalyst.com/hqplayer-embedded/`

**S03 — Mozilla, UniFFI user guide.** Binding generation and supported languages; distinguishes bindings from shipping applications.  
`https://mozilla.github.io/uniffi-rs/`

**S04 — Music Assistant, Sendspin player support.** Integration context for the Sendspin player provider.  
`https://www.music-assistant.io/player-support/sendspin/`

**S05 — Music Assistant, DLNA player support.** Integration context for the DLNA renderer route.  
`https://www.music-assistant.io/player-support/dlna/`

**S06 — Sendspin Rust SDK repository.** Candidate implementation, Apache-2.0 license and work-in-progress label. A repository label is not a compatibility certification.  
`https://github.com/Sendspin/sendspin-rs`

**S07 — Sendspin protocol specification.** Role/format negotiation, stream lifecycle, timing, buffer accounting and protocol security. See particularly Player, Playback Synchronization and Communication.  
`https://www.sendspin-audio.com/build/spec/`

**S08 — Sendspin client implementation guide.** Explains clock convergence, timing parameters, output delay, drift correction and volume/control behavior.  
`https://www.sendspin-audio.com/build/guide/client/`

**S09 — CamillaDSP crate documentation.** Documents reusable components, embedding and lack of library API semantic-version guarantees.  
`https://docs.rs/camilladsp/latest/camilladsp/`

**S10 — Symphonia upstream repository.** MPL-2.0 license and separate container/codec/gapless capability tables.  
`https://github.com/pdeljanov/Symphonia`

**S11 — Rubato upstream repository.** Rust fixed/asynchronous resampling, real-time considerations and MIT/Apache-2.0 choices.  
`https://github.com/HEnquist/rubato`

**S12 — RustFFT upstream repository.** FFT implementation and MIT/Apache-2.0 license choices.  
`https://github.com/ejmahler/RustFFT`

**S13 — CamillaDSP upstream README and manifest.** GPL-3.0/MPL-2.0 licensing, ASIO-feature exception and REW filter integration. Treat these as upstream's documented feature-specific terms; audit the selected revision.  
`https://github.com/HEnquist/camilladsp/blob/master/README.md`  
`https://github.com/HEnquist/camilladsp/blob/master/Cargo.toml`

**S14 — Mozilla, MPL 2.0 license and FAQ.** File-level covered-source and distribution/larger-work obligations. The actual license controls; the FAQ is guidance.  
`https://www.mozilla.org/en-US/MPL/2.0/`  
`https://www.mozilla.org/en-US/MPL/2.0/FAQ/`

**S15 — ALSA library, PCM plugins.** Direct hardware and automatic-conversion plugin distinctions.  
`https://www.alsa-project.org/alsa-doc/alsa-lib/pcm_plugins.html`

**S16 — Apple, Core Audio device properties.** Hog mode and nominal sample-rate interfaces; these APIs do not constitute an end-to-end fidelity test.  
`https://developer.apple.com/documentation/coreaudio/kaudiodevicepropertyhogmode`  
`https://developer.apple.com/documentation/coreaudio/kaudiodevicepropertynominalsamplerate`

**S17 — DoP open standard 1.1, HTML publication.** DSD payload framing/markers and mode-detection behavior.  
`https://dsd-guide.com/dop-open-standard`

**S18 — dCS, What is DoP?** Explains DSD over PCM framing and the risk of altering marker codes with computer volume processing.  
`https://dcsaudio.zendesk.com/hc/en-gb/articles/360015074419-What-is-DoP-DSD-over-PCM`

**S19 — Room EQ Wizard, File menu documentation.** Text EQ export, filter impulse response WAV export and distinction from measured impulse responses.  
`https://www.roomeqwizard.com/help/help_en-GB/html/file.html`

**S20 — AutoEq upstream repository and license.** Profile generation/optimization and MIT code license. This is not blanket verification of every third-party measurement's redistribution rights.  
`https://github.com/jaakkopasanen/AutoEq`  
`https://github.com/jaakkopasanen/AutoEq/blob/master/LICENSE`

**S21 — Apple, AVAudioSession measurement mode.** Intended to minimize system-supplied signal processing.  
`https://developer.apple.com/documentation/avfaudio/avaudiosession/mode-swift.struct/measurement`

**S22 — Android, MediaRecorder.AudioSource.** UNPROCESSED capture support is conditional and falls back to DEFAULT behavior when unavailable.  
`https://developer.android.com/reference/android/media/MediaRecorder.AudioSource`

**S23 — CLAP, official feature overview.** MIT-licensed plugin standard.  
`https://www.cleveraudio.org/1-feature-overview/`

**S24 — Steinberg, VST 3 licensing portal.** Current SDK MIT terms; plugin and trademark rights remain separate considerations.  
`https://steinbergmedia.github.io/vst3_dev_portal/pages/VST%2B3%2BLicensing/Index.html`


## Appendix E. Proposed control contracts and errors

These examples define proposed semantics, not an already shipped wire protocol. M0 must turn them into checked schemas and contract tests. Do not build independent ad hoc JSON handlers in each client.

### E.1 Minimum method catalog

| Method family | Representative methods | Result/constraint |
|---|---|---|
| Capabilities | `capabilities.get`, `schemas.get` | Versioned features, limits, supported nodes/formats and required permissions |
| State/events | `state.get`, `events.subscribe`, `operations.get` | Snapshot revision, sequence cursor, gap recovery and operation status |
| Devices | `devices.list`, `devices.inspect`, `devices.test` | Evidence-aware capabilities; emitted-signal tests require explicit authorization |
| Sessions | `sessions.start`, `sessions.stop`, `sessions.pause`, `sessions.resume`, `sessions.seek` | Explicit source/target, ownership policy and receipt |
| Local queue | `queue.get`, `queue.append`, `queue.remove`, `queue.reorder` | Local authority and expected queue revision |
| Volume | `volume.set`, `mute.set`, `volume.policy.get` | Mode-aware gain handling; cannot circumvent safety caps |
| Graph | `graph.validate`, `graph.prepare`, `graph.commit`, `graph.inspect` | Plan preview, expected revision, prepared token and active graph hash |
| Profiles | `profiles.list`, `profiles.create`, `profiles.update`, `profiles.resolve`, `profiles.apply` | Intent/resolved distinction and explicit selection reason |
| Assets | `assets.import`, `assets.inspect`, `profiles.export` | Bounded import, manifest/provenance and preview; no automatic application |
| Diagnostics | `signal_path.get`, `metrics.get`, `diagnostics.export` | Observed/unknown states and redaction by default |

Mutation methods accept a unique request ID. Configuration mutations also accept expected revisions. Commands must have bounded execution/cancellation semantics; preparing a long convolution kernel returns an operation handle rather than blocking the UI or audio thread.

### E.2 Graph activation transaction

A controller submits a graph and policy for validation. The response states required conversions, expected latency, resources, gain implications, unsupported nodes and the currently observed device capabilities. Preparation returns a short-lived token bound to the graph hash, device/session epoch and expected state revision.

Commit consumes the token only if those preconditions still hold. The immediate receipt may be `accepted`; the later activation event reports `applied` with the resulting graph revision and activation point. A device unplug between prepare and commit invalidates the token. The caller must re-resolve, not reuse an outdated plan.

### E.3 Error taxonomy

| Code | Meaning | Required caller behavior |
|---|---|---|
| `CAPABILITY_UNSUPPORTED` | Node, codec, command or platform feature not implemented | Inspect capabilities; do not retry unchanged |
| `DEVICE_UNAVAILABLE` | Selected output absent/busy or identity stale | Keep output safe; require explicit device/recovery action |
| `FORMAT_UNSUPPORTED` | Exact mode not accepted | Resolve against policy; no implicit conversion in strict mode |
| `FIDELITY_POLICY_CONFLICT` | Requested processing conflicts with preservation intent | Present explicit alternative; do not change mode automatically |
| `SOURCE_BUSY` | Current session does not permit an unapproved takeover | Request explicit source switch |
| `STALE_REVISION` | Concurrent state/configuration changed | Refresh, reconcile and retry with a new request |
| `STALE_SESSION` | Command or audio belongs to an old epoch | Discard/reconcile; never inject into new playback |
| `GRAPH_INVALID` | Graph has a type, topology or parameter problem | Display node/edge-specific validation details |
| `RESOURCE_BUDGET_EXCEEDED` | Plan exceeds admitted memory/CPU/latency limits | Adjust profile/budget explicitly; no hidden quality degradation |
| `ASSET_INVALID` | Unsupported/unsafe/mismatched correction or media asset | Preview import errors; preserve original file |
| `SAFETY_POLICY_BLOCKED` | Gain, fixed-output arming or transition violates protection | Keep muted; require explicit permitted remediation |
| `AUTH_REQUIRED` / `AUTH_DENIED` | Caller not authorized | Pair/authenticate or stop; never fall back to unauthenticated administration |
| `OPERATION_CANCELLED` | Operation deliberately stopped | Reconcile actual state, which may still be the previous configuration |
| `OUTPUT_FAULT` | Runtime backend failure | Stop/mute; expose recovery, never auto-reroute |
| `INTERNAL_FAULT` | Unexpected engine defect | Safe failure plus diagnostic ID; no secret-bearing error payload |

### E.4 Versioning rules

A major schema/API revision changes required semantics. Optional additive fields must remain distinguishable from unsupported required features. A client cannot infer that a node exists because it understands the node's schema; the engine capability response decides availability.

Numeric units must appear in field names or schemas (`gain_db`, `delay_frames`, `sample_rate_hz`) rather than ambiguous bare numbers. IDs are opaque strings; presentation names and OS enumeration indices are not persistent identity. External documentation and CLI help are generated or checked against the same command/error definitions.

## Appendix F. Evidence-record template

The companion `evidence-template.json` is intentionally marked **not_run**. Fill it only from actual test execution. Fixture, build, environment and artifact hashes must be real; placeholders are not evidence. The companion `requirements.json` provides requirement-to-decision, requirement-to-test and requirement dependency links for issue generation and progress tracking.

A future implementation agent should start with T01–T03, not generate every future subsystem. Its first completion claim should name the specific passed gate, test command and evidence artifacts.
