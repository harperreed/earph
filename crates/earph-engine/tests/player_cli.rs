// ABOUTME: Runs the real `earph` player binary against a private ALSA capture device.
// ABOUTME: Checks finished, refused, failed, stopped, and interrupted playback by exit status and output.
#![cfg(target_os = "linux")]
use std::{
    fs,
    io::Read,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct Bench {
    directory: PathBuf,
    capture: PathBuf,
}
impl Bench {
    /// `paced` routes audio into a FIFO that drains at playback speed, so playback takes real time.
    fn new(paced: bool) -> (Self, Option<Pacer>) {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = PathBuf::from("/tmp").join(format!(
            "ep-{:x}-{:x}-{:x}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        // The Engine refuses a runtime below a group-writable directory.
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&directory)
            .unwrap();
        let capture = directory.join("capture.pcm");
        let pacer = paced.then(|| Pacer::start(&capture));
        fs::write(
            directory.join("alsa.conf"),
            format!(
                "pcm.null {{ type null }}\npcm.earph_capture {{ type file slave.pcm null file \"{}\" format raw }}\n",
                capture.display()
            ),
        )
        .unwrap();
        println!("Player bench artifacts: {}", directory.display());
        (Self { directory, capture }, pacer)
    }
    /// Makes ALSA list exactly these outputs. Each one writes the capture file.
    fn list_outputs(&self, names: &[&str]) {
        let mut config = "pcm.null { type null }\n".to_owned();
        for name in names {
            config.push_str(&format!(
                "pcm.{name} {{ type file slave.pcm null file \"{}\" format raw hint {{ show on description \"Desk {name}\" }} }}\n",
                self.capture.display()
            ));
        }
        fs::write(self.directory.join("alsa.conf"), config).unwrap();
    }
    fn tone(&self, name: &str, frames: u32) -> PathBuf {
        self.tone_at(name, frames, 1000)
    }
    fn tone_at(&self, name: &str, frames: u32, level: i16) -> PathBuf {
        let mut wav = [
            b"RIFF".as_slice(),
            &(36 + frames * 4).to_le_bytes(),
            b"WAVEfmt ",
            &16u32.to_le_bytes(),
            &1u16.to_le_bytes(),
            &2u16.to_le_bytes(),
            &48000u32.to_le_bytes(),
            &192000u32.to_le_bytes(),
            &4u16.to_le_bytes(),
            &16u16.to_le_bytes(),
            b"data",
            &(frames * 4).to_le_bytes(),
        ]
        .concat();
        for _ in 0..frames {
            wav.extend_from_slice(&level.to_le_bytes());
            wav.extend_from_slice(&(-level).to_le_bytes());
        }
        let path = self.directory.join(name);
        fs::write(&path, wav).unwrap();
        path
    }
    fn earph(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_earph"));
        command
            .env("EARPH_RUNTIME", self.directory.join("rt"))
            .env("ALSA_CONFIG_PATH", self.directory.join("alsa.conf"))
            .env_remove("EARPH_OUTPUT");
        command
    }
    fn run(&self, arguments: &[&str]) -> Output {
        self.earph().args(arguments).output().unwrap()
    }
    fn succeed(&self, command: &str) {
        let output = self.run(&[command]);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{command}: {} {}",
            text(&output.stdout),
            text(&output.stderr)
        );
    }
    fn spawn(&self, arguments: &[&str]) -> Player {
        Player(Some(
            self.earph()
                .args(arguments)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap(),
        ))
    }
    /// Waits until another `earph status` reports the wanted transport.
    fn until_status(&self, wanted: &str) {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let status = self.run(&["status"]);
            if text(&status.stdout).contains(wanted) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "status never showed {wanted}: {} {}",
                text(&status.stdout),
                text(&status.stderr)
            );
            thread::sleep(Duration::from_millis(50));
        }
    }
}
/// Reads a FIFO at 48 kHz stereo 16-bit speed.
struct Pacer {
    stop: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}
impl Pacer {
    fn start(path: &Path) -> Self {
        rustix::fs::mkfifoat(
            rustix::fs::CWD,
            path,
            rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
        )
        .unwrap();
        let mut fifo = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(i32::try_from(rustix::fs::OFlags::NONBLOCK.bits()).unwrap())
            .open(path)
            .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let join = thread::spawn(move || {
            let mut buffer = [0; 1920];
            while !stopping.load(Ordering::Acquire) {
                let _ = fifo.read(&mut buffer);
                thread::sleep(Duration::from_millis(10));
            }
        });
        Self {
            stop,
            join: Some(join),
        }
    }
}
impl Drop for Pacer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        self.join.take().unwrap().join().unwrap();
    }
}
fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}
/// Ends a spawned player if its test fails before the player exits.
struct Player(Option<Child>);
impl Drop for Player {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
impl Drop for Bench {
    fn drop(&mut self) {
        // Keep artifacts for a failed test. A passing test frees its large sources.
        if !thread::panicking() {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }
}
fn finish(mut player: Player) -> Output {
    let output = player.0.take().unwrap().wait_with_output().unwrap();
    println!("{}{}", text(&output.stdout), text(&output.stderr));
    output
}

#[test]
fn play_sends_every_frame_in_order_then_stops_its_engine() {
    let (bench, _) = Bench::new(false);
    let first = bench.tone("first.wav", 24000);
    let second = bench.tone_at("second.wav", 12000, 2000);
    let output = bench.run(&[
        "play",
        first.to_str().unwrap(),
        second.to_str().unwrap(),
        "--output",
        "earph_capture",
    ]);
    assert_eq!(
        output.status.code(),
        Some(0),
        "{} {}",
        text(&output.stdout),
        text(&output.stderr)
    );
    let stdout = text(&output.stdout);
    assert!(stdout.contains("result   finished"), "{stdout}");
    let pcm = fs::read(&bench.capture).unwrap();
    assert_eq!(pcm.len(), (24000 + 12000) * 4);
    // The second file is twice as loud, so the capture shows the order.
    let peak = |bytes: &[u8]| {
        bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|sample| i16::from_le_bytes(*sample).unsigned_abs())
            .max()
            .unwrap()
    };
    let (first_half, second_half) = pcm.split_at(24000 * 4);
    assert!(peak(first_half) > 0 && peak(second_half) > peak(first_half));
    let status = bench.run(&["status"]);
    assert_eq!(status.status.code(), Some(3));
    assert!(text(&status.stderr).contains("no Engine is running"));
    // The command removed its own entries, so the next run needs no flag.
    let again = bench.run(&["play", first.to_str().unwrap(), "-o", "earph_capture"]);
    assert_eq!(again.status.code(), Some(0), "{}", text(&again.stderr));
}

#[test]
fn play_without_a_named_output_refuses_and_sends_no_audio() {
    let (bench, _) = Bench::new(false);
    let tone = bench.tone("tone.wav", 4800);
    let output = bench.run(&["play", tone.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(2));
    assert!(text(&output.stderr).contains("--output"));
    assert!(!bench.capture.exists());
}

#[test]
fn play_without_a_named_output_uses_the_only_listed_output() {
    let (bench, _) = Bench::new(false);
    let tone = bench.tone("tone.wav", 4800);
    bench.list_outputs(&["only_output"]);
    let output = bench.run(&["play", tone.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    let printed = text(&output.stdout);
    // The line names the output as the device list does, not by its identifier alone.
    assert!(
        printed.contains("output   Desk only_output  [only_output]"),
        "{printed}"
    );
    assert!(
        printed.contains("hint     this volume is quiet"),
        "{printed}"
    );
    assert!(printed.contains("result   finished"), "{printed}");
    assert!(fs::metadata(&bench.capture).unwrap().len() > 0);
}

#[test]
fn play_without_a_named_output_refuses_to_pick_between_several() {
    let (bench, _) = Bench::new(false);
    let tone = bench.tone("tone.wav", 4800);
    bench.list_outputs(&["first_output", "second_output"]);
    let output = bench.run(&["play", tone.to_str().unwrap()]);
    assert_eq!(output.status.code(), Some(2));
    let refusal = text(&output.stderr);
    assert!(
        refusal.contains("first_output") && refusal.contains("second_output"),
        "{refusal}"
    );
    assert!(!bench.capture.exists());
}

#[test]
fn unreadable_and_invalid_files_fail_with_distinct_statuses() {
    let (bench, _) = Bench::new(false);
    let missing = bench.run(&["play", "/nonexistent/earph.wav", "-o", "earph_capture"]);
    assert_eq!(missing.status.code(), Some(2));
    let garbage = bench.directory.join("garbage.wav");
    fs::write(&garbage, b"this is not audio").unwrap();
    let invalid = bench.run(&["play", garbage.to_str().unwrap(), "-o", "earph_capture"]);
    assert_eq!(invalid.status.code(), Some(1), "{}", text(&invalid.stderr));
    assert!(text(&invalid.stderr).contains("cannot play"));
    assert!(!bench.capture.exists());
    // A rejected file leaves nothing behind that blocks the next play.
    let tone = bench.tone("tone.wav", 4800);
    let next = bench.run(&["play", tone.to_str().unwrap(), "-o", "earph_capture"]);
    assert_eq!(next.status.code(), Some(0), "{}", text(&next.stderr));
    let empty = bench.run(&["play", tone.to_str().unwrap(), "--output", ""]);
    assert_eq!(empty.status.code(), Some(2));
}

#[test]
fn another_terminal_can_watch_pause_and_stop_a_running_play() {
    let (bench, _pacer) = Bench::new(true);
    let tone = bench.tone("long.wav", 48000 * 60);
    let play = bench.spawn(&[
        "play",
        tone.to_str().unwrap(),
        "-o",
        "earph_capture",
        "-v",
        "-50",
    ]);
    bench.until_status("playing");
    let status = text(&bench.run(&["status"]).stdout);
    assert!(
        status.contains("long.wav") && status.contains("-50.0 dB"),
        "{status}"
    );
    bench.succeed("pause");
    bench.until_status("paused");
    bench.succeed("resume");
    bench.until_status("playing");
    bench.succeed("stop");
    let output = finish(play);
    assert_eq!(output.status.code(), Some(0));
    assert!(text(&output.stdout).contains("result   stopped"));
}

#[test]
fn interrupting_play_stops_audio_and_the_engine() {
    let (bench, _pacer) = Bench::new(true);
    let tone = bench.tone("long.wav", 48000 * 60);
    let play = bench.spawn(&["play", tone.to_str().unwrap(), "-o", "earph_capture"]);
    bench.until_status("playing");
    rustix::process::kill_process(
        rustix::process::Pid::from_child(play.0.as_ref().unwrap()),
        rustix::process::Signal::INT,
    )
    .unwrap();
    let output = finish(play);
    assert_eq!(output.status.code(), Some(130));
    assert!(text(&output.stdout).contains("result   interrupted"));
    assert_eq!(bench.run(&["status"]).status.code(), Some(3));
}

#[test]
fn devices_lists_outputs_without_a_running_engine() {
    let (bench, _) = Bench::new(false);
    let output = bench.run(&["devices"]);
    assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
    assert_eq!(bench.run(&["status"]).status.code(), Some(3));
}

#[test]
fn a_second_play_never_disturbs_running_playback_without_the_replace_flag() {
    let (bench, _pacer) = Bench::new(true);
    let long = bench.tone("long.wav", 48000 * 60);
    let short = bench.tone_at("short.wav", 4800, 2000);
    let first = bench.spawn(&["play", long.to_str().unwrap(), "-o", "earph_capture"]);
    bench.until_status("playing");
    let refused = bench.run(&["play", short.to_str().unwrap(), "-o", "earph_capture"]);
    assert_eq!(refused.status.code(), Some(2));
    assert!(text(&refused.stderr).contains("--replace-queue"));
    let status = text(&bench.run(&["status"]).stdout);
    assert!(
        status.contains("playing") && status.contains("long.wav"),
        "{status}"
    );
    bench.succeed("stop");
    let output = finish(first);
    assert_eq!(output.status.code(), Some(0));
    assert!(text(&output.stdout).contains("result   stopped"));
    // The first play stopped its own Engine, so the same command now plays.
    let next = bench.run(&["play", short.to_str().unwrap(), "-o", "earph_capture"]);
    assert_eq!(next.status.code(), Some(0), "{}", text(&next.stderr));
}

#[test]
fn transport_commands_without_playback_say_so() {
    let (bench, _pacer) = Bench::new(true);
    let long = bench.tone("long.wav", 48000 * 60);
    let play = bench.spawn(&["play", long.to_str().unwrap(), "-o", "earph_capture"]);
    bench.until_status("playing");
    bench.succeed("stop");
    // The hosting play may still be shutting its Engine down; either answer is a refusal.
    let pause = bench.run(&["pause"]);
    assert!(
        matches!(pause.status.code(), Some(1 | 3)),
        "{:?}",
        pause.status
    );
    finish(play);
}

#[test]
fn play_uses_a_running_engine_and_replaces_its_playback_only_on_request() {
    let (bench, _pacer) = Bench::new(true);
    let long = bench.tone("long.wav", 48000 * 60);
    let short = bench.tone_at("short.wav", 4800, 2000);
    let engine = Player(Some(
        Command::new(env!("CARGO_BIN_EXE_earph-engine"))
            .args(["owner", "--runtime"])
            .arg(bench.directory.join("rt"))
            .env("ALSA_CONFIG_PATH", bench.directory.join("alsa.conf"))
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    ));
    let deadline = Instant::now() + Duration::from_secs(10);
    while bench.run(&["status"]).status.code() != Some(0) {
        assert!(Instant::now() < deadline, "the Engine never answered");
        thread::sleep(Duration::from_millis(20));
    }
    let first = bench.spawn(&["play", long.to_str().unwrap(), "-o", "earph_capture"]);
    bench.until_status("playing");
    let replaced = bench.run(&[
        "play",
        short.to_str().unwrap(),
        "-o",
        "earph_capture",
        "--replace-queue",
    ]);
    assert_eq!(
        replaced.status.code(),
        Some(0),
        "{} {}",
        text(&replaced.stdout),
        text(&replaced.stderr)
    );
    assert!(text(&replaced.stdout).contains("result   finished"));
    // The first play sees its session end, or sees the new session, depending on timing.
    let output = finish(first);
    let stopped =
        output.status.code() == Some(0) && text(&output.stdout).contains("result   stopped");
    let replaced_notice = output.status.code() == Some(1)
        && text(&output.stderr).contains("another controller replaced");
    assert!(stopped || replaced_notice, "{:?}", output.status);
    // Neither play started this Engine, so it keeps running until Quit.
    assert_eq!(bench.run(&["status"]).status.code(), Some(0));
    assert_eq!(bench.run(&["pause"]).status.code(), Some(1));
    bench.succeed("quit");
    assert!(finish(engine).status.success());
}

#[test]
fn seek_moves_running_playback_and_rejects_bad_positions() {
    let (bench, _pacer) = Bench::new(true);
    assert_eq!(bench.run(&["seek", "soon"]).status.code(), Some(2));
    let tone = bench.tone("long.wav", 48000 * 120);
    let play = bench.spawn(&["play", tone.to_str().unwrap(), "-o", "earph_capture"]);
    bench.until_status("playing");
    let seek = bench.run(&["seek", "1:30"]);
    assert_eq!(seek.status.code(), Some(0), "{}", text(&seek.stderr));
    assert!(text(&seek.stdout).contains("1:30"));
    bench.until_status("playing   1:3");
    let past = bench.run(&["seek", "500"]);
    assert_eq!(past.status.code(), Some(1), "{}", text(&past.stderr));
    bench.until_status("playing   1:3");
    bench.succeed("stop");
    assert_eq!(finish(play).status.code(), Some(0));
    assert_eq!(bench.run(&["seek", "10"]).status.code(), Some(3));
}

#[test]
fn play_warns_when_the_volume_is_too_low_to_hear_on_speakers() {
    let (bench, _) = Bench::new(false);
    let tone = bench.tone("tone.wav", 4800);
    let quiet = bench.run(&["play", tone.to_str().unwrap(), "-o", "earph_capture"]);
    assert_eq!(quiet.status.code(), Some(0), "{}", text(&quiet.stderr));
    assert!(
        text(&quiet.stdout).contains("--volume"),
        "{}",
        text(&quiet.stdout)
    );
    let loud = bench.run(&[
        "play",
        tone.to_str().unwrap(),
        "-o",
        "earph_capture",
        "-v",
        "-20",
    ]);
    assert_eq!(loud.status.code(), Some(0), "{}", text(&loud.stderr));
    assert!(
        !text(&loud.stdout).contains("--volume"),
        "{}",
        text(&loud.stdout)
    );
}
