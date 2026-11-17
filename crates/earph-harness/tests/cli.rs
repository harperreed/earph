// ABOUTME: Runs the actual offline executable against real files.
// ABOUTME: Verifies exit status and machine-readable outcomes.
use std::process::Command;
mod support;

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
