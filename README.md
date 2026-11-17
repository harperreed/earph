# Earph

An inspectable, local-first audio renderer and DSP engine in Rust.

The current foundation checks schemas and generates deterministic offline fixtures.
It does not open physical outputs or implement network playback.
The full product contract lives in [the specification](docs/SPEC.md).
Follow [the foundation plan](docs/superpowers/plans/2026-11-16-foundation.md) for current progress.

Use Rust 1.98.1, selected in `mise.toml`.
Run `scripts/check` for formatting, lint, tests, traceability, and dependency checks.

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

Prepared processing uses bounded buffers and performs no filesystem work.
The allocator test instruments allocations and frees during normal and faulty processing.
This does not qualify real backends or their blocking behavior.

Inspect command syntax with `cargo run --locked -p earph-harness -- --help`.
Output is JSON. Failures exit with status 2.
No commands emit sound or silently select a physical output.

First-party code uses MIT. Dependencies retain their own terms.
See `THIRD_PARTY_NOTICES.txt` and `docs/dependency-lock.json`.
After intentional dependency changes, regenerate both with:

```sh
cargo run --locked -p earph-harness -- dependency-lock \
  --output docs/dependency-lock.json --notices THIRD_PARTY_NOTICES.txt
```

Review exact versions, enabled features, source hashes, and notices before accepting the change.
