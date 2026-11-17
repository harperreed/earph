# Offline foundation evidence

The qualification ran from clean source commit `d98945db6e4b17080c6a549ce279a0a6baca5e67`.
Its binary embeds that revision and the exact source and dependency digests.
The execution-time checkout matched the build revision.

`evidence.json` covers AC-06 on Linux x86-64 virtual output.
The run passed 288 clean renders and 18 fault cases, each repeated twice.
Every fault capture matches its expected source prefix.
No physical output, macOS, protocol, DSP, profile, or GUI is qualified.

| Evidence category | Actual artifacts |
|---|---|
| Fixture seeds and formats | `fixtures/manifest.json` and individual fixture manifests |
| Injected fault schedules and outcomes | `fault-results.json` |
| Repeated event sequences | `faults/*.json` |
| Raw captures and their hashes | `faults/*.pcm` and `evidence.json` artifact records |
| Numeric preservation comparisons | `render-results.json` |
| Canonical command and test evidence | `verification.json`, `check.log`, and `test-executables.json` |

The manifest maps the spec's evidence categories to these files.
It avoids duplicating seeds, event data, or captures under additional names.

Verify the matrix with a fresh read:

```sh
cargo run --locked -p earph-harness -- evidence-check \
  --record docs/evidence/foundation/evidence.json
```

The separate canonical check passed all 47 tests, formatting, lint, catalog, and dependency checks.
Its listed acceptance scenarios retain partial status.
See [the review](../../foundation-review.md) for findings and regression fixes.

The evidence checker validates artifacts and outcomes without authenticating their author.
Build and execution records retain independent identities.
Default records redact local output paths.
