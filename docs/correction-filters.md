# Prepared correction filters

PEQ and convolution run through the existing muted graph activation flow.
Preparation resolves coefficients and checks resources before issuing a token.
Processing performs no file access and allocates or frees no storage.
Native platform and hardware qualification remain separate from these integration checks.

Use the [offline correction text importer](correction-text-import.md) for supported REW, AutoEq, and CamillaDSP definitions.
Its report contains graph intent and provenance, and requires the same owner validation flow.

## Parametric EQ

Use the `peq` node with a `filter` object:

```json
{
  "kind": "peq",
  "filter": {
    "kind": "peaking",
    "frequency_hz": 1000,
    "q": 0.707,
    "gain_db": -6
  }
}
```

Supported filter types are `peaking`, `low_shelf`, and `high_shelf`.
Frequency spans 10 Hz through 0.45 times the processing rate.
Gain spans -24 through 24 dB.
Peaking Q spans 0.1 through 20.
Shelves use a fixed slope of one and reject Q or slope fields.
Filters use one normalized biquad with separate state per channel.
The chosen coefficients must pass stability and finite arithmetic checks.
Zero dB filters still declare processing and conflict with strict preservation.

## Correction WAV import

Import a correction file into the same private runtime used by the owner:

```sh
earph-engine import-fir --runtime /absolute/private/runtime correction.wav
```

The command prints an `asset` reference after verifying the stored bytes.
Use `kind: convolution` and put that returned object in the node's `asset` field.
The reference records SHA-256, rate, channels, authored taps per channel, and the `correction` purpose.
Measurement responses cannot enter this node as ready-made correction filters.

The initial importer accepts little-endian RIFF WAV with IEEE floating PCM format tag three.
Samples must use float32 or float64, with mono or stereo channels.
The declared frame alignment and byte rate must match the sample geometry.
Each channel must contain one through 262144 finite authored taps.
Trailing zero taps remain part of that authored support.
The byte ceiling is 5 MiB, including headers and optional padding.
The importer admits at most 256 chunks and 1 MiB of optional chunk payloads.
Optional chunks are `fact`, `JUNK`, and `PAD `.
Embedded tag containers, artwork, extensible WAV, integer WAV, and other containers require separate qualification.

The importer never normalizes, broadcasts channels, changes rate, trims taps, or changes the source file.
Graph preparation requires the exact declared rate and channel layout.
Identical repeated imports are idempotent.
Conflicting or corrupt existing objects reject without replacement.
The store uses private files under `assets/<sha256>.wav` within the runtime directory.
Source paths never enter graph intent or command history.

Preparation verifies stored bytes against the reference and freezes immutable coefficient owners.
Changing a source or stored object afterward cannot change a prepared token's audio or hash.
A later preparation rechecks the store and rejects corruption.
Partial failed imports remain available for inspection.
Cleanup requires separate approval.

## Tails and latency

Convolution uses fixed 256-frame partitions and a 512-point radix-two FFT.
Its scheduling latency is 256 frames.
An authored M-tap filter emits N + 256 + M - 1 frames for a nonempty N-frame source.
An empty source emits no tail.
FIR-only graphs stop each node at its authored cumulative EOF horizon.
This masks numerical FFT ghosts beyond authored support.
Compatible queue joins share one filter history and one final tail.

PEQ has an infinite mathematical tail.
Its preview therefore reports `bounded_iir_truncation` rather than natural completion.
The compiler propagates actual future-output envelopes across cascades, matrices, sums, delays, and compensation.
It checks that each final channel stays within 1e-12 after the selected horizon.
The complete PEQ-containing graph must settle within 1048576 extra frames.
An unprovable or excessive horizon rejects during preparation.

When a graph contains PEQ, convolution waits for old FFT history to clear before propagating tiny persistent input.
This conservative history horizon can exceed its authored FIR support.
The preview records that policy, each filter's cumulative horizon, and the final discarded-output bound.
The runtime flushes global zeros without prematurely draining individual filters.
The graph owns terminal state and exposes only the remaining valid output prefix.
Tail frames never advance source positions.

Scheduling latency does not establish frequency-dependent phase or acoustic delay.
PEQ and convolution therefore leave audible source positions unknown.
Backend consumption counts retain their existing measurement boundaries.

## Bounds and ownership

The compiler propagates per-channel absolute peaks from declared input bounds.
PEQ and convolution include their separate numerical error allowances.
The engine uses these compiler facts for headroom admission and existing volume controls.
Neither attenuation nor a correction filter grants more output authority.
Strict integer routes reject filters before reading correction files or preparing kernel state.

Resident accounting includes node staging, compensation rings, boxed kernels, spectra, history, and retained taps.
Shared taps count once per unique asset within a plan.
Every filter instance retains its own processing state.
Preparation counts retained pipelines and separate resolution and construction scratch allowances.
These counts describe requested payload storage, not process RSS or allocator overhead.
Rejected and cancelled pipelines retain the existing preparation-thread retirement path.

The Linux integration scenarios compare actual owner output with direct PEQ and convolution references.
They also cover queue joins, frozen assets, malformed input, cancellation, allocation counts, and resource rejection.
These checks do not qualify whole-worker real-time performance or DAC preservation.
