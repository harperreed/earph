// ABOUTME: Checks the Engine's compiled package record through the strict Core consumer.
// ABOUTME: Keeps build facts distinct from runtime checkout and executable identity.
use earph_core::provenance::{BuildProducer, RunningBuildProvenance};
use std::{fs, process::Command};
#[allow(dead_code)]
#[path = "../../../build_support/provenance.rs"]
mod provenance;

#[test]
fn engine_embeds_a_bounded_typed_package_record() {
    let build = earph_engine::build_provenance();
    build.validate_for_engine().unwrap();
    let bytes = serde_json::to_vec(build.as_ref()).unwrap();
    assert_eq!(RunningBuildProvenance::decode(&bytes).unwrap(), *build);
    assert_eq!(
        build.record.as_ref().unwrap().producer,
        BuildProducer::EnginePackage
    );
    assert!(bytes.len() <= earph_core::provenance::MAX_BUILD_PROVENANCE_BYTES);
}

#[test]
fn compiled_input_digest_matches_the_engine_package_input_set() {
    let build = earph_engine::build_provenance();
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let record = serde_json::to_value(build.record.as_ref().unwrap()).unwrap();
    assert_eq!(
        record["source_inputs_sha256"],
        provenance::source_digest(&root, provenance::ENGINE_INPUTS).unwrap()
    );
    assert_eq!(
        record["dependency_lock_sha256"],
        provenance::digest(&fs::read(root.join("Cargo.lock")).unwrap())
    );
}

#[test]
fn harness_record_cannot_replace_the_engine_stamp() {
    let mut build = (*earph_engine::build_provenance()).clone();
    build.record.as_mut().unwrap().producer = BuildProducer::Harness;
    let decoded = RunningBuildProvenance::decode(&serde_json::to_vec(&build).unwrap()).unwrap();
    assert!(decoded.validate_for_engine().is_err());
}

#[test]
fn compiled_stamp_probe() {
    let Some(path) = std::env::var_os("EARPH_TEST_BUILD_STAMP_PATH") else {
        return;
    };
    fs::write(
        path,
        serde_json::to_vec(earph_engine::build_provenance().as_ref()).unwrap(),
    )
    .unwrap();
}

#[test]
fn compiled_stamp_does_not_follow_a_changed_runtime_checkout() {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/test-runs")
        .join(format!(
            "build-stamp-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    fs::create_dir_all(&root).unwrap();
    let output = root.join("observed.json");
    let probe = || {
        let result = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "compiled_stamp_probe"])
            .current_dir(&root)
            .env("EARPH_TEST_BUILD_STAMP_PATH", &output)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "probe failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        fs::read(&output).unwrap()
    };
    fs::write(root.join("Cargo.lock"), b"initial runtime checkout").unwrap();
    let before = probe();
    fs::write(root.join("Cargo.lock"), b"changed runtime checkout").unwrap();
    assert_eq!(before, probe());
    assert_eq!(
        RunningBuildProvenance::decode(&before).unwrap(),
        *earph_engine::build_provenance()
    );
    println!("Build stamp fixture retained at {}", root.display());
}

#[test]
fn restored_session_preview_uses_the_current_compiled_engine_stamp() {
    use earph_core::{
        control::{ControlLimits, decode_command},
        diagnostics::{DiagnosticBundle, PreviewRead},
        ids::{OwnerId, ZoneId},
        session::Session,
    };
    let mut previous = (*earph_engine::build_provenance()).clone();
    previous.record.as_mut().unwrap().source_inputs_sha256 = "0".repeat(64).try_into().unwrap();
    let old_session = Session::new_owner(
        ZoneId::new("private-zone").unwrap(),
        OwnerId::new("previous-owner").unwrap(),
        ControlLimits::default(),
    )
    .with_diagnostic_build(std::sync::Arc::new(previous))
    .unwrap();
    let mut restored = Session::restore_owner(
        ZoneId::new("private-zone").unwrap(),
        OwnerId::new("current-owner").unwrap(),
        ControlLimits::default(),
        old_session.checkpoint(),
    )
    .unwrap()
    .with_diagnostic_build(earph_engine::build_provenance())
    .unwrap();
    let command = decode_command(
        &serde_json::to_vec(&serde_json::json!({
            "schema_version":1,"request_id":"build-preview","zone_id":"private-zone",
            "owner_id":"current-owner","session_id":null,"expected_revision":null,
            "payload":{"method":"diagnostics.preview"}
        }))
        .unwrap(),
        &ControlLimits::default(),
    )
    .unwrap();
    let receipt = serde_json::to_value(restored.submit(command)).unwrap();
    let preview =
        PreviewRead::decode(&serde_json::to_vec(&receipt["result"]["value"]).unwrap()).unwrap();
    let bundle = DiagnosticBundle::decode(preview.report.bytes.as_bytes()).unwrap();
    assert_eq!(bundle.running_build, *earph_engine::build_provenance());
}
