# Local playback work

The local source reads WAV, FLAC, and AIFF without changing originals.
The qualified PCM subset uses signed 16, 24, or 32-bit samples and one or two channels.
Integer samples never pass through floating-point storage.
RF64, Wave64, float PCM, multichannel, lossy codecs, and DSD remain outside this qualification.
Unsupported inputs return typed errors.

## Offline inspection and capture

```sh
cargo run --locked -p earph-media -- inspect /path/to/music.flac
cargo run --locked -p earph-media -- render --output target/local-capture /path/to/music.flac
cargo run --locked -p earph-media -- render --output target/local-seek --seek 4097 /path/to/music.aiff
```

Inspection validates the container and first packet, rather than the complete payload.
Rendering validates every packet, expected frame positions, and actual end of input.
Pass compatible tracks as successive arguments to capture their exact concatenation.
Changed formats reject before writing the next track.
Capture files contain packed, little-endian PCM at the reported format.
These commands never open an audio device.

Each capture reserves a new output directory.
Existing output directories reject without overwriting artifacts.
Failures preserve the partial capture and a failed report when publication succeeds.
The report hashes actual captured bytes, including partial writes.
It reports complete frames and any remaining partial-frame bytes separately.
An absent final report means publication did not finish.

## Bounds and safety

The reader admits regular files with nonblocking opens.
It checks container metadata before handing files to Symphonia.
Metadata has a cumulative one MiB limit and a 256-block limit.
Packets have a one MiB encoded limit and a 65,536-frame decoded limit.
Decoding, allocation, and filesystem work belong on a worker.
They never belong in a realtime output callback.

The reader rejects detected alternating DoP markers in 24-bit and 32-bit carriers.
Marker detection does not prove an arbitrary file's intended sample domain.
Keep known DoP and DSD sources outside the PCM lane.
Physical signal emission requires explicit approval for the selected output.

## Verification

The public tests generate known WAV samples and lossless FLAC and AIFF counterparts.
Install FFmpeg to run these fixture-producer tests.
Tests compare integer samples, exact seeks, source bytes, and split-track captures.
Failure tests cover missing files, truncation, false lengths, metadata budgets, DoP, FIFO inputs, and partial writes.

The optional private corpus check prints aggregate results without track names or paths:

```sh
cargo run --release --locked -p earph-media --example qualify_corpus -- /path/to/private/corpus
```

On November 17, 2026, 51 qualified PCM cases matched canonical hashes and frame counts.
Four DoP carriers rejected before PCM exposure.
One RF64 case rejected as an unsupported extended container.
Another 34 cases remained outside the selected format subset.
Source hashes matched before and after every exercised case.
No physical output ran.

These checks establish decoder and offline capture behavior.
They do not establish DAC preservation, acoustic safety, DSP continuity, or full M1 completion.
