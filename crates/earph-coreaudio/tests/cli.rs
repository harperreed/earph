// ABOUTME: Runs the diagnostic executable against real Core Audio devices on macOS.
// ABOUTME: Verifies JSON contracts and registration without starting audio or changing settings.
use serde_json::Value;
use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_earph-coreaudio"))
        .args(args)
        .output()
        .unwrap()
}
fn document(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{error}: {} / {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}
#[test]
fn help_describes_read_only_operations_and_unknown_commands_fail() {
    let output = run(&["--help"]);
    assert!(output.status.success());
    assert!(
        document(&output)["usage"]
            .as_str()
            .unwrap()
            .contains("registration-check")
    );
    let output = run(&["not-a-command"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(document(&output)["error"]["code"], "ASSET_INVALID");
}

#[test]
fn redaction_command_reads_a_real_record_and_preserves_its_source_file() {
    #[cfg(target_os = "macos")]
    let source = run(&["devices"]).stdout;
    #[cfg(not(target_os = "macos"))]
    let source = serde_json::to_vec(&serde_json::json!({"schema_version":1,"command":"devices",
        "inventory":{"devices":[{"uid":"private-device","name":"personal device","output_streams":[]}]}})).unwrap();
    let original: Value = serde_json::from_slice(&source).unwrap();
    let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/coreaudio-cli-tests");
    std::fs::create_dir_all(&directory).unwrap();
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = directory.join(format!("{}-{stamp}-redaction.json", std::process::id()));
    std::fs::write(&path, &source).unwrap();
    let output = run(&["redact", "--record", path.to_str().unwrap()]);
    assert!(output.status.success());
    let result = document(&output);
    assert_eq!(result["identifiers_redacted"], true);
    assert_eq!(
        result["source_record_sha256"],
        earph_harness::sha256(&source)
    );
    for (redacted, original) in result["inventory"]["devices"]
        .as_array()
        .unwrap()
        .iter()
        .zip(original["inventory"]["devices"].as_array().unwrap())
    {
        assert_eq!(redacted["uid"], "redacted-device");
        assert_eq!(redacted["name"], "redacted-device-name");
        assert_eq!(redacted["output_streams"], original["output_streams"]);
    }
    assert_eq!(std::fs::read(path).unwrap(), source);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn native_commands_fail_explicitly_on_other_platforms() {
    let output = run(&["devices"]);
    assert_eq!(output.status.code(), Some(2));
    assert_eq!(document(&output)["error"]["code"], "CAPABILITY_UNSUPPORTED");
}

#[cfg(target_os = "macos")]
mod native {
    use super::*;
    use serde_json::json;

    #[test]
    fn real_device_inspection_and_registration_are_silent_and_explicit() {
        let inventory = run(&["devices"]);
        assert!(
            inventory.status.success(),
            "{}",
            String::from_utf8_lossy(&inventory.stdout)
        );
        let value = document(&inventory);
        assert_eq!(value["evidence_level"], "observed_current_settings");
        assert_eq!(value["signals_submitted"], 0);
        assert_eq!(value["inventory"]["failures"].as_array().unwrap().len(), 0);
        let device = value["inventory"]["devices"]
            .as_array()
            .unwrap()
            .iter()
            .find(|device| device["alive"] == true)
            .unwrap();
        let uid = device["uid"].as_str().unwrap();
        let inspect = run(&["inspect", "--device-uid", uid]);
        assert!(inspect.status.success());
        assert_eq!(document(&inspect)["device"]["uid"], uid);
        let registration = run(&["registration-check", "--device-uid", uid]);
        assert!(
            registration.status.success(),
            "{}",
            String::from_utf8_lossy(&registration.stdout)
        );
        let value = document(&registration);
        assert_eq!(
            value["registration"]["evidence_level"],
            "registered_not_started"
        );
        assert_eq!(value["callbacks_observed"], 0);
        assert_eq!(value["signals_submitted"], 0);
        assert_eq!(value["closed"], true);
        assert_eq!(value["settings_unchanged"], true);
        assert_eq!(value["requested_device_changes"], false);
        assert!(
            value["observed_at_unix_ns"]
                .as_str()
                .is_some_and(|value| value.parse::<u128>().is_ok_and(|value| value > 0))
        );
        assert!(value["build"]["source_inputs_sha256"].is_string());
        assert_eq!(value["environment"]["os"], "macos");
    }

    #[test]
    fn unavailable_uids_and_malformed_requests_do_not_fall_back() {
        for method in ["inspect", "registration-check"] {
            let output = run(&[method, "--device-uid", "earph-nonexistent-device-2389"]);
            assert_eq!(output.status.code(), Some(2));
            assert_eq!(document(&output)["error"]["code"], "DEVICE_UNAVAILABLE");
        }
        let output = run(&["precision-check", "--request", "missing-earph-request.json"]);
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(document(&output)["error"]["code"], "ASSET_INVALID");
    }

    #[test]
    fn precision_preview_keeps_conflicts_distinct_from_opened_evidence() {
        let inventory = document(&run(&["devices"]));
        let device = inventory["inventory"]["devices"]
            .as_array()
            .unwrap()
            .iter()
            .find(|device| device["alive"] == true)
            .unwrap();
        let directory = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/coreaudio-cli-tests");
        std::fs::create_dir_all(&directory).unwrap();
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = directory.join(format!("{}-{stamp}.json", std::process::id()));
        let request = json!({"schema_version":1,"device_uid":device["uid"],"format":{"sample_rate_hz":48000,"channels":"stereo","representation":"signed24"},"buffer_frames":device["buffer_frames"]});
        std::fs::write(&path, serde_json::to_vec(&request).unwrap()).unwrap();
        let output = run(&["precision-check", "--request", path.to_str().unwrap()]);
        let result = document(&output);
        assert_eq!(result["assessment"]["opened"], false);
        assert_eq!(
            result["assessment"]["evidence_level"],
            "observed_current_settings"
        );
        assert_eq!(result["assessment"]["payload_preservation"], "unverified");
        assert_eq!(result["assessment"]["requested"], request);
        if result["assessment"]["matches_current_format"] == true {
            assert!(output.status.success());
        } else {
            assert_eq!(output.status.code(), Some(2));
            assert_eq!(result["error"]["code"], "FORMAT_UNSUPPORTED");
            assert!(
                !result["assessment"]["conflicts"]
                    .as_array()
                    .unwrap()
                    .is_empty()
            );
        }
    }
}
