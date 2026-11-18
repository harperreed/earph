// ABOUTME: Renders owned local WAV sources through the real offline graph executable.
// ABOUTME: Compares captures with standalone kernels and preserves failure artifacts.
use earph_core::{audio::ChannelLayout, dsp_nodes::SrcPreset};
use earph_dsp::resample::PreparedSrc;
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
mod support;

fn directory() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/offline-graph-tests")
        .join(format!(
            "{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    fs::create_dir_all(&path).unwrap();
    path
}
fn wave(path: &Path, rate: u32, values: &[i16]) {
    wave_channels(path, rate, 1, values);
}
fn wave_channels(path: &Path, rate: u32, channels: u16, values: &[i16]) {
    let len = (values.len() * 2) as u32;
    let mut bytes = Vec::new();
    bytes.extend(b"RIFF");
    bytes.extend((36 + len).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend(1u16.to_le_bytes());
    bytes.extend(channels.to_le_bytes());
    bytes.extend(rate.to_le_bytes());
    bytes.extend((rate * 2 * u32::from(channels)).to_le_bytes());
    bytes.extend((2 * channels).to_le_bytes());
    bytes.extend(16u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(len.to_le_bytes());
    for value in values {
        bytes.extend(value.to_le_bytes());
    }
    fs::write(path, bytes).unwrap();
}
fn graph(rates: &[u32]) -> Value {
    let port = |rate| json!({"domain":"pcm","sample_rate_hz":rate,"channels":"mono","precision":"float64","max_quantum_frames":512});
    let mut nodes = vec![json!({"id":"in","kind":{"kind":"input"},"format":port(rates[0])})];
    let mut edges = Vec::new();
    let mut previous = "in".to_string();
    for (index, pair) in rates.windows(2).enumerate() {
        let id = format!("src{index}");
        nodes.push(json!({"id":id,"kind":{"kind":"src","input_rate_hz":pair[0],"preset":"responsive","chunk_frames":64},"format":port(pair[1])}));
        edges.push(json!({"from":previous,"to":id,"from_port":0,"to_port":0}));
        previous = id;
    }
    nodes.push(json!({"id":"out","kind":{"kind":"output"},"format":port(*rates.last().unwrap())}));
    edges.push(json!({"from":previous,"to":"out","from_port":0,"to_port":0}));
    json!({"schema_version":1,"nodes":nodes,"edges":edges,"output":"out"})
}
fn run(dir: &Path, graph: &Value, files: &[PathBuf], format: &str) -> std::process::Output {
    run_with_runtime(dir, graph, files, format, None)
}
fn run_with_runtime(
    dir: &Path,
    graph: &Value,
    files: &[PathBuf],
    format: &str,
    runtime: Option<&Path>,
) -> std::process::Output {
    fs::create_dir_all(dir).unwrap();
    let graph_path = dir.join("graph.json");
    fs::write(&graph_path, serde_json::to_vec(graph).unwrap()).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_earph-engine"));
    command
        .arg("render-graph")
        .arg("--graph")
        .arg(graph_path)
        .arg("--output")
        .arg(dir.join("capture"))
        .args([
            "--sample-format",
            format,
            "--gain-db",
            "0",
            "--ramp-frames",
            "0",
            "--quantum",
            "64",
        ]);
    if let Some(runtime) = runtime {
        command.arg("--runtime").arg(runtime);
    }
    command.arg("--").args(files).output().unwrap()
}
fn convert(input: &[f64], input_rate: u32, output_rate: u32) -> Vec<f64> {
    let mut src = PreparedSrc::prepare(
        input_rate,
        output_rate,
        ChannelLayout::Mono,
        64,
        SrcPreset::Responsive,
        16 << 20,
    )
    .unwrap();
    let mut output = [0.0; 37];
    let mut result = Vec::new();
    let mut offset = 0;
    while offset < input.len() {
        let step = src.process(&input[offset..], &mut output).unwrap();
        offset += step.consumed_frames;
        result.extend_from_slice(&output[..step.produced_frames]);
    }
    src.begin_finish().unwrap();
    loop {
        let step = src.finish(&mut output).unwrap();
        result.extend_from_slice(&output[..step.produced_frames]);
        if step.terminal {
            return result;
        }
    }
}
fn packed(values: &[f64], bits: u32) -> Vec<u8> {
    let reference = 2f64.powi(bits as i32 - 1);
    let mut result = Vec::new();
    for value in values {
        let sample = (value * reference)
            .round()
            .clamp(-reference, reference - 1.0) as i64;
        result.extend_from_slice(&sample.to_le_bytes()[..(bits / 8) as usize]);
    }
    result
}

#[test]
fn unequal_rate_capture_matches_real_standalone_src_for_empty_and_chunk_boundaries() {
    for len in [0, 1, 63, 64, 65] {
        let dir = directory();
        let source = dir.join("owned; source.wav");
        let mut input = vec![0i16; len];
        if let Some(last) = input.last_mut() {
            *last = 8192;
        }
        wave(&source, 48000, &input);
        let original = fs::read(&source).unwrap();
        let output = run(
            &dir,
            &graph(&[48000, 44100]),
            std::slice::from_ref(&source),
            "signed24",
        );
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["status"], "pass");
        assert_eq!(report["source_frames"], len);
        assert_eq!(report["partial_frame_bytes"], 0);
        let normalized: Vec<_> = input.iter().map(|x| f64::from(*x) / 32768.0).collect();
        let expected = packed(&convert(&normalized, 48000, 44100), 24);
        assert_eq!(fs::read(dir.join("capture/capture.pcm")).unwrap(), expected);
        assert_eq!(fs::read(source).unwrap(), original);
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(dir.join("capture/report.json")).unwrap())
                .unwrap(),
            report
        );
    }
}

#[test]
fn declared_asset_payload_is_admitted_before_private_store_resolution() {
    let dir = directory();
    let source = dir.join("input.wav");
    wave_channels(&source, 48000, 2, &[1, -1]);
    let mut graph = graph(&[48000]);
    for node in graph["nodes"].as_array_mut().unwrap() {
        node["format"]["channels"] = "stereo".into();
    }
    let port = graph["nodes"][0]["format"].clone();
    let mut edges = Vec::new();
    let mut previous = "in".to_string();
    for index in 0..64 {
        let id = format!("fir{index}");
        graph["nodes"].as_array_mut().unwrap().push(json!({"id":id,"format":port,"kind":{"kind":"convolution","asset":{"sha256":format!("{index:064x}"),"sample_rate_hz":48000,"channels":"stereo","taps_per_channel":262144,"purpose":"correction"}}}));
        edges.push(json!({"from":previous,"to":id,"from_port":0,"to_port":0}));
        previous = id;
    }
    edges.push(json!({"from":previous,"to":"out","from_port":0,"to_port":0}));
    graph["edges"] = edges.into();
    let output = run(&dir, &graph, &[source], "signed16");
    assert!(!output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        report["error"]["detail"]["code"],
        serde_json::to_value(earph_core::ErrorCode::ResourceBudgetExceeded).unwrap()
    );
}

#[test]
fn split_source_queue_has_one_guard_per_src_and_chained_capture_matches_reference() {
    let dir = directory();
    let first = dir.join("first.wav");
    let second = dir.join("second.wav");
    let input: Vec<i16> = (0..65)
        .map(|i| if i == 64 { 8192 } else { i * 31 })
        .collect();
    wave(&first, 48000, &input[..63]);
    wave(&second, 48000, &input[63..]);
    let result = run(
        &dir,
        &graph(&[48000, 96000, 48000]),
        &[first, second],
        "signed32",
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["source_frames"], 65);
    assert_eq!(report["files_completed"], 2);
    let src: Vec<_> = report["observations"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s.get("src").filter(|v| !v.is_null()))
        .collect();
    assert_eq!(src.len(), 2);
    assert!(src.iter().all(|s| s["guard_frames"] == 1));
    let normalized: Vec<_> = input.iter().map(|v| f64::from(*v) / 32768.0).collect();
    let expected = packed(
        &convert(&convert(&normalized, 48000, 96000), 96000, 48000),
        32,
    );
    assert_eq!(fs::read(dir.join("capture/capture.pcm")).unwrap(), expected);
    assert_eq!(
        report["dsp_output_frames"],
        report["acknowledged_output_frames"]
    );
    assert_eq!(
        report["capture_frames"],
        report["acknowledged_output_frames"]
    );
}

#[test]
fn explicit_attenuation_and_ramp_apply_once_before_each_packed_integer_format() {
    for (format, bits) in [("signed16", 16), ("signed24", 24), ("signed32", 32)] {
        let dir = directory();
        let source = dir.join("input.wav");
        let input = [32767, -32768, 16384, 0];
        wave(&source, 48000, &input);
        let graph_file = dir.join("graph.json");
        fs::write(&graph_file, serde_json::to_vec(&graph(&[48000])).unwrap()).unwrap();
        let result = Command::new(env!("CARGO_BIN_EXE_earph-engine"))
            .arg("render-graph")
            .arg("--graph")
            .arg(&graph_file)
            .arg("--output")
            .arg(dir.join("capture"))
            .args([
                "--sample-format",
                format,
                "--gain-db",
                "-6",
                "--ramp-frames",
                "4",
                "--quantum",
                "64",
                "--",
            ])
            .arg(&source)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stdout)
        );
        let gain = 10f64.powf(-6.0 / 20.0);
        let normalized: Vec<_> = input
            .iter()
            .enumerate()
            .map(|(i, x)| f64::from(*x) / 32768.0 * gain * (i + 1) as f64 / 4.0)
            .collect();
        assert_eq!(
            fs::read(dir.join("capture/capture.pcm")).unwrap(),
            packed(&normalized, bits)
        );
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(dir.join("capture"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        for file in ["capture.pcm", "report.json"] {
            assert_eq!(
                fs::metadata(dir.join("capture").join(file))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }
}

fn filter_reference(input: &[f64], length: usize, c: [f64; 5]) -> Vec<f64> {
    let mut output = vec![0.0; length];
    for i in 0..length {
        output[i] = c[0] * input.get(i).copied().unwrap_or(0.0);
        if i >= 1 {
            output[i] += c[1] * input.get(i - 1).copied().unwrap_or(0.0) - c[3] * output[i - 1];
        }
        if i >= 2 {
            output[i] += c[2] * input.get(i - 2).copied().unwrap_or(0.0) - c[4] * output[i - 2];
        }
    }
    output
}
fn assert_pcm_near(actual: &[u8], expected: &[u8]) {
    assert_eq!(actual.len(), expected.len());
    for (a, b) in actual
        .as_chunks::<2>()
        .0
        .iter()
        .zip(expected.as_chunks::<2>().0)
    {
        assert!((i32::from(i16::from_le_bytes(*a)) - i32::from(i16::from_le_bytes(*b))).abs() <= 1);
    }
}

#[test]
fn same_rate_q_shelf_and_src_final_q_shelf_match_literal_direct_form_reference() {
    // Independent prewarped bilinear analog shelves: A=10^(6/40), k=tan(pi*105/rate), Q=.7.
    // These literal rows do not call the production coefficient generator.
    for (rates, coefficients) in [
        (
            vec![48000],
            [
                1.0034127416329774,
                -1.9834162327859164,
                0.9802681475376862,
                -1.983482239777623,
                0.9836148821789564,
            ],
        ),
        (
            vec![48000, 44100],
            [
                1.0037150220023883,
                -1.9819436597644664,
                0.9785419468483904,
                -1.982021801050503,
                0.9821788275647421,
            ],
        ),
    ] {
        let dir = directory();
        let source = dir.join("impulse.wav");
        wave(&source, 48000, &[8192]);
        let mut graph = graph(&rates);
        let rate = *rates.last().unwrap();
        let port = graph["nodes"].as_array().unwrap().last().unwrap()["format"].clone();
        graph["nodes"].as_array_mut().unwrap().push(json!({"id":"shelf","kind":{"kind":"peq","filter":{"kind":"low_shelf_q","frequency_hz":105.0,"q":0.7,"gain_db":6.0}},"format":port}));
        let edge = graph["edges"].as_array_mut().unwrap().last_mut().unwrap();
        edge["to"] = "shelf".into();
        graph["edges"]
            .as_array_mut()
            .unwrap()
            .push(json!({"from":"shelf","to":"out","from_port":0,"to_port":0}));
        let result = run(&dir, &graph, &[source], "signed16");
        assert!(
            result.status.success(),
            "rate{rate}: {}",
            String::from_utf8_lossy(&result.stdout)
        );
        let actual = fs::read(dir.join("capture/capture.pcm")).unwrap();
        let input = if rates.len() == 1 {
            vec![0.25]
        } else {
            convert(&[0.25], 48000, rate)
        };
        let expected = packed(
            &filter_reference(&input, actual.len() / 2, coefficients),
            16,
        );
        assert_pcm_near(&actual, &expected);
    }
}

#[test]
fn intermediate_fir_uses_only_explicit_private_assets_and_matches_convolution_reference() {
    use std::os::unix::fs::PermissionsExt;
    let dir = directory();
    let source = dir.join("input.wav");
    wave(&source, 48000, &[8192, 0, 0, -4096]);
    let runtime = support::runtime();
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let correction = dir.join("owned-fir.wav");
    let mut wav = Vec::new();
    wav.extend(b"RIFF");
    wav.extend(60u32.to_le_bytes());
    wav.extend(b"WAVEfmt ");
    wav.extend(16u32.to_le_bytes());
    wav.extend(3u16.to_le_bytes());
    wav.extend(1u16.to_le_bytes());
    wav.extend(96000u32.to_le_bytes());
    wav.extend(768000u32.to_le_bytes());
    wav.extend(8u16.to_le_bytes());
    wav.extend(64u16.to_le_bytes());
    wav.extend(b"data");
    wav.extend(24u32.to_le_bytes());
    for coefficient in [0.25f64, 0.0, -0.125] {
        wav.extend(coefficient.to_le_bytes());
    }
    fs::write(&correction, &wav).unwrap();
    let reference = earph_engine::assets::import(&runtime, &correction).unwrap();
    let mut graph = graph(&[48000, 96000, 48000]);
    let port = graph["nodes"][1]["format"].clone();
    graph["nodes"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"fir","kind":{"kind":"convolution","asset":reference},"format":port}));
    graph["edges"][1]["to"] = "fir".into();
    graph["edges"]
        .as_array_mut()
        .unwrap()
        .push(json!({"from":"fir","to":"src1","from_port":0,"to_port":0}));
    let missing = run(&dir, &graph, std::slice::from_ref(&source), "signed16");
    assert!(!missing.status.success());
    let result = run_with_runtime(
        &dir.join("explicit"),
        &graph,
        &[source],
        "signed16",
        Some(&runtime),
    );
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
    let converted = convert(&[0.25, 0.0, 0.0, -0.125], 48000, 96000);
    let mut convolved = vec![0.0; converted.len() + 258];
    for (frame, sample) in converted.iter().enumerate() {
        convolved[256 + frame] += sample * 0.25;
        convolved[258 + frame] -= sample * 0.125;
    }
    let expected = packed(&convert(&convolved, 96000, 48000), 16);
    let actual = fs::read(dir.join("explicit/capture/capture.pcm")).unwrap();
    assert_pcm_near(&actual, &expected);
    assert_eq!(fs::read(correction).unwrap(), wav);
}

#[test]
fn missing_duplicate_unknown_options_and_oversized_graph_return_typed_failures() {
    for args in [
        vec!["render-graph"],
        vec!["render-graph", "--policy", "strict"],
        vec!["render-graph", "--graph", "a", "--graph", "b"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_earph-engine"))
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["status"], "fail");
        assert!(report["capture_bytes"].is_null());
    }
    let dir = directory();
    let graph = dir.join("large.json");
    fs::write(&graph, vec![b' '; (128 << 10) + 1]).unwrap();
    let input = dir.join("input.wav");
    wave(&input, 48000, &[1]);
    let result = Command::new(env!("CARGO_BIN_EXE_earph-engine"))
        .arg("render-graph")
        .arg("--graph")
        .arg(graph)
        .arg("--output")
        .arg(dir.join("capture"))
        .args([
            "--sample-format",
            "signed16",
            "--gain-db",
            "0",
            "--ramp-frames",
            "0",
            "--quantum",
            "64",
            "--",
        ])
        .arg(input)
        .output()
        .unwrap();
    assert!(!result.status.success());
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(
        report["error"]["detail"]["code"],
        serde_json::to_value(earph_core::ErrorCode::ResourceBudgetExceeded).unwrap()
    );
    assert_eq!(report["capture_bytes"], 0);
}

#[test]
fn source_mismatch_and_existing_output_collision_preserve_evidence_and_inputs() {
    let dir = directory();
    let first = dir.join("first.wav");
    let wrong = dir.join("wrong.wav");
    wave(&first, 48000, &[1024; 64]);
    wave(&wrong, 44100, &[2048]);
    let original = fs::read(&first).unwrap();
    let result = run(&dir, &graph(&[48000]), &[first.clone(), wrong], "signed16");
    assert!(!result.status.success());
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["status"], "fail");
    assert_eq!(report["files_completed"], 1);
    assert_eq!(report["source_frames"], 64);
    assert_eq!(report["capture_bytes"], 128);
    assert_eq!(fs::read(&first).unwrap(), original);
    let capture = fs::read(dir.join("capture/capture.pcm")).unwrap();
    let saved_report = fs::read(dir.join("capture/report.json")).unwrap();
    let again = run(&dir, &graph(&[48000]), &[first], "signed16");
    assert!(!again.status.success());
    assert_eq!(fs::read(dir.join("capture/capture.pcm")).unwrap(), capture);
    assert_eq!(
        fs::read(dir.join("capture/report.json")).unwrap(),
        saved_report
    );
    assert!(!String::from_utf8_lossy(&again.stdout).contains(dir.to_str().unwrap()));
}

#[test]
fn strict_graph_wire_rate_port_and_cut_errors_never_publish_passing_capture() {
    for case in 0..5 {
        let dir = directory();
        let source = dir.join("input.wav");
        wave(&source, 48000, &[1024]);
        let mut graph = graph(&[48000, 96000]);
        match case {
            0 => graph["unknown_control"] = true.into(),
            1 => graph["nodes"][1]["kind"]["input_rate_hz"] = 192000.into(),
            2 => graph["nodes"][1]["format"]["max_quantum_frames"] = 64.into(),
            3 => graph["nodes"][1]["kind"]["chunk_frames"] = 3.into(),
            _ => {
                graph = super_bypass_graph();
            }
        }
        let result = run(&dir, &graph, &[source], "signed16");
        assert!(!result.status.success(), "case{case}");
        let report: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(report["status"], "fail");
        assert_eq!(report["capture_bytes"], 0);
        assert!(!report["error"].is_null());
    }
}
fn super_bypass_graph() -> Value {
    let mut graph = graph(&[48000, 48000]);
    let port = graph["nodes"][0]["format"].clone();
    graph["nodes"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"sum","kind":{"kind":"sum"},"format":port}));
    graph["edges"][1]["to"] = "sum".into();
    graph["edges"]
        .as_array_mut()
        .unwrap()
        .push(json!({"from":"in","to":"sum","from_port":0,"to_port":1}));
    graph["edges"]
        .as_array_mut()
        .unwrap()
        .push(json!({"from":"sum","to":"out","from_port":0,"to_port":0}));
    graph
}

#[test]
fn real_file_size_failure_reports_readback_prefix_and_partial_frame_without_acknowledging_the_period()
 {
    use sha2::{Digest, Sha256};
    let dir = directory();
    let source = dir.join("input.wav");
    wave(&source, 48000, &vec![1024; 65536]);
    let graph_file = dir.join("graph.json");
    fs::write(&graph_file, serde_json::to_vec(&graph(&[48000])).unwrap()).unwrap();
    let result = Command::new("bash")
        .args([
            "-c",
            "trap '' XFSZ; ulimit -f 64; exec \"$@\"",
            "offline-file-limit",
        ])
        .arg(env!("CARGO_BIN_EXE_earph-engine"))
        .arg("render-graph")
        .arg("--graph")
        .arg(graph_file)
        .arg("--output")
        .arg(dir.join("capture"))
        .args([
            "--sample-format",
            "signed24",
            "--gain-db",
            "0",
            "--ramp-frames",
            "0",
            "--quantum",
            "64",
            "--",
        ])
        .arg(source)
        .output()
        .unwrap();
    assert!(!result.status.success());
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["status"], "fail");
    let bytes = fs::read(dir.join("capture/capture.pcm")).unwrap();
    assert!(!bytes.is_empty());
    assert_ne!(bytes.len() % 3, 0);
    assert_eq!(report["capture_bytes"], bytes.len());
    assert_eq!(report["capture_frames"], bytes.len() / 3);
    assert_eq!(report["partial_frame_bytes"], bytes.len() % 3);
    assert!(
        report["acknowledged_output_frames"].as_u64().unwrap()
            < report["capture_frames"].as_u64().unwrap()
    );
    let hash: String = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(report["capture_sha256"], hash);
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(dir.join("capture/report.json")).unwrap())
            .unwrap(),
        report
    );
}
