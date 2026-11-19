// ABOUTME: Exercises exact correction previews using shared parsers and owned source bytes.
// ABOUTME: Verifies worker cancellation and explicit source reconstruction without an owner process.
use earph_core::{
    audio::ChannelLayout, dsp_nodes::FirPurpose, graph::FiniteGraphScalar,
    profile_control::ProfileCorrectionInput,
};
use earph_desktop::gui::{
    CorrectionPreview, CorrectionPreviewRequest, CorrectionSource, CorrectionWorker,
};
use earph_media::correction_text::{
    CorrectionFormat, ImportContext, ImportMetadata, read_correction_report,
};
use std::time::{Duration, Instant};

fn request(source: CorrectionSource) -> CorrectionPreviewRequest {
    CorrectionPreviewRequest {
        source,
        context: ImportContext {
            sample_rate_hz: 48000,
            channels: ChannelLayout::Stereo,
            quantum_frames: 256,
        },
        metadata: ImportMetadata::default(),
        fir_preamp: None,
    }
}
fn finish(worker: &mut CorrectionWorker) -> Result<CorrectionPreview, String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(result) = worker.poll() {
            return result;
        }
        assert!(Instant::now() < deadline, "bounded preview did not retire");
        std::thread::yield_now();
    }
}
#[test]
fn exact_text_bytes_and_null_metadata_reach_the_owner_input_for_each_dialect() {
    for (format, source) in [
        (
            CorrectionFormat::Rew,
            include_str!("../../earph-media/tests/fixtures/correction/owned-rew.txt"),
        ),
        (
            CorrectionFormat::AutoEq,
            include_str!("../../earph-media/tests/fixtures/correction/owned-autoeq.txt"),
        ),
        (
            CorrectionFormat::CamillaDsp,
            include_str!("../../earph-media/tests/fixtures/correction/owned-camilladsp.yaml"),
        ),
    ] {
        let mut worker = CorrectionWorker::start(request(CorrectionSource::Text {
            text: source.into(),
            format,
        }))
        .unwrap();
        let preview = finish(&mut worker).unwrap();
        let ProfileCorrectionInput::TextReport {
            report_json,
            source_text,
        } = preview.input()
        else {
            panic!("text input required")
        };
        assert_eq!(source_text.as_bytes(), source.as_bytes());
        let report = read_correction_report(report_json.as_bytes()).unwrap();
        assert_eq!(report.source.bytes, source.len());
        assert_eq!(report.metadata, ImportMetadata::default());
        assert!(worker.retired());
        assert!(!report.definitions.is_empty());
    }
}
#[test]
fn cancellation_discards_late_results_and_joins_before_retirement() {
    let mut worker = CorrectionWorker::start(request(CorrectionSource::Text {
        text: "Preamp: -6 dB\nFilter 1: ON PK Fc 100 Hz Gain 2 dB Q 1\n".into(),
        format: CorrectionFormat::AutoEq,
    }))
    .unwrap();
    worker.cancel();
    assert!(finish(&mut worker).unwrap_err().contains("cancel"));
    assert!(worker.retired());
}
#[test]
fn invalid_source_and_oversize_never_produce_partial_reports() {
    for text in [
        "Preamp: NaN dB\n".into(),
        "x".repeat(65537),
        "Preamp: -6 dB\nFilter 1: ON LS Fc 10 Hz Gain 2 dB Q 1\n".into(),
    ] {
        let mut worker = CorrectionWorker::start(request(CorrectionSource::Text {
            text,
            format: CorrectionFormat::AutoEq,
        }))
        .unwrap();
        assert!(finish(&mut worker).is_err());
    }
}
#[test]
fn report_reconstruction_is_an_explicit_new_source_with_new_hash() {
    let source = "# public owned source\nPreamp: -6 dB\nFilter 1: ON PK Fc 100 Hz Gain 2 dB Q 1\n";
    let mut worker = CorrectionWorker::start(request(CorrectionSource::Text {
        text: source.into(),
        format: CorrectionFormat::AutoEq,
    }))
    .unwrap();
    let original = finish(&mut worker).unwrap();
    let ProfileCorrectionInput::TextReport { report_json, .. } = original.input() else {
        panic!()
    };
    let reconstructed = CorrectionPreviewRequest::from_text_report(report_json.as_bytes()).unwrap();
    let mut worker = CorrectionWorker::start(reconstructed).unwrap();
    let exported = finish(&mut worker).unwrap();
    assert_ne!(original.source_sha256(), exported.source_sha256());
    let (CorrectionPreview::Text { report: a, .. }, CorrectionPreview::Text { report: b, .. }) =
        (&original, &exported)
    else {
        panic!()
    };
    assert_eq!(a.graph, b.graph);
    assert_eq!(a.preamp_db, b.preamp_db);
}
fn wav(channels: u16) -> Vec<u8> {
    let data: Vec<u8> = (0..channels).flat_map(|_| 0.5_f32.to_le_bytes()).collect();
    let mut bytes = Vec::new();
    bytes.extend(b"RIFF");
    bytes.extend((36 + data.len() as u32).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(3u16.to_le_bytes());
    bytes.extend(channels.to_le_bytes());
    bytes.extend(48000u32.to_le_bytes());
    bytes.extend((48000 * u32::from(channels) * 4).to_le_bytes());
    bytes.extend((channels * 4).to_le_bytes());
    bytes.extend(32u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend((data.len() as u32).to_le_bytes());
    bytes.extend(data);
    bytes
}
#[test]
fn fir_preview_preserves_owned_wav_and_rejects_geometry_and_missing_preamp() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/desktop-correction-workflows/fixtures");
    std::fs::create_dir_all(&root).unwrap();
    for channels in [1, 2] {
        let path = root.join(format!("owned-{}-{channels}.wav", std::process::id()));
        let bytes = wav(channels);
        std::fs::write(&path, &bytes).unwrap();
        let mut r = request(CorrectionSource::Fir {
            path: path.to_str().unwrap().into(),
            purpose: FirPurpose::Correction,
        });
        r.context.channels = if channels == 1 {
            ChannelLayout::Mono
        } else {
            ChannelLayout::Stereo
        };
        let mut worker = CorrectionWorker::start(r.clone()).unwrap();
        assert!(finish(&mut worker).is_err());
        r.fir_preamp = Some(FiniteGraphScalar::new(-6.0).unwrap());
        let mut worker = CorrectionWorker::start(r.clone()).unwrap();
        let preview = finish(&mut worker).unwrap();
        assert!(
            matches!(preview.input(), ProfileCorrectionInput::Fir { preamp_db, .. } if preamp_db.get() == -6.0)
        );
        r.context.sample_rate_hz = 44100;
        let mut worker = CorrectionWorker::start(r).unwrap();
        assert!(finish(&mut worker).is_err());
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
}

#[test]
fn text_file_preview_preserves_exact_bytes_and_rejects_invalid_encoding_or_size() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/desktop-correction-workflows/fixtures")
        .join(format!(
            "text-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    std::fs::create_dir_all(&root).unwrap();
    for (index, bytes) in [
        b"Preamp: -6 dB\r\nFilter 1: ON PK Fc 100 Hz Gain 2 dB Q 1\r\n".to_vec(),
        vec![0xff, 0xfe],
        vec![b'x'; 65537],
    ]
    .into_iter()
    .enumerate()
    {
        let path = root.join(format!("owned-{index}.txt"));
        std::fs::write(&path, &bytes).unwrap();
        let mut worker = CorrectionWorker::start(request(CorrectionSource::TextFile {
            path: path.to_str().unwrap().into(),
            format: CorrectionFormat::AutoEq,
        }))
        .unwrap();
        let result = finish(&mut worker);
        if index == 0 {
            let ProfileCorrectionInput::TextReport { source_text, .. } = result.unwrap().input()
            else {
                panic!()
            };
            assert_eq!(source_text.as_bytes(), bytes);
        } else {
            assert!(result.is_err());
        }
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }
    let mut worker = CorrectionWorker::start(request(CorrectionSource::TextFile {
        path: root.to_str().unwrap().into(),
        format: CorrectionFormat::AutoEq,
    }))
    .unwrap();
    assert!(finish(&mut worker).is_err());
}
