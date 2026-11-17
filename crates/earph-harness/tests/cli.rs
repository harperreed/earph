// ABOUTME: Runs the actual offline executable against real files.
// ABOUTME: Verifies exit status and machine-readable outcomes.
use std::process::Command;
mod support;

#[test]
fn renderer_consumes_real_payload_and_keeps_capture_on_backend_failure() {
    let temporary = support::TestDir::new();
    let fixtures = temporary.0.join("fixtures");
    earph_harness::fixture::write_bundle(&fixtures).unwrap();
    let fixture = fixtures.join("signed24-stereo-48000-seeded_noise.json");
    let original = std::fs::read(fixtures.join("signed24-stereo-48000-seeded_noise.pcm")).unwrap();
    let output_dir = temporary.0.join("render");
    let output = Command::new(env!("CARGO_BIN_EXE_earph-harness"))
        .args(["render", "--fixture"])
        .arg(&fixture)
        .args(["--block-frames", "64", "--output"])
        .arg(&output_dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(
        std::fs::read(output_dir.join("capture.pcm")).unwrap(),
        original
    );
    let schedule = temporary.0.join("faults.json");
    std::fs::write(
        &schedule,
        br#"{"schema_version":1,"faults":[{"at_frame":7,"fault":{"kind":"backend_failure"}}]}"#,
    )
    .unwrap();
    let failed = temporary.0.join("failed");
    let output = Command::new(env!("CARGO_BIN_EXE_earph-harness"))
        .args(["render", "--fixture"])
        .arg(&fixture)
        .args(["--block-frames", "256", "--output"])
        .arg(&failed)
        .arg("--faults")
        .arg(&schedule)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(
        std::fs::read(failed.join("capture.pcm")).unwrap(),
        original[..42]
    );
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(failed.join("report.json")).unwrap()).unwrap();
    assert_eq!(report["status"], "fail");
    assert_eq!(report["output_frames"], 7);
    let response: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(response["error"]["code"], "OUTPUT_FAULT");
}

#[test]
fn renderer_rejects_corrupt_payloads_and_path_escape_before_capture() {
    let temporary = support::TestDir::new();
    let fixtures = temporary.0.join("fixtures");
    earph_harness::fixture::write_bundle(&fixtures).unwrap();
    let path = fixtures.join("signed16-mono-44100-boundary.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    for value in [
        serde_json::json!("../private.pcm"),
        serde_json::json!("missing.pcm"),
    ] {
        manifest["payload_file"] = value;
        std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let output_dir = temporary.0.join("rejected");
        let output = Command::new(env!("CARGO_BIN_EXE_earph-harness"))
            .args(["render", "--fixture"])
            .arg(&path)
            .args(["--block-frames", "64", "--output"])
            .arg(&output_dir)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(!output_dir.exists());
    }
}

#[test]
fn fixture_files_and_manifests_are_repeatable_and_never_overwritten() {
    let temporary = support::TestDir::new();
    let first = temporary.0.join("first");
    let second = temporary.0.join("second");
    for directory in [&first, &second] {
        let output = Command::new(env!("CARGO_BIN_EXE_earph-harness"))
            .args(["fixtures", "--output"])
            .arg(directory)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
    let a = std::fs::read(first.join("manifest.json")).unwrap();
    assert_eq!(a, std::fs::read(second.join("manifest.json")).unwrap());
    let manifest: serde_json::Value = serde_json::from_slice(&a).unwrap();
    assert_eq!(manifest["fixtures"].as_array().unwrap().len(), 72);
    for file in manifest["fixtures"].as_array().unwrap() {
        let path = file["payload_file"].as_str().unwrap();
        let bytes = std::fs::read(first.join(path)).unwrap();
        assert_eq!(bytes, std::fs::read(second.join(path)).unwrap());
        assert_eq!(earph_harness::sha256(&bytes), file["payload_sha256"]);
    }
    let retry = Command::new(env!("CARGO_BIN_EXE_earph-harness"))
        .args(["fixtures", "--output"])
        .arg(&first)
        .output()
        .unwrap();
    assert!(!retry.status.success());
    assert_eq!(a, std::fs::read(first.join("manifest.json")).unwrap());
}

#[test]
fn catalog_command_checks_real_document() {
    let output = Command::new(env!("CARGO_BIN_EXE_earph-harness"))
        .args(["catalog-check", "--catalog", "docs/requirements.json"])
        .current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../.."))
        .output()
        .unwrap();
    assert!(output.status.success());
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["requirements"], 69);
}

#[test]
fn invalid_command_and_missing_input_fail_with_json() {
    for args in [
        vec!["missing-method"],
        vec!["catalog-check", "--catalog", "missing.json"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_earph-harness"))
            .args(args)
            .output()
            .unwrap();
        assert!(!output.status.success());
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result["status"], "fail");
        assert!(result["error"]["code"].is_string());
    }
}
