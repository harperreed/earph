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
    use std::fs;
    let runtime = support::runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
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
