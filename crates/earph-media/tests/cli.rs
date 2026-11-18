// ABOUTME: Verifies executable rendering through real lossless files.
// ABOUTME: Compares exact captures and checks failed renders remain failures.
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

fn directory() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/media-cli-tests")
        .join(format!(
            "{}-{:?}-{}",
            std::process::id(),
            std::thread::current().id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
    fs::create_dir_all(&dir).unwrap();
    dir
}
fn fixture(dir: &Path) -> PathBuf {
    let wav = dir.join("reference.wav");
    let mut data: Vec<u8> = Vec::new();
    for i in 0..8199 * 2 {
        data.extend(&((i as i64 * 7919 % 16777216 - 8388608) as i32).to_le_bytes()[..3]);
    }
    let mut bytes = Vec::new();
    bytes.extend(b"RIFF");
    bytes.extend((36 + data.len() as u32).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16_u32.to_le_bytes());
    bytes.extend(1_u16.to_le_bytes());
    bytes.extend(2_u16.to_le_bytes());
    bytes.extend(48000_u32.to_le_bytes());
    bytes.extend(288000_u32.to_le_bytes());
    bytes.extend(6_u16.to_le_bytes());
    bytes.extend(24_u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend((data.len() as u32).to_le_bytes());
    bytes.extend(data);
    fs::write(&wav, bytes).unwrap();
    wav
}
fn render(output: &Path, files: &[&Path]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_earph-media"))
        .args(["render", "--output"])
        .arg(output)
        .args(files)
        .output()
        .unwrap()
}

#[test]
fn executable_render_and_split_lossless_join_match_source_samples() {
    let dir = directory();
    let wav = fixture(&dir);
    let original = fs::read(&wav).unwrap();
    let mut parts = Vec::new();
    for (name, filter) in [
        ("first.flac", "atrim=end_sample=4097"),
        ("second.flac", "atrim=start_sample=4097"),
    ] {
        let part = dir.join(name);
        let result = Command::new("ffmpeg")
            .args(["-nostdin", "-v", "error", "-i"])
            .arg(&wav)
            .args(["-af", filter, "-c:a", "flac"])
            .arg(&part)
            .output()
            .expect("FFmpeg fixture producer");
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        parts.push(part);
    }
    let output = dir.join("capture");
    let result = render(&output, &[&parts[0], &parts[1]]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
    let report: Value = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report["status"], "pass");
    assert_eq!(report["frames"], 8199);
    assert_eq!(
        fs::read(output.join("capture.pcm")).unwrap(),
        original[44..]
    );
    assert_eq!(fs::read(&wav).unwrap(), original);
    let second = render(&output, &[&wav]);
    assert!(
        !second.status.success(),
        "existing artifacts must never be overwritten"
    );
}

#[test]
fn failed_render_preserves_partial_capture_and_error_report() {
    let dir = directory();
    let wav = fixture(&dir);
    let output = dir.join("capture");
    let result = render(&output, &[&wav, &dir.join("missing.wav")]);
    assert!(!result.status.success());
    let report: Value =
        serde_json::from_slice(&fs::read(output.join("report.json")).unwrap()).unwrap();
    assert_eq!(report["status"], "fail");
    assert_eq!(report["files_completed"], 1);
    assert_eq!(report["error"]["code"], "ASSET_INVALID");
    assert_eq!(
        fs::read(output.join("capture.pcm")).unwrap(),
        fs::read(wav).unwrap()[44..]
    );
}

#[cfg(unix)]
#[test]
fn fifo_input_is_rejected_without_waiting_for_a_writer() {
    let dir = directory();
    let fifo = dir.join("input.fifo");
    assert!(
        Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_earph-media"))
        .arg("inspect")
        .arg(fifo)
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(!status.success());
            break;
        }
        if std::time::Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("FIFO admission blocked waiting for a writer");
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_partial_write_failure_reports_the_actual_capture_hash() {
    use sha2::{Digest, Sha256};
    let dir = directory();
    let wav = fixture(&dir);
    let output = dir.join("limited-capture");
    let result = Command::new("bash")
        .args([
            "-c",
            "trap '' XFSZ\nulimit -f 8\nexec \"$@\"",
            "earph-media-test",
            env!("CARGO_BIN_EXE_earph-media"),
            "render",
            "--output",
        ])
        .arg(&output)
        .arg(wav)
        .output()
        .unwrap();
    assert!(!result.status.success());
    let report: Value =
        serde_json::from_slice(&fs::read(output.join("report.json")).unwrap()).unwrap();
    let capture = fs::read(output.join("capture.pcm")).unwrap();
    assert!(!capture.is_empty());
    let hash: String = Sha256::digest(&capture)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    assert_eq!(report["capture_sha256"], hash);
    assert_eq!(report["frames"], capture.len() / 6);
}
