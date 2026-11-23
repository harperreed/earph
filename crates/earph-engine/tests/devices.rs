// ABOUTME: Exercises read-only discovery through the actual engine owner and exec helper.
// ABOUTME: Preserves isolated ALSA fixtures and verifies replay without physical output access.
#![cfg(target_os = "linux")]
#[path = "support/device_process.rs"]
mod device_process;
mod support;
use earph_engine::{EngineOwner, request};
use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read},
    os::unix::fs::DirBuilderExt,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

struct Owner(Child);
impl Drop for Owner {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            self.0.kill().unwrap();
        }
        self.0.wait().unwrap();
    }
}
fn document(id: &str, payload: Value) -> Vec<u8> {
    serde_json::to_vec(&json!({"schema_version":1,"request_id":id,"zone_id":"main","owner_id":null,"session_id":null,"expected_revision":null,"payload":payload})).unwrap()
}
fn call(runtime: &std::path::Path, bytes: &[u8]) -> Value {
    serde_json::from_slice(&request(runtime, bytes).unwrap()).unwrap()
}
#[test]
fn real_cli_discovers_configured_hints_and_retains_exact_terminal_replay() {
    let runtime = support::runtime();
    fs::DirBuilder::new().mode(0o700).create(&runtime).unwrap();
    let config = runtime.join("discovery-alsa.conf");
    fs::write(&config, "ctl.hw { @args [ CARD ] @args.CARD { type string } type hw card $CARD }\npcm.null { type null hint { show on description \"Owned discard\" } }\n").unwrap();
    let mut owner = Owner(
        Command::new(env!("CARGO_BIN_EXE_earph-engine"))
            .args(["owner", "--runtime"])
            .arg(&runtime)
            .current_dir(&runtime)
            .env("ALSA_CONFIG_PATH", config.file_name().unwrap())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut boot = String::new();
    BufReader::new(owner.0.stdout.take().unwrap())
        .read_line(&mut boot)
        .unwrap();
    if boot.is_empty() {
        let mut error = String::new();
        owner
            .0
            .stderr
            .take()
            .unwrap()
            .read_to_string(&mut error)
            .unwrap();
        panic!("owner ended before publishing its bootstrap: {error}");
    }
    assert_eq!(
        serde_json::from_str::<Value>(&boot).unwrap()["status"],
        "owner"
    );
    let caps = call(
        &runtime,
        &document("caps", json!({"method":"capabilities.get"})),
    );
    assert_eq!(
        caps["receipt"]["result"]["value"]["devices"]["backends"],
        json!(["alsa"])
    );
    let query = document("list", json!({"method":"devices.list","backend":"alsa"}));
    let accepted = call(&runtime, &query);
    assert_eq!(accepted["receipt"]["status"], "accepted");
    let deadline = Instant::now() + Duration::from_secs(10);
    let settled = loop {
        let reply = call(&runtime, &query);
        if reply["receipt"]["status"] != "accepted" {
            break reply;
        }
        assert!(Instant::now() < deadline, "discovery did not settle");
        std::thread::sleep(Duration::from_millis(2));
    };
    assert_eq!(settled["receipt"]["status"], "applied", "{settled}");
    assert_eq!(call(&runtime, &query), settled);
    let value = &settled["receipt"]["result"]["value"];
    assert_eq!(value["read_boundary"], "alsa_pcm_hints");
    assert!(
        value["observation"]["devices"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v["endpoint"] == "null")
    );
    for endpoint in ["null", "not-an-advertised-hint"] {
        let inspect = document(
            endpoint,
            json!({"method":"devices.inspect","backend":"alsa","endpoint":endpoint}),
        );
        let deadline = Instant::now() + Duration::from_secs(10);
        let inspected = loop {
            let reply = call(&runtime, &inspect);
            if reply["receipt"]["status"] != "accepted" {
                break reply;
            }
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(2));
        };
        assert_eq!(inspected["receipt"]["status"], "applied", "{inspected}");
        let observation = &inspected["receipt"]["result"]["value"]["observation"];
        assert_eq!(observation["kind"], "inspection");
        assert_eq!(observation["endpoint"], endpoint);
        assert_eq!(observation.get("current_properties"), Some(&Value::Null));
        assert_eq!(observation["advertisement"].is_null(), endpoint != "null");
        assert_eq!(call(&runtime, &inspect), inspected);
    }
    let state = call(&runtime, &document("state", json!({"method":"state.get"})));
    let state = &state["receipt"]["result"]["value"];
    assert_eq!(state["revision"], 0);
    assert_eq!(state["muted"], true);
    assert_eq!(state["output"], Value::Null);
    let shutdown = serde_json::to_vec(&json!({"schema_version":1,"request_id":"shutdown","zone_id":"main","owner_id":state["owner_id"],"session_id":null,"expected_revision":0,"payload":{"method":"owner.shutdown"}})).unwrap();
    assert_eq!(call(&runtime, &shutdown)["receipt"]["status"], "applied");
    assert!(owner.0.wait().unwrap().success());
    assert!(config.exists());
}

#[test]
fn embedded_owner_without_explicit_helper_advertises_no_native_discovery() {
    let runtime = support::runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let thread = std::thread::spawn(move || owner.serve().unwrap());
    let result = call(
        &runtime,
        &document("caps", json!({"method":"capabilities.get"})),
    );
    assert_eq!(
        result["receipt"]["result"]["value"]["devices"]["backends"],
        json!([])
    );
    let denied = call(
        &runtime,
        &document("list", json!({"method":"devices.list","backend":"alsa"})),
    );
    assert_eq!(denied["receipt"]["error"]["code"], "CAPABILITY_UNSUPPORTED");
    let state = call(&runtime, &document("state", json!({"method":"state.get"})));
    let shutdown = serde_json::to_vec(&json!({"schema_version":1,"request_id":"shutdown","zone_id":"main","owner_id":state["receipt"]["result"]["value"]["owner_id"],"session_id":null,"expected_revision":0,"payload":{"method":"owner.shutdown"}})).unwrap();
    assert_eq!(call(&runtime, &shutdown)["receipt"]["status"], "applied");
    thread.join().unwrap();
}

#[test]
fn waiting_helper_keeps_safety_queries_responsive_and_inherits_no_owner_descriptors() {
    let executable = device_process::fixture("wait");
    let helper = earph_engine::DeviceDiscoveryHelper::from_executable(&executable).unwrap();
    let runtime = support::runtime();
    let owner = EngineOwner::bind(&runtime)
        .unwrap()
        .with_device_discovery(helper);
    let (done, finished) = std::sync::mpsc::sync_channel(1);
    let thread = std::thread::spawn(move || {
        let result = owner.serve();
        let _ = done.send(result);
    });
    let pending = document(
        "pending-list",
        json!({"method":"devices.list","backend":"alsa"}),
    );
    assert_eq!(call(&runtime, &pending)["receipt"]["status"], "accepted");
    let deadline = Instant::now() + Duration::from_secs(3);
    while !executable.with_extension("descriptors").exists() {
        assert!(Instant::now() < deadline, "owned helper did not start");
        std::thread::sleep(Duration::from_millis(2));
    }
    let descriptors = fs::read_to_string(executable.with_extension("descriptors")).unwrap();
    assert!(
        !descriptors.contains(runtime.to_str().unwrap()),
        "owner runtime descriptor crossed exec: {descriptors}"
    );
    assert!(!descriptors.contains("owner.lock"));
    assert!(!descriptors.contains(".sock"));
    assert!(
        !descriptors
            .lines()
            .any(|value| value.starts_with("socket:[")),
        "an owner socket crossed exec: {descriptors}"
    );
    let mut collision: Value = serde_json::from_slice(&pending).unwrap();
    collision["payload"]["method"] = json!("devices.inspect");
    collision["payload"]["endpoint"] = json!("null");
    assert_eq!(
        call(&runtime, &serde_json::to_vec(&collision).unwrap())["receipt"]["error"]["code"],
        "ASSET_INVALID"
    );
    let state = call(
        &runtime,
        &document("state-live", json!({"method":"state.get"})),
    );
    let value = &state["receipt"]["result"]["value"];
    let control = |id: &str, payload: Value, revision: u64| {
        serde_json::to_vec(&json!({"schema_version":1,"request_id":id,"zone_id":"main","owner_id":value["owner_id"],"session_id":null,"expected_revision":revision,"payload":payload})).unwrap()
    };
    let started = Instant::now();
    assert_eq!(
        call(
            &runtime,
            &control("mute", json!({"method":"mute.set","muted":true}), 0)
        )["receipt"]["status"],
        "applied"
    );
    let stopped = call(
        &runtime,
        &control("stop-idle", json!({"method":"sessions.stop"}), 1),
    );
    assert_eq!(stopped["receipt"]["status"], "rejected");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "read-only discovery blocked safety responses"
    );
    assert_eq!(call(&runtime, &pending)["receipt"]["status"], "accepted");
    let shutdown = control("shutdown", json!({"method":"owner.shutdown"}), 1);
    assert_eq!(call(&runtime, &shutdown)["receipt"]["status"], "applied");
    assert!(
        finished
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .is_ok()
    );
    thread.join().unwrap();
    let pid = fs::read_to_string(executable.with_extension("pid")).unwrap();
    assert!(!std::path::Path::new(&format!("/proc/{}", pid.trim())).exists());
}
