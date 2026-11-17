// ABOUTME: Runs the actual offline executable against real files.
// ABOUTME: Verifies exit status and machine-readable outcomes.
use std::process::Command;

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
