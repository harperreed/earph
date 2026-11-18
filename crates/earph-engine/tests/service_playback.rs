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
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let reply = self.send(document);
            if reply["receipt"]["status"] != "accepted" {
                assert_eq!(reply["receipt"]["status"], "applied", "{reply}");
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
        let runtime = support::runtime();
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
