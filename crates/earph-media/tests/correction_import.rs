// ABOUTME: Verifies real correction imports through typed graphs and independent signal consumers.
// ABOUTME: Preserves source parameters, preamp, channel choices, provenance, and disabled filter policy.
use earph_core::{audio::ChannelLayout, graph::NodeKind};
use earph_media::correction_text::{
    CorrectionFormat, ImportContext, ImportMetadata, export_correction_text, import_correction_text,
};

fn context() -> ImportContext {
    ImportContext {
        sample_rate_hz: 48000,
        channels: ChannelLayout::Stereo,
        quantum_frames: 64,
    }
}
const AUTOEQ: &str = "Preamp: -6.0 dB\nFilter 1: ON PK Fc 1000 Hz Gain -6.0 dB Q 1.00\nFilter 2: OFF PK Fc 200 Hz Gain 3.0 dB Q 2.00\n";

#[test]
fn imports_exact_peaking_parameters_and_preamp_without_granting_volume_authority() {
    let imported = import_correction_text(
        AUTOEQ.as_bytes(),
        CorrectionFormat::AutoEq,
        context(),
        ImportMetadata::default(),
    )
    .unwrap();
    assert_eq!(imported.preamp_db, -6.0);
    assert_eq!(imported.definitions.len(), 2);
    assert_eq!(imported.pipeline.len(), 2);
    assert!(imported.pipeline[1].bypassed);
    assert_eq!(imported.source.bytes, AUTOEQ.len());
    assert_eq!(imported.source.sha256.len(), 64);
    assert_eq!(
        imported
            .graph
            .nodes
            .iter()
            .filter(|n| matches!(n.kind, NodeKind::Peq { .. }))
            .count(),
        1
    );
    let output = export_correction_text(&imported).unwrap();
    let roundtrip = import_correction_text(
        output.as_bytes(),
        CorrectionFormat::AutoEq,
        context(),
        ImportMetadata::default(),
    )
    .unwrap();
    assert_eq!(imported.semantic_sha256, roundtrip.semantic_sha256);
    assert_eq!(imported.graph, roundtrip.graph);
}

#[test]
fn rejects_source_shelves_instead_of_changing_their_q_or_slope() {
    let source = "Preamp: -6 dB\nFilter 1: ON LSC Fc 105 Hz Gain 3 dB Q 0.70\n";
    let error = import_correction_text(
        source.as_bytes(),
        CorrectionFormat::AutoEq,
        context(),
        ImportMetadata::default(),
    )
    .unwrap_err();
    assert_eq!(error.code, "UNSUPPORTED_FILTER");
}

#[test]
fn real_format_layouts_roundtrip_without_losing_disabled_or_channel_semantics() {
    for (format, source) in [
        (
            CorrectionFormat::Rew,
            include_str!("fixtures/correction/owned-rew.txt"),
        ),
        (
            CorrectionFormat::AutoEq,
            include_str!("fixtures/correction/owned-autoeq.txt"),
        ),
        (
            CorrectionFormat::CamillaDsp,
            include_str!("fixtures/correction/owned-camilladsp.yaml"),
        ),
    ] {
        let metadata = ImportMetadata {
            headphone_model: Some("Owned fixture".into()),
            measurement_provider: Some("Owned synthetic response".into()),
            target: Some("Flat test target".into()),
            parameter_set: Some("PK-only".into()),
            revision: Some("fixture-1".into()),
            redistribution_license: Some("MIT test data".into()),
        };
        let imported =
            import_correction_text(source.as_bytes(), format, context(), metadata.clone()).unwrap();
        let exported = export_correction_text(&imported).unwrap();
        let reimported =
            import_correction_text(exported.as_bytes(), format, context(), metadata).unwrap();
        assert_eq!(imported.semantic_sha256, reimported.semantic_sha256);
        assert_eq!(imported.graph, reimported.graph);
        assert_ne!(imported.source.sha256, reimported.source.sha256);
        let report = serde_json::to_vec(&imported).unwrap();
        assert_eq!(
            earph_media::correction_text::read_correction_report(&report).unwrap(),
            imported
        );
    }
}

fn measured_gain(graph: &earph_core::graph::GraphDocument, channel: usize, frequency: f64) -> f64 {
    use earph_core::{
        audio::{PcmFormat, PcmRepresentation},
        ids::Epochs,
    };
    use earph_dsp::{CompileContext, GraphBudget, PreparedGraph, ProcessingPolicy};
    let format = PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Float64).unwrap();
    let plan = earph_dsp::compile(
        graph,
        &CompileContext {
            input_format: format,
            output_format: format,
            quantum_frames: 64,
            policy: ProcessingPolicy::AllowProcessing,
            budget: GraphBudget::default(),
        },
    )
    .unwrap();
    let mut prepared = PreparedGraph::prepare(plan, Epochs::default()).unwrap();
    let (mut squared_input, mut squared_output) = (0.0, 0.0);
    for start in (0..9600).step_by(64) {
        let mut input = [0.0; 128];
        for frame in 0..64 {
            input[2 * frame + channel] =
                0.1 * (std::f64::consts::TAU * frequency * (start + frame) as f64 / 48000.0).sin();
        }
        let mut output = [0.0; 128];
        prepared.process(&input, &mut output).unwrap();
        if start >= 4800 {
            for frame in 0..64 {
                squared_input += input[2 * frame + channel].powi(2);
                squared_output += output[2 * frame + channel].powi(2);
                assert!(output[2 * frame + 1 - channel].abs() < 1e-15);
            }
        }
    }
    10.0 * (squared_output / squared_input).log10()
}

#[test]
fn actual_dsp_consumers_preserve_preamp_and_asymmetric_camilla_channels() {
    let auto = import_correction_text(
        include_bytes!("fixtures/correction/owned-autoeq.txt"),
        CorrectionFormat::AutoEq,
        context(),
        ImportMetadata::default(),
    )
    .unwrap();
    assert!((measured_gain(&auto.graph, 0, 1000.0) - (-12.0)).abs() < 1e-8);
    let rew = import_correction_text(
        include_bytes!("fixtures/correction/owned-rew.txt"),
        CorrectionFormat::Rew,
        context(),
        ImportMetadata::default(),
    )
    .unwrap();
    assert!((measured_gain(&rew.graph, 0, 1000.0) - (-6.0)).abs() < 1e-8);
    let camilla = import_correction_text(
        include_bytes!("fixtures/correction/owned-camilladsp.yaml"),
        CorrectionFormat::CamillaDsp,
        context(),
        ImportMetadata::default(),
    )
    .unwrap();
    assert!((measured_gain(&camilla.graph, 0, 1000.0) - (-12.0)).abs() < 1e-8);
    assert!((measured_gain(&camilla.graph, 1, 1000.0) - (-6.0)).abs() < 1e-8);
    assert!(camilla.pipeline[2].bypassed);
    assert_eq!(camilla.definitions.len(), 3);
}

#[test]
fn yaml_rejects_ambiguous_or_executable_constructs_and_unsupported_active_controls() {
    let fixtures = [
        "filters: &defs {}\npipeline: []\n",
        "filters: !!map {}\npipeline: []\n",
        "filters: {<<: {bad: value}}\npipeline: []\n",
        "filters: {}\nfilters: {}\npipeline: []\n",
        "%YAML 1.2\n---\nfilters: {}\npipeline: []\n",
        "filters: {}\npipeline: []\n---\nfilters: {}\npipeline: []\n",
        "filters: {}\npipeline: [{type: Mixer, name: crossfeed}]\n",
        "filters: {shelf: {type: Biquad, parameters: {type: Lowshelf, freq: 100, gain: 3, q: 0.7}}}\npipeline: []\n",
        "filters: {peak: {type: Biquad, parameters: {type: Peaking, freq: 100, gain: 3, bandwidth: 1}}}\npipeline: [{type: Filter, names: [peak]}]\n",
        "filters: {fir: {type: Conv, parameters: {filename: /private/not-read.wav}}}\npipeline: []\n",
        "filters: {}\npipeline: []\ndevices: {samplerate: 96000}\n",
    ];
    for source in fixtures {
        assert!(
            import_correction_text(
                source.as_bytes(),
                CorrectionFormat::CamillaDsp,
                context(),
                ImportMetadata::default()
            )
            .is_err(),
            "{source}"
        );
    }
    let base = include_str!("fixtures/correction/owned-camilladsp.yaml");
    for channels in ["[2]", "[0, 0]", "[0, 1, 2]"] {
        let source = base.replace("channels: [0]", &format!("channels: {channels}"));
        assert_eq!(
            import_correction_text(
                source.as_bytes(),
                CorrectionFormat::CamillaDsp,
                context(),
                ImportMetadata::default()
            )
            .unwrap_err()
            .code,
            "INVALID_CHANNELS"
        );
    }
}

#[test]
fn imported_reports_reject_graph_tampering_and_unknown_fields() {
    let imported = import_correction_text(
        AUTOEQ.as_bytes(),
        CorrectionFormat::AutoEq,
        context(),
        ImportMetadata::default(),
    )
    .unwrap();
    let mut value = serde_json::to_value(&imported).unwrap();
    value["graph"]["nodes"][1]["kind"]["gain_linear"] = serde_json::json!(1.0);
    assert!(
        earph_media::correction_text::read_correction_report(&serde_json::to_vec(&value).unwrap())
            .is_err()
    );
    let mut value = serde_json::to_value(&imported).unwrap();
    value["apply"] = serde_json::json!(true);
    assert!(
        earph_media::correction_text::read_correction_report(&serde_json::to_vec(&value).unwrap())
            .is_err()
    );
}

#[test]
fn reports_cannot_relabel_camilla_gain_controls_as_autoeq_text() {
    let mut imported = import_correction_text(
        include_bytes!("fixtures/correction/owned-camilladsp.yaml"),
        CorrectionFormat::CamillaDsp,
        context(),
        ImportMetadata::default(),
    )
    .unwrap();
    imported.source.format = CorrectionFormat::AutoEq;
    assert_eq!(
        earph_media::correction_text::read_correction_report(
            &serde_json::to_vec(&imported).unwrap()
        )
        .unwrap_err()
        .code,
        "INVALID_REPORT"
    );
}

#[test]
fn text_rejects_missing_filters_with_a_precise_diagnostic() {
    let error = import_correction_text(
        b"Preamp: 0 dB\n",
        CorrectionFormat::AutoEq,
        context(),
        ImportMetadata::default(),
    )
    .unwrap_err();
    assert_eq!(error.code, "NO_FILTERS");
}

#[test]
fn bounds_source_size_filter_count_numeric_values_and_untrusted_directives() {
    use earph_media::correction_text::MAX_CORRECTION_BYTES;
    let oversized = vec![b'#'; MAX_CORRECTION_BYTES + 1];
    assert_eq!(
        import_correction_text(
            &oversized,
            CorrectionFormat::Rew,
            context(),
            ImportMetadata::default()
        )
        .unwrap_err()
        .code,
        "RESOURCE_LIMIT"
    );
    let oversized_invalid = vec![0xff; MAX_CORRECTION_BYTES + 1];
    assert_eq!(
        import_correction_text(
            &oversized_invalid,
            CorrectionFormat::Rew,
            context(),
            ImportMetadata::default()
        )
        .unwrap_err()
        .code,
        "RESOURCE_LIMIT"
    );
    for source in [
        "Preamp: NaN dB\nFilter 1: ON PK Fc 1000 Hz Gain -6 dB Q 1\n",
        "Preamp: 0 dB\nFilter 1: ON PK Fc 1000 Hz Gain 99 dB Q 1\n",
        "Preamp: 0 dB\nFilter 1: ON PK Fc 1000 Hz Gain -6 dB Q 0\n",
        "Preamp: 0 dB\nInclude: /private/not-read.txt\n",
        "Preamp: 0 dB\nFilter 1: ON PK Fc 1000 Hz Gain -6 dB Q 1\nFilter 1: ON PK Fc 200 Hz Gain 3 dB Q 1\n",
    ] {
        assert!(
            import_correction_text(
                source.as_bytes(),
                CorrectionFormat::AutoEq,
                context(),
                ImportMetadata::default()
            )
            .is_err()
        );
    }
    let mut source = String::from("Preamp: 0 dB\n");
    for i in 1..=33 {
        source.push_str(&format!("Filter {i}: ON PK Fc 1000 Hz Gain 0 dB Q 1\n"));
    }
    assert_eq!(
        import_correction_text(
            source.as_bytes(),
            CorrectionFormat::AutoEq,
            context(),
            ImportMetadata::default()
        )
        .unwrap_err()
        .code,
        "RESOURCE_LIMIT"
    );
}

#[test]
fn rew_metadata_slots_do_not_swallow_malformed_processing() {
    let source = "Filter Settings file\nRoom EQ V5.20.14\nEqualiser: Generic\nPreamp -6 dB\nFilter 1: ON PK Fc 1000 Hz Gain -6 dB Q 1\n";
    assert!(
        import_correction_text(
            source.as_bytes(),
            CorrectionFormat::Rew,
            context(),
            ImportMetadata::default()
        )
        .is_err()
    );
}

#[test]
fn rew_version_metadata_cannot_capture_a_private_source_path() {
    let source = "Filter Settings file\nRoom EQ V/private/build\nEqualiser: Generic\nFilter 1: ON PK Fc 1000 Hz Gain -6 dB Q 1\n";
    assert!(
        import_correction_text(
            source.as_bytes(),
            CorrectionFormat::Rew,
            context(),
            ImportMetadata::default()
        )
        .is_err()
    );
}

#[test]
fn yaml_special_nonfinite_numbers_have_a_precise_diagnostic() {
    let source = include_str!("fixtures/correction/owned-camilladsp.yaml")
        .replace("gain: -6.0", "gain: .nan");
    assert_eq!(
        import_correction_text(
            source.as_bytes(),
            CorrectionFormat::CamillaDsp,
            context(),
            ImportMetadata::default()
        )
        .unwrap_err()
        .code,
        "NONFINITE_VALUE"
    );
}

#[test]
fn yaml_parser_enforces_structure_budgets_before_format_decoding() {
    for source in [
        format!(
            "filters: {{}}\npipeline: {}0{}\n",
            "[".repeat(14),
            "]".repeat(14)
        ),
        format!("filters: {{}}\npipeline: [{}]\n", vec!["0"; 4100].join(",")),
        format!(
            "filters: {{}}\npipeline: []\ndescription: '{}'\n",
            "x".repeat(1025)
        ),
    ] {
        assert_eq!(
            import_correction_text(
                source.as_bytes(),
                CorrectionFormat::CamillaDsp,
                context(),
                ImportMetadata::default()
            )
            .unwrap_err()
            .code,
            "RESOURCE_LIMIT"
        );
    }
    for source in [
        "filters: *missing\npipeline: []\n",
        "filters: {a: &tag {type: Gain, parameters: {gain: 0}}}\npipeline: []\n",
    ] {
        assert!(
            import_correction_text(
                source.as_bytes(),
                CorrectionFormat::CamillaDsp,
                context(),
                ImportMetadata::default()
            )
            .is_err()
        );
    }
}

#[test]
fn camilla_gain_controls_and_empty_channels_have_exact_signal_semantics() {
    let source = "filters:\n  flip: {type: Gain, parameters: {gain: 0.25, scale: linear, inverted: true}}\n  quiet: {type: Gain, parameters: {gain: 10, scale: linear, mute: true}}\npipeline:\n  - {type: Filter, channels: [], names: [quiet]}\n  - {type: Filter, channels: [0], names: [flip, flip]}\n  - {type: Filter, channels: [1], names: [quiet]}\n";
    let imported = import_correction_text(
        source.as_bytes(),
        CorrectionFormat::CamillaDsp,
        context(),
        ImportMetadata::default(),
    )
    .unwrap();
    assert!((measured_gain(&imported.graph, 0, 430.0) - 20.0 * 0.0625_f64.log10()).abs() < 1e-9);
    assert_eq!(measured_gain(&imported.graph, 1, 430.0), f64::NEG_INFINITY);
    assert_eq!(imported.pipeline[1].names, ["flip", "flip"]);
    assert!(imported.pipeline[0].channels.is_empty());
}

#[test]
fn peaking_response_matches_an_independent_transfer_function_away_from_center() {
    let imported = import_correction_text(
        AUTOEQ.as_bytes(),
        CorrectionFormat::AutoEq,
        context(),
        ImportMetadata::default(),
    )
    .unwrap();
    let omega = std::f64::consts::TAU * 1000.0 / 48000.0;
    let amplitude = 10.0_f64.powf(-6.0 / 40.0);
    let alpha = omega.sin() / 2.0;
    let numerator = [
        1.0 + alpha * amplitude,
        -2.0 * omega.cos(),
        1.0 - alpha * amplitude,
    ];
    let denominator = [
        1.0 + alpha / amplitude,
        -2.0 * omega.cos(),
        1.0 - alpha / amplitude,
    ];
    for frequency in [250.0, 3000.0] {
        let angle = std::f64::consts::TAU * frequency / 48000.0;
        let power = |coefficients: [f64; 3]| {
            let real = coefficients[0]
                + coefficients[1] * angle.cos()
                + coefficients[2] * (2.0 * angle).cos();
            let imaginary = -coefficients[1] * angle.sin() - coefficients[2] * (2.0 * angle).sin();
            real * real + imaginary * imaginary
        };
        let expected = -6.0 + 10.0 * (power(numerator) / power(denominator)).log10();
        assert!((measured_gain(&imported.graph, 0, frequency) - expected).abs() < 1e-8);
    }
}

#[test]
fn export_rejects_an_encoding_larger_than_the_source_admission_limit() {
    let mut source = String::from("filters:\n");
    for index in 0..32 {
        source.push_str(&format!(
            "  f{index}:\n    type: Gain\n    description: \"{}\"\n    parameters: {{gain: 0}}\n",
            "\t".repeat(1024)
        ));
    }
    source.push_str("pipeline: []\n");
    let imported = import_correction_text(
        source.as_bytes(),
        CorrectionFormat::CamillaDsp,
        context(),
        ImportMetadata::default(),
    )
    .unwrap();
    assert_eq!(
        export_correction_text(&imported).unwrap_err().code,
        "RESOURCE_LIMIT"
    );
}
