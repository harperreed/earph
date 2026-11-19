// ABOUTME: Exercises local playback through the real owner and client executables.
// ABOUTME: Captures ALSA file-to-null output without opening physical audio devices.
#![cfg(target_os = "linux")]
#[path = "support/graphs.rs"]
mod graph_fixtures;
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

struct CaptureGate {
    fifo: fs::File,
    capacity: usize,
    release: Option<std::sync::mpsc::SyncSender<()>>,
    finished: std::sync::Arc<std::sync::atomic::AtomicBool>,
    drain: Option<std::thread::JoinHandle<Vec<u8>>>,
}
impl CaptureGate {
    fn new(path: &Path) -> Self {
        use std::os::unix::fs::OpenOptionsExt;
        rustix::fs::mkfifoat(
            rustix::fs::CWD,
            path,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .unwrap();
        let fifo = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(i32::try_from(rustix::fs::OFlags::NONBLOCK.bits()).unwrap())
            .open(path)
            .unwrap();
        rustix::pipe::fcntl_setpipe_size(&fifo, 4096).unwrap();
        let capacity = rustix::pipe::fcntl_getpipe_size(&fifo).unwrap();
        let mut reader = fifo.try_clone().unwrap();
        let (release, ready) = std::sync::mpsc::sync_channel(1);
        let finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopping = finished.clone();
        let drain = std::thread::spawn(move || {
            ready.recv().unwrap();
            let mut result = Vec::new();
            let mut buffer = [0; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => result.extend_from_slice(&buffer[..count]),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if stopping.load(std::sync::atomic::Ordering::Acquire) {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("capture FIFO read failed: {error}"),
                }
            }
            result
        });
        Self {
            fifo,
            capacity,
            release: Some(release),
            finished,
            drain: Some(drain),
        }
    }
    fn wait_full(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        // The selected native period is 64 stereo S24 frames, or 384 bytes.
        while rustix::io::ioctl_fionread(&self.fifo).unwrap() + 384 <= self.capacity as u64 {
            assert!(
                Instant::now() < deadline,
                "actual capture FIFO never filled"
            );
            std::thread::yield_now();
        }
    }
    fn release(&mut self) {
        if let Some(release) = self.release.take() {
            release.send(()).unwrap();
        }
    }
    fn finish(mut self) -> Vec<u8> {
        self.release();
        self.finished
            .store(true, std::sync::atomic::Ordering::Release);
        self.drain.take().unwrap().join().unwrap()
    }
}
impl Drop for CaptureGate {
    fn drop(&mut self) {
        self.release();
        self.finished
            .store(true, std::sync::atomic::Ordering::Release);
        if let Some(drain) = self.drain.take() {
            drain.join().unwrap();
        }
    }
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
    let opened = controller.wait(|state| state["transport"] == "buffering");
    let pause = controller.document(json!({"method":"sessions.pause"}), None);
    let receipt = controller.send(&pause);
    assert_eq!(receipt["receipt"]["status"], "accepted", "{receipt}");
    controller.settled(&pause);
    let paused = controller.state();
    assert_eq!(paused["playback"]["backend"], opened["playback"]["backend"]);
    assert_eq!(paused["playback"]["submitted_frames"], 0);
    let seek = controller.document(
        json!({"method":"sessions.seek","position_frames":4097}),
        None,
    );
    let receipt = controller.send(&seek);
    assert_eq!(receipt["receipt"]["status"], "accepted", "{receipt}");
    controller.settled(&seek);
    let sought = controller.wait(|state| state["transport"] == "paused");
    assert_eq!(sought["playback"]["backend"], opened["playback"]["backend"]);
    assert_eq!(sought["playback"]["source_position_frames"], 4097);
    assert_eq!(sought["playback"]["submitted_frames"], 0);
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
fn executable_imported_fir_preserves_one_tail_across_a_real_queue_join() {
    let directory = directory();
    let first = directory.join("fir-first.wav");
    let second = directory.join("fir-second.wav");
    let mut samples = wave(&first, 0, 65);
    samples.extend(wave(&second, 65, 70));
    let (config, capture) = configuration(&directory);
    let host = Host::start(&config);
    let controller = &host.controller;
    let correction = directory.join("correction.wav");
    let mut bytes = Vec::new();
    bytes.extend(b"RIFF");
    bytes.extend(84_u32.to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16_u32.to_le_bytes());
    bytes.extend(3_u16.to_le_bytes());
    bytes.extend(2_u16.to_le_bytes());
    bytes.extend(48000_u32.to_le_bytes());
    bytes.extend(768000_u32.to_le_bytes());
    bytes.extend(16_u16.to_le_bytes());
    bytes.extend(64_u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(48_u32.to_le_bytes());
    for v in [0.5_f64, 0.25, -0.25, 0.125, 0.0, 0.0] {
        bytes.extend(v.to_le_bytes());
    }
    fs::write(&correction, &bytes).unwrap();
    let imported = Command::new(env!("CARGO_BIN_EXE_earph-engine"))
        .args(["import-fir", "--runtime"])
        .arg(&controller.runtime)
        .arg(&correction)
        .output()
        .unwrap();
    assert!(
        imported.status.success(),
        "{}",
        String::from_utf8_lossy(&imported.stderr)
    );
    let asset = serde_json::from_slice::<Value>(&imported.stdout).unwrap()["asset"].clone();
    select_output(controller, "software", "compatible_conversion");
    consent(controller);
    append_selected(controller, &first);
    controller.apply(json!({"method":"queue.append","path":second}));
    start(controller);
    controller.wait(|s| s["transport"] == "buffering");
    let mut graph = delay_gain_graph(0, 1.0);
    graph["nodes"][1]["kind"] = json!({"kind":"convolution","asset":asset});
    let (_, ready) = prepare_graph(controller, graph);
    assert_eq!(ready["preview"]["tail_frames"], 258);
    assert_eq!(ready["preview"]["timing_kind"], "same_rate");
    assert_eq!(ready["preview"]["latency_frames"], 256);
    assert_eq!(ready["preview"]["tail_policy"], "authored_finite_support");
    assert_eq!(ready["preview"]["node_tails"][0]["horizon_frames"], 258);
    fs::write(
        controller
            .runtime
            .join("assets")
            .join(format!("{}.wav", asset["sha256"].as_str().unwrap())),
        b"changed after preparation",
    )
    .unwrap();
    let commit=controller.document(json!({"method":"graph.commit","token":ready["token"],"graph_hash":ready["preview"]["graph_hash"]}),None);
    controller.send(&commit);
    controller.settled(&commit);
    assert_eq!(controller.state()["muted"], true);
    assert_no_capture(&capture);
    controller.apply(json!({"method":"mute.set","muted":false}));
    let completed = controller.wait(|s| s["transport"] == "idle" || s["transport"] == "failed");
    assert!(completed["error"].is_null(), "{completed}");
    assert_eq!(completed["playback"]["submitted_frames"], 393);
    assert_eq!(completed["playback"]["source_position_frames"], 70);
    assert_eq!(completed["playback"]["processing"]["saturation_count"], 0);
    assert!(completed["audible_position_frames"].is_null());
    let mut expected = vec![[0.0_f64; 2]; 393];
    for (i, frame) in samples.as_chunks::<6>().0.iter().enumerate() {
        for (ch, taps) in [[0.5, -0.25], [0.25, 0.125]].iter().enumerate() {
            let x = f64::from(integer24(&frame[ch * 3..ch * 3 + 3]));
            expected[256 + i][ch] += x * taps[0];
            expected[257 + i][ch] += x * taps[1];
        }
    }
    let expected: Vec<_> = expected
        .into_iter()
        .flatten()
        .flat_map(|x| ((x * 0.01).round() as i32).to_le_bytes()[..3].to_vec())
        .collect();
    let actual = fs::read(&capture).unwrap();
    assert_eq!(actual.len(), expected.len());
    for (a, b) in actual
        .as_chunks::<3>()
        .0
        .iter()
        .zip(expected.as_chunks::<3>().0.iter())
    {
        assert!((integer24(a) - integer24(b)).abs() <= 1);
    }
    assert_eq!(fs::read(&correction).unwrap(), bytes);
    controller.apply(json!({"method":"owner.shutdown"}));
}

#[test]
fn executable_peq_publishes_and_renders_a_bounded_truncation() {
    use earph_core::{audio::ChannelLayout, dsp_nodes::PeqFilter, graph::FiniteGraphScalar};
    let directory = directory();
    let source = directory.join("peq.wav");
    let raw = wave(&source, 0, 73);
    let (config, capture) = configuration(&directory);
    let host = Host::start(&config);
    let controller = &host.controller;
    select_output(controller, "software", "compatible_conversion");
    consent(controller);
    append_selected(controller, &source);
    start(controller);
    controller.wait(|s| s["transport"] == "buffering");
    let mut graph = delay_gain_graph(0, 1.0);
    graph["nodes"][1]["kind"] = json!({"kind":"peq","filter":{"kind":"peaking","frequency_hz":1000.0,"q":0.707,"gain_db":-6.0}});
    let (_, ready) = prepare_graph(controller, graph);
    let tail = ready["preview"]["tail_frames"].as_u64().unwrap() as usize;
    assert!(tail > 0 && tail <= 1048576);
    assert_eq!(ready["preview"]["tail_policy"], "bounded_iir_truncation");
    assert!(
        ready["preview"]["terminal_output_bounds"]
            .as_array()
            .unwrap()
            .iter()
            .all(|v| v.as_f64().unwrap() <= 1e-12)
    );
    let commit=controller.document(json!({"method":"graph.commit","token":ready["token"],"graph_hash":ready["preview"]["graph_hash"]}),None);
    controller.send(&commit);
    controller.settled(&commit);
    assert_eq!(controller.state()["muted"], true);
    assert_no_capture(&capture);
    controller.apply(json!({"method":"mute.set","muted":false}));
    let completed = controller.wait(|s| s["transport"] == "idle" || s["transport"] == "failed");
    assert!(completed["error"].is_null(), "{completed}");
    assert_eq!(
        completed["playback"]["submitted_frames"],
        (73 + tail) as u64
    );
    assert!(completed["audible_position_frames"].is_null());
    let f = |x| FiniteGraphScalar::new(x).unwrap();
    let filter = PeqFilter::Peaking {
        frequency_hz: f(1000.0),
        q: f(0.707),
        gain_db: f(-6.0),
    };
    let [b0, b1, b2, a1, a2] =
        earph_dsp::peq::PeqDesign::prepare(&filter, 48000, ChannelLayout::Stereo)
            .unwrap()
            .coefficients();
    let mut history = [[0.0_f64; 4]; 2];
    let mut expected = Vec::new();
    for frame in 0..73 + tail {
        for (ch, h) in history.iter_mut().enumerate() {
            let x = if frame < 73 {
                f64::from(integer24(&raw[frame * 6 + ch * 3..frame * 6 + ch * 3 + 3]))
            } else {
                0.0
            };
            let y = b0 * x + b1 * h[0] + b2 * h[1] - a1 * h[2] - a2 * h[3];
            h[1] = h[0];
            h[0] = x;
            h[3] = h[2];
            h[2] = y;
            expected.extend_from_slice(&((y * 0.01).round() as i32).to_le_bytes()[..3]);
        }
    }
    let actual = fs::read(capture).unwrap();
    assert_eq!(actual.len(), expected.len());
    for (a, b) in actual
        .as_chunks::<3>()
        .0
        .iter()
        .zip(expected.as_chunks::<3>().0.iter())
    {
        assert!((integer24(a) - integer24(b)).abs() <= 1);
    }
    controller.apply(json!({"method":"owner.shutdown"}));
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
fn executable_profile_application_stays_muted_and_renders_exact_saved_intent() {
    let directory = directory();
    let source = directory.join("profile.wav");
    let original = wave(&source, 0, 1025);
    let (config, capture) = configuration(&directory);
    let mut host = Host::start(&config);
    let controller = &host.controller;
    select_output(controller, "software", "compatible_conversion");
    controller.apply(json!({"method":"volume.caps.set","software_millidecibels":-30000,"hardware_millidecibels":null}));
    consent(controller);
    let draft = json!({"name":"File headphones","source_policy":"local_file","fidelity":"compatible_conversion",
        "output":{"backend":"alsa","route":"system","device_id":"acceptance-output","manual_setup":"headphones"},
        "volume":{"mode":"software","software_cap_millidecibels":-10000,"hardware_cap_millidecibels":null},
        "latency":{"mode":"balanced","max_resident_bytes":268435456,"max_preparation_bytes":268435456,"max_latency_frames":null},"corrections":[]});
    let intent = json!({"graph":delay_gain_graph(0, 0.5),"processing":{"kind":"float64","quantization":"round_ties_away_saturate_no_dither"}});
    let save = controller.document(
        json!({"method":"profiles.create","id":"headphones","draft":draft,"graph":intent}),
        None,
    );
    let saved_receipt = controller.settled(&save);
    let saved = saved_receipt["receipt"]["result"]["value"].clone();
    append_selected(controller, &source);
    start(controller);
    controller.wait(|s| s["transport"] == "buffering");
    let opened = controller.state();
    assert!(opened["playback"]["backend"]["device_identity_sha256"].is_null());
    let bind = controller.document(
        json!({"method":"profiles.bind","id":"headphones","expected_profile_revision":1,
        "profile_sha256":saved["profile_sha256"],"expected_device_epoch":opened["device_epoch"]}),
        None,
    );
    let refused = controller.send(&bind);
    assert_eq!(
        refused["receipt"]["error"]["code"],
        "CAPABILITY_UNSUPPORTED"
    );
    assert_eq!(controller.state()["output_consented"], true);
    assert_eq!(
        controller.state()["software_gain_db"],
        opened["software_gain_db"]
    );
    assert_eq!(
        controller.query(json!({"method":"capabilities.get"}))["profiles"]["exact_association_backends"],
        json!([])
    );
    let prepare = controller.document(json!({"method":"profiles.prepare","id":"headphones","expected_profile_revision":1,"profile_sha256":saved["profile_sha256"]}), None);
    assert_eq!(controller.send(&prepare)["receipt"]["status"], "accepted");
    let prepared_receipt = controller.settled(&prepare);
    let prepared = prepared_receipt["receipt"]["result"]["value"].clone();
    assert!(controller.state()["configured_profile"].is_null());
    let apply = controller.document(json!({"method":"profiles.apply","id":"headphones","expected_profile_revision":1,
        "profile_sha256":saved["profile_sha256"],"token":prepared["token"],"graph_hash":prepared["preview"]["graph_hash"]}), None);
    assert_eq!(controller.send(&apply)["receipt"]["status"], "accepted");
    let applied = controller.settled(&apply);
    assert!(applied["receipt"]["activation_frame"].is_number());
    let state = controller.state();
    assert_eq!(state["configured_profile"]["id"], "headphones");
    assert_eq!(state["configured_profile"]["revision"], 1);
    assert_eq!(state["volume_policy"]["software_cap_db"], -30.0);
    assert_eq!(state["local_volume_policy"]["software_cap_db"], -30.0);
    assert_eq!(state["software_gain_db"], -40.0);
    assert_eq!(state["muted"], true);
    assert_eq!(state["output_consented"], false);
    assert_no_capture(&capture);
    let unmute =
        controller.send(&controller.document(json!({"method":"mute.set","muted":false}), None));
    assert_eq!(unmute["receipt"]["error"]["code"], "SAFETY_POLICY_BLOCKED");
    let update = controller.document(json!({"method":"profiles.update","id":"headphones","expected_profile_revision":1,"draft":draft,"graph":intent}), None);
    let live_update = controller.send(&update);
    assert_eq!(live_update["receipt"]["error"]["code"], "SOURCE_BUSY");
    controller.apply(json!({"method":"output.consent","device_id":"acceptance-output","profile_id":"headphones"}));
    controller.apply(json!({"method":"mute.set","muted":false}));
    let done = controller.wait(|s| s["transport"] == "idle" || s["transport"] == "failed");
    assert!(done["error"].is_null(), "{done}");
    controller.apply(json!({"method":"owner.shutdown"}));
    assert!(host.child.wait().unwrap().success());
    let actual = fs::read(&capture).unwrap();
    assert_eq!(actual.len(), original.len());
    for (actual, original) in actual
        .as_chunks::<3>()
        .0
        .iter()
        .zip(original.as_chunks::<3>().0.iter())
    {
        let expected = (f64::from(integer24(original)) * 0.005).round() as i32;
        assert!((integer24(actual) - expected).abs() <= 1);
    }
    let mut restarted = Host::start_at(&config, host.controller.runtime.clone());
    let restored = restarted.controller.state();
    assert_eq!(restored["configured_profile"]["revision"], 1);
    assert_eq!(restored["volume_policy"]["software_cap_db"], -30.0);
    assert_eq!(restored["output_consented"], false);
    assert_eq!(restored["muted"], true);
    assert_eq!(restored["transport"], "paused");
    let graph = restarted
        .controller
        .query(json!({"method":"graph.inspect"}));
    assert!(graph["active"].is_null());
    assert!(graph["prepared"].is_null());
    let reconsent = restarted.controller.apply(json!({"method":"output.consent",
        "device_id":restored["output"]["device_id"],"profile_id":restored["output"]["profile_id"]}));
    let before_update = restarted.controller.state();
    assert_eq!(before_update["output_consented"], true);
    let update = restarted.controller.document(json!({"method":"profiles.update","id":"headphones","expected_profile_revision":1,"draft":draft,"graph":intent}), None);
    let updated = restarted.controller.settled(&update);
    let after_update = restarted.controller.state();
    assert!(after_update["configured_profile"].is_null());
    assert_eq!(after_update["muted"], true);
    assert_eq!(after_update["output_consented"], false);
    assert_eq!(
        after_update["local_volume_policy"]["software_cap_db"],
        -30.0
    );
    let checkpoint: earph_core::session::SessionCheckpoint =
        serde_json::from_slice(&fs::read(restarted.controller.runtime.join("queue.json")).unwrap())
            .unwrap();
    assert!(checkpoint.configured_profile.is_none());
    restarted
        .controller
        .apply(json!({"method":"owner.shutdown"}));
    assert!(restarted.child.wait().unwrap().success());
    let mut after_update_restart = Host::start_at(&config, host.controller.runtime.clone());
    let recovered_update = after_update_restart.controller.state();
    assert!(recovered_update["configured_profile"].is_null());
    assert_eq!(recovered_update["output_consented"], false);
    assert_eq!(recovered_update["muted"], true);
    after_update_restart
        .controller
        .apply(json!({"method":"owner.shutdown"}));
    assert!(after_update_restart.child.wait().unwrap().success());
    fs::write(directory.join("profile-application-receipts.json"), serde_json::to_vec_pretty(&json!({
        "save":saved_receipt,"prepare":prepared_receipt,"apply":applied,"configured_state":state,
        "denied_unmute":unmute,"denied_live_update":live_update,"restored_state":restored,
        "reconsent_before_update":reconsent,"before_update":before_update,
        "stopped_update":updated,"after_update":after_update,
        "persisted_update":checkpoint,"recovered_update":recovered_update,
    })).unwrap()).unwrap();
    eprintln!(
        "{}",
        json!({"profile_application_artifacts":directory,"runtime":host.controller.runtime})
    );
}

#[test]
fn executable_fixed_profiles_require_fresh_arming_and_restore_lower_gain() {
    for processed in [false, true] {
        let directory = directory();
        let source = directory.join("fixed-profile.wav");
        let original = wave(&source, 0, 1025);
        let (config, capture) = configuration(&directory);
        let mut host = Host::start(&config);
        let controller = &host.controller;
        let fidelity = if processed {
            "compatible_conversion"
        } else {
            "strict_preservation"
        };
        select_output(controller, "fixed", fidelity);
        controller.apply(json!({"method":"volume.set","millidecibels":0,"ramp_frames":0}));
        consent(controller);
        controller.apply(json!({"method":"volume.arm_fixed","device_id":"acceptance-output","profile_id":"acceptance-profile"}));
        let draft = json!({"name":"Owned fixed intent","source_policy":"local_file","fidelity":fidelity,
            "output":{"backend":"alsa","route":"system","device_id":"acceptance-output","manual_setup":"headphones"},
            "volume":{"mode":"fixed","software_cap_millidecibels":0,"hardware_cap_millidecibels":null},
            "latency":{"mode":"balanced","max_resident_bytes":268435456,"max_preparation_bytes":268435456,"max_latency_frames":null},"corrections":[]});
        let port = json!({"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"signed24","max_quantum_frames":64});
        let intent = if processed {
            json!({"graph":delay_gain_graph(0,0.5),"processing":{"kind":"float64","quantization":"round_ties_away_saturate_no_dither"}})
        } else {
            json!({"graph":{"schema_version":1,"nodes":[{"id":"in","kind":{"kind":"input"},"format":port},{"id":"out","kind":{"kind":"output"},"format":port}],"edges":[{"from":"in","to":"out","from_port":0,"to_port":0}],"output":"out"},"processing":{"kind":"integer_bypass"}})
        };
        let save = controller.document(
            json!({"method":"profiles.create","id":"headphones","draft":draft,"graph":intent}),
            None,
        );
        let saved = controller.settled(&save)["receipt"]["result"]["value"].clone();
        append_selected(controller, &source);
        start(controller);
        controller.wait(|s| s["transport"] == "buffering");
        controller.apply(json!({"method":"volume.caps.set","software_millidecibels":-20000,"hardware_millidecibels":null}));
        let blocked = controller.document(json!({"method":"profiles.prepare","id":"headphones","expected_profile_revision":1,"profile_sha256":saved["profile_sha256"]}), None);
        assert_eq!(
            controller.send(&blocked)["receipt"]["error"]["code"],
            "SAFETY_POLICY_BLOCKED"
        );
        assert_eq!(controller.state()["software_gain_db"], -20.0);
        controller.apply(json!({"method":"volume.caps.set","software_millidecibels":0,"hardware_millidecibels":null}));
        assert_eq!(controller.state()["software_gain_db"], -20.0);
        controller.apply(json!({"method":"volume.set","millidecibels":0,"ramp_frames":0}));
        controller.apply(json!({"method":"volume.arm_fixed","device_id":"acceptance-output","profile_id":"acceptance-profile"}));
        let prepare = controller.document(json!({"method":"profiles.prepare","id":"headphones","expected_profile_revision":1,"profile_sha256":saved["profile_sha256"]}), None);
        let prepared = controller.settled(&prepare)["receipt"]["result"]["value"].clone();
        assert_eq!(
            prepared["preview"]["profile_application"]["volume_mode"],
            "fixed"
        );
        assert_eq!(
            prepared["preview"]["profile_application"]["fidelity"],
            fidelity
        );
        let apply = controller.document(json!({"method":"profiles.apply","id":"headphones","expected_profile_revision":1,
            "profile_sha256":saved["profile_sha256"],"token":prepared["token"],"graph_hash":prepared["preview"]["graph_hash"]}), None);
        controller.settled(&apply);
        let installed = controller.state();
        assert_eq!(installed["configured_profile"]["id"], "headphones");
        assert_eq!(installed["output_consented"], false);
        assert_eq!(installed["software_gain_db"], 0.0);
        assert_eq!(installed["local_volume_policy"]["software_cap_db"], 0.0);
        assert_no_capture(&capture);
        controller.apply(json!({"method":"output.consent","device_id":"acceptance-output","profile_id":"headphones"}));
        let unmute = controller.document(json!({"method":"mute.set","muted":false}), None);
        assert_eq!(
            controller.send(&unmute)["receipt"]["error"]["code"],
            "SAFETY_POLICY_BLOCKED"
        );
        controller.apply(json!({"method":"volume.arm_fixed","device_id":"acceptance-output","profile_id":"headphones"}));
        controller.apply(json!({"method":"mute.set","muted":false}));
        let done = controller.wait(|s| s["transport"] == "idle" || s["transport"] == "failed");
        assert!(done["error"].is_null(), "{done}");
        controller.apply(json!({"method":"owner.shutdown"}));
        assert!(host.child.wait().unwrap().success());
        let actual = fs::read(&capture).unwrap();
        if processed {
            assert_eq!(actual.len(), original.len());
            for (a, b) in actual
                .as_chunks::<3>()
                .0
                .iter()
                .zip(original.as_chunks::<3>().0)
            {
                assert!((integer24(a) - (f64::from(integer24(b)) * 0.5).round() as i32).abs() <= 1);
            }
        } else {
            assert_eq!(actual, original);
        }
        let mut restarted = Host::start_at(&config, host.controller.runtime.clone());
        let restored = restarted.controller.state();
        assert_eq!(restored["configured_profile"]["revision"], 1);
        assert_eq!(restored["software_gain_db"], -40.0);
        assert_eq!(restored["muted"], true);
        assert_eq!(restored["output_consented"], false);
        assert!(restored["playback"].is_null());
        restarted.controller.apply(json!({"method":"output.consent","device_id":"acceptance-output","profile_id":"headphones"}));
        assert_eq!(restarted.controller.state()["output_consented"], true);
        let update = restarted.controller.document(json!({"method":"profiles.update","id":"headphones","expected_profile_revision":1,"draft":draft,"graph":intent}), None);
        restarted.controller.settled(&update);
        let updated = restarted.controller.state();
        assert!(updated["configured_profile"].is_null());
        assert_eq!(updated["output_consented"], false);
        assert_eq!(updated["local_volume_policy"]["software_cap_db"], 0.0);
        assert_eq!(updated["software_gain_db"], -40.0);
        restarted
            .controller
            .apply(json!({"method":"owner.shutdown"}));
        assert!(restarted.child.wait().unwrap().success());
    }
}

fn src_reference(bytes: &[u8]) -> Vec<u8> {
    use earph_core::{audio::ChannelLayout, dsp_nodes::SrcPreset};
    use earph_dsp::resample::PreparedSrc;
    let mut values: Vec<f64> = bytes
        .as_chunks::<3>()
        .0
        .iter()
        .map(|b| {
            let sample =
                i32::from_le_bytes([b[0], b[1], b[2], if b[2] & 128 == 0 { 0 } else { 255 }]);
            f64::from(sample) / 8388608.0
        })
        .collect();
    for (input, output) in [(48000, 44100), (44100, 48000)] {
        let mut src = PreparedSrc::prepare(
            input,
            output,
            ChannelLayout::Stereo,
            64,
            SrcPreset::Responsive,
            16 << 20,
        )
        .unwrap();
        let mut converted = Vec::new();
        let mut buffer = [0.0; 74];
        let mut offset = 0;
        while offset < values.len() / 2 {
            let step = src.process(&values[offset * 2..], &mut buffer).unwrap();
            offset += step.consumed_frames;
            converted.extend_from_slice(&buffer[..step.produced_frames * 2]);
        }
        src.begin_finish().unwrap();
        loop {
            let step = src.finish(&mut buffer).unwrap();
            converted.extend_from_slice(&buffer[..step.produced_frames * 2]);
            if step.terminal {
                break;
            }
        }
        values = converted;
    }
    let gain = 10_f64.powf(-48.0 / 20.0);
    values
        .into_iter()
        .flat_map(|v| {
            let value = (v * gain * 8388608.0).round().clamp(-8388608.0, 8388607.0) as i32;
            value.to_le_bytes()[..3].to_vec()
        })
        .collect()
}

#[test]
fn executable_streaming_graph_joins_sources_and_drains_the_actual_src_tail() {
    for lengths in [[0, 0], [1, 0], [63, 1], [64, 65], [4097, 4102]] {
        let directory = directory();
        let first = directory.join("first.wav");
        let second = directory.join("second.wav");
        let mut input = wave(&first, 0, lengths[0]);
        input.extend(wave(&second, lengths[0], lengths[1]));
        let expected = src_reference(&input);
        let (config, capture) = configuration(&directory);
        let mut host = Host::start(&config);
        let controller = &host.controller;
        select_output(controller, "software", "compatible_conversion");
        controller.apply(json!({"method":"volume.set","millidecibels":-48000,"ramp_frames":0}));
        consent(controller);
        append_selected(controller, &first);
        controller.apply(json!({"method":"queue.append","path":second}));
        start(controller);
        controller.wait(|s| s["transport"] == "buffering");
        let (_, ready) = prepare_graph(controller, graph_fixtures::src_roundtrip());
        let commit = controller.document(json!({"method":"graph.commit","token":ready["token"],"graph_hash":ready["preview"]["graph_hash"]}),None);
        assert_eq!(controller.send(&commit)["receipt"]["status"], "accepted");
        controller.settled(&commit);
        assert_no_capture(&capture);
        controller.apply(json!({"method":"mute.set","muted":false}));
        let complete = controller.wait(|s| s["transport"] == "idle");
        assert!(complete["playback"]["audible_entry_id"].is_null());
        assert!(complete["playback"]["audible_source_position_frames"].is_null());
        assert_eq!(
            complete["playback"]["processing"]["timing_kind"],
            "streaming"
        );
        assert_eq!(
            complete["playback"]["processing"]["streaming"]["source_accepted_frames"],
            lengths.iter().sum::<usize>()
        );
        assert_eq!(
            complete["playback"]["processing"]["streaming"]["generated_output_frames"],
            expected.len() / 6
        );
        assert_eq!(complete["playback"]["submitted_frames"], expected.len() / 6);
        controller.apply(json!({"method":"owner.shutdown"}));
        assert!(host.child.wait().unwrap().success());
        assert_eq!(
            fs::read(capture).unwrap_or_default(),
            expected,
            "lengths={lengths:?}"
        );
    }
}

fn install_src(controller: &Controller) {
    let (_, ready) = prepare_graph(controller, graph_fixtures::src_roundtrip());
    let commit = controller.document(json!({"method":"graph.commit","token":ready["token"],"graph_hash":ready["preview"]["graph_hash"]}),None);
    assert_eq!(controller.send(&commit)["receipt"]["status"], "accepted");
    controller.settled(&commit);
}

#[test]
fn executable_streaming_pause_and_resume_keep_one_continuous_guard_and_exact_capture() {
    let directory = directory();
    let source = directory.join("continuous.wav");
    let second = directory.join("after-hold.wav");
    let mut original = wave(&source, 0, 32769);
    original.extend(wave(&second, 32769, 32768));
    let expected = src_reference(&original);
    let (config, capture) = configuration(&directory);
    let mut capture_gate = CaptureGate::new(&capture);
    let mut host = Host::start(&config);
    let controller = &host.controller;
    select_output(controller, "software", "compatible_conversion");
    controller.apply(json!({"method":"volume.set","millidecibels":-48000,"ramp_frames":0}));
    consent(controller);
    append_selected(controller, &source);
    start(controller);
    controller.wait(|s| s["transport"] == "buffering");
    install_src(controller);
    controller.apply(json!({"method":"mute.set","muted":false}));
    capture_gate.wait_full();
    controller.wait(|s| {
        s["transport"] == "playing" && s["playback"]["submitted_frames"].as_u64().unwrap_or(0) > 0
    });
    let pause = controller.document(json!({"method":"sessions.pause"}), None);
    assert_eq!(controller.send(&pause)["receipt"]["status"], "accepted");
    capture_gate.release();
    controller.settled(&pause);
    let paused = controller.wait(|s| s["transport"] == "paused");
    let before = &paused["playback"]["processing"]["streaming"];
    assert!(before["source_accepted_frames"].as_u64().unwrap() > 0);
    assert!(
        before["output_epochs"]["stream"].as_u64().unwrap()
            > before["source_epochs"]["stream"].as_u64().unwrap()
    );
    let stable = controller.state();
    assert_eq!(stable["playback"], paused["playback"]);
    controller.apply(json!({"method":"queue.append","path":second}));
    let resume = controller.document(json!({"method":"sessions.resume"}), None);
    assert_eq!(controller.send(&resume)["receipt"]["status"], "accepted");
    let completed = controller.wait(|s| s["transport"] == "idle" || s["transport"] == "failed");
    assert!(completed["error"].is_null(), "{completed}");
    assert_eq!(
        completed["playback"]["processing"]["streaming"]["source_accepted_frames"],
        65537
    );
    controller.apply(json!({"method":"owner.shutdown"}));
    assert!(host.child.wait().unwrap().success());
    assert_eq!(capture_gate.finish(), expected);
    assert_eq!(fs::read(&source).unwrap()[44..], original[..32769 * 6]);
}

#[test]
fn executable_streaming_native_write_fault_keeps_actual_progress_and_revokes_consent() {
    let directory = directory();
    let source = directory.join("fault.wav");
    wave(&source, 0, 65537);
    let config = directory.join("alsa-full.conf");
    fs::write(&config,"pcm.null { type null }\npcm.earph_capture { type file slave.pcm null file \"/dev/full\" format raw }\n").unwrap();
    let host = Host::start(&config);
    let controller = &host.controller;
    select_output(controller, "software", "compatible_conversion");
    controller.apply(json!({"method":"volume.set","millidecibels":-48000,"ramp_frames":0}));
    consent(controller);
    append_selected(controller, &source);
    start(controller);
    controller.wait(|s| s["transport"] == "buffering");
    install_src(controller);
    controller.apply(json!({"method":"mute.set","muted":false}));
    let failed = controller.wait(|s| s["transport"] == "failed");
    assert_eq!(failed["error"], "OUTPUT_FAULT");
    assert_eq!(failed["output_consented"], false);
    assert_eq!(failed["muted"], true);
    let observation = &failed["playback"];
    let streaming = &observation["processing"]["streaming"];
    assert!(streaming["source_accepted_frames"].as_u64().unwrap() > 0);
    assert_eq!(
        streaming["source_accepted_frames"],
        observation["source_position_frames"]
    );
    assert!(
        streaming["generated_output_frames"].as_u64().unwrap()
            >= streaming["submission_frontier_frames"].as_u64().unwrap()
    );
    assert!(observation["audible_source_position_frames"].is_null());
    assert!(observation["processing"]["audio_payload_bytes"].is_null());
    let unmute =
        controller.send(&controller.document(json!({"method":"mute.set","muted":false}), None));
    assert_eq!(unmute["receipt"]["status"], "rejected");
    controller.apply(json!({"method":"owner.shutdown"}));
}

#[test]
fn executable_streaming_seek_starts_a_new_guard_at_the_exact_source_suffix() {
    let directory = directory();
    let source = directory.join("seek.wav");
    let original = wave(&source, 0, 8199);
    let expected = src_reference(&original[17 * 6..]);
    let (config, capture) = configuration(&directory);
    let mut host = Host::start(&config);
    let controller = &host.controller;
    select_output(controller, "software", "compatible_conversion");
    controller.apply(json!({"method":"volume.set","millidecibels":-48000,"ramp_frames":0}));
    consent(controller);
    append_selected(controller, &source);
    start(controller);
    controller.wait(|s| s["transport"] == "buffering");
    install_src(controller);
    let seek = controller.document(json!({"method":"sessions.seek","position_frames":17}), None);
    assert_eq!(controller.send(&seek)["receipt"]["status"], "accepted");
    controller.settled(&seek);
    let sought = controller.state();
    assert_eq!(sought["playback"]["source_position_frames"], 17);
    assert_eq!(
        sought["playback"]["processing"]["streaming"]["source_accepted_frames"],
        0
    );
    assert_eq!(
        sought["playback"]["processing"]["streaming"]["generated_output_frames"],
        0
    );
    controller.apply(json!({"method":"mute.set","muted":false}));
    let completed = controller.wait(|s| s["transport"] == "idle" || s["transport"] == "failed");
    assert!(completed["error"].is_null(), "{completed}");
    assert_eq!(
        completed["playback"]["processing"]["streaming"]["source_accepted_frames"],
        8199 - 17
    );
    controller.apply(json!({"method":"owner.shutdown"}));
    assert!(host.child.wait().unwrap().success());
    assert_eq!(fs::read(capture).unwrap(), expected);
}

#[test]
fn executable_streaming_replacement_resumes_a_same_rate_graph_at_accepted_source_position() {
    let directory = directory();
    let source = directory.join("replace.wav");
    let original = wave(&source, 0, 65537);
    let (config, capture) = configuration(&directory);
    let mut capture_gate = CaptureGate::new(&capture);
    let mut host = Host::start(&config);
    let controller = &host.controller;
    select_output(controller, "software", "compatible_conversion");
    controller.apply(json!({"method":"volume.set","millidecibels":-48000,"ramp_frames":0}));
    consent(controller);
    append_selected(controller, &source);
    start(controller);
    controller.wait(|s| s["transport"] == "buffering");
    install_src(controller);
    controller.apply(json!({"method":"mute.set","muted":false}));
    capture_gate.wait_full();
    controller.wait(|s| {
        s["transport"] == "playing" && s["playback"]["submitted_frames"].as_u64().unwrap_or(0) > 0
    });
    controller.apply(json!({"method":"mute.set","muted":true}));
    capture_gate.release();
    let muted = controller
        .wait(|s| s["playback"]["processing"]["streaming"]["output_epochs"] == s["epochs"]);
    let accepted = muted["playback"]["source_position_frames"]
        .as_u64()
        .unwrap() as usize;
    let retained = muted["playback"]["processing"]["streaming"]["backend_origin_frames"]
        .as_u64()
        .unwrap() as usize;
    let (_, ready) = prepare_graph(controller, delay_gain_graph(0, 0.5));
    let commit = controller.document(json!({"method":"graph.commit","token":ready["token"],"graph_hash":ready["preview"]["graph_hash"]}),None);
    assert_eq!(controller.send(&commit)["receipt"]["status"], "accepted");
    controller.settled(&commit);
    let installed = controller.state();
    assert_eq!(installed["playback"]["source_position_frames"], accepted);
    assert_eq!(
        installed["playback"]["processing"]["source_boundary"]["source_position_frames"],
        accepted
    );
    assert_eq!(
        installed["playback"]["processing"]["timing_kind"],
        "same_rate"
    );
    let gain = 10_f64.powf(-48.0 / 20.0);
    let expected: Vec<u8> = original[accepted * 6..]
        .as_chunks::<3>()
        .0
        .iter()
        .flat_map(|b| {
            let v = f64::from(integer24(b)) / 8388608.0 * 0.5;
            ((v * gain * 8388608.0).round() as i32).to_le_bytes()[..3].to_vec()
        })
        .collect();
    controller.apply(json!({"method":"mute.set","muted":false}));
    let completed = controller.wait(|s| s["transport"] == "idle" || s["transport"] == "failed");
    assert!(completed["error"].is_null(), "{completed}");
    controller.apply(json!({"method":"owner.shutdown"}));
    assert!(host.child.wait().unwrap().success());
    let actual = capture_gate.finish();
    assert_eq!(&actual[retained * 6..], expected);
}

#[test]
fn executable_streaming_stop_closes_the_native_owner_and_preserves_its_written_prefix() {
    let directory = directory();
    let source = directory.join("stop-stream.wav");
    let original = wave(&source, 0, 65537);
    let (config, capture) = configuration(&directory);
    let mut capture_gate = CaptureGate::new(&capture);
    let mut host = Host::start(&config);
    let controller = &host.controller;
    select_output(controller, "software", "compatible_conversion");
    controller.apply(json!({"method":"volume.set","millidecibels":-48000,"ramp_frames":0}));
    consent(controller);
    append_selected(controller, &source);
    start(controller);
    controller.wait(|s| s["transport"] == "buffering");
    install_src(controller);
    controller.apply(json!({"method":"mute.set","muted":false}));
    capture_gate.wait_full();
    controller.wait(|s| {
        s["transport"] == "playing" && s["playback"]["submitted_frames"].as_u64().unwrap_or(0) > 0
    });
    let stop = controller.document(json!({"method":"sessions.stop"}), None);
    assert_eq!(controller.send(&stop)["receipt"]["status"], "accepted");
    capture_gate.release();
    controller.settled(&stop);
    let stopped = controller.wait(|s| s["transport"] == "idle");
    assert!(stopped["error"].is_null(), "{stopped}");
    assert_eq!(stopped["muted"], true);
    let accepted = stopped["playback"]["processing"]["streaming"]["source_accepted_frames"]
        .as_u64()
        .unwrap() as usize;
    let submitted = stopped["playback"]["submitted_frames"].as_u64().unwrap() as usize;
    assert!(accepted > 0 && accepted < 65537);
    assert!(submitted > 0);
    controller.apply(json!({"method":"owner.shutdown"}));
    assert!(host.child.wait().unwrap().success());
    let actual = capture_gate.finish();
    let expected = src_reference(&original[..accepted * 6]);
    assert_eq!(actual.len(), submitted * 6);
    assert_eq!(actual, expected[..actual.len()]);
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
fn executable_validates_src_without_allocating_a_runtime_owner_or_changing_playback() {
    let directory = directory();
    let source = directory.join("src-validation.wav");
    wave(&source, 0, 513);
    let (config, capture) = configuration(&directory);
    let host = Host::start(&config);
    let controller = &host.controller;
    select_output(controller, "software", "compatible_conversion");
    consent(controller);
    append_selected(controller, &source);
    start(controller);
    let before = controller.wait(|state| state["transport"] == "buffering");
    let command = controller.document(
        json!({"method":"graph.validate",
        "graph":graph_fixtures::src_roundtrip(),
        "processing":{"kind":"float64","quantization":"round_ties_away_saturate_no_dither"}}),
        None,
    );
    assert_eq!(controller.send(&command)["receipt"]["status"], "accepted");
    let reply = controller.settled(&command);
    let preview = &reply["receipt"]["result"]["value"];
    assert_eq!(preview["timing_kind"], "streaming");
    assert_eq!(preview["stages"].as_array().unwrap().len(), 4);
    assert!(preview.get("latency_frames").is_none());
    assert!(preview.get("tail_frames").is_none());
    let after = controller.state();
    assert_eq!(after["revision"], before["revision"]);
    assert_eq!(after["epochs"], before["epochs"]);
    assert_eq!(after["playback"], before["playback"]);
    let inspection = controller.query(json!({"method":"graph.inspect"}));
    assert!(inspection["prepared"].is_null());
    assert_eq!(inspection["service"]["resident_bytes"], 0);
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
        assert_eq!(ready["preview"]["backend"]["audio_storage_bytes"], 384);
        assert_eq!(ready["preview"]["resources"]["backend_audio_bytes"], 384);
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
        assert_eq!(state["playback"]["backend"], ready["preview"]["backend"]);
        assert_eq!(inspection["active"]["backend"], ready["preview"]["backend"]);
        assert_eq!(state["graph_epoch"], before_epoch + 1);
        assert_eq!(
            state["playback"]["processing"]["graph_hash"],
            inspection["active"]["graph_hash"]
        );
        assert_eq!(state["playback"]["processing"]["latency_frames"], delay);
        assert_eq!(state["playback"]["processing"]["timing_kind"], "same_rate");
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
