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
