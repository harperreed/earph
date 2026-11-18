# Earph

An inspectable, local-first audio renderer and DSP engine in Rust.

The foundation checks schemas and generates deterministic offline fixtures.
The Linux ALSA spike adds explicit output discovery and exact format negotiation.
The local source adds exact integer WAV, FLAC, and AIFF decoding and offline track concatenation.
The private owner service adds queue control and paused, muted crash recovery.
Physical playback integration and network playback remain unfinished.
The full product contract lives in [the specification](docs/SPEC.md).
Follow [the local playback plan](docs/superpowers/plans/2026-11-17-local-playback.md) for current progress.
See [local decoding](docs/local-playback.md) for supported files and offline commands.
See [the state-machine map](docs/state-machines.md) for state owners, transitions, and recovery actions.
See [the ALSA spike](docs/alsa-spike.md) for Linux output commands, evidence, and qualification limits.
See [the Core Audio spike](docs/coreaudio-spike.md) for macOS inspection, precision assessment, and silent registration checks.

Use Rust 1.98.1, selected in `mise.toml`.
Run `scripts/check` for formatting, lint, tests, traceability, and dependency checks.
The lossless fixture tests require FFmpeg.

Generate owned PCM fixtures into a new directory:

```sh
cargo run --locked -p earph-harness -- fixtures --output target/fixtures
```

The tool writes 72 signed integer fixtures and versioned manifests.
Patterns include silence, tagged impulses, signed boundaries, and seeded noise.
Every fixture has 1031 frames at a declared format and seed.
The generator never overwrites an existing output directory.

Render a fixture through virtual output:

```sh
cargo run --locked -p earph-harness -- render \
  --fixture target/fixtures/signed24-stereo-48000-boundary.json \
  --block-frames 256 --output target/capture
```

The tool consumes the actual payload after validating its manifest and hash.
It writes `capture.pcm`, `capture.sha256`, and `report.json`.
Only engine-output preservation is measured.
Decoded progress and output consumption remain separate.
The virtual clock derives time from total consumed frames without block-rounding drift.

Append `--faults PATH` to inject a version 1 fault schedule.
Each entry declares `at_frame` and a typed `fault` object.
Supported fault kinds are `short_write`, `zero_progress`, `underrun`, `backend_failure`, `malformed_chunk`, and `stale_epoch`.
Short writes include a positive `max_frames` field.
Fault positions increase strictly and precede the fixture's end.
Failures retain partial output and a failed report.
The final render report appears only after capture and hash writes succeed.
Qualification validates evidence before publishing a passing final record.

Prepared processing uses bounded buffers and performs no filesystem work.
The allocator test instruments allocations and frees during normal and faulty processing.
This does not qualify real backends or their blocking behavior.

Run the full offline qualification into a new directory:

```sh
mkdir -p target/evidence
cargo run --locked -p earph-harness -- qualify --output target/evidence/foundation
cargo run --locked -p earph-harness -- evidence-check \
  --record target/evidence/foundation/evidence.json
```

Qualification runs 288 clean renders and 18 fault cases, each repeated twice.
It compares actual decoded sample values and captures against on-disk fixtures.
Records include source revision, dirty state, executable hash, dependency-lock hash, environment, and artifact hashes.
The build embeds its source revision, source digest, lock hash, compiler, target, and profile.
Execution-time checkout state remains a separate observation.
Inspect embedded provenance with `cargo run --locked -p earph-harness -- build-info`.
Recorded arguments redact local output paths.
Timestamps use Unix nanoseconds, declared in each record.

`evidence-check` checks the current foundation matrix and its measurement boundary.
It rejects corrupt artifacts, missing provenance, changed outcomes, and broader qualification claims.
It does not authenticate an author or replace independent review.
Interrupted writes leave partial artifacts without a passing final report.

The offline qualified scope is AC-06 on Linux virtual output.
Other scenarios have partial test coverage or remain not run.
The next gate covers real backend spikes and the remaining M0 decisions.

The session model starts muted and keeps paused intent across repeated seeks.
Output progress advances event sequences without invalidating transport-command revisions.
Fixture loading admits raw bytes and typed samples together before reading the payload.

Inspect command syntax with `cargo run --locked -p earph-harness -- --help`.
Output is JSON. Failures exit with status 2.
The harness commands remain offline and never select a physical output.
The separate `earph-alsa` executable opens only an explicitly selected endpoint.
Its fixture exercise requires `--allow-signal` after approval for that route.

First-party code uses MIT. Dependencies retain their own terms.
See `THIRD_PARTY_NOTICES.txt` and `docs/dependency-lock.json`.
After intentional dependency changes, regenerate both with:

```sh
cargo run --locked -p earph-harness -- dependency-lock \
  --output docs/dependency-lock.json --notices THIRD_PARTY_NOTICES.txt
```

Review exact versions, enabled features, source hashes, and notices before accepting the change.
