# Q-controlled correction shelves

Extend the correction importer with explicit Q-controlled shelves.
Preserve the existing fixed-S=1 shelf kinds and playback authority.
Keep full DSP-06 open until profile and Session import work also ships.

## Contract

Add `LowShelfQ` and `HighShelfQ` to Core `PeqFilter`.
Use wire names `low_shelf_q` and `high_shelf_q` with frequency, gain, and explicit Q.
Retain frequency 10 Hz through 0.45 times rate, gain -24 through 24 dB, and Q 0.1 through 20.
Reject unknown controls and ambiguous missing Q.

AutoEq's pinned shelf kernels use alpha = sin(omega)/(2Q).
CamillaDSP's pinned Q shelf kernels use the equivalent beta = sqrt(A)*sin(omega)/Q.
The existing fixed-S=1 shelf beta stays sqrt(2A)*sin(omega).
Do not equate Q with slope.
Feed the new coefficients through the same PeqDesign interval stability, peak, persistent-input, and tail machinery.
Do not introduce a second numerical authority or runtime.
Allow honest numerical-proof or tail-budget rejection.

Import explicit AutoEq LSC/HSC Q shelves and CamillaDSP Lowshelf/Highshelf Q definitions.
Preserve controls and inactive filter policy through typed reports and source export.
Keep the documented REW PK subset.
Reject slope, bandwidth, corner-frequency shelf forms, and other unsupported controls.

## Primary evidence

Checked November 18, 2026.
AutoEq exporter and coefficients use revision `7ae0f56d53074872b028649617a22bbb4232feb7`.
Read `autoeq/frequency_response.py` and `autoeq/peq.py` from the official repository.
CamillaDSP configuration and coefficients use revision `05e9cfcdf43c0dfe078ed3feb8af4c8bd701fd74`.
Read `src/config/mod.rs` and `src/filters/biquad.rs` from the official repository.
Equalizer APO's official configuration reference distinguishes LSC/HSC Q examples from dB-per-octave forms.
Record exact source links in the import guide.
Owned fixtures contain synthetic values in exporter layouts, rather than downloaded measurement data.

## Verification and ownership

- [x] Establish the bounded design and inspect primary formulas before code.
- [x] Verify unchanged Core, PEQ, and importer baselines.
- [x] Prove strict new wire contracts red, then implement Core variants.
- [x] Prove coefficient and response behavior red, then extend the existing design.
- [x] Prove AutoEq/Camilla import and export behavior red, then implement both adapters.
- [x] Exercise actual CLI and DSP consumers with a PK/LSC/HSC preset and preamp.
- [x] Verify peak, tail, numerical admission, lifecycle, and allocation boundaries.
- [x] Run the canonical check and the parent's independent main-model review.
- [x] Prepare the reviewed named unit for commit and return its evidence.

This unit owns Core dsp_nodes, DSP peq, their tests, correction import modules/tests/fixtures, and related docs.
SRC owns Core graph and DSP compile/model/stream/resample/lib.
Root owns engine and native output integration.
No hardware, private corpus, push, deletion, reset, or hook bypass belongs to this unit.
Preserve the earlier YAML test-first gap in its existing task note.
Use red/green before all new behavior here.

## Verified checkpoint

Focused red evidence remains under ignored `target/q-shelf`.
`core-wire-red.log` rejects the absent shelf kind before its implementation.
`response-red.log` fails the actual Q shelf response scenario before the new kinds exist.
`core-wire-green.log` and `response-green.log` record those passing scenarios afterward.
`import-red.log` records rejected complete AutoEq shelf intent before importer support.
`import-first-green.log` records complete AutoEq and Camilla preset roundtrip and response checks afterward.
`diagnostic-red.log` records an incorrect shelf Q diagnostic before its fix.
`media-final.log` records the complete media suite after that fix.
`peq-expanded.log` includes shelf boundaries, adversarial peaks, persistent forcing, terminal residuals, and heap checks.
`clippy-first.log` records strict workspace Clippy.
`unsupported-diagnostic-red.log` records an obsolete PK-only rejection message before its fix.
`media-reviewed.log` records the media suite after that fix.

The existing fixed-S=1 coefficient calculation remains unchanged.
`fixed-coefficient-preservation.json` compares all 48 old numeric rows with their unchanged coefficient arrays.
New Q shelf impulse receipts also appear in `target/peq-numerics`.
Algorithm identities remain unchanged for existing kinds.
New kind and Q fields naturally enter the existing compiler intent hash.
No compiler, runtime lifecycle, or numerical-proof implementation changed.
`scripts/check` passes on the frozen reviewed source in `canonical-reviewed.log`.
It records 485 passing tests, zero failures, and three existing ignored tests.
Formatting, strict Clippy, catalog validation, and dependency validation pass.
The canonical log SHA-256 is `ce3b83f5cbabac9d594e2c04282bb4fc5831718bf0f4bbade4e822dfcb25f8c4`.

The independent SRC compiler agent reviewed the unit on the session's main model.
That agent did not author this source.
`compiler-independent-review.md` records approval with no material findings.
It binds staged tree `4508cb0600ac095d21300616eaf7661774b2dd0c` and patch `8d348b3eb7220cdc6f38a338fa7990e5485a115c488258f264cdcf9410987d86`.
The reviewer independently ran Core, PEQ, importer, and actual CLI scenarios.
The final reread found the obsolete PK-only diagnostic, fixed with retained red/green proof before review.
No material issue remains in this bounded unit.

Precommit checks ran with dirty-source provenance against base `ac5d85b`.
The source-input SHA-256 is `1a0bd2fcde462144ff9a622dbbf928c27ca87c84f36799f02fcf5ab7ff9439ac`.
The dependency-lock SHA-256 remains `2989a4638b2e2f6297a0cd3740f680324f39651386b5efd09daa196fb09e0574`.
The local handoff records clean post-commit provenance and the exact unit SHA.
Root owns final integration; this unit performs no push or hardware action.
