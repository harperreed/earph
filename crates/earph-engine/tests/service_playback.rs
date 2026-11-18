// ABOUTME: Exercises local playback through the real owner and client executables.
// ABOUTME: Captures ALSA file-to-null output without opening physical audio devices.
#![cfg(target_os = "linux")]
mod support;
use serde_json::{Value, json};
use std::{
    cell::Cell,
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct Controller {
    runtime: PathBuf,
    next: Cell<u64>,
}
impl Controller {
    fn id(&self) -> String {
        let next = self.next.get();
        self.next.set(next + 1);
        format!("acceptance-{}-{next}", std::process::id())
    }
    fn send(&self, document: &Value) -> Value {
        let mut child = Command::new(env!("CARGO_BIN_EXE_earph-engine"))
            .args(["request", "--runtime"])
            .arg(&self.runtime)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = serde_json::to_vec(document).unwrap();
        input.push(b'\n');
        child.stdin.take().unwrap().write_all(&input).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            !output.stdout.is_empty(),
            "client returned no reply: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    fn query(&self, payload: Value) -> Value {
        let reply = self.send(&json!({
            "schema_version":1,"request_id":self.id(),"zone_id":"main",
            "owner_id":null,"session_id":null,"expected_revision":null,"payload":payload
        }));
        assert_eq!(reply["receipt"]["status"], "applied", "{reply}");
        reply["receipt"]["result"]["value"].clone()
    }
    fn state(&self) -> Value {
        self.query(json!({"method":"state.get"}))
    }
    fn document(&self, payload: Value, session: Option<&str>) -> Value {
        let state = self.state();
        json!({"schema_version":1,"request_id":self.id(),"zone_id":"main",
            "owner_id":state["owner_id"],
            "session_id":session.map_or_else(||state["session_id"].clone(),Value::from),
            "expected_revision":state["revision"],"payload":payload})
    }
    fn apply(&self, payload: Value) -> Value {
        let reply = self.send(&self.document(payload, None));
        assert_eq!(reply["receipt"]["status"], "applied", "{reply}");
        reply
    }
    fn wait(&self, predicate: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let state = self.state();
            if predicate(&state) {
                return state;
            }
            assert!(
                Instant::now() < deadline,
                "owner did not reach expected state: {state}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn settled(&self, document: &Value) -> Value {
        let reply = self.terminal(document);
        assert_eq!(reply["receipt"]["status"], "applied", "{reply}");
        reply
    }
    fn terminal(&self, document: &Value) -> Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let reply = self.send(document);
            if reply["receipt"]["status"] != "accepted" {
                return reply;
            }
            assert!(
                Instant::now() < deadline,
                "operation did not settle: {reply}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

struct Host {
    child: Child,
    controller: Controller,
}
impl Host {
    fn start(config: &Path) -> Self {
        Self::start_at(config, support::runtime())
    }
    fn start_at(config: &Path, runtime: PathBuf) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_earph-engine"))
            .args(["owner", "--runtime"])
            .arg(&runtime)
            .env("ALSA_CONFIG_PATH", config)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&line).unwrap()["status"],
            "owner"
        );
        Self {
            child,
            controller: Controller {
                runtime,
                next: Cell::new(0),
            },
        }
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn directory() -> PathBuf {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/service-playback")
        .join(format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    fs::create_dir_all(&path).unwrap();
    path
}
fn wave(path: &Path, start: usize, frames: usize) -> Vec<u8> {
    let mut samples: Vec<u8> = Vec::new();
    for index in start * 2..(start + frames) * 2 {
        let value = (index as i64 * 7919 % 16777216 - 8388608) as i32;
        samples.extend(&value.to_le_bytes()[..3]);
    }
    let mut bytes: Vec<u8> = Vec::new();
    bytes.extend(b"RIFF");
    bytes.extend((36 + samples.len() as u32).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16_u32.to_le_bytes());
    bytes.extend(1_u16.to_le_bytes());
    bytes.extend(2_u16.to_le_bytes());
    bytes.extend(48000_u32.to_le_bytes());
    bytes.extend(288000_u32.to_le_bytes());
    bytes.extend(6_u16.to_le_bytes());
    bytes.extend(24_u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend((samples.len() as u32).to_le_bytes());
    bytes.extend(&samples);
    fs::write(path, bytes).unwrap();
    samples
}
fn configuration(directory: &Path) -> (PathBuf, PathBuf) {
    let capture = directory.join("capture.pcm");
    let escaped = capture
        .to_str()
        .unwrap()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let config = directory.join("alsa.conf");
    fs::write(&config, format!("pcm.null {{ type null }}\npcm.earph_capture {{ type file slave.pcm null file \"{escaped}\" format raw }}\n")).unwrap();
    (config, capture)
}
fn select_output(controller: &Controller, mode: &str, fidelity: &str) {
    controller.apply(json!({"method":"output.select","output":{
        "backend":"alsa","route":"system","endpoint":"earph_capture",
        "device_id":"acceptance-output","profile_id":"acceptance-profile",
        "period_frames":64,"buffer_frames":256,
        "volume_mode":mode,"fidelity":fidelity
    }}));
}
fn consent(controller: &Controller) {
    controller.apply(json!({"method":"output.consent","device_id":"acceptance-output","profile_id":"acceptance-profile"}));
}
fn select_fixed(controller: &Controller) {
    select_output(controller, "fixed", "strict_preservation");
    controller.apply(json!({"method":"volume.set","millidecibels":0,"ramp_frames":0}));
    consent(controller);
    controller.apply(json!({"method":"volume.arm_fixed","device_id":"acceptance-output","profile_id":"acceptance-profile"}));
}
fn append_selected(controller: &Controller, file: &Path) {
    controller.apply(json!({"method":"queue.append","path":file}));
    let queue = controller.query(json!({"method":"queue.get","offset":0,"limit":64}));
    controller.apply(json!({"method":"queue.select","id":queue["entries"][0]["id"]}));
}
fn start(controller: &Controller) -> Value {
    let document = controller.document(json!({"method":"sessions.start","source":"local_file","device_id":"acceptance-output","takeover":false,"seekable":true}),Some("acceptance-session"));
    let reply = controller.send(&document);
    assert_eq!(reply["receipt"]["status"], "accepted", "{reply}");
    document
}
fn assert_no_capture(capture: &Path) {
    match fs::read(capture) {
        Ok(bytes) => assert!(bytes.is_empty(), "muted preparation emitted samples"),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!("cannot inspect muted capture: {error}"),
    }
}

#[test]
fn executable_fixed_output_joins_an_appended_lossless_track_exactly() {
    let directory = directory();
    let first = directory.join("first.wav");
    let second_wave = directory.join("second.wav");
    let second = directory.join("second.flac");
    let mut expected = wave(&first, 0, 4097);
    expected.extend(wave(&second_wave, 4097, 4102));
    let producer = Command::new("ffmpeg")
        .args(["-nostdin", "-v", "error", "-i"])
        .arg(&second_wave)
        .args(["-c:a", "flac"])
        .arg(&second)
        .output()
        .unwrap();
    assert!(
        producer.status.success(),
        "{}",
        String::from_utf8_lossy(&producer.stderr)
    );
    let originals = [fs::read(&first).unwrap(), fs::read(&second).unwrap()];
    let (config, capture) = configuration(&directory);
    let host = Host::start(&config);
    let controller = &host.controller;
    select_fixed(controller);
    append_selected(controller, &first);
    let start = start(controller);
    controller.wait(|state| state["transport"] == "buffering");
    assert_no_capture(&capture);
    controller.apply(json!({"method":"queue.append","path":second}));
    controller.apply(json!({"method":"mute.set","muted":false}));
    let completed =
        controller.wait(|state| state["transport"] == "idle" || state["transport"] == "failed");
    assert!(completed["error"].is_null(), "{completed}");
    assert_eq!(fs::read(&capture).unwrap(), expected);
    let receipt = controller.send(&start);
    assert_eq!(receipt["receipt"]["status"], "applied", "{receipt}");
    assert_eq!(receipt["receipt"]["activation_frame"], 0);
    assert_eq!(fs::read(&first).unwrap(), originals[0]);
    assert_eq!(fs::read(&second).unwrap(), originals[1]);
    controller.apply(json!({"method":"owner.shutdown"}));
}

#[test]
fn executable_pause_seek_and_resume_preserve_paused_intent_and_exact_suffix() {
    let directory = directory();
    let source = directory.join("source.wav");
    let original = wave(&source, 0, 8199);
    let (config, capture) = configuration(&directory);
    let host = Host::start(&config);
    let controller = &host.controller;
    select_fixed(controller);
    append_selected(controller, &source);
    start(controller);
    controller.wait(|state| state["transport"] == "buffering");
    let pause = controller.document(json!({"method":"sessions.pause"}), None);
    let receipt = controller.send(&pause);
    assert_eq!(receipt["receipt"]["status"], "accepted", "{receipt}");
    controller.settled(&pause);
    let seek = controller.document(
        json!({"method":"sessions.seek","position_frames":4097}),
        None,
    );
    let receipt = controller.send(&seek);
    assert_eq!(receipt["receipt"]["status"], "accepted", "{receipt}");
    controller.settled(&seek);
    controller.wait(|state| state["transport"] == "paused");
    controller.apply(json!({"method":"mute.set","muted":false}));
    assert_eq!(controller.state()["transport"], "paused");
    assert_no_capture(&capture);
    let resume = controller.document(json!({"method":"sessions.resume"}), None);
    let receipt = controller.send(&resume);
    assert_eq!(receipt["receipt"]["status"], "accepted", "{receipt}");
    let completed =
        controller.wait(|state| state["transport"] == "idle" || state["transport"] == "failed");
    assert!(completed["error"].is_null(), "{completed}");
    assert_eq!(fs::read(&capture).unwrap(), original[4097 * 6..]);
    controller.apply(json!({"method":"owner.shutdown"}));
}

fn integer24(bytes: &[u8]) -> i32 {
    (i32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0]) << 8) >> 8
}

fn delay_gain_graph(delay_frames: u64, gain: f64) -> Value {
    let port = json!({"domain":"pcm","sample_rate_hz":48000,"channels":"stereo",
        "precision":"float64","max_quantum_frames":64});
    json!({"schema_version":1,"nodes":[
        {"id":"input","kind":{"kind":"input"},"format":port},
        {"id":"delay","kind":{"kind":"delay","delay_frames":delay_frames},"format":port},
        {"id":"gain","kind":{"kind":"gain","gain_linear":gain},"format":port},
        {"id":"output","kind":{"kind":"output"},"format":port}],
        "edges":[{"from":"input","to":"delay","from_port":0,"to_port":0},
            {"from":"delay","to":"gain","from_port":0,"to_port":0},
            {"from":"gain","to":"output","from_port":0,"to_port":0}],"output":"output"})
}

fn prepare_graph(controller: &Controller, graph: Value) -> (Value, Value) {
    let request = controller.document(
        json!({"method":"graph.prepare", "graph":graph,
        "processing":{"kind":"float64","quantization":"round_ties_away_saturate_no_dither"}}),
        None,
    );
    let reply = controller.send(&request);
    assert_eq!(reply["receipt"]["status"], "accepted", "{reply}");
    let result = controller.settled(&request)["receipt"]["result"]["value"].clone();
    (request, result)
}

#[test]
fn executable_retains_invalid_graph_digests_over_ipc() {
    let directory = directory();
    let (config, _) = configuration(&directory);
    let host = Host::start(&config);
    let controller = &host.controller;
    let mut request = controller.document(
        json!({"method":"graph.commit","token":"candidate","graph_hash":"invalid"}),
        None,
    );
    let rejected = controller.send(&request);
    assert_eq!(rejected["receipt"]["status"], "rejected", "{rejected}");
    assert_eq!(
        rejected["receipt"]["error"]["code"], "ASSET_INVALID",
        "{rejected}"
    );
    assert_eq!(controller.send(&request), rejected);
    request["payload"] = json!({"method":"state.get"});
    let reused = controller.send(&request);
    assert_eq!(reused["receipt"]["status"], "rejected", "{reused}");
    assert_eq!(
        reused["receipt"]["error"]["message"], "request ID reused for different content",
        "{reused}"
    );
    let mut oversized = controller.document(
        json!({"method":"graph.commit","token":"candidate","graph_hash":"x".repeat(129)}),
        None,
    );
    let rejected = controller.send(&oversized);
    assert_eq!(
        rejected["error"]["code"], "RESOURCE_BUDGET_EXCEEDED",
        "{rejected}"
    );
    oversized["payload"] = json!({"method":"state.get"});
    let reused = controller.send(&oversized);
    assert_eq!(reused["receipt"]["status"], "applied", "{reused}");
    controller.apply(json!({"method":"owner.shutdown"}));
}

#[test]
fn executable_retains_bounded_semantic_rejections_over_ipc() {
    let directory = directory();
    let (config, _) = configuration(&directory);
    let host = Host::start(&config);
    let controller = &host.controller;
    let mut graph = delay_gain_graph(0, 1.0);
    graph["nodes"][1]["kind"] =
        json!({"kind":"matrix", "input_channels":"stereo", "coefficients":[1,0,1]});
    let mut request = controller.document(
        json!({"method":"graph.prepare", "graph":graph,
        "processing":{"kind":"integer_bypass"}}),
        None,
    );
    let rejected = controller.send(&request);
    assert_eq!(rejected["receipt"]["status"], "rejected", "{rejected}");
    assert_eq!(
        rejected["receipt"]["error"]["code"], "GRAPH_INVALID",
        "{rejected}"
    );
    request["payload"] = json!({"method":"state.get"});
    let reused = controller.send(&request);
    assert_eq!(reused["receipt"]["status"], "rejected", "{reused}");
    assert_eq!(
        reused["receipt"]["error"]["code"], "ASSET_INVALID",
        "{reused}"
    );
    controller.apply(json!({"method":"owner.shutdown"}));
}

#[test]
fn executable_graph_rejects_competing_preparation_and_keeps_the_working_plan_on_failure() {
    let directory = directory();
    let source = directory.join("source.wav");
    wave(&source, 0, 513);
    let (config, capture) = configuration(&directory);
    let host = Host::start(&config);
    let controller = &host.controller;
    select_output(controller, "software", "compatible_conversion");
    consent(controller);
    append_selected(controller, &source);
    start(controller);
    controller.wait(|state| state["transport"] == "buffering");
    let (prepare, ready) = prepare_graph(controller, delay_gain_graph(3, 0.5));
    let competitor = controller.document(prepare["payload"].clone(), None);
    let rejected = controller.send(&competitor);
    assert_eq!(rejected["receipt"]["status"], "rejected", "{rejected}");
    assert_eq!(rejected["receipt"]["error"]["code"], "SOURCE_BUSY");
    let commit = controller.document(
        json!({"method":"graph.commit","token":ready["token"],
        "graph_hash":ready["preview"]["graph_hash"]}),
        None,
    );
    assert_eq!(controller.send(&commit)["receipt"]["status"], "accepted");
    controller.settled(&commit);
    let before = controller.query(json!({"method":"graph.inspect"}));
    for (graph, code) in [
        (delay_gain_graph(3, 101.0), "SAFETY_POLICY_BLOCKED"),
        (
            delay_gain_graph(16_777_216, 0.5),
            "RESOURCE_BUDGET_EXCEEDED",
        ),
    ] {
        let invalid = controller.document(
            json!({"method":"graph.prepare", "graph":graph,
            "processing":{"kind":"float64","quantization":"round_ties_away_saturate_no_dither"}}),
            None,
        );
        assert_eq!(controller.send(&invalid)["receipt"]["status"], "accepted");
        let failure = controller.terminal(&invalid);
        assert_eq!(failure["receipt"]["status"], "failed", "{failure}");
        assert_eq!(failure["receipt"]["error"]["code"], code);
    }
    let after = controller.query(json!({"method":"graph.inspect"}));
    assert_eq!(before["active"], after["active"]);
    assert_eq!(before["configured"], after["configured"]);
    assert_eq!(after["graph_epoch"], 1);
    assert_no_capture(&capture);
    controller.apply(json!({"method":"owner.shutdown"}));
}

#[test]
fn executable_paused_graph_swap_publishes_the_installed_observation() {
    let directory = directory();
    let source = directory.join("paused-graph.wav");
    wave(&source, 0, 513);
    let (config, capture) = configuration(&directory);
    let host = Host::start(&config);
    let controller = &host.controller;
    select_output(controller, "software", "compatible_conversion");
    consent(controller);
    append_selected(controller, &source);
    start(controller);
    controller.wait(|state| state["transport"] == "buffering");
    let pause = controller.document(json!({"method":"sessions.pause"}), None);
    controller.send(&pause);
    controller.settled(&pause);
    for delay in [3, 7] {
        let before_epoch = controller.state()["graph_epoch"].as_u64().unwrap();
        let (_, ready) = prepare_graph(controller, delay_gain_graph(delay, 0.5));
        let commit = controller.document(
            json!({"method":"graph.commit",
            "token":ready["token"],"graph_hash":ready["preview"]["graph_hash"]}),
            None,
        );
        controller.send(&commit);
        controller.settled(&commit);
        let state = controller.state();
        let inspection = controller.query(json!({"method":"graph.inspect"}));
        assert_eq!(state["transport"], "paused");
        assert_eq!(state["muted"], true);
        assert_eq!(state["graph_epoch"], before_epoch + 1);
        assert_eq!(
            state["playback"]["processing"]["graph_hash"],
            inspection["active"]["graph_hash"]
        );
        assert_eq!(state["playback"]["processing"]["latency_frames"], delay);
        assert_eq!(
            state["playback"]["processing"]["resident_bytes"],
            inspection["active"]["resources"]["pipeline_bytes"]
        );
    }
    assert_no_capture(&capture);
    controller.apply(json!({"method":"owner.shutdown"}));
}

#[test]
fn executable_restore_keeps_enough_revision_capacity_for_a_complete_startup() {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let directory = directory();
    let source = directory.join("revision-source.wav");
    let original = wave(&source, 0, 513);
    let (config, _) = configuration(&directory);
    let mut seed = Host::start(&config);
    select_output(&seed.controller, "software", "compatible_conversion");
    append_selected(&seed.controller, &source);
    seed.controller.apply(json!({"method":"owner.shutdown"}));
    assert!(seed.child.wait().unwrap().success());
    let checkpoint: earph_core::session::SessionCheckpoint =
        serde_json::from_slice(&fs::read(seed.controller.runtime.join("queue.json")).unwrap())
            .unwrap();
    for (revision, restored_revision) in [(u64::MAX - 1, 0), (u64::MAX - 64, u64::MAX - 64)] {
        let runtime = support::runtime();
        fs::DirBuilder::new().mode(0o700).create(&runtime).unwrap();
        let mut candidate = checkpoint.clone();
        candidate.revision = revision;
        let path = runtime.join("queue.json");
        fs::write(&path, serde_json::to_vec(&candidate).unwrap()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let mut host = Host::start_at(&config, runtime);
        let controller = &host.controller;
        assert_eq!(controller.state()["revision"], restored_revision);
        consent(controller);
        let started = start(controller);
        controller.wait(|state| state["transport"] == "buffering");
        let (_, ready) = prepare_graph(controller, delay_gain_graph(3, 0.5));
        let commit = controller.document(
            json!({"method":"graph.commit",
            "token":ready["token"],"graph_hash":ready["preview"]["graph_hash"]}),
            None,
        );
        controller.send(&commit);
        controller.settled(&commit);
        controller.apply(json!({"method":"mute.set","muted":false}));
        let completed =
            controller.wait(|state| state["transport"] == "idle" || state["transport"] == "failed");
        assert_eq!(completed["transport"], "idle", "{completed}");
        assert!(completed["error"].is_null(), "{completed}");
        controller.settled(&started);
        controller.apply(json!({"method":"owner.shutdown"}));
        assert!(host.child.wait().unwrap().success());
    }
    assert_eq!(fs::read(source).unwrap()[44..], original);
}

#[test]
fn executable_graph_rebuffer_keeps_the_plan_and_reports_only_source_position() {
    let directory = directory();
    let source = directory.join("graph-seek.wav");
    let original = wave(&source, 0, 513);
    let (config, capture) = configuration(&directory);
    let host = Host::start(&config);
    let controller = &host.controller;
    select_output(controller, "software", "compatible_conversion");
    consent(controller);
    append_selected(controller, &source);
    start(controller);
    controller.wait(|state| state["transport"] == "buffering");
    let (_, ready) = prepare_graph(controller, delay_gain_graph(5, 0.5));
    let commit = controller.document(
        json!({"method":"graph.commit", "token":ready["token"],
        "graph_hash":ready["preview"]["graph_hash"]}),
        None,
    );
    assert_eq!(controller.send(&commit)["receipt"]["status"], "accepted");
    controller.settled(&commit);
    for payload in [
        json!({"method":"sessions.pause"}),
        json!({"method":"sessions.seek","position_frames":100}),
    ] {
        let command = controller.document(payload, None);
        assert_eq!(controller.send(&command)["receipt"]["status"], "accepted");
        controller.settled(&command);
    }
    let inspection = controller.query(json!({"method":"graph.inspect"}));
    assert_eq!(inspection["graph_epoch"], 1);
    assert_eq!(
        inspection["active"]["graph_hash"],
        ready["preview"]["graph_hash"]
    );
    assert_no_capture(&capture);
    controller.apply(json!({"method":"mute.set","muted":false}));
    assert_eq!(controller.state()["transport"], "paused");
    let resume = controller.document(json!({"method":"sessions.resume"}), None);
    assert_eq!(controller.send(&resume)["receipt"]["status"], "accepted");
    let completed =
        controller.wait(|state| state["transport"] == "idle" || state["transport"] == "failed");
    assert!(completed["error"].is_null(), "{completed}");
    assert_eq!(completed["playback"]["submitted_frames"], 418);
    assert_eq!(completed["playback"]["source_position_frames"], 513);
    assert_eq!(completed["playback"]["audible_source_position_frames"], 513);
    let mut expected = vec![0_u8; 5 * 6];
    for input in original[100 * 6..].as_chunks::<3>().0 {
        let value = (f64::from(integer24(input)) * 0.5 * 0.01).round() as i32;
        expected.extend_from_slice(&value.to_le_bytes()[..3]);
    }
    assert_eq!(fs::read(capture).unwrap(), expected);
    controller.apply(json!({"method":"owner.shutdown"}));
}

#[test]
fn executable_restart_restores_graph_intent_muted_without_runtime_tokens() {
    let directory = directory();
    let source = directory.join("graph-restore.wav");
    wave(&source, 0, 513);
    let (config, capture) = configuration(&directory);
    let mut host = Host::start(&config);
    let controller = &host.controller;
    select_output(controller, "software", "compatible_conversion");
    consent(controller);
    append_selected(controller, &source);
    start(controller);
    controller.wait(|state| state["transport"] == "buffering");
    let (_, ready) = prepare_graph(controller, delay_gain_graph(3, 0.5));
    let commit = controller.document(
        json!({"method":"graph.commit", "token":ready["token"],
        "graph_hash":ready["preview"]["graph_hash"]}),
        None,
    );
    assert_eq!(controller.send(&commit)["receipt"]["status"], "accepted");
    controller.settled(&commit);
    let configured = controller.query(json!({"method":"graph.inspect"}))["configured"].clone();
    let runtime = controller.runtime.clone();
    controller.apply(json!({"method":"owner.shutdown"}));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(status) = host.child.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "owner did not shut down after graph retirement"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    let restored = Host::start_at(&config, runtime);
    let controller = &restored.controller;
    let inspection = controller.query(json!({"method":"graph.inspect"}));
    assert_eq!(inspection["configured"], configured);
    assert!(inspection["active"].is_null());
    assert!(inspection["prepared"].is_null());
    assert_eq!(inspection["service"]["resident_bytes"], 0);
    assert_eq!(controller.state()["muted"], true);
    consent(controller);
    let unmute = controller.document(json!({"method":"mute.set","muted":false}), None);
    let rejected = controller.send(&unmute);
    assert_eq!(rejected["receipt"]["status"], "rejected");
    assert_eq!(
        rejected["receipt"]["error"]["code"],
        "SAFETY_POLICY_BLOCKED"
    );
    assert_no_capture(&capture);
    controller.apply(json!({"method":"owner.shutdown"}));
}

#[test]
fn executable_graph_activation_preserves_joins_and_drains_one_exact_tail() {
    let directory = directory();
    let first = directory.join("graph-first.wav");
    let second = directory.join("graph-second.wav");
    let mut samples = wave(&first, 0, 65);
    samples.extend(wave(&second, 65, 70));
    let originals = [fs::read(&first).unwrap(), fs::read(&second).unwrap()];
    let (config, capture) = configuration(&directory);
    let host = Host::start(&config);
    let controller = &host.controller;
    select_output(controller, "software", "compatible_conversion");
    consent(controller);
    append_selected(controller, &first);
    controller.apply(json!({"method":"queue.append","path":second}));
    start(controller);
    let before = controller.wait(|state| state["transport"] == "buffering");
    let prepare = controller.document(
        json!({"method":"graph.prepare",
        "graph":delay_gain_graph(7, 0.5),"processing":{"kind":"float64",
            "quantization":"round_ties_away_saturate_no_dither"}}),
        None,
    );
    let accepted = controller.send(&prepare);
    assert_eq!(accepted["receipt"]["status"], "accepted", "{accepted}");
    let ready = controller.settled(&prepare)["receipt"]["result"]["value"].clone();
    assert_eq!(controller.state()["revision"], before["revision"]);
    assert_eq!(ready["preview"]["latency_frames"], 7);
    assert_eq!(ready["preview"]["tail_frames"], 7);
    let commit = controller.document(
        json!({"method":"graph.commit", "token":ready["token"],
        "graph_hash":ready["preview"]["graph_hash"]}),
        None,
    );
    let accepted = controller.send(&commit);
    assert_eq!(accepted["receipt"]["status"], "accepted", "{accepted}");
    let applied = controller.settled(&commit);
    assert_eq!(applied["receipt"]["activation_frame"], 0);
    let inspection = controller.query(json!({"method":"graph.inspect"}));
    assert_eq!(inspection["graph_epoch"], 1);
    assert!(inspection["service"]["resident_bytes"].as_u64().unwrap() > 0);
    assert_eq!(
        inspection["active"]["graph_hash"],
        ready["preview"]["graph_hash"]
    );
    assert_eq!(controller.state()["muted"], true);
    assert_no_capture(&capture);
    controller.apply(json!({"method":"mute.set","muted":false}));
    let completed =
        controller.wait(|state| state["transport"] == "idle" || state["transport"] == "failed");
    assert!(completed["error"].is_null(), "{completed}");
    assert_eq!(completed["playback"]["submitted_frames"], 142);
    assert_eq!(completed["playback"]["source_position_frames"], 70);
    assert_eq!(completed["playback"]["processing"]["latency_frames"], 7);
    assert_eq!(completed["playback"]["processing"]["tail_frames"], 7);
    assert_eq!(completed["playback"]["processing"]["saturation_count"], 0);
    assert!(
        completed["playback"]["processing"]["audio_payload_bytes"]
            .as_u64()
            .unwrap()
            > 0
    );
    let mut expected = vec![0_u8; 7 * 6];
    for input in samples.as_chunks::<3>().0 {
        let value = (f64::from(integer24(input)) * 0.5 * 0.01).round() as i32;
        expected.extend_from_slice(&value.to_le_bytes()[..3]);
    }
    assert_eq!(fs::read(&capture).unwrap(), expected);
    assert_eq!(fs::read(first).unwrap(), originals[0]);
    assert_eq!(fs::read(second).unwrap(), originals[1]);
    let deadline = Instant::now() + Duration::from_secs(10);
    let intent = loop {
        let inspection = controller.query(json!({"method":"graph.inspect"}));
        if inspection["service"]["resident_bytes"] == 0
            && inspection["service"]["pending_retirements"] == 0
        {
            break inspection;
        }
        assert!(
            Instant::now() < deadline,
            "graph resources did not retire: {inspection}"
        );
        std::thread::sleep(Duration::from_millis(5));
    };
    assert!(intent["configured"].is_object());
    controller.apply(json!({"method":"owner.shutdown"}));
}

#[test]
fn executable_software_volume_obeys_the_level_and_settles_after_its_ramp() {
    let directory = directory();
    let source = directory.join("volume.wav");
    let original = wave(&source, 0, 8199);
    let (config, capture) = configuration(&directory);
    let host = Host::start(&config);
    let controller = &host.controller;
    select_output(controller, "software", "compatible_conversion");
    consent(controller);
    controller.apply(json!({"method":"volume.set","millidecibels":-40000,"ramp_frames":960}));
    append_selected(controller, &source);
    start(controller);
    controller.wait(|state| state["transport"] == "buffering");
    assert_no_capture(&capture);
    controller.apply(json!({"method":"mute.set","muted":false}));
    let completed =
        controller.wait(|state| state["transport"] == "idle" || state["transport"] == "failed");
    assert!(completed["error"].is_null(), "{completed}");
    assert_eq!(completed["software_gain_db"], -40.0);
    let actual = fs::read(&capture).unwrap();
    assert_eq!(actual.len(), original.len());
    for (index, (input, output)) in original
        .as_chunks::<3>()
        .0
        .iter()
        .zip(actual.as_chunks::<3>().0.iter())
        .enumerate()
    {
        let expected = (f64::from(integer24(input)) * 0.01).round() as i32;
        let observed = integer24(output);
        assert!(
            observed.unsigned_abs() <= expected.unsigned_abs() + 1,
            "software volume exceeded its level at scalar {index}"
        );
        if index >= 960 * 2 {
            assert_eq!(observed, expected, "ramp did not settle at scalar {index}");
        }
    }
    controller.apply(json!({"method":"owner.shutdown"}));
}

#[test]
fn executable_missing_source_publishes_an_item_failure_and_stays_muted() {
    let directory = directory();
    let missing = directory.join("missing.wav");
    let (config, capture) = configuration(&directory);
    let host = Host::start(&config);
    let controller = &host.controller;
    select_fixed(controller);
    append_selected(controller, &missing);
    let request = start(controller);
    let failed = controller.wait(|state| state["transport"] == "failed");
    assert_eq!(failed["error"], "ASSET_INVALID");
    assert_eq!(failed["muted"], true);
    let queue = controller.query(json!({"method":"queue.get","offset":0,"limit":64}));
    let status = &queue["entries"][0]["status"];
    assert_eq!(status["state"], "failed", "{status}");
    assert_eq!(status["code"], "ASSET_INVALID", "{status}");
    let receipt = controller.send(&request);
    assert_eq!(receipt["receipt"]["status"], "failed", "{receipt}");
    assert_no_capture(&capture);
    controller.apply(json!({"method":"owner.shutdown"}));
}

#[test]
fn executable_backend_failure_retains_the_native_diagnostic_after_safe_stop() {
    let directory = directory();
    let source = directory.join("owned.wav");
    wave(&source, 0, 4096);
    let config = directory.join("alsa.conf");
    fs::write(&config, "pcm.null { type null }\npcm.earph_capture { type file slave.pcm null file \"/dev/full\" format raw }\n").unwrap();
    let mut host = Host::start(&config);
    let controller = &host.controller;
    select_output(controller, "software", "compatible_conversion");
    consent(controller);
    append_selected(controller, &source);
    start(controller);
    controller.wait(|state| state["transport"] == "buffering");
    controller.apply(json!({"method":"mute.set","muted":false}));
    let state = controller.wait(|state| state["transport"] == "failed");
    assert_eq!(state["error"], "OUTPUT_FAULT");
    assert_eq!(state["muted"], true);
    assert_eq!(state["output_consented"], false);
    controller.apply(json!({"method":"owner.shutdown"}));
    assert!(host.child.wait().unwrap().success());
    let mut diagnostic = String::new();
    host.child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut diagnostic)
        .unwrap();
    let fault = diagnostic
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find_map(|value| value.get("backend_fault").cloned())
        .expect("terminal native fault lost its diagnostic stage and errno");
    assert_eq!(fault["code"], "OUTPUT_FAULT");
    assert!(
        fault["native_errno"]
            .as_i64()
            .is_some_and(|errno| errno > 0)
    );
    assert!(
        fault["message"]
            .as_str()
            .is_some_and(|message| message.contains("ALSA"))
    );
}
