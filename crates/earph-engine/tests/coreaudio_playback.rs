// ABOUTME: Exercises generated local media through the real owner, CLI, and selected Mac output.
// ABOUTME: Requires explicit hardware approval and retains raw evidence in a private report.
#![cfg(target_os = "macos")]
mod support;
use earph_engine::EngineOwner;
use serde_json::{Value, json};
use std::{
    cell::Cell,
    fs::{self, OpenOptions},
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Controller {
    runtime: PathBuf,
    next: Cell<u64>,
}
impl Controller {
    fn send(&self, document: &Value) -> Value {
        let mut child = Command::new(env!("CARGO_BIN_EXE_earph-engine"))
            .args(["request", "--runtime"])
            .arg(&self.runtime)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(&serde_json::to_vec(document).unwrap())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(!output.stdout.is_empty(), "CLI returned no reply");
        serde_json::from_slice(&output.stdout).unwrap()
    }
    fn document(&self, payload: Value, state: Option<&Value>, starting: bool) -> Value {
        let n = self.next.get();
        self.next.set(n + 1);
        json!({"schema_version":1,"request_id":format!("mac-acceptance-{n}"),"zone_id":"main",
            "owner_id":state.map(|s|&s["owner_id"]),
            "session_id":if starting {json!("owned-hardware-acceptance")} else {state.map(|s|s["session_id"].clone()).unwrap_or(Value::Null)},
            "expected_revision":state.map(|s|&s["revision"]),"payload":payload})
    }
    fn state(&self) -> Value {
        self.send(&self.document(json!({"method":"state.get"}),None,false))["receipt"]["result"]["value"].clone()
    }
    fn apply(&self, payload: Value) -> Value {
        let state = self.state();
        let document = self.document(payload, Some(&state), false);
        self.settle(&document)
    }
    fn settle(&self, document: &Value) -> Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let reply = self.send(document);
            match reply["receipt"]["status"].as_str() {
                Some("accepted") => assert!(
                    Instant::now() < deadline,
                    "operation did not settle: {reply}"
                ),
                Some("applied") => return reply,
                _ => panic!("operation failed: {reply}"),
            }
            thread::sleep(Duration::from_millis(2));
        }
    }
    fn wait(&self, transport: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let state = self.state();
            if state["transport"] == transport {
                return state;
            }
            assert_ne!(state["transport"], "failed", "playback failed: {state}");
            assert!(
                Instant::now() < deadline,
                "playback did not reach {transport}: {state}"
            );
            thread::sleep(Duration::from_millis(2));
        }
    }
}
fn wave(path: &Path, frames: usize) {
    let mut payload = Vec::with_capacity(frames * 6);
    for frame in 0..frames {
        let ramp = (frame.min(frames - 1 - frame).min(960) as f64) / 960.0;
        let value =
            ((frame as f64 * 440.0 * std::f64::consts::TAU / 48000.0).sin() * 2097151.0 * ramp)
                .round() as i32;
        for _ in 0..2 {
            payload.extend_from_slice(&value.to_le_bytes()[..3]);
        }
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(b"RIFF").unwrap();
    file.write_all(&(36 + payload.len() as u32).to_le_bytes())
        .unwrap();
    file.write_all(b"WAVEfmt ").unwrap();
    for bytes in [
        16_u32.to_le_bytes().to_vec(),
        1_u16.to_le_bytes().to_vec(),
        2_u16.to_le_bytes().to_vec(),
        48000_u32.to_le_bytes().to_vec(),
        288000_u32.to_le_bytes().to_vec(),
        6_u16.to_le_bytes().to_vec(),
        24_u16.to_le_bytes().to_vec(),
        b"data".to_vec(),
        (payload.len() as u32).to_le_bytes().to_vec(),
        payload,
    ] {
        file.write_all(&bytes).unwrap();
    }
    file.sync_all().unwrap();
}
fn settings(device: &earph_coreaudio::DeviceSnapshot) -> Value {
    json!({"uid":device.uid,"rate":device.nominal_rate_hz,"buffer":device.buffer_frames,
        "hog_owner":device.hog_owner_pid,"streams":device.output_streams})
}

#[test]
#[ignore = "requires Doctor Biz's hardware approval and an explicit private Core Audio selection"]
fn owned_media_plays_through_owner_and_cli_with_unknown_audible_timing() {
    run_owned_media(false);
}

#[test]
#[ignore = "requires Doctor Biz's hardware approval and exact private device selection; starts silent hardware"]
fn owned_silent_44100_system_lifecycle_on_selected_48000_device() {
    run_owned_media(true);
}

fn run_owned_media(system_mismatch: bool) {
    assert_eq!(
        std::env::var("EARPH_ENGINE_PLAYBACK_APPROVAL").as_deref(),
        Ok("granted"),
        "hardware playback approval is required"
    );
    let selected: Value = serde_json::from_slice(
        &fs::read(
            std::env::var_os("EARPH_ENGINE_PLAYBACK_REQUEST")
                .expect("private device selection is required"),
        )
        .unwrap(),
    )
    .unwrap();
    let uid = selected["device_uid"]
        .as_str()
        .expect("explicit device UID is required");
    let before = earph_coreaudio::inspect(uid).unwrap();
    assert_eq!(
        before.nominal_rate_hz, 48000.0,
        "acceptance never changes the selected sample rate"
    );
    if system_mismatch {
        assert_eq!(before.buffer_frames, 512);
    }
    let report_path = PathBuf::from(
        std::env::var_os("EARPH_ENGINE_PLAYBACK_REPORT")
            .expect("a new private report path is required"),
    );
    let mut report = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(&report_path)
        .unwrap();
    let directory = report_path.parent().unwrap();
    let source = directory.join("owned-generated.wav");
    if system_mismatch {
        silent_wave(&source, 72000);
    } else {
        wave(&source, 72000);
    }
    let source_sha256 = earph_harness::sha256(&fs::read(&source).unwrap());
    let runtime = support::runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let join = thread::spawn(move || owner.serve());
    let controller = Controller {
        runtime,
        next: Cell::new(0),
    };
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        controller.apply(json!({"method":"output.select","output":{"backend":"core_audio","route":"system",
            "endpoint":uid,"device_id":"native-acceptance","profile_id":"native-acceptance",
            "period_frames":if system_mismatch {64} else {before.buffer_frames},"buffer_frames":if system_mismatch {256} else {(before.buffer_frames*8).min(65536)},
            "volume_mode":"software","fidelity":"compatible_conversion"}}));
        controller.apply(json!({"method":"volume.set","millidecibels":-40000,"ramp_frames":960}));
        controller.apply(json!({"method":"output.consent","device_id":"native-acceptance","profile_id":"native-acceptance"}));
        controller.apply(json!({"method":"queue.append","path":source}));
        let queue = controller.apply(json!({"method":"queue.get","offset":0,"limit":64}));
        controller.apply(json!({"method":"queue.select","id":queue["receipt"]["result"]["value"]["entries"][0]["id"]}));
        let state = controller.state();
        let start = controller.document(json!({"method":"sessions.start","source":"local_file","device_id":"native-acceptance","takeover":false,"seekable":true}),Some(&state),true);
        assert_eq!(controller.send(&start)["receipt"]["status"], "accepted");
        let ready = controller.wait("buffering");
        assert_eq!(ready["muted"], true);
        controller.apply(json!({"method":"mute.set","muted":false}));
        controller.wait("playing");
        controller.apply(json!({"method":"sessions.pause"}));
        let paused = controller.wait("paused");
        assert!(paused["playback"]["audible_source_position_frames"].is_null());
        controller.apply(json!({"method":"sessions.seek","position_frames":12000}));
        let seeked = controller.wait("paused");
        assert_eq!(seeked["playback"]["submitted_frames"], 0);
        let resumed_at = Instant::now();
        controller.apply(json!({"method":"sessions.resume"}));
        let ended = controller.wait("idle");
        let resumed_elapsed = resumed_at.elapsed();
        let resumed_elapsed_ns = resumed_elapsed.as_nanos();
        let expected_suffix_duration = Duration::from_secs_f64(60000.0 / 44100.0);
        let duration_tolerance = Duration::from_millis(20);
        assert!(ended["error"].is_null(), "{ended}");
        assert_eq!(ended["playback"]["submitted_frames"], 60000);
        assert!(ended["playback"]["consumed_frames"].is_null());
        assert!(ended["playback"]["backend_delay_frames"].is_null());
        assert_eq!(
            ended["playback"]["backend"]["client_format"]["representation"],
            "float32"
        );
        assert_eq!(ended["playback"]["backend"]["preservation"], "modified");
        if system_mismatch {
            // This measures resume request through natural owner completion, never acoustic presentation.
            // The 20 ms budget covers one 512/48k callback, four 64/44.1k periods, and polling.
            assert!(
                resumed_elapsed >= expected_suffix_duration - duration_tolerance,
                "source-rate drain completed too early: {resumed_elapsed:?}"
            );
            assert!(
                resumed_elapsed < Duration::from_secs(10),
                "source-rate drain exceeded the owner observation deadline"
            );
            assert_eq!(
                ended["playback"]["output_activity"]["close_confirmed"],
                true
            );
            assert_eq!(
                ended["playback"]["consumption_basis"],
                "audio_queue_sample_time_not_audible_position"
            );
            assert!(ended["playback"]["audible_source_position_frames"].is_null());
            assert_eq!(
                ended["playback"]["backend"]["client_format"]["sample_rate_hz"],
                44100.0
            );
            assert_eq!(
                ended["playback"]["backend"]["device_format"]["sample_rate_hz"],
                48000.0
            );
        }
        controller.settle(&start);
        let cancelled = if system_mismatch {
            controller.apply(json!({"method":"queue.select","id":queue["receipt"]["result"]["value"]["entries"][0]["id"]}));
            let state = controller.state();
            let restart = controller.document(json!({"method":"sessions.start","source":"local_file","device_id":"native-acceptance","takeover":false,"seekable":true}),Some(&state),true);
            assert_eq!(controller.send(&restart)["receipt"]["status"], "accepted");
            controller.wait("buffering");
            controller.apply(json!({"method":"mute.set","muted":false}));
            controller.wait("playing");
            controller.apply(json!({"method":"mute.set","muted":true}));
            let muted = controller.state();
            assert_eq!(muted["muted"], true);
            controller.apply(json!({"method":"sessions.stop"}));
            let cancelled = controller.wait("idle");
            assert!(cancelled["error"].is_null(), "{cancelled}");
            assert_eq!(cancelled["muted"], true);
            assert_eq!(
                cancelled["playback"]["output_activity"]["close_confirmed"],
                true
            );
            json!({"muted":muted,"stopped":cancelled})
        } else {
            Value::Null
        };
        json!({"ready":ready,"paused":paused,"seeked":seeked,"ended":ended,"resumed_elapsed_ns":resumed_elapsed_ns,"expected_suffix_client_frames":60000,"expected_suffix_duration_ns":if system_mismatch {json!(expected_suffix_duration.as_nanos())} else {Value::Null},"duration_lower_tolerance_ns":if system_mismatch {json!(duration_tolerance.as_nanos())} else {Value::Null},"duration_measurement_boundary":"resume_request_to_owner_natural_idle_not_acoustic_presentation","cancelled":cancelled})
    }));
    controller.apply(json!({"method":"owner.shutdown"}));
    join.join().unwrap().unwrap();
    let after = earph_coreaudio::inspect(uid).unwrap();
    let unchanged = if system_mismatch {
        serde_json::to_value(&before).unwrap() == serde_json::to_value(&after).unwrap()
    } else {
        settings(&before) == settings(&after)
    };
    let source_unchanged = source_sha256 == earph_harness::sha256(&fs::read(&source).unwrap());
    let document = json!({"schema_version":1,"status":if result.is_ok()&&unchanged&&source_unchanged {"pass"}else{"fail"},
        "settings_unchanged":unchanged,"before":before,"after":after,"observations":result.as_ref().ok(),
        "source":if system_mismatch {"owned_generated_silent_signed16_stereo"} else {"owned_generated_440hz_signed24_stereo"},"sample_rate_hz":if system_mismatch {44100} else {48000},"source_frames":72000,
        "source_sha256":source_sha256,"source_unchanged":source_unchanged,
        "software_gain_db":-40,"maximum_output_dbfs":if system_mismatch {Value::Null} else {json!(-52.0)},"dac_preservation":"unverified",
        "build":earph_harness::build_provenance()});
    report
        .write_all(&serde_json::to_vec_pretty(&document).unwrap())
        .unwrap();
    report.sync_all().unwrap();
    assert!(unchanged, "selected settings changed");
    assert!(source_unchanged, "generated source changed during playback");
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

fn silent_wave(path: &Path, frames: usize) {
    let payload_bytes = u32::try_from(frames * 4).unwrap();
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .unwrap();
    file.write_all(b"RIFF").unwrap();
    file.write_all(&(36 + payload_bytes).to_le_bytes()).unwrap();
    file.write_all(b"WAVEfmt ").unwrap();
    file.write_all(&16_u32.to_le_bytes()).unwrap();
    file.write_all(&1_u16.to_le_bytes()).unwrap();
    file.write_all(&2_u16.to_le_bytes()).unwrap();
    file.write_all(&44100_u32.to_le_bytes()).unwrap();
    file.write_all(&176400_u32.to_le_bytes()).unwrap();
    file.write_all(&4_u16.to_le_bytes()).unwrap();
    file.write_all(&16_u16.to_le_bytes()).unwrap();
    file.write_all(b"data").unwrap();
    file.write_all(&payload_bytes.to_le_bytes()).unwrap();
    file.write_all(&vec![0_u8; payload_bytes as usize]).unwrap();
    file.sync_all().unwrap();
}
