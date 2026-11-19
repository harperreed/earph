// ABOUTME: Exercises the actual offline correction CLI and consumes its validated graph reports.
// ABOUTME: Checks source preservation, metadata roundtrips, hostile filenames, and precise rejection results.
use serde_json::Value;
use std::{
    fs,
    path::PathBuf,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};
fn directory() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!(
        "earph-correction-cli-{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(&path).unwrap();
    path
}
#[test]
fn real_cli_exports_and_reimports_graph_and_metadata_without_touching_the_source() {
    let dir = directory();
    let source = dir.join("owned correction; touch NEVER.txt");
    let original = include_bytes!("fixtures/correction/owned-autoeq.txt");
    fs::write(&source, original).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_earph-correction-import"))
        .args([
            "--format",
            "autoeq",
            "--rate",
            "48000",
            "--channels",
            "stereo",
            "--quantum",
            "64",
            "--model",
            "Owned fixture",
            "--provider",
            "Owned synthetic response",
            "--target",
            "Flat test target",
            "--parameter-set",
            "PK-only",
            "--revision",
            "fixture-1",
            "--license",
            "MIT test data",
            "--",
        ])
        .arg(&source)
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["status"], "validated");
    assert_eq!(report["metadata"]["headphone_model"], "Owned fixture");
    assert!(report.get("owner_id").is_none());
    assert!(report.get("output_consent").is_none());
    assert_eq!(fs::read(&source).unwrap(), original);
    assert!(!dir.join("NEVER.txt").exists());
    let file = dir.join("import-report.json");
    fs::write(&file, &output.stdout).unwrap();
    let checked = Command::new(env!("CARGO_BIN_EXE_earph-correction-import"))
        .arg("--validate-report")
        .arg(&file)
        .output()
        .unwrap();
    assert!(checked.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&checked.stdout).unwrap(),
        report
    );
    let exported = Command::new(env!("CARGO_BIN_EXE_earph-correction-import"))
        .arg("--export")
        .arg(&file)
        .output()
        .unwrap();
    assert!(exported.status.success());
    let imported = earph_media::correction_text::read_correction_report(&output.stdout).unwrap();
    let again = earph_media::correction_text::import_correction_text(
        &exported.stdout,
        imported.source.format,
        imported.context,
        imported.metadata.clone(),
    )
    .unwrap();
    assert_eq!(again.semantic_sha256, imported.semantic_sha256);
    assert_eq!(again.graph, imported.graph);
}

#[test]
fn real_cli_rejects_unsupported_shelf_slopes_and_never_reports_an_applied_graph() {
    let dir = directory();
    let source = dir.join("unsupported.txt");
    fs::write(
        &source,
        b"Preamp: -6 dB\nFilter 1: ON LSC 12 dB Fc 105 Hz Gain 3 dB\n",
    )
    .unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_earph-correction-import"))
        .args([
            "--format",
            "autoeq",
            "--rate",
            "48000",
            "--channels",
            "stereo",
            "--quantum",
            "64",
        ])
        .arg(&source)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "UNSUPPORTED_FILTER");
    assert!(!String::from_utf8_lossy(&output.stderr).contains(source.to_str().unwrap()));
}

#[test]
fn invalid_format_options_reject_before_the_source_is_opened() {
    let dir = directory();
    let source = dir.join("missing-file.txt");
    let output = Command::new(env!("CARGO_BIN_EXE_earph-correction-import"))
        .args(["--rate", "48000", "--channels", "stereo", "--quantum", "64"])
        .arg(source)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "USAGE");
}

#[test]
fn invalid_processing_context_rejects_before_the_source_is_opened() {
    let dir = directory();
    let output = Command::new(env!("CARGO_BIN_EXE_earph-correction-import"))
        .args([
            "--format",
            "autoeq",
            "--rate",
            "0",
            "--channels",
            "stereo",
            "--quantum",
            "64",
        ])
        .arg(dir.join("missing-file.txt"))
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "INVALID_CONTEXT");
}

#[test]
fn real_cli_graphs_drive_the_actual_dsp_consumer_for_all_three_formats() {
    use earph_core::{
        audio::{ChannelLayout, PcmFormat, PcmRepresentation},
        ids::Epochs,
    };
    use earph_dsp::{CompileContext, GraphBudget, PreparedGraph, ProcessingPolicy};
    let dir = directory();
    let pcm = PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Float64).unwrap();
    let alpha = (std::f64::consts::TAU * 1000.0 / 48000.0).sin() / 2.0;
    let amplitude = 10.0_f64.powf(-6.0 / 40.0);
    let peaking_first = (1.0 + alpha * amplitude) / (1.0 + alpha / amplitude);
    let preamp = 10.0_f64.powf(-6.0 / 20.0);
    let shelf_first = |high: bool, center: f64, q: f64, gain: f64| {
        let a = 10.0_f64.powf(gain / 40.0);
        let s = 1.0 / (std::f64::consts::PI * center / 48000.0).tan();
        let damping = a.sqrt() * s / q;
        if high {
            a * (a * s * s + damping + 1.0) / (s * s + damping + a)
        } else {
            a * (s * s + damping + a) / (a * s * s + damping + 1.0)
        }
    };
    let preset_first = 10.0_f64.powf(-18.0 / 20.0)
        * shelf_first(false, 105.0, 0.7, 6.0)
        * peaking_first
        * shelf_first(true, 8000.0, 1.2, -3.0);
    for (index, (format, source, expected)) in [
        (
            "autoeq",
            include_bytes!("fixtures/correction/owned-autoeq.txt").as_slice(),
            [preamp * peaking_first; 2],
        ),
        (
            "rew",
            include_bytes!("fixtures/correction/owned-rew.txt").as_slice(),
            [peaking_first; 2],
        ),
        (
            "camilladsp",
            include_bytes!("fixtures/correction/owned-camilladsp.yaml").as_slice(),
            [preamp * peaking_first, preamp],
        ),
        (
            "autoeq",
            include_bytes!("fixtures/correction/owned-autoeq-shelves.txt").as_slice(),
            [preset_first; 2],
        ),
        (
            "camilladsp",
            include_bytes!("fixtures/correction/owned-camilladsp-shelves.yaml").as_slice(),
            [preset_first; 2],
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let file = dir.join(format!("{format}-{index}.txt"));
        fs::write(&file, source).unwrap();
        let output = Command::new(env!("CARGO_BIN_EXE_earph-correction-import"))
            .args([
                "--format",
                format,
                "--rate",
                "48000",
                "--channels",
                "stereo",
                "--quantum",
                "64",
            ])
            .arg(&file)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report = earph_media::correction_text::read_correction_report(&output.stdout).unwrap();
        let plan = earph_dsp::compile(
            &report.graph,
            &CompileContext {
                input_format: pcm,
                output_format: pcm,
                quantum_frames: 64,
                policy: ProcessingPolicy::AllowProcessing,
                budget: GraphBudget::default(),
            },
        )
        .unwrap();
        let mut graph = PreparedGraph::prepare(plan, Epochs::default()).unwrap();
        let mut input = [0.0; 128];
        input[0] = 0.1;
        input[1] = 0.1;
        let mut rendered = [0.0; 128];
        graph.process(&input, &mut rendered).unwrap();
        for ch in 0..2 {
            assert!(
                (rendered[ch] - 0.1 * expected[ch]).abs() < 1e-14,
                "{format} channel {ch}"
            );
        }
        assert_eq!(fs::read(&file).unwrap(), source);
        let report_file = dir.join(format!("{format}-{index}-report.json"));
        fs::write(&report_file, &output.stdout).unwrap();
        let exported = Command::new(env!("CARGO_BIN_EXE_earph-correction-import"))
            .arg("--export")
            .arg(&report_file)
            .output()
            .unwrap();
        assert!(
            exported.status.success(),
            "{}",
            String::from_utf8_lossy(&exported.stderr)
        );
        let again = earph_media::correction_text::import_correction_text(
            &exported.stdout,
            report.source.format,
            report.context,
            report.metadata.clone(),
        )
        .unwrap();
        assert_eq!(again.graph, report.graph);
        assert_eq!(again.semantic_sha256, report.semantic_sha256);
    }
}

#[test]
fn cli_export_preserves_an_exact_limit_source_without_an_extra_byte() {
    use earph_core::audio::ChannelLayout;
    use earph_media::correction_text::{
        CorrectionFormat, ImportContext, ImportMetadata, MAX_CORRECTION_BYTES,
        export_correction_text, import_correction_text,
    };
    let source = |tabs: usize, plain: usize| {
        let mut source = String::from("filters:\n");
        for index in 0..32 {
            let count = tabs.saturating_sub(index * 1024).min(1024);
            source.push_str(&format!("  f{index}:\n    type: Gain\n    description: \"{}{}\"\n    parameters: {{gain: 0}}\n", "\t".repeat(count), if index==31 { "x".repeat(plain) } else { String::new() }));
        }
        source.push_str("pipeline: []\n");
        source
    };
    let context = ImportContext {
        sample_rate_hz: 48000,
        channels: ChannelLayout::Stereo,
        quantum_frames: 64,
    };
    let base = import_correction_text(
        source(0, 0).as_bytes(),
        CorrectionFormat::CamillaDsp,
        context,
        ImportMetadata::default(),
    )
    .unwrap();
    let difference = MAX_CORRECTION_BYTES - export_correction_text(&base).unwrap().len();
    let imported = import_correction_text(
        source(difference / 2, difference % 2).as_bytes(),
        CorrectionFormat::CamillaDsp,
        context,
        ImportMetadata::default(),
    )
    .unwrap();
    let expected = export_correction_text(&imported).unwrap();
    assert_eq!(expected.len(), MAX_CORRECTION_BYTES);
    let dir = directory();
    let report = dir.join("exact-report.json");
    fs::write(&report, serde_json::to_vec(&imported).unwrap()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_earph-correction-import"))
        .arg("--export")
        .arg(&report)
        .current_dir(&dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        output.stdout.len(),
        MAX_CORRECTION_BYTES,
        "CLI added bytes after export admission"
    );
    assert_eq!(output.stdout, expected.as_bytes());
    let again = import_correction_text(
        &output.stdout,
        CorrectionFormat::CamillaDsp,
        context,
        imported.metadata,
    )
    .unwrap();
    assert_eq!(again.semantic_sha256, imported.semantic_sha256);
}

#[test]
fn exported_source_delivery_failure_reports_the_existing_cli_error() {
    use earph_core::audio::ChannelLayout;
    use earph_media::correction_text::{
        CorrectionFormat, ImportContext, ImportMetadata, import_correction_text,
    };
    use std::{
        os::{fd::OwnedFd, unix::net::UnixStream},
        process::Stdio,
    };
    let report=import_correction_text(b"filters: {gain: {type: Gain, parameters: {gain: -6}}}\npipeline: [{type: Filter, names: [gain]}]\n",
        CorrectionFormat::CamillaDsp, ImportContext { sample_rate_hz:48000,channels:ChannelLayout::Stereo,quantum_frames:64 },ImportMetadata::default()).unwrap();
    let dir = directory();
    let path = dir.join("report.json");
    fs::write(&path, serde_json::to_vec(&report).unwrap()).unwrap();
    let (consumer, writer) = UnixStream::pair().unwrap();
    drop(consumer);
    let output = Command::new(env!("CARGO_BIN_EXE_earph-correction-import"))
        .arg("--export")
        .arg(&path)
        .current_dir(&dir)
        .stdout(Stdio::from(OwnedFd::from(writer)))
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let error: Value = serde_json::from_slice(&output.stderr).unwrap();
    assert_eq!(error["error"]["code"], "OUTPUT_UNAVAILABLE");
}

fn child_file_limit(command: &mut Command, bytes: u64) {
    use std::os::unix::process::CommandExt;
    // SAFETY: Only the child calls the resource-limit syscalls, without allocation or signal changes.
    unsafe {
        command.pre_exec(move || {
            let mut limit = rustix::process::getrlimit(rustix::process::Resource::Fsize);
            limit.current = Some(bytes);
            rustix::process::setrlimit(rustix::process::Resource::Fsize, limit)
                .map_err(std::io::Error::from)
        });
    }
}

#[test]
fn actual_cli_file_limit_returns_output_error_and_preserves_the_kernel_limit() {
    use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt, process::Stdio};
    let dir = directory();
    let path = dir.join("stdout.txt");
    let out = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_earph-correction-import"));
    command
        .arg("--help")
        .stdout(Stdio::from(out))
        .current_dir(&dir);
    child_file_limit(&mut command, 1);
    let result = command.output().unwrap();
    assert_eq!(
        result.status.code(),
        Some(2),
        "file limit terminated correction CLI: {:?}",
        result.status
    );
    let error: Value = serde_json::from_slice(&result.stderr).unwrap();
    assert_eq!(error["error"]["code"], "OUTPUT_UNAVAILABLE");
    assert_eq!(fs::metadata(path).unwrap().len(), 1);
}

#[test]
fn failed_error_delivery_keeps_correction_cli_exit_two_without_panicking() {
    use std::{
        os::{fd::OwnedFd, unix::net::UnixStream},
        process::Stdio,
    };
    let dir = directory();
    let (consumer, writer) = UnixStream::pair().unwrap();
    drop(consumer);
    let result = Command::new(env!("CARGO_BIN_EXE_earph-correction-import"))
        .arg("--unknown")
        .current_dir(dir)
        .stderr(Stdio::from(OwnedFd::from(writer)))
        .output()
        .unwrap();
    assert_eq!(
        result.status.code(),
        Some(2),
        "failed stderr changed correction CLI exit: {:?}",
        result.status
    );
}

#[test]
fn zero_limit_on_error_stderr_keeps_correction_cli_exit_two() {
    use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt, process::Stdio};
    let dir = directory();
    let path = dir.join("stderr.json");
    let err = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&path)
        .unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_earph-correction-import"));
    command
        .arg("--unknown")
        .stderr(Stdio::from(err))
        .current_dir(&dir);
    child_file_limit(&mut command, 0);
    let result = command.output().unwrap();
    assert_eq!(
        result.status.code(),
        Some(2),
        "stderr limit terminated correction CLI: {:?}",
        result.status
    );
    assert_eq!(fs::metadata(path).unwrap().len(), 0);
}
