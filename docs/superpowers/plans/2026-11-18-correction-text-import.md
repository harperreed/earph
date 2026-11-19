# Correction text import

Implement the bounded T12 text import checkpoint without changing Core or playback authority.
Keep full DSP-06 completion open while Q-controlled shelves remain unsupported.

## Scope

Read explicit REW Generic, AutoEq parametric, and CamillaDSP filter formats.
Support peaking filters with frequency, gain, and Q.
Support static CamillaDSP Gain filters and ordered channel selections.
Preserve disabled supported filters and empty REW slots in the report.
Reject unsupported definitions, including source shelf Q or dB-per-octave slope controls.
Never reinterpret those controls as Core's fixed shelf slope.

Emit graph intent and provenance through a pure library API and a standalone offline CLI.
Represent correction preamp as graph gain.
Keep the engine's volume and activation controls authoritative.
Never apply, fetch, activate, or unmute an imported document.

Use explicit processing rate, channels, and quantum.
Retain optional model, provider, target, parameter-set, revision, and license metadata.
Hash source bytes without recording the source's absolute file path.
Provide validated report export and same-format reimport checks.

## Source evidence

The REW help documents text filter exports and different equaliser control meanings.
The AutoEq exporter emits preamp followed by PK, LSC, or HSC filter lines.
The CamillaDSP 4.1 README defines YAML filters and ordered pipeline steps.
Its shelf slope uses dB per octave and does not match Core's fixed S parameter.
Its per-step channels and bypass flags remain observable during import.
Record exact URLs and revisions in the import guide and private research receipts.

Use yaml-rust2 0.13.0 with optional encoding support disabled.
Audit its low-level events and reject anchors, aliases, tags, duplicate keys, and excessive structure before building values.
Retain its actual bundled MIT and Apache license files through the canonical dependency inventory.

## Verification

The initial public API test failed before implementation.
The initial YAML event adapter was written before its focused fixture tests.
That branch has no test-first proof.
Preserve its source and history rather than manufacturing that proof.
Its actual parser limits and channel semantics require direct consumer tests and independent review.
The REW header boundary regression has retained red/green evidence.
Return to red/green before further behavior changes.

- [x] Read the specification and inspect primary format sources.
- [x] Verify the unchanged media baseline.
- [x] Verify strict parsing and parameter preservation; retain the available red/green evidence.
- [x] Test actual DSP graph consumers against independent response calculations.
- [x] Exercise the real CLI and roundtrip exports with retained source files.
- [x] Check malformed input, resource limits, path directives, and unsupported controls.
- [x] Run the canonical check and independent main-model review.
- [x] Prepare the reviewed import unit for its named commit and return its evidence.

All existing source files, worktrees, and test artifacts remain preserved.
This unit performs no hardware, microphone, credential, publication, or deletion work.

## Implementation checkpoint

The guide documents exact supported controls and pinned primary sources.
The pure library and CLI share one parser and graph builder.
Reports preserve normalized parameters, inactive definitions, metadata, source identity, and semantic identity.
Real CLI outputs drive actual DSP impulse consumers for all three formats.
Independent transfer calculations check off-center peaking response.
Asymmetric channel, repeated filter, linear Gain, inversion, and mute checks pass.
Parser tests cover aliases, anchors, tags, duplicate keys, byte limits, event quotas, depth, and scalar limits.
Report checks reject graph edits, unknown fields, and incompatible source-dialect labels.
Escaped exports exceeding the source admission limit reject explicitly.

Focused evidence remains under ignored `target/correction-import`.
`public-api-red.log` records the missing initial API before implementation.
`layout-response-first.log` and `layout-response-green.log` record the REW header fix.
`license-detection-red.log` and `license-detection-green.log` record the actual license-discovery fix.
`rew-metadata-red.log`, `rew-version-red.log`, and `yaml-number-red.log` record malformed-input regressions.
`strict-metadata-green.log` records those fixes.
`export-budget-red.log` and `consumer-budgets-green.log` record escaped-output admission.
`report-dialect-red.log` and `report-dialect-green.log` record source-dialect report validation.
`clippy-final.log` records strict workspace Clippy after those changes.
`report-id-red.log` and `report-id-green.log` record the canonical text-ID report fix.
Edited report IDs such as `01` and `+1` reject without silent normalization.
Root approved the broad staged tree `fa70409e9baf62a099c922fc9e602973b427dfe1`.
Root also approved the focused report-ID patch `f13c9f664a5794cc83b11e6e189dccf82e81e5129e5c863d5140cd3acaf9c0d6`.
The review found no remaining Important or Critical defects in the declared subset.
The final reread found that report-ID issue and verified its fix.

`scripts/check` passed on the frozen reviewed source.
`canonical-reviewed.log` records 461 passing tests, zero failures, and three existing ignored tests.
Strict workspace Clippy, formatting, catalog validation, and dependency validation passed.
The source-input SHA-256 is `597ade1e49c590138fd2c55c4566f067c2b8453ddaeb47ba0f82481a28fe6637`.
The dependency-lock SHA-256 is `2989a4638b2e2f6297a0cd3740f680324f39651386b5efd09daa196fb09e0574`.
Checks ran before commit with `source_dirty: true`; they do not claim committed-build provenance.
The local handoff retains post-commit provenance and the exact unit commit.

The named commit contains nineteen owned files.
Root owns integration into the current application branch.
No remote push or hardware qualification belongs to this checkpoint.
