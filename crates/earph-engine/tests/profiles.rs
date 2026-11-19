// ABOUTME: Saves real listening profiles through the authoritative owner and JSON client.
// ABOUTME: Verifies immutable objects, stale edits, safety preservation, and restart recovery.
use earph_engine::{EngineOwner, request};
use serde_json::{Value, json};
use std::{
    path::Path,
    thread,
    time::{Duration, Instant},
};
#[path = "support/graphs.rs"]
mod graph_fixtures;
mod support;

fn graph() -> Value {
    let port = json!({"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"signed16","max_quantum_frames":64});
    json!({"graph":{"schema_version":1,"output":"out","nodes":[
        {"id":"in","kind":{"kind":"input"},"format":port},
        {"id":"out","kind":{"kind":"output"},"format":port}],
        "edges":[{"from":"in","to":"out","from_port":0,"to_port":0}]},
        "processing":{"kind":"integer_bypass"}})
}
fn draft() -> Value {
    json!({"name":"Desk headphones","source_policy":"local_file","fidelity":"compatible_conversion",
        "output":{"backend":"alsa","route":"system","device_id":"desk-dac","manual_setup":"headphones"},
        "volume":{"mode":"software","software_cap_millidecibels":-20000,"hardware_cap_millidecibels":null},
        "latency":{"mode":"balanced","max_resident_bytes":268435456,"max_preparation_bytes":268435456,"max_latency_frames":null},"corrections":[]})
}
fn envelope(id: &str, state: &Value, payload: Value) -> Value {
    json!({"schema_version":1,"request_id":id,"zone_id":"main","owner_id":state["owner_id"],
        "session_id":state["session_id"],"expected_revision":state["revision"],"payload":payload})
}
fn call(runtime: &Path, command: &Value) -> Value {
    serde_json::from_slice(&request(runtime, &serde_json::to_vec(command).unwrap()).unwrap())
        .unwrap()
}
fn state(runtime: &Path) -> Value {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let id = format!(
        "state-{}",
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    call(
        runtime,
        &envelope(&id, &Value::Null, json!({"method":"state.get"})),
    )["receipt"]["result"]["value"]
        .clone()
}
fn finish(runtime: &Path, command: &Value) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let result = call(runtime, command);
        if result["receipt"]["status"] != "accepted" {
            return result;
        }
        assert!(Instant::now() < deadline, "save never settled: {result}");
        thread::sleep(Duration::from_millis(2));
    }
}
fn shutdown(runtime: &Path) {
    let result = call(
        runtime,
        &envelope(
            "shutdown",
            &state(runtime),
            json!({"method":"owner.shutdown"}),
        ),
    );
    assert_eq!(result["receipt"]["status"], "applied", "{result}");
}

#[test]
fn owner_advertises_bounded_application_and_refuses_imported_device_trust() {
    let runtime = support::runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let host = thread::spawn(move || owner.serve().unwrap());
    let before = state(&runtime);
    let capabilities = call(
        &runtime,
        &envelope(
            "capabilities",
            &before,
            json!({"method":"capabilities.get"}),
        ),
    );
    let profiles = &capabilities["receipt"]["result"]["value"]["profiles"];
    assert_eq!(profiles["apply"], true);
    assert_eq!(profiles["exact_associations"], false);
    assert_eq!(
        profiles["application"],
        json!({"volume_modes":["software"],"requires_current_output":true,
        "requires_output_consent":true,"requires_matching_fidelity":true,
        "live_selected_profile_update":false,"finite_aggregate_src_latency":false})
    );
    let bind = envelope(
        "bind",
        &before,
        json!({"method":"profiles.bind","id":"headphones",
        "expected_profile_revision":1,"profile_sha256":"c".repeat(64),"expected_device_epoch":0}),
    );
    let denied = call(&runtime, &bind);
    assert_eq!(denied["receipt"]["error"]["code"], "CAPABILITY_UNSUPPORTED");
    let mut imported_trust = bind;
    imported_trust["payload"]["device_identity_sha256"] = json!("d".repeat(64));
    assert_eq!(
        earph_core::control::decode_command(
            &serde_json::to_vec(&imported_trust).unwrap(),
            &earph_core::control::ControlLimits::default()
        )
        .unwrap_err()
        .code,
        earph_core::ErrorCode::AssetInvalid
    );
    assert_eq!(state(&runtime)["revision"], before["revision"]);
    shutdown(&runtime);
    host.join().unwrap();
}

#[test]
fn real_owner_saves_updates_resolves_and_restores_without_output_authority() {
    let runtime = support::runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let host = thread::spawn(move || owner.serve().unwrap());
    let before = state(&runtime);
    let create = envelope(
        "create",
        &before,
        json!({"method":"profiles.create","id":"headphones","draft":draft(),"graph":graph()}),
    );
    let saved = finish(&runtime, &create);
    assert_eq!(saved["receipt"]["status"], "applied", "{saved}");
    assert_eq!(
        saved["receipt"]["result"]["value"]["profile"]["revision"],
        1
    );
    let after = state(&runtime);
    for field in [
        "muted",
        "software_gain_db",
        "output",
        "output_consented",
        "volume_policy",
        "graph_epoch",
    ] {
        assert_eq!(after[field], before[field], "{field}");
    }
    assert_eq!(after["revision"], 1);
    let get = envelope(
        "get",
        &Value::Null,
        json!({"method":"profiles.get","id":"headphones"}),
    );
    let fetched = call(&runtime, &get);
    assert_eq!(fetched["receipt"]["result"]["value"]["graph"], graph());
    let hash = fetched["receipt"]["result"]["value"]["profile_sha256"]
        .as_str()
        .unwrap();
    assert!(
        runtime
            .join("profiles")
            .join(format!("{hash}.profile.json"))
            .exists()
    );
    let list = call(
        &runtime,
        &envelope(
            "list",
            &Value::Null,
            json!({"method":"profiles.list","offset":0,"limit":16}),
        ),
    );
    assert_eq!(list["receipt"]["result"]["value"]["total"], 1);
    let resolve = call(
        &runtime,
        &envelope(
            "resolve",
            &Value::Null,
            json!({"method":"profiles.resolve","manual_profile_id":"headphones","output":draft()["output"]}),
        ),
    );
    assert_eq!(resolve["receipt"]["result"]["value"]["reason"], "manual");
    let update = envelope(
        "update",
        &after,
        json!({"method":"profiles.update","id":"headphones","expected_profile_revision":1,"draft":draft(),"graph":graph()}),
    );
    assert_eq!(finish(&runtime, &update)["receipt"]["status"], "applied");
    let stale = envelope(
        "stale",
        &state(&runtime),
        json!({"method":"profiles.update","id":"headphones","expected_profile_revision":1,"draft":draft(),"graph":graph()}),
    );
    assert_eq!(
        call(&runtime, &stale)["receipt"]["error"]["code"],
        "STALE_REVISION"
    );
    shutdown(&runtime);
    host.join().unwrap();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let host = thread::spawn(move || owner.serve().unwrap());
    assert_eq!(
        call(&runtime, &get)["receipt"]["result"]["value"]["profile"]["revision"],
        2
    );
    let restored = state(&runtime);
    assert_eq!(restored["transport"], "paused");
    assert_eq!(restored["muted"], true);
    assert_eq!(restored["output_consented"], false);
    shutdown(&runtime);
    host.join().unwrap();
}

#[test]
fn catalog_limit_pages_and_unverified_resolution_remain_explicit() {
    let runtime = support::runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let host = thread::spawn(move || owner.serve().unwrap());
    for n in 0..32 {
        let save = envelope(
            &format!("save-{n}"),
            &state(&runtime),
            json!({"method":"profiles.create","id":format!("desk-{n:02}"),"draft":draft(),"graph":graph()}),
        );
        let result = finish(&runtime, &save);
        assert_eq!(
            result["receipt"]["status"], "applied",
            "profile {n}: {result}"
        );
    }
    let extra = envelope(
        "extra",
        &state(&runtime),
        json!({"method":"profiles.create","id":"extra","draft":draft(),"graph":graph()}),
    );
    assert_eq!(
        call(&runtime, &extra)["receipt"]["error"]["code"],
        "RESOURCE_BUDGET_EXCEEDED"
    );
    for offset in [0, 16, 32] {
        let list = call(
            &runtime,
            &envelope(
                &format!("list-{offset}"),
                &Value::Null,
                json!({"method":"profiles.list","offset":offset,"limit":16}),
            ),
        );
        let page = &list["receipt"]["result"]["value"];
        assert_eq!(page["total"], 32);
        assert_eq!(
            page["entries"].as_array().unwrap().len(),
            if offset < 32 { 16 } else { 0 }
        );
    }
    let resolve = call(
        &runtime,
        &envelope(
            "auto",
            &Value::Null,
            json!({"method":"profiles.resolve","manual_profile_id":null,"output":draft()["output"]}),
        ),
    );
    assert_eq!(
        resolve["receipt"]["result"]["value"]["reason"],
        "no_verified_match"
    );
    let wrong =
        json!({"backend":"alsa","route":"system","device_id":"another-dac","manual_setup":null});
    let resolve = call(
        &runtime,
        &envelope(
            "wrong-device",
            &Value::Null,
            json!({"method":"profiles.resolve","manual_profile_id":"desk-00","output":wrong}),
        ),
    );
    assert_eq!(resolve["receipt"]["error"]["code"], "DEVICE_UNAVAILABLE");
    shutdown(&runtime);
    host.join().unwrap();
}

#[test]
fn invalid_topology_and_unavailable_correction_cannot_create_catalog_entries() {
    let runtime = support::runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let host = thread::spawn(move || owner.serve().unwrap());
    let mut invalid = graph();
    invalid["graph"]["edges"] = json!([]);
    let save = envelope(
        "invalid",
        &state(&runtime),
        json!({"method":"profiles.create","id":"invalid","draft":draft(),"graph":invalid}),
    );
    assert_eq!(
        finish(&runtime, &save)["receipt"]["error"]["code"],
        "GRAPH_INVALID"
    );
    let mut unsupported = draft();
    unsupported["corrections"] = json!([{"kind":"text_report","object_sha256":"0".repeat(64)}]);
    let save = envelope(
        "unsupported",
        &state(&runtime),
        json!({"method":"profiles.create","id":"invalid","draft":unsupported,"graph":graph()}),
    );
    assert_eq!(
        finish(&runtime, &save)["receipt"]["error"]["code"],
        "CAPABILITY_UNSUPPORTED"
    );
    assert_eq!(state(&runtime)["revision"], 0);
    shutdown(&runtime);
    host.join().unwrap();
}

#[test]
fn corrupted_immutable_graph_refuses_update_and_restore_without_overwrite() {
    use std::fs;
    let runtime = support::runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let host = thread::spawn(move || owner.serve().unwrap());
    let save = envelope(
        "save",
        &state(&runtime),
        json!({"method":"profiles.create","id":"desk","draft":draft(),"graph":graph()}),
    );
    let saved = finish(&runtime, &save);
    assert_eq!(saved["receipt"]["status"], "applied");
    let hash = saved["receipt"]["result"]["value"]["profile"]["graph_sha256"]
        .as_str()
        .unwrap();
    let path = runtime.join("profiles").join(format!("{hash}.graph.json"));
    fs::write(&path, b"corrupt test object").unwrap();
    let update = envelope(
        "update",
        &state(&runtime),
        json!({"method":"profiles.update","id":"desk","expected_profile_revision":1,"draft":draft(),"graph":graph()}),
    );
    assert_eq!(
        finish(&runtime, &update)["receipt"]["error"]["code"],
        "ASSET_INVALID"
    );
    assert_eq!(fs::read(&path).unwrap(), b"corrupt test object");
    assert_eq!(state(&runtime)["revision"], 1);
    shutdown(&runtime);
    host.join().unwrap();
    assert_eq!(
        EngineOwner::bind(&runtime).err().unwrap().code,
        earph_core::ErrorCode::AssetInvalid
    );
}

#[test]
fn actual_owner_and_request_executables_save_and_restore_the_same_intent() {
    use std::{
        io::{BufRead, BufReader, Write},
        process::{Command, Stdio},
    };
    let runtime = support::runtime();
    for generation in 0..2 {
        let mut host = Command::new(env!("CARGO_BIN_EXE_earph-engine"))
            .args(["owner", "--runtime"])
            .arg(&runtime)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(host.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&line).unwrap()["status"],
            "owner"
        );
        if generation == 0 {
            let save = envelope(
                "cli-save",
                &state(&runtime),
                json!({"method":"profiles.create","id":"cli-desk","draft":draft(),"graph":graph()}),
            );
            let mut child = Command::new(env!("CARGO_BIN_EXE_earph-engine"))
                .args(["request", "--runtime"])
                .arg(&runtime)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            writeln!(child.stdin.take().unwrap(), "{save}").unwrap();
            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(finish(&runtime, &save)["receipt"]["status"], "applied");
        }
        let get = envelope(
            "cli-get",
            &Value::Null,
            json!({"method":"profiles.get","id":"cli-desk"}),
        );
        assert_eq!(
            call(&runtime, &get)["receipt"]["result"]["value"]["graph"],
            graph()
        );
        assert_eq!(state(&runtime)["muted"], true);
        shutdown(&runtime);
        assert!(host.wait().unwrap().success());
    }
}

#[test]
fn saved_fir_graph_requires_verified_asset_bytes_and_exact_geometry() {
    use std::{fs, os::unix::fs::DirBuilderExt};
    let runtime = support::runtime();
    fs::DirBuilder::new().mode(0o700).create(&runtime).unwrap();
    let source = runtime.join("authored-correction.wav");
    let mut bytes = Vec::new();
    bytes.extend(b"RIFF");
    bytes.extend(44_u32.to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16_u32.to_le_bytes());
    bytes.extend(3_u16.to_le_bytes());
    bytes.extend(1_u16.to_le_bytes());
    bytes.extend(48000_u32.to_le_bytes());
    bytes.extend(384000_u32.to_le_bytes());
    bytes.extend(8_u16.to_le_bytes());
    bytes.extend(64_u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(8_u32.to_le_bytes());
    bytes.extend(0.5_f64.to_le_bytes());
    fs::write(&source, bytes).unwrap();
    let reference = earph_engine::assets::import(&runtime, &source).unwrap();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let mut intent = graph();
    for node in intent["graph"]["nodes"].as_array_mut().unwrap() {
        node["format"]["precision"] = json!("float64");
        node["format"]["channels"] = json!("mono");
    }
    let port = intent["graph"]["nodes"][0]["format"].clone();
    intent["graph"]["nodes"].as_array_mut().unwrap().insert(
        1,
        json!({"id":"fir","kind":{"kind":"convolution","asset":reference},"format":port}),
    );
    intent["graph"]["edges"] = json!([{"from":"in","to":"fir","from_port":0,"to_port":0},{"from":"fir","to":"out","from_port":0,"to_port":0}]);
    intent["processing"] =
        json!({"kind":"float64","quantization":"round_ties_away_saturate_no_dither"});
    let host = thread::spawn(move || owner.serve().unwrap());
    let save = envelope(
        "fir-save",
        &state(&runtime),
        json!({"method":"profiles.create","id":"fir","draft":draft(),"graph":intent}),
    );
    let saved = finish(&runtime, &save);
    assert_eq!(saved["receipt"]["status"], "applied", "{saved}");
    intent["graph"]["nodes"][1]["kind"]["asset"]["taps_per_channel"] = json!(2);
    let wrong = envelope(
        "fir-wrong",
        &state(&runtime),
        json!({"method":"profiles.create","id":"wrong","draft":draft(),"graph":intent}),
    );
    assert_eq!(
        finish(&runtime, &wrong)["receipt"]["error"]["code"],
        "ASSET_INVALID"
    );
    intent["graph"]["nodes"][1]["kind"]["asset"]["sha256"] = json!("0".repeat(64));
    let absent = envelope(
        "fir-absent",
        &state(&runtime),
        json!({"method":"profiles.create","id":"absent","draft":draft(),"graph":intent}),
    );
    assert_eq!(
        finish(&runtime, &absent)["receipt"]["error"]["code"],
        "ASSET_INVALID"
    );
    shutdown(&runtime);
    host.join().unwrap();
}

#[test]
fn integer_meter_intent_reports_unsupported_instead_of_saving_a_bypass() {
    let runtime = support::runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let host = thread::spawn(move || owner.serve().unwrap());
    let mut intent = graph();
    let port = intent["graph"]["nodes"][0]["format"].clone();
    intent["graph"]["nodes"].as_array_mut().unwrap().insert(
        1,
        json!({"id":"meter","kind":{"kind":"meter"},"format":port}),
    );
    intent["graph"]["edges"] = json!([{"from":"in","to":"meter","from_port":0,"to_port":0},{"from":"meter","to":"out","from_port":0,"to_port":0}]);
    let save = envelope(
        "meter",
        &state(&runtime),
        json!({"method":"profiles.create","id":"meter","draft":draft(),"graph":intent}),
    );
    let result = finish(&runtime, &save);
    assert_eq!(
        result["receipt"]["error"]["code"], "CAPABILITY_UNSUPPORTED",
        "{result}"
    );
    shutdown(&runtime);
    host.join().unwrap();
}

#[test]
fn checkpoint_failure_retires_owner_and_preserves_the_prior_catalog_for_recovery() {
    use std::fs;
    let runtime = support::runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let host = thread::spawn(move || owner.serve());
    let save = envelope(
        "save",
        &state(&runtime),
        json!({"method":"profiles.create","id":"desk","draft":draft(),"graph":graph()}),
    );
    assert_eq!(finish(&runtime, &save)["receipt"]["status"], "applied");
    let prior = fs::read(runtime.join("queue.json")).unwrap();
    fs::rename(
        runtime.join("queue.json"),
        runtime.join("preserved-baseline.json"),
    )
    .unwrap();
    fs::create_dir(runtime.join("queue.json")).unwrap();
    let update = envelope(
        "update",
        &state(&runtime),
        json!({"method":"profiles.update","id":"desk","expected_profile_revision":1,"draft":draft(),"graph":graph()}),
    );
    let accepted = call(&runtime, &update);
    assert_eq!(accepted["receipt"]["status"], "accepted");
    assert_eq!(
        host.join().unwrap().unwrap_err().code,
        earph_core::ErrorCode::AuthDenied
    );
    assert_eq!(
        fs::read(runtime.join("preserved-baseline.json")).unwrap(),
        prior
    );
    assert_eq!(fs::read_dir(runtime.join("profiles")).unwrap().count(), 3);
    fs::rename(
        runtime.join("queue.json"),
        runtime.join("preserved-obstruction"),
    )
    .unwrap();
    fs::rename(
        runtime.join("preserved-baseline.json"),
        runtime.join("queue.json"),
    )
    .unwrap();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let host = thread::spawn(move || owner.serve().unwrap());
    let get = call(
        &runtime,
        &envelope(
            "restored",
            &Value::Null,
            json!({"method":"profiles.get","id":"desk"}),
        ),
    );
    assert_eq!(get["receipt"]["result"]["value"]["profile"]["revision"], 1);
    assert_eq!(state(&runtime)["muted"], true);
    shutdown(&runtime);
    host.join().unwrap();
}

#[test]
fn a_real_checkpoint_without_its_objects_refuses_recovery_instead_of_an_empty_catalog() {
    use std::{
        fs,
        os::unix::fs::{DirBuilderExt, PermissionsExt},
    };
    let runtime = support::runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let host = thread::spawn(move || owner.serve().unwrap());
    let save = envelope(
        "save",
        &state(&runtime),
        json!({"method":"profiles.create","id":"desk","draft":draft(),"graph":graph()}),
    );
    assert_eq!(finish(&runtime, &save)["receipt"]["status"], "applied");
    shutdown(&runtime);
    host.join().unwrap();
    let copied = support::runtime();
    fs::DirBuilder::new().mode(0o700).create(&copied).unwrap();
    let bytes = fs::read(runtime.join("queue.json")).unwrap();
    fs::write(copied.join("queue.json"), &bytes).unwrap();
    fs::set_permissions(copied.join("queue.json"), fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        EngineOwner::bind(&copied).err().unwrap().code,
        earph_core::ErrorCode::AssetInvalid
    );
    assert_eq!(fs::read(copied.join("queue.json")).unwrap(), bytes);
}

#[test]
fn offline_src_validation_enforces_declared_preparation_and_latency_limits() {
    let runtime = support::runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let host = thread::spawn(move || owner.serve().unwrap());
    let intent = json!({"graph":graph_fixtures::src_roundtrip(),"processing":{"kind":"float64","quantization":"round_ties_away_saturate_no_dither"}});
    let mut bounded = draft();
    bounded["latency"]["max_latency_frames"] = json!(1024);
    let finite = envelope(
        "finite",
        &state(&runtime),
        json!({"method":"profiles.create","id":"finite","draft":bounded,"graph":intent}),
    );
    assert_eq!(
        finish(&runtime, &finite)["receipt"]["error"]["code"],
        "CAPABILITY_UNSUPPORTED"
    );
    bounded["latency"]["max_latency_frames"] = Value::Null;
    bounded["latency"]["max_resident_bytes"] = json!(1024);
    bounded["latency"]["max_preparation_bytes"] = json!(1024);
    let tiny = envelope(
        "tiny",
        &state(&runtime),
        json!({"method":"profiles.create","id":"tiny","draft":bounded,"graph":intent}),
    );
    assert_eq!(
        finish(&runtime, &tiny)["receipt"]["error"]["code"],
        "RESOURCE_BUDGET_EXCEEDED"
    );
    let valid = envelope(
        "valid",
        &state(&runtime),
        json!({"method":"profiles.create","id":"valid","draft":draft(),"graph":intent}),
    );
    let saved = finish(&runtime, &valid);
    assert_eq!(saved["receipt"]["status"], "applied", "{saved}");
    assert_eq!(state(&runtime)["output"], Value::Null);
    shutdown(&runtime);
    host.join().unwrap();
}

#[test]
fn owner_imports_a_real_report_and_original_source_without_changing_playback_authority() {
    use earph_media::correction_text::{
        CorrectionFormat, ImportContext, ImportMetadata, import_correction_text,
    };
    let source =
        include_str!("../../earph-media/tests/fixtures/correction/owned-autoeq-shelves.txt");
    let report = import_correction_text(
        source.as_bytes(),
        CorrectionFormat::AutoEq,
        ImportContext {
            sample_rate_hz: 48000,
            channels: earph_core::audio::ChannelLayout::Stereo,
            quantum_frames: 64,
        },
        ImportMetadata {
            headphone_model: Some("Owned fixture".into()),
            redistribution_license: Some("Owned test data".into()),
            ..Default::default()
        },
    )
    .unwrap();
    let runtime = support::runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let host = thread::spawn(move || owner.serve().unwrap());
    let before = state(&runtime);
    let import = envelope(
        "import-report",
        &before,
        json!({"method":"profiles.import_correction", "id":"imported", "expected_profile_revision":null,
        "draft":draft(), "input":{"kind":"text_report", "report_json":serde_json::to_string(&report).unwrap(), "source_text":source}}),
    );
    let saved = finish(&runtime, &import);
    assert_eq!(saved["receipt"]["status"], "applied", "{saved}");
    let get = call(
        &runtime,
        &envelope(
            "get-import",
            &Value::Null,
            json!({"method":"profiles.get","id":"imported"}),
        ),
    );
    let record = &get["receipt"]["result"]["value"];
    assert_eq!(
        record["graph"]["graph"],
        serde_json::to_value(&report.graph).unwrap()
    );
    let document = record["corrections"][0]["document_json"].as_str().unwrap();
    assert_eq!(
        earph_media::correction_text::read_correction_report(document.as_bytes()).unwrap(),
        report
    );
    assert_eq!(record["profile"]["corrections"][0]["kind"], "text_report");
    assert!(record["correction_preview"]["required_headroom_db"].is_number());
    let after = state(&runtime);
    for field in [
        "output",
        "muted",
        "software_gain_db",
        "output_consented",
        "volume_policy",
        "graph_epoch",
    ] {
        assert_eq!(after[field], before[field]);
    }
    shutdown(&runtime);
    host.join().unwrap();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let host = thread::spawn(move || owner.serve().unwrap());
    let restored = call(
        &runtime,
        &envelope(
            "get-restored-import",
            &Value::Null,
            json!({"method":"profiles.get","id":"imported"}),
        ),
    );
    assert_eq!(
        restored["receipt"]["result"]["value"]["corrections"],
        record["corrections"]
    );
    shutdown(&runtime);
    host.join().unwrap();
}

#[test]
fn owner_imports_real_fir_provenance_and_refuses_wrong_context_without_a_profile() {
    let runtime = support::runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let source = runtime.join("owned.wav");
    let mut bytes = Vec::new();
    bytes.extend(b"RIFF");
    bytes.extend(44_u32.to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16_u32.to_le_bytes());
    bytes.extend(3_u16.to_le_bytes());
    bytes.extend(1_u16.to_le_bytes());
    bytes.extend(48000_u32.to_le_bytes());
    bytes.extend(384000_u32.to_le_bytes());
    bytes.extend(8_u16.to_le_bytes());
    bytes.extend(64_u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(8_u32.to_le_bytes());
    bytes.extend(0.5_f64.to_le_bytes());
    std::fs::write(&source, &bytes).unwrap();
    let input = json!({"kind":"fir","path":source,"purpose":"correction", "context":{"sample_rate_hz":48000,"channels":"mono","quantum_frames":64},
        "metadata_json":"{\"measurement_provider\":\"Owned synthetic response\",\"headphone_model\":null,\"target\":null,\"parameter_set\":null,\"revision\":null,\"redistribution_license\":null}","preamp_db":-6.0});
    let host = thread::spawn(move || owner.serve().unwrap());
    let request = envelope(
        "fir-import",
        &state(&runtime),
        json!({"method":"profiles.import_correction","id":"fir-import","expected_profile_revision":null,"draft":draft(),"input":input}),
    );
    let saved = finish(&runtime, &request);
    assert_eq!(saved["receipt"]["status"], "applied", "{saved}");
    let record = &saved["receipt"]["result"]["value"];
    let manifest = earph_media::read_fir_manifest(
        record["corrections"][0]["document_json"]
            .as_str()
            .unwrap()
            .as_bytes(),
    )
    .unwrap();
    assert_eq!(manifest.source_bytes, bytes.len());
    assert_eq!(manifest.preamp_db.get(), -6.0);
    assert_eq!(manifest.channel_map, [0]);
    assert!(manifest.metadata.redistribution_license.is_none());
    assert_eq!(record["profile"]["corrections"][0]["kind"], "fir_manifest");
    let published = || {
        [runtime.join("profiles"), runtime.join("assets")]
            .into_iter()
            .flat_map(|directory| std::fs::read_dir(directory).unwrap())
            .map(|entry| {
                let path = entry.unwrap().path();
                (
                    path.strip_prefix(&runtime).unwrap().to_owned(),
                    std::fs::read(path).unwrap(),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let original_objects = published();
    let wrong_source = runtime.join("wrong-context.wav");
    let mut wrong_bytes = bytes.clone();
    let tail = wrong_bytes.len() - 8;
    wrong_bytes[tail..].copy_from_slice(&0.25_f64.to_le_bytes());
    std::fs::write(&wrong_source, &wrong_bytes).unwrap();
    let mut wrong = input;
    wrong["path"] = json!(wrong_source);
    wrong["context"]["channels"] = json!("stereo");
    let request = envelope(
        "wrong-layout",
        &state(&runtime),
        json!({"method":"profiles.import_correction","id":"wrong-layout","expected_profile_revision":null,"draft":draft(),"input":wrong}),
    );
    assert_eq!(
        finish(&runtime, &request)["receipt"]["error"]["code"],
        "ASSET_INVALID"
    );
    assert_eq!(state(&runtime)["revision"], 1);
    assert_eq!(
        published(),
        original_objects,
        "rejected FIR context published candidate dependencies"
    );
    assert_eq!(std::fs::read(wrong_source).unwrap(), wrong_bytes);
    assert_eq!(std::fs::read(source).unwrap(), bytes);
    shutdown(&runtime);
    host.join().unwrap();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let host = thread::spawn(move || owner.serve().unwrap());
    let get = call(
        &runtime,
        &envelope(
            "fir-restored",
            &Value::Null,
            json!({"method":"profiles.get","id":"fir-import"}),
        ),
    );
    assert_eq!(
        get["receipt"]["result"]["value"]["corrections"],
        record["corrections"]
    );
    shutdown(&runtime);
    host.join().unwrap();
}

#[path = "support/correction.rs"]
mod correction_fixtures;

fn request_cli(runtime: &Path, command: &Value) -> Value {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    let mut child = Command::new(env!("CARGO_BIN_EXE_earph-engine"))
        .args(["request", "--runtime"])
        .arg(runtime)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    writeln!(child.stdin.take().unwrap(), "{command}").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn real_correction_and_request_clis_roundtrip_reports_and_render_saved_controls() {
    use earph_media::correction_text::{import_correction_text, read_correction_report};
    use std::process::Command;
    let correction_cli = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("earph-correction-import");
    assert!(
        correction_cli.is_file(),
        "build the actual earph-media earph-correction-import binary first"
    );
    let runtime = support::runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let host = thread::spawn(move || owner.serve().unwrap());
    let fixtures =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../earph-media/tests/fixtures/correction");
    for (index, (format, filename, shelves, expected)) in [
        ("rew", "owned-rew.txt", false, [-6.0, -6.0]),
        ("autoeq", "owned-autoeq.txt", false, [-12.0, -12.0]),
        ("autoeq", "owned-autoeq-shelves.txt", true, [0.0, 0.0]),
        ("camilladsp", "owned-camilladsp.yaml", false, [-12.0, -6.0]),
        (
            "camilladsp",
            "owned-camilladsp-shelves.yaml",
            true,
            [0.0, 0.0],
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let source = std::fs::read_to_string(fixtures.join(filename)).unwrap();
        let source_path = runtime.join(format!("owned-{index}.txt"));
        std::fs::write(&source_path, &source).unwrap();
        let output = Command::new(&correction_cli)
            .args([
                "--format",
                format,
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
                "--license",
                "Owned test data",
                "--",
            ])
            .arg(&source_path)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report = read_correction_report(&output.stdout).unwrap();
        let command = envelope(
            &format!("cli-import-{index}"),
            &state(&runtime),
            json!({"method":"profiles.import_correction","id":format!("cli-{index}"),
            "expected_profile_revision":null,"draft":draft(),"input":{"kind":"text_report","report_json":String::from_utf8(output.stdout).unwrap(),"source_text":source}}),
        );
        let accepted = request_cli(&runtime, &command);
        assert!(
            ["accepted", "applied"].contains(&accepted["receipt"]["status"].as_str().unwrap()),
            "{accepted}"
        );
        let saved = finish(&runtime, &command);
        assert_eq!(saved["receipt"]["status"], "applied", "{saved}");
        let get = envelope(
            &format!("cli-get-{index}"),
            &Value::Null,
            json!({"method":"profiles.get","id":format!("cli-{index}")}),
        );
        let fetched = request_cli(&runtime, &get);
        let record = &fetched["receipt"]["result"]["value"];
        let retrieved = record["corrections"][0]["document_json"].as_str().unwrap();
        assert_eq!(
            read_correction_report(retrieved.as_bytes()).unwrap(),
            report
        );
        let report_path = runtime.join(format!("report-{index}.json"));
        std::fs::write(&report_path, retrieved).unwrap();
        let checked = Command::new(&correction_cli)
            .arg("--validate-report")
            .arg(&report_path)
            .output()
            .unwrap();
        assert!(checked.status.success());
        let exported = Command::new(&correction_cli)
            .arg("--export")
            .arg(&report_path)
            .output()
            .unwrap();
        assert!(exported.status.success());
        let again = import_correction_text(
            &exported.stdout,
            report.source.format,
            report.context,
            report.metadata.clone(),
        )
        .unwrap();
        assert_eq!(again.semantic_sha256, report.semantic_sha256);
        let graph: earph_core::graph::GraphDocument =
            serde_json::from_value(record["graph"]["graph"].clone()).unwrap();
        let frequency = if shelves { 3000.0 } else { 1000.0 };
        for (channel, expected) in expected.into_iter().enumerate() {
            let expected = if shelves {
                correction_fixtures::shelf_response(frequency)
            } else {
                expected
            };
            assert!(
                (correction_fixtures::measured_gain(&graph, channel, frequency) - expected).abs()
                    < 1e-7,
                "{filename} channel {channel}"
            );
        }
        assert_eq!(std::fs::read_to_string(&source_path).unwrap(), source);
        assert!(!retrieved.contains(source_path.to_str().unwrap()));
    }
    assert_eq!(state(&runtime)["revision"], 5);
    assert_eq!(state(&runtime)["output"], Value::Null);
    shutdown(&runtime);
    host.join().unwrap();
}

#[test]
fn correction_import_rejects_tampering_stale_edits_and_missing_source_recovery() {
    use earph_media::correction_text::{
        CorrectionFormat, ImportContext, ImportMetadata, import_correction_text,
    };
    let source = include_str!("../../earph-media/tests/fixtures/correction/owned-autoeq.txt");
    let report = import_correction_text(
        source.as_bytes(),
        CorrectionFormat::AutoEq,
        ImportContext {
            sample_rate_hz: 48000,
            channels: earph_core::audio::ChannelLayout::Stereo,
            quantum_frames: 64,
        },
        ImportMetadata::default(),
    )
    .unwrap();
    let report_value = serde_json::to_value(&report).unwrap();
    let runtime = support::runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let host = thread::spawn(move || owner.serve().unwrap());
    for (index, input) in [
        json!({"kind":"text_report","report_json":serde_json::to_string(&report).unwrap(),"source_text":source.replace("-6.0", "-5.0")}),
        { let mut altered=report_value.clone(); altered["source"]["sha256"]=json!("0".repeat(64)); json!({"kind":"text_report","report_json":altered.to_string(),"source_text":source}) },
        { let mut altered=report_value.clone(); altered["graph"]["schema_version"]=json!(2); json!({"kind":"text_report","report_json":altered.to_string(),"source_text":source}) },
        { let mut altered=report_value.clone(); altered["unsupported_control"]=json!(true); json!({"kind":"text_report","report_json":altered.to_string(),"source_text":source}) },
    ].into_iter().enumerate() {
        let command = envelope(&format!("tampered-{index}"), &state(&runtime), json!({"method":"profiles.import_correction","id":"correction",
            "expected_profile_revision":null,"draft":draft(),"input":input}));
        let result = finish(&runtime, &command);
        assert_eq!(result["receipt"]["status"], "failed", "{result}");
        assert_eq!(state(&runtime)["revision"], 0);
    }
    let input = json!({"kind":"text_report","report_json":serde_json::to_string(&report).unwrap(),"source_text":source});
    let create = envelope(
        "valid-correction",
        &state(&runtime),
        json!({"method":"profiles.import_correction","id":"correction",
        "expected_profile_revision":null,"draft":draft(),"input":input}),
    );
    let original = finish(&runtime, &create);
    assert_eq!(original["receipt"]["status"], "applied", "{original}");
    let stale = envelope(
        "stale-correction",
        &state(&runtime),
        json!({"method":"profiles.import_correction","id":"correction",
        "expected_profile_revision":2,"draft":draft(),"input":input}),
    );
    assert_eq!(
        finish(&runtime, &stale)["receipt"]["error"]["code"],
        "STALE_REVISION"
    );
    let mut edited = draft();
    edited["name"] = json!("Edited correction intent");
    let update = envelope(
        "updated-correction",
        &state(&runtime),
        json!({"method":"profiles.import_correction","id":"correction",
        "expected_profile_revision":1,"draft":edited,"input":input}),
    );
    let updated = finish(&runtime, &update);
    assert_eq!(updated["receipt"]["status"], "applied", "{updated}");
    assert_eq!(
        updated["receipt"]["result"]["value"]["profile"]["revision"],
        2
    );
    let original_hash = original["receipt"]["result"]["value"]["profile_sha256"]
        .as_str()
        .unwrap();
    assert!(
        runtime
            .join("profiles")
            .join(format!("{original_hash}.profile.json"))
            .is_file()
    );
    shutdown(&runtime);
    host.join().unwrap();
    let dependency = runtime
        .join("profiles")
        .join(format!("{}.text-source.txt", report.source.sha256));
    let preserved = dependency.with_extension("preserved");
    std::fs::rename(&dependency, &preserved).unwrap();
    assert_eq!(
        EngineOwner::bind(&runtime).err().unwrap().code,
        earph_core::ErrorCode::AssetInvalid
    );
    assert_eq!(std::fs::read_to_string(&preserved).unwrap(), source);
    std::fs::rename(&preserved, &dependency).unwrap();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let host = thread::spawn(move || owner.serve().unwrap());
    let fetched = call(
        &runtime,
        &envelope(
            "after-source-recovery",
            &Value::Null,
            json!({"method":"profiles.get","id":"correction"}),
        ),
    );
    assert_eq!(
        fetched["receipt"]["result"]["value"]["profile"]["revision"],
        2
    );
    assert_eq!(state(&runtime)["output"], Value::Null);
    shutdown(&runtime);
    host.join().unwrap();
}

#[test]
fn stereo_fir_profile_renders_exact_ordered_taps_with_its_saved_preamp() {
    use earph_core::{
        audio::{ChannelLayout, PcmFormat, PcmRepresentation},
        ids::Epochs,
    };
    let runtime = support::runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let source = runtime.join("stereo-correction.wav");
    let values = [0.5_f64, -0.25, 0.0, 0.125];
    let mut bytes = Vec::new();
    bytes.extend(b"RIFF");
    bytes.extend(68_u32.to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16_u32.to_le_bytes());
    bytes.extend(3_u16.to_le_bytes());
    bytes.extend(2_u16.to_le_bytes());
    bytes.extend(48000_u32.to_le_bytes());
    bytes.extend(768000_u32.to_le_bytes());
    bytes.extend(16_u16.to_le_bytes());
    bytes.extend(64_u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(32_u32.to_le_bytes());
    for value in values {
        bytes.extend(value.to_le_bytes());
    }
    std::fs::write(&source, &bytes).unwrap();
    let host = thread::spawn(move || owner.serve().unwrap());
    let import = envelope(
        "stereo-fir",
        &state(&runtime),
        json!({"method":"profiles.import_correction","id":"stereo","expected_profile_revision":null,
        "draft":draft(),"input":{"kind":"fir","path":source,"purpose":"correction","context":{"sample_rate_hz":48000,"channels":"stereo","quantum_frames":64},
        "metadata_json":"{}","preamp_db":-6.0}}),
    );
    let saved = finish(&runtime, &import);
    assert_eq!(saved["receipt"]["status"], "applied", "{saved}");
    let record = &saved["receipt"]["result"]["value"];
    let manifest = earph_media::read_fir_manifest(
        record["corrections"][0]["document_json"]
            .as_str()
            .unwrap()
            .as_bytes(),
    )
    .unwrap();
    assert_eq!(manifest.channel_map, [0, 1]);
    assert_eq!(manifest.asset.taps_per_channel, 2);
    let correction = earph_engine::assets::resolve(&runtime, &manifest.asset).unwrap();
    assert_eq!(correction.taps, [vec![0.5, 0.0], vec![-0.25, 0.125]]);
    let coefficients =
        earph_dsp::FirCoefficients::new(correction.reference, correction.taps).unwrap();
    let graph = serde_json::from_value(record["graph"]["graph"].clone()).unwrap();
    let format = PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Float64).unwrap();
    let plan = earph_dsp::compile_with_assets(
        &graph,
        &earph_dsp::CompileContext {
            input_format: format,
            output_format: format,
            quantum_frames: 64,
            policy: earph_dsp::ProcessingPolicy::AllowProcessing,
            budget: Default::default(),
        },
        &[coefficients],
    )
    .unwrap();
    let latency = plan.latency_frames() as usize;
    let mut dsp = earph_dsp::PreparedGraph::prepare(plan, Epochs::default()).unwrap();
    let mut input = [0.0; 128];
    input[0] = 0.1;
    input[1] = 0.1;
    let mut rendered = Vec::new();
    for _ in 0..=(latency + 1) / 64 {
        let mut output = [0.0; 128];
        dsp.process(&input, &mut output).unwrap();
        rendered.extend(output);
        input.fill(0.0);
    }
    let gain = 10.0_f64.powf(-6.0 / 20.0);
    assert!(rendered[..2 * latency].iter().all(|v| v.abs() < 1e-12));
    for (observed, tap) in rendered[2 * latency..2 * latency + 4].iter().zip(values) {
        assert!((observed - 0.1 * gain * tap).abs() < 1e-12);
    }
    assert_eq!(std::fs::read(&source).unwrap(), bytes);
    shutdown(&runtime);
    host.join().unwrap();
    let asset_path = runtime
        .join("assets")
        .join(format!("{}.wav", manifest.asset.sha256.as_str()));
    let preserved = asset_path.with_extension("preserved");
    std::fs::rename(&asset_path, &preserved).unwrap();
    assert_eq!(
        EngineOwner::bind(&runtime).err().unwrap().code,
        earph_core::ErrorCode::AssetInvalid
    );
    assert_eq!(std::fs::read(preserved).unwrap(), bytes);
}
