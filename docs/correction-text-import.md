# Correction text imports

The offline importer creates graph intent and a provenance report.
It never connects to an engine, applies a graph, unmutes output, or changes volume.
This checkpoint implements a documented subset of DSP-06 and AC-30.
Full AutoEq preset support remains incomplete because common Q-controlled shelves reject.
Profiles and Session import commands remain separate work.

## Run an import

```sh
cargo run --locked -p earph-media --bin earph-correction-import -- \
  --format autoeq --rate 48000 --channels stereo --quantum 256 \
  --model 'Owned headphones' --provider 'Owned measurement' \
  --target 'My target' --parameter-set 'PK only' --revision '1' \
  --license 'Private use' -- correction.txt > import-report.json
```

Choose `rew`, `autoeq`, or `camilladsp` explicitly.
Rate, mono/stereo channels, and processing quantum are required.
There are no implicit processing defaults.
The quantum spans 1 through 8192 frames.
Filter frequency must fit the supplied rate.
Metadata options are optional, with 256 bytes per value.
Supply truthful source and redistribution metadata; the importer cannot infer those rights.

The JSON report contains `graph`, source SHA-256, source byte count, normalized controls, metadata, and a semantic SHA-256.
It records `status: validated` for syntax, domain limits, and graph structure.
It does not certify executable headroom, preparation resources, hardware support, or successful playback.
Pass the `graph` through the existing engine graph validation and muted preparation flow before activation.
The engine remains the authority for output consent and volume.
Correction preamp becomes a DSP Gain node.

The importer reads one regular UTF-8 source file and leaves it unchanged.
It never reads referenced files or downloads content.
Reports omit the source path and file name.
Labels, descriptions, and explicit metadata remain source content; inspect them before sharing.
Source and semantic hashes identify bytes and normalized intent; they do not authenticate authorship.

## Supported source controls

| Format | Supported | Rejected |
|---|---|---|
| REW Generic text | `Filter N: ON/OFF PK Fc F Hz Gain G dB Q Q`; explicit `None` slots; optional preamp | Other equalisers, bandwidth controls, shelves, filter families, and processing directives |
| AutoEq parametric | Required `Preamp: G dB`; ordered PK lines; explicit OFF state | LSC/HSC shelves and other APO commands |
| CamillaDSP filter fragment | `Biquad` Peaking with `freq`, `gain`, `q`; static `Gain`; ordered `Filter` pipeline steps | Full device configurations, mixers, processors, convolution paths, other biquads, bandwidth controls, and shelves |

Numeric controls retain their parsed binary64 value without decimal rounding or Q conversion.
Text units may attach to the number or follow it as a separate token.
Values must be finite.
Peaking frequency spans 10 Hz through 0.45 times the processing rate.
Peaking gain spans -24 through 24 dB, and Q spans 0.1 through 20.
Preamp and static dB Gain span -150 through 150 dB.
Static linear Gain spans -10 through 10.
Headroom admission can reject otherwise valid source controls.

Core shelves currently use fixed S=1.
AutoEq shelves export Q, while CamillaDSP shelves accept Q or slope in dB per octave.
Those controls do not establish the same response.
The importer rejects them instead of changing their sound.
Unsupported definitions reject even when unused or bypassed.

REW accepts its documented `Filter Settings file` header and `Room EQ V...` version.
If that header appears, `Equaliser: Generic` must precede filters.
One measurement label and known date, notes, range, and target-level lines may precede filters.
Those measurement fields do not become audio controls or provenance metadata.
Supply metadata options explicitly.
Unknown text lines reject.
An omitted REW preamp means unity DSP gain, recorded as `preamp_explicit: false`.

CamillaDSP accepts `filters` and `pipeline` at the top level.
Gain supports `scale: dB` or `linear`, plus `inverted` and `mute` booleans.
An absent or null Gain scale means dB.
Pipeline `channels` selects zero-based channels from the declared layout.
Absent or null channels select all channels; an empty list selects none.
Duplicate and out-of-range channels reject.
The singular legacy `channel` field rejects.
Pipeline order and repeated filter names remain exact.
Separate stereo chains use matrix selection, scalar filters, matrix placement, and a sum.

The report retains disabled filters, bypassed steps, empty-channel steps, unused definitions, and REW None slots.
Its explicit policy is `pipeline_only_retain_inactive_definitions`.
Only active named filters enter the graph.

## Roundtrip and library use

```sh
cargo run --locked -p earph-media --bin earph-correction-import -- \
  --validate-report import-report.json > validated-report.json
cargo run --locked -p earph-media --bin earph-correction-import -- \
  --export import-report.json > correction-export.txt
```

Report validation rebuilds the graph and checks its agreement with normalized controls and the semantic hash.
It also rejects unknown report fields.
Exports preserve the same source dialect and numeric controls.
CamillaDSP exports use JSON syntax, a supported YAML 1.2 subset.
Source formatting, comments, and measurement headers are not reproduced.
Reimport with the same context and metadata to compare graph intent and semantic identity.
Source hashes normally change after export.
If escaping expands an export beyond 64 KiB, export rejects with `RESOURCE_LIMIT`.

Rust callers use `earph_media::correction_text::import_correction_text` with bytes, format, context, and metadata.
The CLI calls that same function.
Use `read_correction_report` and `export_correction_text` for validated report roundtrips.
Errors expose stable codes and optional text line numbers.
CLI success exits zero; failures exit two and emit JSON on stderr.

## Resource and YAML bounds

Source text is capped at 64 KiB, with 4096 bytes per line.
The importer permits 32 definitions, 32 pipeline steps, and 32 total named filter references.
Names span 128 bytes and cannot contain paths, substitutions, or controls.
Descriptions span 1024 bytes.
Graphs retain Core's existing node and edge limits.
JSON report input is capped at 256 KiB before deserialization.

YAML parsing uses pinned `yaml-rust2` 0.13.0 low-level events.
It does not invoke the whole-document loader or expand aliases.
The adapter caps parser events at 4096, nesting at twelve levels, and each decoded scalar at 1024 bytes.
It rejects aliases, anchors, tags, directives, merge keys, duplicate keys, and multiple documents.
Mappings require string keys.
Known parameter fields require their exact types.
This is a restricted filter importer, not general CamillaDSP configuration support.

## Format and dependency evidence

Checked on November 18, 2026.
The owned fixtures contain synthetic values in actual documented/exporter layouts.
They were not generated by running the vendor applications.
No third-party measurement corpus was copied.

REW documents text exports and equaliser-specific controls in its [file help](https://www.roomeqwizard.com/help/help_en-GB/html/file.html),
[equaliser help](https://www.roomeqwizard.com/help/help_en-GB/html/equaliser.html), and
[filter help](https://www.roomeqwizard.com/help/help_en-GB/html/eqfilters.html).
Its older DSP1124P PA/BW layout remains unsupported.

The [AutoEq exporter at revision 7ae0f56](https://github.com/jaakkopasanen/AutoEq/blob/7ae0f56d53074872b028649617a22bbb4232feb7/autoeq/frequency_response.py)
emits one preamp followed by PK, LSC, or HSC lines with frequency, gain, and Q.
Its exporter emits ON; the importer also preserves explicit APO-style OFF state.

The [CamillaDSP 4.1 guide at revision 05e9cfc](https://github.com/HEnquist/camilladsp/blob/05e9cfcdf43c0dfe078ed3feb8af4c8bd701fd74/README.md)
and [configuration definitions](https://github.com/HEnquist/camilladsp/blob/05e9cfcdf43c0dfe078ed3feb8af4c8bd701fd74/src/config/mod.rs)
define the imported filters, channel selections, and bypass behavior.

The selected [yaml-rust2 published source](https://github.com/Ethiraric/yaml-rust2/tree/e12069447073e0c8dd9ceab415159c4c92927534)
provides parser events without requiring its alias-expanding document loader.
The crate archive SHA-256 is `57e5b818a27a4cd30884ea380857a5e56f7ec3ba24a3990a3cc0b95af3238e18`.
Optional encoding support is disabled; the importer supplies checked UTF-8.
Its scanner bounds flow depth at 255; the adapter's lower depth and byte limits bound accepted work.
The scanner may allocate within the admitted source before an adapter event rejects.
This unit makes no process-RSS or hostile-input CPU benchmark claim.

The dependency adds arraydeque 0.5.1, hashlink 0.12.2, hashbrown 0.17.1, and foldhash 0.2.0.
Cargo.lock pins the complete selected graph and checksums.
yaml-rust2, hashlink, and hashbrown offer MIT or Apache-2.0.
arraydeque offers MIT/Apache-2.0; foldhash uses Zlib.
The canonical inventory records actual selected features and bundled license hashes.
The inventory now recognizes `MIT-LICENSE` and `Apache-LICENSE` alongside its existing filename conventions.
[Dependency inventory](dependency-lock.json) and [third-party notices](../THIRD_PARTY_NOTICES.txt) were regenerated with the canonical generator.
