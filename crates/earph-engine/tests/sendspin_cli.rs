// ABOUTME: Runs the real `earph sendspin` command against the test Sendspin server and a private ALSA capture.
// ABOUTME: Checks the exact capture, status, refusals, a wrong key, interruption, and control from a second process.
#[cfg(not(target_os = "linux"))]
fn main() {}
#[cfg(target_os = "linux")]
fn main() {
    linux::main()
}
#[cfg(target_os = "linux")]
#[path = "support/sendspin_credential_helper.rs"]
mod helper;
#[cfg(target_os = "linux")]
#[path = "support/sendspin_cli_pairing.rs"]
mod pairing;
#[cfg(target_os = "linux")]
#[allow(dead_code, reason = "this test uses a subset of the shared peer")]
#[path = "support/sendspin_player_peer.rs"]
mod peer;

/// The synthetic helper serves this document: the fixture identity, paired with the test peer.
#[cfg(target_os = "linux")]
fn document() -> earph_sendspin::credentials::CommittedCredentials {
    use earph_sendspin::pairing::{PairingRecord, RecordCatalog};
    let server = earph_sendspin::noise::static_public_key(&peer::SERVER_PRIVATE).unwrap();
    let record = PairingRecord {
        server_public: server,
        psk: peer::RECORD_PSK,
        last_used_order: 1,
    };
    let catalog = RecordCatalog::from_committed(5, vec![record], vec![]).unwrap();
    earph_sendspin::credentials::CommittedCredentials::new(
        peer::CLIENT_PRIVATE,
        peer::PAIRING_PSK,
        catalog,
        Some(server),
    )
    .unwrap()
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{helper, pairing, peer};
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
    use serde_json::{Value, json};
    use std::{
        fs,
        net::{IpAddr, TcpListener, UdpSocket},
        os::unix::fs::DirBuilderExt,
        path::PathBuf,
        process::{Child, Command, Output, Stdio},
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    const CHUNK_FRAMES: usize = 1200;
    const PREAMBLE_FRAMES: usize = 4800;
    const PATTERN_FRAMES: usize = 43200;
    const DEADLINE: Duration = Duration::from_secs(20);

    pub fn main() {
        let chosen = std::env::args().nth(1);
        if chosen.as_deref() == Some("credential-helper") {
            return helper::run();
        }
        let cases: [(&str, fn()); 9] = [
            (
                "plays",
                plays_the_stream_shows_status_and_exits_when_the_server_closes,
            ),
            ("refuses", refuses_bad_arguments_before_it_starts_anything),
            ("wrong_key", a_wrong_server_key_fails_without_audio),
            ("interrupt", an_interrupt_stops_the_stream_and_the_engine),
            ("pause", pause_stops_the_stream_and_ends_the_command),
            ("stop", stop_from_another_terminal_ends_the_command),
            ("quit", quit_from_another_terminal_ends_the_command),
            (
                "init",
                pairing::init_creates_one_identity_and_refuses_every_other_store_state,
            ),
            (
                "token",
                pairing::token_prints_the_stored_pairing_token_to_standard_output_only,
            ),
        ];
        // An argument runs one case: `cargo test --test sendspin_cli -- wrong_key`.
        for (name, case) in cases {
            if chosen.as_deref().is_none_or(|chosen| chosen == name) {
                println!("case {name}");
                case();
            }
        }
        println!("Sendspin command line checks passed");
    }
    pub(super) fn text(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).into_owned()
    }
    /// Every frame differs from every other frame, and the first byte is nonzero.
    pub(super) fn pattern() -> Vec<u8> {
        (0..PATTERN_FRAMES as u16)
            .flat_map(|frame| {
                let left = (frame + 1).to_le_bytes();
                let right = (!frame).to_le_bytes();
                [left[0], left[1], right[0], right[1]]
            })
            .collect()
    }
    /// Ends a spawned command if its test fails before the command exits.
    pub(super) struct Player(pub(super) Option<Child>);
    impl Drop for Player {
        fn drop(&mut self) {
            if let Some(mut child) = self.0.take() {
                let _ = child.kill();
                // A failed test shows what its command printed.
                if let Ok(output) = child.wait_with_output() {
                    println!("{}{}", text(&output.stdout), text(&output.stderr));
                }
            }
        }
    }
    impl Player {
        pub(super) fn exited(&mut self) -> bool {
            self.0.as_mut().unwrap().try_wait().unwrap().is_some()
        }
        /// Waits for the command with a deadline, so a hung command fails the test.
        pub(super) fn finish(self) -> Output {
            let output = self.finish_unprinted();
            println!("{}{}", text(&output.stdout), text(&output.stderr));
            output
        }
        /// Waits as `finish` does. Keeps output that holds a secret out of the test log.
        pub(super) fn finish_unprinted(mut self) -> Output {
            let deadline = Instant::now() + DEADLINE;
            while !self.exited() {
                assert!(Instant::now() < deadline, "the command did not exit");
                std::thread::sleep(Duration::from_millis(10));
            }
            self.0.take().unwrap().wait_with_output().unwrap()
        }
    }

    /// One private ALSA capture device, one credential fixture, and the listener of the test server.
    pub(super) struct Bench {
        pub(super) directory: PathBuf,
        pub(super) listener: TcpListener,
        pub(super) key: String,
    }
    impl Bench {
        pub(super) fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            // The Engine needs a short absolute runtime path below a private directory.
            let directory = PathBuf::from("/tmp").join(format!(
                "ep-{:x}-{:x}-{:x}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let private = |path: &PathBuf| fs::DirBuilder::new().mode(0o700).create(path).unwrap();
            private(&directory);
            private(&directory.join("store"));
            fs::write(
                directory.join("alsa.conf"),
                format!(
                    "pcm.null {{ type null }}\npcm.earph_capture {{ type file slave.pcm null file \"{}\" format raw }}\n",
                    directory.join("capture.pcm").display()
                ),
            )
            .unwrap();
            // The transport refuses loopback pins, so the peer listens on a private interface.
            let socket = UdpSocket::bind("0.0.0.0:0").unwrap();
            socket.connect("192.0.2.1:9").unwrap();
            let interface = socket.local_addr().unwrap().ip();
            assert!(matches!(interface, IpAddr::V4(ip) if ip.is_private()));
            println!("Sendspin command artifacts: {}", directory.display());
            let public = earph_sendspin::noise::static_public_key(&peer::SERVER_PRIVATE).unwrap();
            Self {
                directory,
                listener: TcpListener::bind((interface, 0)).unwrap(),
                key: URL_SAFE_NO_PAD.encode(public),
            }
        }
        pub(super) fn url(&self) -> String {
            let address = self.listener.local_addr().unwrap();
            format!("ws://{}:{}/sendspin", address.ip(), address.port())
        }
        pub(super) fn earph(&self, arguments: &[&str]) -> Player {
            let child = Command::new(env!("CARGO_BIN_EXE_earph"))
                .args(arguments)
                .env("EARPH_RUNTIME", self.directory.join("rt"))
                .env("ALSA_CONFIG_PATH", self.directory.join("alsa.conf"))
                .env_remove("EARPH_OUTPUT")
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            Player(Some(child))
        }
        /// Runs `earph sendspin` with the fixture credential store, the capture output, and 0 dB.
        pub(super) fn sendspin(&self, target: &[&str]) -> Player {
            self.sendspin_with("owner", target)
        }
        /// Runs `earph sendspin` against one named case of the synthetic credential helper.
        pub(super) fn sendspin_with(&self, case: &str, target: &[&str]) -> Player {
            let helper = std::env::current_exe().unwrap();
            let store = self.directory.join("store");
            let address = format!("fixture:{case}:{}", store.display());
            let lock = store.join(format!("{case}.lock"));
            let fixed = [
                "--output",
                "earph_capture",
                "--volume",
                "0",
                "--period",
                "480",
                "--buffer",
                "1920",
                "--credential-helper",
                helper.to_str().unwrap(),
                "--credential-store",
                &address,
                "--credential-lock",
                lock.to_str().unwrap(),
            ];
            self.earph(&[&["sendspin"], target, &fixed].concat())
        }
        /// The bytes of the document that the synthetic helper holds for one case.
        pub(super) fn stored_bytes(&self, case: &str) -> Option<Vec<u8>> {
            let path = self
                .directory
                .join("store")
                .join(format!("{case}-document"));
            let bytes = fs::read(path).ok()?;
            Some(serde_json::from_slice(&bytes).unwrap())
        }
        pub(super) fn stored(
            &self,
            case: &str,
        ) -> Option<earph_sendspin::credentials::CommittedCredentials> {
            let bytes = self.stored_bytes(case)?;
            // A failure here must not print the document.
            earph_sendspin::credentials::CommittedCredentials::decode_secret(&bytes).ok()
        }
        /// True when the helper received a write for this case.
        pub(super) fn write_issued(&self, case: &str) -> bool {
            self.directory
                .join("store")
                .join(format!("{case}-commit"))
                .exists()
        }
        pub(super) fn state(&self) -> Value {
            // The Engine answers a repeated request ID with its first receipt.
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let id = format!("test-state-{}", NEXT.fetch_add(1, Ordering::Relaxed));
            let bytes = json!({"schema_version":1,"request_id":id,"zone_id":"main",
                "owner_id":null,"session_id":null,"expected_revision":null,"payload":{"method":"state.get"}})
            .to_string();
            let reply = earph_engine::request(&self.directory.join("rt"), bytes.as_bytes());
            let reply: Value = serde_json::from_slice(&reply.expect("the Engine answers")).unwrap();
            reply["receipt"]["result"]["value"].clone()
        }
        /// Serves the peer's clock replies until the Engine reports the wanted fact.
        pub(super) fn serve(
            &self,
            peer: &mut peer::PlayerPeer,
            what: &str,
            done: impl Fn(&Value) -> bool,
        ) {
            let mut checked = Instant::now();
            peer.serve_until(what, || {
                // An IPC round trip per poll would starve the peer's clock replies.
                if checked.elapsed() < Duration::from_millis(20) {
                    return false;
                }
                checked = Instant::now();
                done(&self.state())
            });
        }
        /// Runs a second `earph` to its end while the peer keeps answering the first one.
        pub(super) fn meanwhile(&self, peer: &mut peer::PlayerPeer, mut second: Player) -> Output {
            peer.serve_until("a second command exits", || second.exited());
            second.finish()
        }
        /// Starts the command, connects the peer, and streams the pattern once the command unmuted.
        fn streaming(&self) -> (Player, peer::PlayerPeer, u64) {
            let mut player = self.sendspin(&["--server", &self.url(), "--server-key", &self.key]);
            self.listener.set_nonblocking(true).unwrap();
            let deadline = Instant::now() + DEADLINE;
            // The peer's accept blocks, so a command that exits early must fail here first.
            while self.listener.peek_pending().is_none() {
                assert!(!player.exited(), "the command exited before it connected");
                assert!(Instant::now() < deadline, "the command never connected");
                std::thread::sleep(Duration::from_millis(5));
            }
            self.listener.set_nonblocking(false).unwrap();
            let mut peer = peer::PlayerPeer::accept(&self.listener);
            peer.activate_player();
            self.serve(&mut peer, "the command unmutes", |state| {
                state["muted"] == false
            });
            peer.start_stream();
            let sent = [vec![0; PREAMBLE_FRAMES * 4], pattern()].concat();
            let first = peer.server_now_us() + 200_000;
            for (index, chunk) in sent.chunks(CHUNK_FRAMES * 4).enumerate() {
                peer.send_chunk(first + index as i64 * 25_000, chunk);
                // The transport ends a connection that holds more than 32 undrained events.
                let paced = Instant::now() + Duration::from_millis(5);
                peer.serve_until("chunk pacing", || Instant::now() >= paced);
            }
            let frames = (sent.len() / 4) as u64;
            self.serve(&mut peer, "the output accepts every sent frame", |state| {
                state["sendspin_playback"]["submitted_frames"].as_u64() >= Some(frames)
            });
            (player, peer, frames)
        }
        pub(super) fn no_engine_remains(&self) {
            let status = self.earph(&["status"]).finish();
            assert_eq!(status.status.code(), Some(3));
            assert!(text(&status.stderr).contains("no Engine is running"));
        }
    }
    impl Drop for Bench {
        fn drop(&mut self) {
            // Keep artifacts for a failed test. A passing test frees its capture.
            if !std::thread::panicking() {
                let _ = fs::remove_dir_all(&self.directory);
            }
        }
    }
    pub(super) trait Pending {
        fn peek_pending(&self) -> Option<()>;
    }
    impl Pending for TcpListener {
        /// Reports a waiting connection without accepting it.
        fn peek_pending(&self) -> Option<()> {
            let mut pending = [rustix::event::PollFd::new(
                self,
                rustix::event::PollFlags::IN,
            )];
            let none = rustix::event::Timespec::try_from(Duration::ZERO).unwrap();
            (rustix::event::poll(&mut pending, Some(&none)).unwrap() == 1).then_some(())
        }
    }

    fn plays_the_stream_shows_status_and_exits_when_the_server_closes() {
        let bench = Bench::new();
        let (player, mut peer, frames) = bench.streaming();
        let status = bench.meanwhile(&mut peer, bench.earph(&["status", "--json"]));
        let state: Value = serde_json::from_slice(&status.stdout).unwrap();
        assert_eq!(state["source"], "sendspin");
        assert_eq!(state["transport"], "playing");
        assert_eq!(state["audible_position_frames"], Value::Null);
        let playback = &state["sendspin_playback"];
        assert_eq!(playback["timing"]["qualification"], "unqualified");
        assert!(playback["submitted_frames"].as_u64() >= Some(frames));
        assert_eq!(
            state["playback"]["audible_source_position_frames"],
            Value::Null
        );
        let plain = text(&bench.meanwhile(&mut peer, bench.earph(&["status"])).stdout);
        assert!(
            plain.contains("playing") && plain.contains("sync unqualified"),
            "{plain}"
        );
        // The server comes only from launch configuration, so a second command cannot attach.
        let target = ["--server", &bench.url(), "--server-key", &bench.key];
        let second = bench.meanwhile(&mut peer, bench.sendspin(&target));
        assert_eq!(second.status.code(), Some(2));
        let refusal = text(&second.stderr);
        assert!(refusal.contains("already runs"), "{refusal}");
        assert!(!refusal.contains(&bench.key));
        peer.end_stream();
        bench.serve(&mut peer, "the stream end returns to buffering", |state| {
            state["transport"] == "buffering"
        });
        peer.close();
        let output = player.finish();
        assert_eq!(output.status.code(), Some(0));
        let stdout = text(&output.stdout);
        assert!(stdout.contains("output   earph_capture"), "{stdout}");
        assert!(stdout.contains("sync unqualified"), "{stdout}");
        assert!(!stdout.contains("audible"), "{stdout}");
        let last = stdout.lines().last().unwrap();
        assert!(last.starts_with("result   ended"), "{last}");
        // Nothing else writes this output, so the capture holds the anchor silence and the sent bytes.
        let capture = fs::read(bench.directory.join("capture.pcm")).unwrap();
        let pattern = pattern();
        let audible = capture.iter().position(|byte| *byte != 0).unwrap();
        let pad = audible - PREAMBLE_FRAMES * 4;
        println!("anchor silence: {} frames", pad / 4);
        assert!(
            pad.is_multiple_of(4) && pad < 4800 * 4,
            "anchor silence: {pad} bytes"
        );
        assert!(
            capture[audible..] == pattern,
            "captured {} bytes after {audible} zeros; sent {}",
            capture.len() - audible,
            pattern.len()
        );
        bench.no_engine_remains();
    }
    fn refuses_bad_arguments_before_it_starts_anything() {
        let bench = Bench::new();
        let url = bench.url();
        let cases: [(&[&str], &str); 5] = [
            (&["--server-key", &bench.key], "--server is needed"),
            (
                &[
                    "--server",
                    "wss://10.1.2.3:8927/sendspin",
                    "--server-key",
                    &bench.key,
                ],
                "wss",
            ),
            (
                &[
                    "--server",
                    "ws://127.0.0.1:8927/sendspin",
                    "--server-key",
                    &bench.key,
                ],
                "loopback",
            ),
            (
                &["--server", &url, "--server-key", "not-a-server-key!"],
                "--server-key",
            ),
            (&["--server", &url], "--server-key"),
        ];
        for (target, problem) in cases {
            let output = bench.sendspin(target).finish();
            let refusal = text(&output.stderr);
            assert_eq!(output.status.code(), Some(2), "{target:?}: {refusal}");
            assert!(refusal.contains(problem), "{target:?}: {refusal}");
            assert!(!refusal.contains(&bench.key) && !refusal.contains("not-a-server-key!"));
            assert!(!bench.directory.join("rt").exists(), "{target:?} started");
        }
    }
    fn a_wrong_server_key_fails_without_audio() {
        let bench = Bench::new();
        let wrong = URL_SAFE_NO_PAD.encode([22; 32]);
        let output = bench
            .sendspin(&["--server", &bench.url(), "--server-key", &wrong])
            .finish();
        let failure = text(&output.stderr);
        assert_eq!(output.status.code(), Some(1), "{failure}");
        assert!(failure.contains("denied") && failure.contains("--server-key"));
        assert!(!failure.contains(&wrong), "{failure}");
        assert!(
            bench.listener.peek_pending().is_none(),
            "a denied start opens no connection"
        );
        let capture = fs::read(bench.directory.join("capture.pcm")).unwrap_or_default();
        assert!(capture.is_empty(), "a denied start writes no audio");
        bench.no_engine_remains();
    }
    fn an_interrupt_stops_the_stream_and_the_engine() {
        let bench = Bench::new();
        let (player, _peer, _) = bench.streaming();
        rustix::process::kill_process(
            rustix::process::Pid::from_child(player.0.as_ref().unwrap()),
            rustix::process::Signal::INT,
        )
        .unwrap();
        let output = player.finish();
        assert_eq!(output.status.code(), Some(130));
        assert!(text(&output.stdout).contains("result   interrupted"));
        bench.no_engine_remains();
    }
    fn pause_stops_the_stream_and_ends_the_command() {
        let bench = Bench::new();
        let (player, mut peer, _) = bench.streaming();
        let mute = bench.meanwhile(&mut peer, bench.earph(&["mute"]));
        assert_eq!(mute.status.code(), Some(0), "{}", text(&mute.stderr));
        let status = text(&bench.meanwhile(&mut peer, bench.earph(&["status"])).stdout);
        assert!(status.contains("muted"), "{status}");
        for arguments in [&["resume"][..], &["seek", "10"]] {
            let refused = bench.meanwhile(&mut peer, bench.earph(arguments));
            let refusal = text(&refused.stderr);
            assert_eq!(refused.status.code(), Some(1), "{arguments:?}: {refusal}");
            let message = format!("{} is not supported for a Sendspin stream", arguments[0]);
            assert!(refusal.contains(&message), "{arguments:?}: {refusal}");
        }
        assert_eq!(bench.state()["transport"], "playing");
        // The pause cancels the connection, so the peer has nothing left to answer.
        let pause = bench.earph(&["pause"]).finish();
        assert_eq!(pause.status.code(), Some(0), "{}", text(&pause.stderr));
        assert!(text(&pause.stdout).contains("stopped (a Sendspin stream cannot pause)"));
        let output = player.finish();
        assert_eq!(output.status.code(), Some(0));
        assert!(text(&output.stdout).contains("result   ended"));
        bench.no_engine_remains();
    }
    fn stop_from_another_terminal_ends_the_command() {
        let bench = Bench::new();
        let (player, _peer, _) = bench.streaming();
        let stop = bench.earph(&["stop"]).finish();
        assert_eq!(stop.status.code(), Some(0), "{}", text(&stop.stderr));
        assert!(text(&stop.stdout).contains("stopped"));
        let output = player.finish();
        assert_eq!(output.status.code(), Some(0));
        assert!(text(&output.stdout).contains("result   ended"));
        bench.no_engine_remains();
    }
    fn quit_from_another_terminal_ends_the_command() {
        let bench = Bench::new();
        let (player, _peer, _) = bench.streaming();
        let quit = bench.earph(&["quit"]).finish();
        assert_eq!(quit.status.code(), Some(0), "{}", text(&quit.stderr));
        let output = player.finish();
        assert_eq!(output.status.code(), Some(0), "{}", text(&output.stderr));
        assert!(text(&output.stdout).contains("result   ended"));
        bench.no_engine_remains();
    }
}
