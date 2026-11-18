# Offline graph capture

`earph-engine render-graph` processes local audio into a new capture directory.
It opens no output device and grants no playback consent.
Live owner playback does not use SRC through this command.

Pass a strict `GraphDocument` JSON file and choose every output control explicitly.
For example:

```sh
earph-engine render-graph \
  --graph graph.json \
  --output new-capture \
  --sample-format signed24 \
  --gain-db -18 \
  --ramp-frames 480 \
  --quantum 64 \
  -- track-one.wav track-two.wav
```

Choose `signed16`, `signed24`, or `signed32` for the packed little-endian capture.
Gain accepts -120 through 0 dB.
The ramp starts at silence and spans output frames.
Its maximum length is one output second.
Quantum accepts one through 8192 frames, within every graph port's declared limit.
The graph must use explicit f64 PCM ports.
Explicit SRC nodes may give the graph different input and output rates.
The compiler rejects unsupported rates, chunks, layouts, bypass paths, and non-final PEQ islands.

Supply one through 256 local lossless source files.
Every source must have exactly the same decoded rate, channel layout, and integer representation.
The graph must match the source geometry at its input.
File joins retain filter and SRC history through one continuous stream.
The command finishes after real source EOF and the complete bounded graph tail.
It retains all returned SRC output, including explicit guard and drain padding.
It does not trim output to a nominal rate ratio or an assumed delay.

If the graph contains FIR references, pass `--runtime PRIVATE_ASSET_DIR` explicitly.
Use the existing `import-fir` command to populate that private runtime's immutable asset store.
The runtime follows the existing owner's absolute-path, ownership, permissions, and short-path limits.
The renderer reads only those referenced assets and never changes their source files.

The output directory must not exist.
The command creates it with mode 0700 and creates `capture.pcm` and `report.json` with mode 0600.
It preserves existing directories, sources, and partial artifacts after failures.
It emits a typed JSON report on standard output.
Exit zero means the capture and report completed successfully.
Every failure returns a nonzero exit status.

The report separates accepted source frames, generated DSP output, acknowledged file output, and actual capture bytes.
It hashes a readback of the actual capture, including any prefix left by a failed write.
It reports complete capture frames and any remaining partial-frame bytes.
If no capture exists or observation fails, unknown capture facts remain null.
The report records explicit gain, ramp, quantum, quantization policy, formats, resolved graph identity, and per-stage facts.
It omits private source and output paths.

Stage facts include rate domains, actual SRC counts, guard/padding, local filter latency, and bounded finish support.
An observed truncated library delay is not exact group delay.
Aggregate exact audible timing remains unknown.
Resource figures bound requested component storage and declared scratch overlap.
They do not qualify allocator overhead, process RSS, realtime playback, DAC behavior, or acoustic quality.
