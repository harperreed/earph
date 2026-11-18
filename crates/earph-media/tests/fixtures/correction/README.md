# Owned correction fixtures

These fixtures use owned synthetic values in documented source layouts.
They contain no downloaded measurements or user recordings.
The vendor applications did not generate these files.

`owned-autoeq.txt` follows AutoEq's `write_eqapo_parametric_eq` output grammar.
`owned-rew.txt` follows REW's text export header and Generic PK controls.
`owned-camilladsp.yaml` follows CamillaDSP 4.1 filter and ordered pipeline definitions.
The guide records pinned primary source references and unsupported controls.

The real DSP consumer checks the 1000 Hz response and asymmetric channel behavior.
A separate transfer-function calculation checks off-center peaking response.
The CLI tests import each file and render an impulse through its emitted graph.
