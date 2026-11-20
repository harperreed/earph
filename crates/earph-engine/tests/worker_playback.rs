// ABOUTME: Tests reservation, fault, and pause behavior through real private IPC.
// ABOUTME: Uses isolated child processes with generated media and ALSA file-to-null routes.
#![cfg(target_os = "linux")]
mod support;
use earph_core::{control::*, ids::*};
use earph_engine::{EngineOwner, request};
use serde_json::{Value, json};
use std::{
    cell::Cell,
    fs,
    path::{Path, PathBuf},
    process::Command as Process,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

fn isolated(name: &str, diagnostic: Option<&str>) -> bool {
    if std::env::var("EARPH_WORKER_TEST_CHILD").as_deref() == Ok(name) {
        return false;
    }
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/worker-playback")
        .join(format!(
            "{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    fs::create_dir_all(&directory).unwrap();
    let config = directory.join("alsa.conf");
    let capture = directory.join("capture.pcm");
    fs::write(&config, format!("pcm.null {{ type null }}\npcm.capture {{ type file slave.pcm null file \"{}\" format raw }}\npcm.full {{ type file slave.pcm null file \"/dev/full\" format raw }}\n", capture.display())).unwrap();
    let output = Process::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env("EARPH_WORKER_TEST_CHILD", name)
        .env("EARPH_WORKER_TEST_ARTIFACTS", &directory)
        .env("ALSA_CONFIG_PATH", config)
        .output()
        .unwrap();
    fs::write(directory.join("test.stdout"), &output.stdout).unwrap();
    fs::write(directory.join("test.stderr"), &output.stderr).unwrap();
    assert!(
        output.status.success(),
        "{} / {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if let Some(diagnostic) = diagnostic {
        assert!(String::from_utf8_lossy(&output.stderr).contains(diagnostic));
    }
    true
}
fn artifacts() -> PathBuf {
    std::env::var_os("EARPH_WORKER_TEST_ARTIFACTS")
        .unwrap()
        .into()
}
fn wave(path: &Path, rate: u32, frames: usize, origin: usize) -> Vec<u8> {
    let mut payload = Vec::new();
    for scalar in origin * 2..(origin + frames) * 2 {
        let value = ((scalar as i64 * 17 + 19) % 16777216 - 8388608) as i32;
        payload.extend(&value.to_le_bytes()[..3]);
    }
    let mut bytes = Vec::new();
    bytes.extend(b"RIFF");
    bytes.extend((36 + payload.len() as u32).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16_u32.to_le_bytes());
    bytes.extend(1_u16.to_le_bytes());
    bytes.extend(2_u16.to_le_bytes());
    bytes.extend(rate.to_le_bytes());
    bytes.extend((rate * 6).to_le_bytes());
    bytes.extend(6_u16.to_le_bytes());
    bytes.extend(24_u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend((payload.len() as u32).to_le_bytes());
    bytes.extend(&payload);
    fs::write(path, bytes).unwrap();
    payload
}
struct Controller {
    runtime: PathBuf,
    next: Cell<u64>,
}
impl Controller {
    fn id(&self) -> RequestId {
        let n = self.next.get();
        self.next.set(n + 1);
        RequestId::new(format!("worker-{n}")).unwrap()
    }
    fn state(&self) -> Value {
        self.query(Command::State)["receipt"]["result"]["value"].clone()
    }
    fn query(&self, payload: Command) -> Value {
        let command = CommandEnvelope {
            schema_version: 1,
            request_id: self.id(),
            zone_id: ZoneId::new("main").unwrap(),
            owner_id: None,
            session_id: None,
            expected_revision: None,
            payload,
        };
        serde_json::from_slice(
            &request(&self.runtime, &serde_json::to_vec(&command).unwrap()).unwrap(),
        )
        .unwrap()
    }
    fn send(&self, payload: Command) -> Value {
        let state = self.state();
        let session_id = if matches!(payload, Command::Start { .. }) {
            Some(SessionId::new("native-session").unwrap())
        } else {
            serde_json::from_value(state["session_id"].clone()).unwrap()
        };
        let command = CommandEnvelope {
            schema_version: 1,
            request_id: self.id(),
            zone_id: ZoneId::new("main").unwrap(),
            owner_id: serde_json::from_value(state["owner_id"].clone()).unwrap(),
            session_id,
            expected_revision: state["revision"].as_u64(),
            payload,
        };
        serde_json::from_slice(
            &request(&self.runtime, &serde_json::to_vec(&command).unwrap()).unwrap(),
        )
        .unwrap()
    }
    fn apply(&self, payload: Command) {
        let reply = self.send(payload);
        assert_eq!(reply["receipt"]["status"], "applied", "{reply}");
    }
    fn wait(&self, condition: impl Fn(&Value) -> bool) -> Value {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let state = self.state();
            if condition(&state) {
                return state;
            }
            assert!(Instant::now() < deadline, "{state}");
            thread::sleep(Duration::from_millis(2));
        }
    }
    fn queue(&self) -> Value {
        self.query(Command::QueueGet {
            offset: 0,
            limit: 64,
        })["receipt"]["result"]["value"]
            .clone()
    }
    fn append(&self, path: &Path) {
        self.apply(Command::QueueAppend {
            path: path.to_str().unwrap().into(),
        });
    }
    fn start(&self) {
        let reply = self.send(Command::Start {
            source: SourceKind::LocalFile,
            device_id: DeviceId::new("native").unwrap(),
            takeover: false,
            seekable: true,
        });
        assert_eq!(reply["receipt"]["status"], "accepted", "{reply}");
    }
}
fn host(
    endpoint: &str,
) -> (
    Controller,
    thread::JoinHandle<Result<(), earph_core::EngineError>>,
) {
    let runtime = support::runtime();
    let owner = EngineOwner::bind(&runtime).unwrap();
    let join = thread::spawn(move || owner.serve());
    let controller = Controller {
        runtime,
        next: Cell::new(0),
    };
    controller.apply(serde_json::from_value(json!({"method":"output.select","output":{"backend":"alsa","route":"system","endpoint":endpoint,"device_id":"native","profile_id":"native-profile","period_frames":64,"buffer_frames":256,"volume_mode":"fixed","fidelity":"strict_preservation"}})).unwrap());
    controller.apply(Command::VolumeSet {
        millidecibels: 0,
        ramp_frames: 0,
    });
    controller.apply(Command::OutputConsent {
        device_id: DeviceId::new("native").unwrap(),
        profile_id: ProfileId::new("native-profile").unwrap(),
    });
    controller.apply(Command::FixedArm {
        device_id: DeviceId::new("native").unwrap(),
        profile_id: ProfileId::new("native-profile").unwrap(),
    });
    (controller, join)
}

#[test]
fn exact_software_zero_is_distinct_from_finite_minus_120_db_in_real_signed24_capture() {
    if isolated(
        "exact_software_zero_is_distinct_from_finite_minus_120_db_in_real_signed24_capture",
        None,
    ) {
        return;
    }
    let directory = artifacts();
    let source = directory.join("nonzero-signed24.wav");
    wave(&source, 48000, 4096, 0);
    for zero in [false, true] {
        let (c, join) = host("capture");
        c.apply(serde_json::from_value(json!({"method":"output.select","output":{"backend":"alsa","route":"system","endpoint":"capture","device_id":"native","profile_id":"native-profile","period_frames":64,"buffer_frames":256,"volume_mode":"software","fidelity":"compatible_conversion"}})).unwrap());
        c.apply(Command::OutputConsent {
            device_id: DeviceId::new("native").unwrap(),
            profile_id: ProfileId::new("native-profile").unwrap(),
        });
        c.apply(Command::VolumeSet {
            millidecibels: -120000,
            ramp_frames: 0,
        });
        if zero {
            c.apply(Command::VolumeSilence);
        }
        c.append(&source);
        c.start();
        c.wait(|state| state["transport"] == "buffering");
        c.apply(Command::Mute { muted: false });
        let state = c.wait(|state| state["transport"] == "idle" || state["transport"] == "failed");
        assert!(state["error"].is_null(), "{state}");
        assert_eq!(state["software_gain_zero"], zero);
        assert_eq!(state["software_gain_db"], -120.0);
        assert_eq!(state["playback"]["source_position_frames"], 4096);
        let captured = fs::read(directory.join("capture.pcm")).unwrap();
        assert_eq!(captured.len(), 4096 * 6);
        assert_eq!(captured.iter().all(|sample| *sample == 0), zero);
        fs::write(
            directory.join(if zero {
                "exact-zero.pcm"
            } else {
                "finite-minus120.pcm"
            }),
            captured,
        )
        .unwrap();
        c.apply(Command::OwnerShutdown);
        join.join().unwrap().unwrap();
    }
}

#[test]
fn factory_defaults_reaches_the_live_worker_before_finite_playback_resumes() {
    if isolated(
        "factory_defaults_reaches_the_live_worker_before_finite_playback_resumes",
        None,
    ) {
        return;
    }
    let directory = artifacts();
    let source = directory.join("factory-source.wav");
    let original = wave(&source, 48000, 300000, 0);
    let (c, join) = host("capture");
    c.apply(serde_json::from_value(json!({"method":"output.select","output":{"backend":"alsa","route":"system","endpoint":"capture","device_id":"native","profile_id":"native-profile","period_frames":64,"buffer_frames":256,"volume_mode":"software","fidelity":"compatible_conversion"}})).unwrap());
    c.apply(Command::OutputConsent {
        device_id: DeviceId::new("native").unwrap(),
        profile_id: ProfileId::new("native-profile").unwrap(),
    });
    c.append(&source);
    c.start();
    c.wait(|state| state["transport"] == "buffering");
    c.apply(Command::Mute { muted: false });
    c.wait(|state| {
        state["transport"] == "playing"
            && state["playback"]["submitted_frames"].as_u64().unwrap_or(0) > 0
    });
    let before = c.state();
    c.apply(Command::VolumeFactoryDefaults);
    let muted = c.wait(|state| state["transport"] == "buffering" && state["muted"] == true);
    assert_eq!(muted["software_gain_db"], -40.0);
    assert_eq!(muted["software_gain_zero"], false);
    assert_eq!(muted["source"], before["source"]);
    assert_eq!(muted["session_id"], before["session_id"]);
    assert_eq!(muted["output_consented"], before["output_consented"]);
    let capture = directory.join("capture.pcm");
    let prefix = fs::read(&capture).unwrap();
    assert!(!prefix.is_empty() && prefix.len() < original.len());
    assert_eq!(prefix, original[..prefix.len()]);
    c.apply(Command::Mute { muted: false });
    let done = c.wait(|state| state["transport"] == "idle" || state["transport"] == "failed");
    assert!(done["error"].is_null(), "{done}");
    let captured = fs::read(capture).unwrap();
    assert_eq!(captured.len(), original.len());
    assert_eq!(captured[..prefix.len()], prefix);
    for (actual, raw) in captured[prefix.len()..]
        .as_chunks::<3>()
        .0
        .iter()
        .zip(original[prefix.len()..].as_chunks::<3>().0)
    {
        let mut bytes = [0_u8; 4];
        bytes[..3].copy_from_slice(raw);
        bytes[3] = if raw[2] & 0x80 == 0 { 0 } else { 255 };
        let attenuated = (f64::from(i32::from_le_bytes(bytes)) * 0.01).round() as i32;
        assert_eq!(actual, &attenuated.to_le_bytes()[..3]);
    }
    c.apply(Command::OwnerShutdown);
    join.join().unwrap().unwrap();
}

#[test]
fn incompatible_future_is_visible_before_any_samples_are_written() {
    if isolated(
        "incompatible_future_is_visible_before_any_samples_are_written",
        None,
    ) {
        return;
    }
    let directory = artifacts();
    let first = directory.join("first.wav");
    let other = directory.join("other.wav");
    wave(&first, 48000, 500, 0);
    wave(&other, 44100, 500, 500);
    let (c, join) = host("capture");
    c.append(&first);
    c.append(&other);
    c.start();
    let state = c.wait(|state| state["transport"] == "failed");
    assert_eq!(state["error"], "FORMAT_UNSUPPORTED");
    assert_eq!(state["muted"], true);
    let queue = c.queue();
    assert_eq!(queue["entries"][1]["status"]["code"], "FORMAT_UNSUPPORTED");
    assert!(fs::read(directory.join("capture.pcm")).is_err());
    let id = serde_json::from_value(queue["entries"][1]["id"].clone()).unwrap();
    c.apply(Command::QueueRemove { id });
    c.apply(Command::OwnerShutdown);
    join.join().unwrap().unwrap();
}

#[test]
fn reserved_next_is_protected_and_farther_reorder_changes_the_actual_capture() {
    if isolated(
        "reserved_next_is_protected_and_farther_reorder_changes_the_actual_capture",
        None,
    ) {
        return;
    }
    let directory = artifacts();
    let (c, join) = host("capture");
    let mut payloads = Vec::new();
    for i in 0..4 {
        let path = directory.join(format!("track-{i}.wav"));
        payloads.push(wave(&path, 48000, 127 + i, i * 1000));
        c.append(&path);
    }
    c.start();
    c.wait(|state| state["transport"] == "buffering");
    let queue = c.queue();
    let next: QueueEntryId = serde_json::from_value(queue["entries"][1]["id"].clone()).unwrap();
    assert_eq!(
        c.state()["reserved_next_entry_id"],
        queue["entries"][1]["id"]
    );
    let reply = c.send(Command::QueueRemove { id: next });
    assert_eq!(reply["receipt"]["error"]["code"], "SOURCE_BUSY");
    let farther = serde_json::from_value(queue["entries"][3]["id"].clone()).unwrap();
    c.apply(Command::QueueMove {
        id: farther,
        new_index: 2,
    });
    c.apply(Command::Mute { muted: false });
    let state = c.wait(|state| state["transport"] == "idle" || state["transport"] == "failed");
    assert!(state["error"].is_null(), "{state}");
    assert_eq!(
        state["playback"]["backend"]["client_format"]["representation"],
        "signed24"
    );
    assert_eq!(
        state["playback"]["backend"]["client_format"]["sample_rate_hz"].as_f64(),
        Some(48000.0)
    );
    assert!(state["playback"]["backend"]["device_format"].is_null());
    assert_eq!(state["playback"]["backend"]["preservation"], "unverified");
    let expected = [
        payloads[0].as_slice(),
        payloads[1].as_slice(),
        payloads[3].as_slice(),
        payloads[2].as_slice(),
    ]
    .concat();
    assert_eq!(fs::read(directory.join("capture.pcm")).unwrap(), expected);
    let first = serde_json::from_value(queue["entries"][0]["id"].clone()).unwrap();
    c.apply(Command::QueueRemove { id: first });
    c.apply(Command::OwnerShutdown);
    join.join().unwrap().unwrap();
}

#[test]
fn native_write_failure_stops_the_worker_and_revokes_output_consent() {
    if isolated(
        "native_write_failure_stops_the_worker_and_revokes_output_consent",
        Some("write failed"),
    ) {
        return;
    }
    let directory = artifacts();
    let source = directory.join("source.wav");
    wave(&source, 48000, 8199, 0);
    let (c, join) = host("full");
    c.append(&source);
    c.start();
    c.wait(|state| state["transport"] == "buffering");
    c.apply(Command::Mute { muted: false });
    let state = c.wait(|state| state["transport"] == "failed");
    assert_eq!(state["error"], "OUTPUT_FAULT");
    assert!(state["playback"]["consumed_frames"].is_null(), "{state}");
    assert!(
        state["playback"]["backend_delay_frames"].is_null(),
        "{state}"
    );
    assert_eq!(state["output_consented"], false);
    assert_eq!(state["muted"], true);
    let reply = c.send(Command::Mute { muted: false });
    assert_eq!(reply["receipt"]["error"]["code"], "SAFETY_POLICY_BLOCKED");
    c.apply(Command::OwnerShutdown);
    join.join().unwrap().unwrap();
}

#[test]
fn pause_of_running_output_resumes_from_its_real_consumed_cursor() {
    if isolated(
        "pause_of_running_output_resumes_from_its_real_consumed_cursor",
        None,
    ) {
        return;
    }
    let directory = artifacts();
    let source = directory.join("source.wav");
    let original = wave(&source, 48000, 300000, 0);
    let (c, join) = host("capture");
    c.append(&source);
    c.start();
    c.wait(|state| state["transport"] == "buffering");
    c.apply(Command::Mute { muted: false });
    c.wait(|state| {
        state["transport"] == "playing"
            && state["playback"]["submitted_frames"].as_u64().unwrap_or(0) > 0
    });
    let pause = c.send(Command::Pause);
    assert_eq!(pause["receipt"]["status"], "accepted", "{pause}");
    let paused = c.wait(|state| state["transport"] == "paused" && state["operation"].is_null());
    let cursor = paused["playback"]["audible_source_position_frames"]
        .as_u64()
        .unwrap();
    assert!(cursor > 0 && cursor < 300000, "{paused}");
    assert_eq!(paused["playback"]["consumed_frames"], 0);
    assert_eq!(
        paused["playback"]["consumption_basis"],
        "alsa_client_estimate"
    );
    thread::sleep(Duration::from_millis(10));
    assert_eq!(
        c.state()["playback"]["audible_source_position_frames"],
        cursor
    );
    let resumed = c.send(Command::Resume);
    assert_eq!(resumed["receipt"]["status"], "accepted");
    let state = c.wait(|state| state["transport"] == "idle" || state["transport"] == "failed");
    assert!(state["error"].is_null(), "{state}");
    assert_eq!(fs::read(directory.join("capture.pcm")).unwrap(), original);
    c.apply(Command::OwnerShutdown);
    join.join().unwrap().unwrap();
}

#[test]
fn seeking_to_eof_finishes_without_emitting_a_replacement_frame() {
    if isolated(
        "seeking_to_eof_finishes_without_emitting_a_replacement_frame",
        None,
    ) {
        return;
    }
    let directory = artifacts();
    let source = directory.join("source.wav");
    wave(&source, 48000, 64, 0);
    let (c, join) = host("capture");
    c.append(&source);
    c.start();
    c.wait(|state| state["transport"] == "buffering");
    assert_eq!(c.send(Command::Pause)["receipt"]["status"], "accepted");
    c.wait(|state| state["transport"] == "paused" && state["operation"].is_null());
    assert_eq!(
        c.send(Command::Seek {
            position_frames: 64
        })["receipt"]["status"],
        "accepted"
    );
    c.wait(|state| state["transport"] == "paused" && state["operation"].is_null());
    c.apply(Command::Mute { muted: false });
    assert_eq!(c.send(Command::Resume)["receipt"]["status"], "accepted");
    let state = c.wait(|state| state["transport"] == "idle" || state["transport"] == "failed");
    assert!(state["error"].is_null(), "{state}");
    assert_eq!(state["playback"]["submitted_frames"], 0);
    assert!(fs::read(directory.join("capture.pcm")).unwrap().is_empty());
    c.apply(Command::OwnerShutdown);
    join.join().unwrap().unwrap();
}

#[test]
fn stopping_running_output_waits_for_native_close_and_preserves_the_written_prefix() {
    if isolated(
        "stopping_running_output_waits_for_native_close_and_preserves_the_written_prefix",
        None,
    ) {
        return;
    }
    let directory = artifacts();
    let source = directory.join("source.wav");
    let original = wave(&source, 48000, 300000, 0);
    let (c, join) = host("capture");
    c.append(&source);
    c.start();
    c.wait(|state| state["transport"] == "buffering");
    c.apply(Command::Mute { muted: false });
    c.wait(|state| state["transport"] == "playing");
    let stop = c.send(Command::Stop);
    assert_eq!(stop["receipt"]["status"], "accepted", "{stop}");
    let stopped = c.wait(|state| state["transport"] == "idle" && state["operation"].is_null());
    let frames = stopped["playback"]["submitted_frames"].as_u64().unwrap() as usize;
    let capture = directory.join("capture.pcm");
    let bytes = fs::read(&capture).unwrap();
    assert_eq!(bytes.len(), frames * 6);
    assert_eq!(bytes, original[..frames * 6]);
    assert!(frames > 0 && frames < 300000);
    thread::sleep(Duration::from_millis(10));
    assert_eq!(fs::read(capture).unwrap(), bytes);
    let first = serde_json::from_value(c.queue()["entries"][0]["id"].clone()).unwrap();
    c.apply(Command::QueueRemove { id: first });
    c.apply(Command::OwnerShutdown);
    join.join().unwrap().unwrap();
}
