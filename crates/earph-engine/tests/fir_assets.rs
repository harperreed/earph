// ABOUTME: Exercises real correction imports through the public engine executable.
// ABOUTME: Verifies immutable store identity, private ownership, and corrupt-object rejection.
mod support;
use serde_json::Value;
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Command};
fn wave() -> Vec<u8> {
    let values = [0.5_f64, -0.25, 0.0];
    let mut bytes = Vec::new();
    bytes.extend(b"RIFF");
    bytes.extend(60_u32.to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16_u32.to_le_bytes());
    bytes.extend(3_u16.to_le_bytes());
    bytes.extend(1_u16.to_le_bytes());
    bytes.extend(48000_u32.to_le_bytes());
    bytes.extend(384000_u32.to_le_bytes());
    bytes.extend(8_u16.to_le_bytes());
    bytes.extend(64_u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(24_u32.to_le_bytes());
    for value in values {
        bytes.extend(value.to_le_bytes());
    }
    bytes
}
fn import(runtime: &Path, source: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_earph-engine"))
        .args(["import-fir", "--runtime"])
        .arg(runtime)
        .arg(source)
        .output()
        .unwrap()
}
#[test]
fn cli_import_is_idempotent_private_and_bound_to_immutable_bytes() {
    let runtime = support::runtime();
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let source = runtime.join("original.wav");
    let original = wave();
    fs::write(&source, &original).unwrap();
    let first = import(&runtime, &source);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let report: Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(report["status"], "pass");
    assert_eq!(report["asset"]["taps_per_channel"], 3);
    let second = import(&runtime, &source);
    assert!(second.status.success());
    assert_eq!(
        serde_json::from_slice::<Value>(&second.stdout).unwrap()["asset"],
        report["asset"]
    );
    let path = runtime.join("assets").join(format!(
        "{}.wav",
        report["asset"]["sha256"].as_str().unwrap()
    ));
    assert_eq!(fs::read(&path).unwrap(), original);
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    fs::write(&source, b"changed source").unwrap();
    let reference = serde_json::from_value(report["asset"].clone()).unwrap();
    assert_eq!(
        earph_engine::assets::resolve(&runtime, &reference)
            .unwrap()
            .taps[0],
        [0.5, -0.25, 0.0]
    );
    fs::write(&path, b"corrupt stored bytes").unwrap();
    assert!(earph_engine::assets::resolve(&runtime, &reference).is_err());
    fs::write(&source, &original).unwrap();
    assert!(!import(&runtime, &source).status.success());
    assert_eq!(fs::read(&path).unwrap(), b"corrupt stored bytes");
}

#[test]
fn correction_store_rejects_symlinks_and_readable_objects() {
    use std::os::unix::fs::symlink;
    let runtime = support::runtime();
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let source = runtime.join("source.wav");
    fs::write(&source, wave()).unwrap();
    let output = import(&runtime, &source);
    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout).unwrap();
    let reference = serde_json::from_value(report["asset"].clone()).unwrap();
    let object = runtime.join("assets").join(format!(
        "{}.wav",
        report["asset"]["sha256"].as_str().unwrap()
    ));
    fs::set_permissions(&object, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(
        earph_engine::assets::resolve(&runtime, &reference)
            .err()
            .unwrap()
            .code,
        earph_core::ErrorCode::AuthDenied
    );
    assert!(!import(&runtime, &source).status.success());
    let linked = support::runtime();
    let target = support::runtime();
    for path in [&linked, &target] {
        fs::create_dir(path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let source = linked.join("source.wav");
    fs::write(&source, wave()).unwrap();
    symlink(&target, linked.join("assets")).unwrap();
    assert!(!import(&linked, &source).status.success());
    assert_eq!(fs::read_dir(&target).unwrap().count(), 0);
}

#[test]
fn direct_fir_import_cannot_write_beside_a_locked_owner() {
    let runtime = support::runtime();
    let owner = earph_engine::EngineOwner::bind(&runtime).unwrap();
    let source = runtime.join("authored.wav");
    fs::write(&source, wave()).unwrap();
    let result = earph_engine::assets::import(&runtime, &source);
    assert_eq!(result.unwrap_err().code, earph_core::ErrorCode::SourceBusy);
    assert!(!runtime.join("assets").exists());
    drop(owner);
    assert!(earph_engine::assets::import(&runtime, &source).is_ok());
}

#[test]
fn non_utf8_fir_paths_keep_direct_import_and_explain_the_owner_wire_limit() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let runtime = support::runtime();
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let source = runtime.join(OsString::from_vec(b"owned-\xff.wav".to_vec()));
    let original = wave();
    fs::write(&source, &original).unwrap();
    let first = import(&runtime, &source);
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    let expected: Value = serde_json::from_slice(&first.stdout).unwrap();
    let owner = earph_engine::EngineOwner::bind(&runtime).unwrap();
    let forwarded = import(&runtime, &source);
    drop(owner);
    assert_eq!(forwarded.status.code(), Some(3));
    let error: Value = serde_json::from_slice(&forwarded.stderr).unwrap();
    assert_eq!(error["error"]["code"], "ASSET_INVALID");
    assert_eq!(
        error["error"]["message"],
        "owner FIR forwarding requires UTF-8 paths"
    );
    assert_eq!(fs::read(&source).unwrap(), original);
    let direct_retry = import(&runtime, &source);
    assert!(direct_retry.status.success());
    let retried: Value = serde_json::from_slice(&direct_retry.stdout).unwrap();
    assert_eq!(retried["asset"], expected["asset"]);
}

#[test]
fn existing_fir_cli_forwards_to_the_running_owner() {
    use serde_json::json;
    let runtime = support::runtime();
    let owner = earph_engine::EngineOwner::bind(&runtime).unwrap();
    let source = runtime.join("authored.wav");
    fs::write(&source, wave()).unwrap();
    let host = std::thread::spawn(move || owner.serve().unwrap());
    let result = Command::new(env!("CARGO_BIN_EXE_earph-engine"))
        .args(["import-fir", "--runtime"])
        .arg(&runtime)
        .arg("authored.wav")
        .current_dir(&runtime)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["status"], "pass");
    let query = json!({"schema_version":1,"request_id":"after-import","zone_id":"main","owner_id":null,"session_id":null,"expected_revision":null,"payload":{"method":"state.get"}});
    let reply: Value = serde_json::from_slice(
        &earph_engine::request(&runtime, &serde_json::to_vec(&query).unwrap()).unwrap(),
    )
    .unwrap();
    let state = &reply["receipt"]["result"]["value"];
    assert_eq!(
        state["revision"], 1,
        "CLI bypassed owner admission: {reply}"
    );
    let shutdown = json!({"schema_version":1,"request_id":"shutdown","zone_id":"main","owner_id":state["owner_id"],"session_id":null,"expected_revision":state["revision"],"payload":{"method":"owner.shutdown"}});
    let reply: Value = serde_json::from_slice(
        &earph_engine::request(&runtime, &serde_json::to_vec(&shutdown).unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(reply["receipt"]["status"], "applied");
    host.join().unwrap();
}
