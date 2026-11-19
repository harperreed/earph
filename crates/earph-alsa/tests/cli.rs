// ABOUTME: Runs the ALSA executable through real isolated plugin configurations.
// ABOUTME: Checks JSON evidence, approval gates, alias rejection, and captured fixture bytes.
#![cfg(target_os = "linux")]
use serde_json::{Value, json};
use std::{
    path::Path,
    process::{Command, Output},
};
mod support;

fn run(config: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_earph-alsa"))
        .env("ALSA_CONFIG_PATH", config)
        .args(args)
        .output()
        .unwrap()
}
fn document(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "{e}: {} / {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}
fn setup() -> (std::path::PathBuf, std::path::PathBuf, Value) {
    let directory = support::directory();
    let config = directory.join("alsa.conf");
    let capture = directory.join("capture.pcm");
    std::fs::write(
        &config,
        format!(
            r#"
ctl.hw {{ @args [ CARD ] @args.CARD {{ type string }} type hw card $CARD }}
pcm.null {{ type null hint {{ show on description "Discard samples" }} }}
pcm.capture {{ type file slave.pcm null file "{}" format raw }}
pcm.hw {{ @args [ CARD DEV ] @args.CARD {{ type string }} @args.DEV {{ type integer }} type null }}
pcm.only_48000 {{ type rate slave {{ pcm null rate 48000 }} converter "linear" }}
namehint.pcm [ "capture_only|Capture only|IOIDInput" ]
"#,
            capture.display()
        ),
    )
    .unwrap();
    let request = json!({"schema_version":1,"device":"null","route":"system","format":{"sample_rate_hz":48000,"channels":"stereo","representation":"signed24"},"period_frames":64,"buffer_frames":256});
    (directory, config, request)
}

#[test]
fn discovery_and_silent_open_report_only_the_observed_evidence() {
    let (directory, config, request) = setup();
    let output = run(&config, &["devices"]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    let value = document(&output);
    assert_eq!(value["evidence_level"], "advertised");
    assert_eq!(value["signals_submitted"], 0);
    assert!(
        value["devices"]
            .as_array()
            .unwrap()
            .iter()
            .any(|device| device["device"] == "null")
    );
    assert!(
        !value["devices"]
            .as_array()
            .unwrap()
            .iter()
            .any(|device| device["device"] == "capture_only")
    );
    let path = directory.join("request.json");
    std::fs::write(&path, serde_json::to_vec(&request).unwrap()).unwrap();
    for _ in 0..2 {
        let output = run(&config, &["open", "--request", path.to_str().unwrap()]);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let value = document(&output);
        assert_eq!(value["report"]["evidence_level"], "opened");
        assert_eq!(value["report"]["requested"], request);
        assert_eq!(value["report"]["observed"]["alsa_format"], "S24_3LE");
        assert_eq!(value["report"]["payload_preservation"], "unverified");
        assert_eq!(value["signals_submitted"], 0);
        assert_eq!(value["closed"], true);
        assert!(value["build"]["source_inputs_sha256"].is_string());
        assert!(value["environment"]["alsa_version"].is_string());
    }
}

#[test]
fn discovery_refuses_oversize_hint_text_and_entry_counts() {
    for many in [false, true] {
        let (directory, _, _) = setup();
        let config = directory.join("bounded-hints.conf");
        let text = if many {
            (0..257).map(|i| format!("pcm.hint{i:03} {{ type null hint {{ show on description \"Device {i}\" }} }}\n")).collect::<String>()
        } else {
            format!(
                "pcm.null {{ type null hint {{ show on description \"{}\" }} }}\n",
                "x".repeat(4097)
            )
        };
        let control = "ctl.hw { @args [ CARD ] @args.CARD { type string } type hw card $CARD }\n";
        std::fs::write(&config, format!("{control}{text}")).unwrap();
        let output = run(&config, &["devices"]);
        assert!(
            !output.status.success(),
            "oversize discovery passed, many={many}"
        );
        assert_eq!(
            document(&output)["error"]["code"],
            "RESOURCE_BUDGET_EXCEEDED",
            "many={many}: {} / {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn a_hw_alias_cannot_masquerade_as_a_direct_hardware_stream() {
    let (directory, config, mut request) = setup();
    request["device"] = "hw:0,0".into();
    request["route"] = "direct_hardware".into();
    let path = directory.join("request.json");
    std::fs::write(&path, serde_json::to_vec(&request).unwrap()).unwrap();
    let output = run(&config, &["open", "--request", path.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(
        document(&output)["error"]["code"],
        "FIDELITY_POLICY_CONFLICT"
    );
}

#[test]
fn fixture_exercise_requires_approval_before_opening_or_reading_assets() {
    let (_, config, _) = setup();
    let output = run(
        &config,
        &[
            "exercise",
            "--request",
            "missing.json",
            "--fixture",
            "missing.json",
        ],
    );
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(document(&output)["error"]["code"], "SAFETY_POLICY_BLOCKED");
}

#[test]
fn actual_fixture_bytes_cross_the_alsa_file_plugin_without_conversion() {
    let (directory, config, mut request) = setup();
    let fixtures = directory.join("fixtures");
    earph_harness::fixture::write_bundle(&fixtures).unwrap();
    request["device"] = "capture".into();
    let path = directory.join("request.json");
    std::fs::write(&path, serde_json::to_vec(&request).unwrap()).unwrap();
    let fixture = fixtures.join("signed24-stereo-48000-boundary.json");
    let output = run(
        &config,
        &[
            "exercise",
            "--request",
            path.to_str().unwrap(),
            "--fixture",
            fixture.to_str().unwrap(),
            "--allow-signal",
        ],
    );
    assert!(
        output.status.success(),
        "{} / {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let result = document(&output);
    assert_eq!(result["exercise"]["evidence_level"], "exercised");
    assert_eq!(result["exercise"]["submitted_frames"], 1031);
    assert_eq!(result["exercise"]["backend_delay_frames"], 0);
    assert_eq!(
        result["exercise"]["measurement_boundary"],
        "alsa_client_estimate"
    );
    assert_eq!(result["report"]["payload_preservation"], "unverified");
    assert_eq!(
        std::fs::read(directory.join("capture.pcm")).unwrap(),
        std::fs::read(fixtures.join("signed24-stereo-48000-boundary.pcm")).unwrap()
    );
}

#[test]
fn invalid_requests_and_missing_devices_produce_structured_failures() {
    let (directory, config, mut request) = setup();
    let path = directory.join("request.json");
    for (device, code) in [
        ("does_not_exist", "DEVICE_UNAVAILABLE"),
        ("", "ASSET_INVALID"),
    ] {
        request["device"] = device.into();
        std::fs::write(&path, serde_json::to_vec(&request).unwrap()).unwrap();
        let output = run(&config, &["open", "--request", path.to_str().unwrap()]);
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(document(&output)["error"]["code"], code);
        if device == "does_not_exist" {
            assert!(String::from_utf8_lossy(&output.stderr).contains("Unknown PCM"));
        }
    }
    std::fs::write(&path, b"{bad json").unwrap();
    let output = run(&config, &["open", "--request", path.to_str().unwrap()]);
    assert_eq!(document(&output)["error"]["code"], "ASSET_INVALID");
    let output = run(&config, &["--help"]);
    assert!(output.status.success());
    assert!(
        document(&output)["usage"]
            .as_str()
            .unwrap()
            .contains("--allow-signal")
    );
}

#[test]
fn mismatched_fixture_fails_before_the_output_file_is_created() {
    let (directory, config, mut request) = setup();
    let fixtures = directory.join("fixtures");
    earph_harness::fixture::write_bundle(&fixtures).unwrap();
    request["device"] = "capture".into();
    let path = directory.join("request.json");
    std::fs::write(&path, serde_json::to_vec(&request).unwrap()).unwrap();
    let fixture = fixtures.join("signed16-stereo-48000-boundary.json");
    let output = run(
        &config,
        &[
            "exercise",
            "--request",
            path.to_str().unwrap(),
            "--fixture",
            fixture.to_str().unwrap(),
            "--allow-signal",
        ],
    );
    assert_eq!(document(&output)["error"]["code"], "FORMAT_UNSUPPORTED");
    assert!(!directory.join("capture.pcm").exists());
}

#[test]
fn compatibility_conversion_keeps_downstream_transformations_unknown() {
    let (directory, config, mut request) = setup();
    request["device"] = "only_48000".into();
    request["format"]["sample_rate_hz"] = 44100.into();
    request["format"]["representation"] = "signed16".into();
    let path = directory.join("request.json");
    std::fs::write(&path, serde_json::to_vec(&request).unwrap()).unwrap();
    let output = run(&config, &["open", "--request", path.to_str().unwrap()]);
    assert!(
        output.status.success(),
        "{} / {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let value = document(&output);
    assert_eq!(value["report"]["pcm_type"], "RATE");
    assert_eq!(
        value["report"]["observed"]["format"]["sample_rate_hz"],
        44100
    );
    assert_eq!(value["report"]["downstream_transformations"], "unknown");
    assert_eq!(value["report"]["payload_preservation"], "unverified");
}

#[test]
fn matrix_captures_all_fixture_formats_with_real_alsa_plugins() {
    let directory = support::directory();
    let fixtures = directory.join("fixtures");
    let bundle = earph_harness::fixture::write_bundle(&fixtures).unwrap();
    std::fs::create_dir(directory.join("cases")).unwrap();
    let mut results = Vec::new();
    for fixture in &bundle.fixtures {
        let stem = Path::new(&fixture.payload_file)
            .file_stem()
            .unwrap()
            .to_str()
            .unwrap();
        for period in [1, 64, 256] {
            let case_name = format!("{stem}-{period}");
            let case = directory.join("cases").join(&case_name);
            std::fs::create_dir(&case).unwrap();
            let config = b"pcm.null { type null }\npcm.capture { type file slave.pcm null file \"capture.pcm\" format raw }\n";
            std::fs::write(case.join("alsa.conf"), config).unwrap();
            let request = json!({"schema_version":1,"device":"capture","route":"system","format":fixture.spec.format,"period_frames":period,"buffer_frames":period * 4});
            let request = serde_json::to_vec_pretty(&request).unwrap();
            std::fs::write(case.join("request.json"), &request).unwrap();
            let fixture_path = format!("../../fixtures/{stem}.json");
            let args = [
                "exercise",
                "--request",
                "request.json",
                "--fixture",
                &fixture_path,
                "--allow-signal",
            ];
            let output = Command::new(env!("CARGO_BIN_EXE_earph-alsa"))
                .current_dir(&case)
                .env("ALSA_CONFIG_PATH", "alsa.conf")
                .args(args)
                .output()
                .unwrap();
            std::fs::write(case.join("report.json"), &output.stdout).unwrap();
            assert!(
                output.status.success(),
                "{case_name}: {} / {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                output.stderr.is_empty(),
                "{case_name}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let captured = std::fs::read(case.join("capture.pcm")).unwrap();
            let expected = std::fs::read(fixtures.join(&fixture.payload_file)).unwrap();
            assert_eq!(captured, expected, "{case_name}");
            let result = document(&output);
            assert_eq!(result["exercise"]["submitted_frames"], fixture.spec.frames);
            results.push(json!({"case":format!("cases/{case_name}"),"command":args,
                "fixture":format!("fixtures/{stem}.json"),"status":"pass",
                "capture_sha256":earph_harness::sha256(&captured),
                "expected_payload_sha256":fixture.payload_sha256,
                "request_sha256":earph_harness::sha256(&request),
                "config_sha256":earph_harness::sha256(config),
                "report_sha256":earph_harness::sha256(&output.stdout)}));
        }
    }
    assert_eq!(results.len(), 216);
    let evidence = json!({"schema_version":1,"status":"pass","measurement_boundary":"alsa_file_plugin",
        "fixture_count":72,"case_count":216,"physical_output":"not_run","pipewire_coexistence":"not_run","results":results});
    std::fs::write(
        directory.join("matrix.json"),
        serde_json::to_vec_pretty(&evidence).unwrap(),
    )
    .unwrap();
    earph_alsa::validate_matrix(&directory.join("matrix.json")).unwrap();
    let check = Command::new(env!("CARGO_BIN_EXE_earph-alsa"))
        .args(["matrix-check", "--record"])
        .arg(directory.join("matrix.json"))
        .output()
        .unwrap();
    assert!(
        check.status.success(),
        "{}",
        String::from_utf8_lossy(&check.stdout)
    );
    assert_eq!(document(&check)["case_count"], 216);
    let first_capture = directory
        .join(evidence["results"][0]["case"].as_str().unwrap())
        .join("capture.pcm");
    let original = std::fs::read(&first_capture).unwrap();
    let mut corrupt = original.clone();
    corrupt[0] ^= 1;
    std::fs::write(&first_capture, corrupt).unwrap();
    assert!(earph_alsa::validate_matrix(&directory.join("matrix.json")).is_err());
    std::fs::write(&first_capture, original).unwrap();
    let report_path = first_capture.with_file_name("report.json");
    let original_report = std::fs::read(&report_path).unwrap();
    for (field, incorrect) in [
        ("buffer_frames", json!(9999)),
        ("period_frames", json!(999)),
        ("rate_numerator", json!(1)),
        ("rate_denominator", json!(0)),
        ("significant_bits", json!(0)),
        ("alsa_format", json!("FLOAT_LE")),
        ("access", json!("MMAP_INTERLEAVED")),
    ] {
        let mut changed: Value = serde_json::from_slice(&original_report).unwrap();
        changed["report"]["observed"][field] = incorrect;
        let changed_bytes = serde_json::to_vec(&changed).unwrap();
        std::fs::write(&report_path, &changed_bytes).unwrap();
        let mut changed_matrix = evidence.clone();
        changed_matrix["results"][0]["report_sha256"] =
            earph_harness::sha256(&changed_bytes).into();
        std::fs::write(
            directory.join("matrix.json"),
            serde_json::to_vec(&changed_matrix).unwrap(),
        )
        .unwrap();
        assert!(
            earph_alsa::validate_matrix(&directory.join("matrix.json")).is_err(),
            "accepted mismatched installed {field}"
        );
    }
    std::fs::write(&report_path, original_report).unwrap();
    for field in ["physical_output", "pipewire_coexistence"] {
        let mut exaggerated = evidence.clone();
        exaggerated[field] = "pass".into();
        std::fs::write(
            directory.join("matrix.json"),
            serde_json::to_vec(&exaggerated).unwrap(),
        )
        .unwrap();
        assert!(earph_alsa::validate_matrix(&directory.join("matrix.json")).is_err());
    }
    let mut escaped = evidence.clone();
    escaped["results"][0]["case"] = "../outside".into();
    std::fs::write(
        directory.join("matrix.json"),
        serde_json::to_vec(&escaped).unwrap(),
    )
    .unwrap();
    assert!(earph_alsa::validate_matrix(&directory.join("matrix.json")).is_err());
    std::fs::write(
        directory.join("matrix.json"),
        serde_json::to_vec_pretty(&evidence).unwrap(),
    )
    .unwrap();
    println!("ALSA matrix artifacts retained at {}", directory.display());
}
