# Rust Audiophile Player — specification bundle

Design baseline dated November 16, 2026 (America/Chicago).

## Contents

- `SPEC.md`: Full product/engineering specification, decision ledger, requirements, acceptance scenarios, implementation order and primary-source register.
- `requirements.json`: Live catalog with 69 requirements, dependency edges, decision links and 50 acceptance scenarios. It tracks scoped implementation and verification progress.
- `evidence-template.json`: Empty test-evidence record. Fill it from actual execution, not assumptions.
- `validation.json`: Structural validation of this specification only. It does not report audio, software or hardware qualification.

## First implementation target

The T01–T03 foundation now has passing Linux virtual-output evidence in `evidence/foundation/`.
Other gates remain partial or not run.
The [T05 ALSA spike](alsa-spike.md) adds Linux output with real plugin tests and silent host negotiation.
It does not qualify physical playback or complete M0.

Start with T01–T03: shared domain/control contracts, exact dependency/license decisions, deterministic fixtures, virtual output and the offline test harness. Then build the M1 local playback slice on macOS and Linux. Implement later subsystems only after their prerequisites and acceptance contracts exist.

The user's first-party code license decision is MIT. Third-party dependencies and assets retain their own terms.

## Decision-record exception

The user selected D for question 25, but the option text is absent from the available transcript. The document preserves that selection and labels the first-release plan as a proposed engineering baseline rather than inventing an approved meaning.
