// ABOUTME: Validates actual qualification records and artifact integrity.
// ABOUTME: Rejects false passes, missing provenance, and escaped artifact paths.
use earph_harness::evidence;
use serde_json::json;
mod support;

#[test]
fn real_qualification_can_be_verified_after_fresh_read() {
    let temporary = support::TestDir::new();
    let output = temporary.0.join("qualification");
    evidence::qualify(&output).unwrap();
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(output.join("evidence.json")).unwrap()).unwrap();
    evidence::validate(&record, &output).unwrap();
    assert_eq!(record["observed_result"]["clean_renders"], 288);
    assert_eq!(record["observed_result"]["fault_cases"], 18);
    assert_eq!(
        record["environment"]["measurement_boundary"],
        "engine_output"
    );
    for (pointer, value) in [
        ("/commit", json!("")),
        ("/build_id", json!("")),
        ("/dependency_lock_sha256", json!("no-hash")),
        ("/requirement_ids", json!(["MISSING"])),
        ("/requirement_ids", json!(["SAFE-01"])),
        ("/acceptance_ids", json!(["AC-99"])),
        ("/acceptance_ids", json!(["AC-21"])),
        ("/status", json!("fail")),
        ("/observed_result/clean_renders", json!(0)),
        ("/artifacts/0/path", json!("../private")),
        ("/artifacts/0/sha256", json!("0".repeat(64))),
    ] {
        let mut damaged = record.clone();
        *damaged.pointer_mut(pointer).unwrap() = value;
        assert!(
            evidence::validate(&damaged, &output).is_err(),
            "accepted damaged {pointer}"
        );
    }
    for key in ["build_id", "graph_sha256", "reviewer"] {
        let mut damaged = record.clone();
        damaged.as_object_mut().unwrap().remove(key);
        assert!(
            evidence::validate(&damaged, &output).is_err(),
            "accepted missing {key}"
        );
    }
    let fault_path = output.join("fault-results.json");
    let original_faults = std::fs::read(&fault_path).unwrap();
    let mut faults: serde_json::Value = serde_json::from_slice(&original_faults).unwrap();
    faults[0]["fault"]["max_frames"] = json!(4);
    let changed_faults = serde_json::to_vec(&faults).unwrap();
    std::fs::write(&fault_path, &changed_faults).unwrap();
    let mut damaged = record.clone();
    for artifact in damaged["artifacts"].as_array_mut().unwrap() {
        if artifact["path"] == "fault-results.json" {
            artifact["sha256"] = earph_harness::sha256(&changed_faults).into();
            artifact["bytes"] = changed_faults.len().into();
        }
    }
    assert!(
        evidence::validate(&damaged, &output).is_err(),
        "accepted a different fault configuration"
    );
    std::fs::write(fault_path, original_faults).unwrap();
    let artifact = record["artifacts"][0]["path"].as_str().unwrap();
    std::fs::write(output.join(artifact), b"truncated").unwrap();
    assert!(evidence::validate(&record, &output).is_err());
    assert!(evidence::qualify(&output).is_err());
}

#[test]
fn failed_matrix_cannot_be_relabelled_as_passing_evidence() {
    let temporary = support::TestDir::new();
    let output = temporary.0.join("qualification");
    let mut record = evidence::qualify(&output).unwrap();
    let matrix_path = output.join("render-results.json");
    let mut matrix: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&matrix_path).unwrap()).unwrap();
    matrix[0]["sample_differences"] = json!(1);
    let bytes = serde_json::to_vec(&matrix).unwrap();
    std::fs::write(matrix_path, &bytes).unwrap();
    for artifact in record["artifacts"].as_array_mut().unwrap() {
        if artifact["path"] == "render-results.json" {
            artifact["sha256"] = earph_harness::sha256(&bytes).into();
            artifact["bytes"] = bytes.len().into();
        }
    }
    assert!(evidence::validate(&record, &output).is_err());
}

#[test]
fn failed_output_creation_preserves_existing_file() {
    let temporary = support::TestDir::new();
    let output = temporary.0.join("is-a-file");
    std::fs::write(&output, b"owned existing file").unwrap();
    assert!(evidence::qualify(&output).is_err());
    assert_eq!(std::fs::read(&output).unwrap(), b"owned existing file");
}
