// ABOUTME: Runs a real owner with a Sendspin launch configuration against the real test peer.
// ABOUTME: Drives it over IPC and checks the ALSA capture, stop and reuse, denial, takeover, and shutdown.
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
#[allow(dead_code, reason = "this test uses a subset of the shared peer")]
#[path = "support/sendspin_player_peer.rs"]
mod peer;

/// The synthetic helper serves this document: the fixture identity, paired with the test peer.
#[cfg(target_os = "linux")]
fn document() -> earph_sendspin::credentials::CommittedCredentials {
    use earph_sendspin::pairing::{PairingRecord, RecordCatalog};
    let server = linux::server_public();
    let record = PairingRecord {
        server_public: server,
        psk: peer::RECORD_PSK,
        last_used_order: 1,
    };
    earph_sendspin::credentials::CommittedCredentials::new(
        peer::CLIENT_PRIVATE,
        peer::PAIRING_PSK,
        RecordCatalog::from_committed(5, vec![record], vec![]).unwrap(),
        Some(server),
    )
    .unwrap()
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{helper, peer};
    use earph_engine::{
        EngineOwner, SendspinConfiguration, sendspin_credentials::StoreConfiguration,
    };
    use serde_json::{Value, json};
    use std::{
        cell::Cell,
        fs,
        io::{BufRead, BufReader, Read},
        net::{IpAddr, TcpListener, UdpSocket},
        os::unix::fs::DirBuilderExt,
        path::PathBuf,
        process::{Child, Command, Stdio},
        sync::{
            Arc,
            atomic::{AtomicBool, AtomicU64, Ordering},
        },
        thread::JoinHandle,
        time::{Duration, Instant, SystemTime, UNIX_EPOCH},
    };

    const CHUNK_FRAMES: usize = 1200;
    const PREAMBLE_FRAMES: usize = 4800;
    const PATTERN_FRAMES: usize = 43200;
    const DEADLINE: Duration = Duration::from_secs(15);
    /// Half of the runtime's retirement fallback, so a fallback exit fails the test.
    const EXIT_DEADLINE: Duration = Duration::from_secs(5);
    /// The scheduler waits until a chunk is at most 5 ms early, then fills that lead with silence.
    const ANCHOR_PAD_FRAMES: u64 = 240;

    pub fn server_public() -> [u8; 32] {
        earph_sendspin::noise::static_public_key(&peer::SERVER_PRIVATE).unwrap()
    }
    pub fn main() {
        let arguments: Vec<String> = std::env::args().skip(1).collect();
        match arguments.first().map(String::as_str) {
            Some("credential-helper") => helper::run(),
            Some("owner") => owner(&arguments[1..]),
            // A name runs the cases that contain it. No argument runs every case.
            filter => {
                let cases: [(&str, fn()); 9] = [
                    (
                        "plays_the_stream_and_reports_unqualified_timing",
                        plays_the_stream_and_reports_unqualified_timing,
                    ),
                    (
                        "stop_retires_the_connection_and_a_second_start_plays",
                        stop_retires_the_connection_and_a_second_start_plays,
                    ),
                    (
                        "pause_stops_the_stream_and_leaves_the_owner_reusable",
                        pause_stops_the_stream_and_leaves_the_owner_reusable,
                    ),
                    (
                        "a_wrong_server_key_is_denied_without_audio",
                        a_wrong_server_key_is_denied_without_audio,
                    ),
                    (
                        "shutdown_during_a_stream_exits_cleanly",
                        shutdown_during_a_stream_exits_cleanly,
                    ),
                    (
                        "a_local_start_takes_the_output_and_the_retired_stream_stays_silent",
                        a_local_start_takes_the_output_and_the_retired_stream_stays_silent,
                    ),
                    (
                        "a_sendspin_start_takes_the_output_from_local_playback",
                        a_sendspin_start_takes_the_output_from_local_playback,
                    ),
                    (
                        "a_takeover_during_a_reconnect_ends_the_retries",
                        a_takeover_during_a_reconnect_ends_the_retries,
                    ),
                    (
                        "a_stop_or_a_shutdown_during_a_handover_ends_cleanly",
                        a_stop_or_a_shutdown_during_a_handover_ends_cleanly,
                    ),
                ];
                for (name, case) in cases {
                    if filter.is_none_or(|filter| name.contains(filter)) {
                        case();
                    }
                }
                println!("Sendspin owner process checks passed");
            }
        }
    }
    /// Child role: hosts one real owner. Every connection input comes from these arguments.
    fn owner(arguments: &[String]) {
        let [runtime, url, pin, key, store] = arguments else {
            panic!("owner role needs runtime, url, pin, key, and store");
        };
        let key: Vec<u8> = (0..key.len())
            .step_by(2)
            .map(|index| u8::from_str_radix(&key[index..index + 2], 16).unwrap())
            .collect();
        let owner = EngineOwner::bind(runtime.as_ref())
            .unwrap()
            .with_sendspin(SendspinConfiguration {
                server_url: url.clone(),
                pinned_address: pin.parse().unwrap(),
                server_public_key: key.try_into().unwrap(),
                credentials: StoreConfiguration {
                    helper: std::env::current_exe().unwrap(),
                    native: earph_credentials::NativeConfig::SecretService {
                        address: format!("fixture:owner:{store}"),
                    },
                    lock_path: PathBuf::from(store).join("owner.lock"),
                    operation_timeout: Duration::from_secs(2),
                },
            })
            .unwrap();
        println!("{}", json!({"status":"owner"}));
        owner.serve().unwrap();
    }
    /// The transport refuses loopback pins, so the peer listens on a private interface.
    fn interface() -> IpAddr {
        let socket = UdpSocket::bind("0.0.0.0:0").unwrap();
        socket.connect("192.0.2.1:9").unwrap();
        let address = socket.local_addr().unwrap().ip();
        assert!(matches!(address, IpAddr::V4(ip) if ip.is_private()));
        address
    }
    /// Every frame differs from every other frame, and the first byte is nonzero.
    fn pattern() -> Vec<u8> {
        (0..PATTERN_FRAMES as u16)
            .flat_map(|frame| {
                let left = (frame + 1).to_le_bytes();
                let right = (!frame).to_le_bytes();
                [left[0], left[1], right[0], right[1]]
            })
            .collect()
    }
    const LOCAL_FRAMES: usize = 96000;
    /// The local file's frames: both channels carry the same value, far from zero.
    fn local_pattern() -> Vec<u8> {
        (0..LOCAL_FRAMES)
            .flat_map(|frame| {
                let value = (1000 + frame % 20000) as u16;
                [value.to_le_bytes(), value.to_le_bytes()].concat()
            })
            .collect()
    }
    /// The chunks of a stream that a takeover retires. No frame equals a `pattern` frame.
    fn retired_pattern(frames: u16) -> Vec<u8> {
        (0..frames)
            .flat_map(|frame| {
                let left = (frame + 1).to_le_bytes();
                let right = (!frame - 1).to_le_bytes();
                [left[0], left[1], right[0], right[1]]
            })
            .collect()
    }
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Frame {
        Silence,
        Stream,
        Retired,
        Local,
    }
    /// Names the source of each captured frame. The three patterns differ in their channel sum.
    fn classify(capture: &[u8]) -> Vec<Frame> {
        assert!(capture.len().is_multiple_of(4), "{} bytes", capture.len());
        capture
            .chunks(4)
            .map(|frame| {
                let left = u16::from_le_bytes([frame[0], frame[1]]);
                let right = u16::from_le_bytes([frame[2], frame[3]]);
                match left.wrapping_add(right) {
                    0 if left == 0 => Frame::Silence,
                    0 => Frame::Stream,
                    u16::MAX => Frame::Retired,
                    _ if left == right && (1000..21000).contains(&left) => Frame::Local,
                    _ => panic!("captured a frame of no source: {left} {right}"),
                }
            })
            .collect()
    }
    /// Returns the captured bytes of one source, in order.
    fn frames_of(capture: &[u8], kinds: &[Frame], wanted: Frame) -> Vec<u8> {
        capture
            .chunks(4)
            .zip(kinds)
            .filter(|(_, kind)| **kind == wanted)
            .flat_map(|(frame, _)| frame.to_vec())
            .collect()
    }

    /// Holds the capture as a FIFO. It keeps every byte that any source wrote, in order,
    /// where the file device would keep only the bytes since the last open.
    /// A closed gate fills the pipe and then blocks the playback worker in its write.
    struct Gate {
        fifo: fs::File,
        open: Arc<AtomicBool>,
        finished: Arc<AtomicBool>,
        drain: Option<JoinHandle<Vec<u8>>>,
    }
    impl Gate {
        fn new(path: &std::path::Path, open: bool) -> Self {
            use std::os::unix::fs::OpenOptionsExt;
            rustix::fs::mkfifoat(
                rustix::fs::CWD,
                path,
                rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
            )
            .unwrap();
            // This end stays open, so a writer never blocks in its open and never sees a closed pipe.
            let fifo = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(i32::try_from(rustix::fs::OFlags::NONBLOCK.bits()).unwrap())
                .open(path)
                .unwrap();
            let mut reader = fifo.try_clone().unwrap();
            let open = Arc::new(AtomicBool::new(open));
            let finished = Arc::new(AtomicBool::new(false));
            let (opened, stopping) = (open.clone(), finished.clone());
            let drain = std::thread::spawn(move || {
                let mut captured = Vec::new();
                let mut buffer = [0; 8192];
                loop {
                    let stopping = stopping.load(Ordering::Acquire);
                    if opened.load(Ordering::Acquire) {
                        match reader.read(&mut buffer) {
                            Ok(count) if count > 0 => {
                                captured.extend_from_slice(&buffer[..count]);
                                continue;
                            }
                            Ok(_) => {}
                            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                            Err(error) => panic!("capture FIFO read failed: {error}"),
                        }
                    }
                    if stopping {
                        return captured;
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            });
            Self {
                fifo,
                open,
                finished,
                drain: Some(drain),
            }
        }
        fn held_bytes(&self) -> u64 {
            rustix::io::ioctl_fionread(&self.fifo).unwrap()
        }
        fn open(&self) {
            self.open.store(true, Ordering::Release);
        }
        /// Returns everything that reached the output. Call it after the owner exits.
        fn finish(mut self) -> Vec<u8> {
            self.open();
            self.finished.store(true, Ordering::Release);
            self.drain.take().unwrap().join().unwrap()
        }
    }
    impl Drop for Gate {
        fn drop(&mut self) {
            self.finished.store(true, Ordering::Release);
            if let Some(drain) = self.drain.take() {
                let _ = drain.join();
            }
        }
    }

    /// One owner process, its private ALSA capture device, and the listener its server uses.
    struct Bench {
        directory: PathBuf,
        listener: TcpListener,
        owner: Child,
        exited: bool,
        next: Cell<u64>,
        gate: Option<Gate>,
    }
    impl Bench {
        fn launch(server_key: [u8; 32]) -> Self {
            Self::launch_with(server_key, None)
        }
        /// Launches an owner whose capture is a FIFO, and queues the local file.
        fn gated(open: bool) -> Self {
            let bench = Self::launch_with(server_public(), Some(open));
            let samples = local_pattern();
            let mut wave = Vec::new();
            wave.extend(b"RIFF");
            wave.extend((36 + samples.len() as u32).to_le_bytes());
            wave.extend(b"WAVEfmt ");
            wave.extend(16_u32.to_le_bytes());
            wave.extend(1_u16.to_le_bytes());
            wave.extend(2_u16.to_le_bytes());
            wave.extend(48000_u32.to_le_bytes());
            wave.extend(192000_u32.to_le_bytes());
            wave.extend(4_u16.to_le_bytes());
            wave.extend(16_u16.to_le_bytes());
            wave.extend(b"data");
            wave.extend((samples.len() as u32).to_le_bytes());
            wave.extend(&samples);
            let path = bench.directory.join("local.wav");
            fs::write(&path, wave).unwrap();
            bench.apply(json!({"method":"queue.append","path":path}));
            bench
        }
        fn launch_with(server_key: [u8; 32], gate: Option<bool>) -> Self {
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
            let gate = gate.map(|open| Gate::new(&directory.join("capture.pcm"), open));
            let listener = TcpListener::bind((interface(), 0)).unwrap();
            let address = listener.local_addr().unwrap();
            let mut owner = Command::new(std::env::current_exe().unwrap())
                .arg("owner")
                .arg(directory.join("rt"))
                .arg(format!("ws://{}:{}/sendspin", address.ip(), address.port()))
                .arg(address.ip().to_string())
                .arg(server_key.map(|byte| format!("{byte:02x}")).concat())
                .arg(directory.join("store"))
                .env("ALSA_CONFIG_PATH", directory.join("alsa.conf"))
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(fs::File::create(directory.join("owner.log")).unwrap())
                .spawn()
                .unwrap();
            let mut line = String::new();
            BufReader::new(owner.stdout.take().unwrap())
                .read_line(&mut line)
                .unwrap();
            assert_eq!(line.trim(), json!({"status":"owner"}).to_string());
            let bench = Self {
                directory,
                listener,
                owner,
                exited: false,
                next: Cell::new(0),
                gate,
            };
            println!("Sendspin owner artifacts: {}", bench.directory.display());
            bench.select_output();
            bench
        }
        /// Sends one real IPC envelope. The owner may not accept connections yet, so this retries.
        fn send(&self, document: &Value) -> Value {
            let bytes = serde_json::to_vec(document).unwrap();
            let deadline = Instant::now() + DEADLINE;
            loop {
                match earph_engine::request(&self.directory.join("rt"), &bytes) {
                    Ok(reply) => return serde_json::from_slice(&reply).unwrap(),
                    Err(error) => assert!(Instant::now() < deadline, "IPC failed: {error}"),
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        fn query(&self, method: &str) -> Value {
            let next = self.next.replace(self.next.get() + 1);
            let reply = self.send(&json!({
                "schema_version":1,"request_id":format!("query-{next}"),"zone_id":"main",
                "owner_id":null,"session_id":null,"expected_revision":null,"payload":{"method":method}
            }));
            assert_eq!(reply["receipt"]["status"], "applied", "{reply}");
            reply["receipt"]["result"]["value"].clone()
        }
        fn state(&self) -> Value {
            self.query("state.get")
        }
        fn phase(&self) -> Value {
            self.query("sources.sendspin.get")["phase"].clone()
        }
        /// Sends one mutation against fresh state. A background revision can race it, so it retries.
        fn command(&self, payload: Value, session: Option<&str>) -> Value {
            let deadline = Instant::now() + DEADLINE;
            loop {
                let state = self.state();
                let next = self.next.replace(self.next.get() + 1);
                let reply = self.send(&json!({
                    "schema_version":1,"request_id":format!("command-{next}"),"zone_id":"main",
                    "owner_id":state["owner_id"],
                    "session_id":session.map_or_else(|| state["session_id"].clone(), Value::from),
                    "expected_revision":state["revision"],"payload":payload
                }));
                if reply["receipt"]["error"]["code"] != "STALE_REVISION" {
                    return reply["receipt"].clone();
                }
                assert!(Instant::now() < deadline, "revision never settled: {reply}");
            }
        }
        fn apply(&self, payload: Value) {
            let receipt = self.command(payload, None);
            assert_eq!(receipt["status"], "applied", "{receipt}");
        }
        fn select_output(&self) {
            self.apply(json!({"method":"output.select","output":{
                "backend":"alsa","route":"system","endpoint":"earph_capture","device_id":"capture",
                "profile_id":"profile","period_frames":480,"buffer_frames":1920,
                "volume_mode":"software","fidelity":"compatible_conversion"}}));
            self.apply(
                json!({"method":"output.consent","device_id":"capture","profile_id":"profile"}),
            );
            self.apply(json!({"method":"volume.set","millidecibels":0,"ramp_frames":0}));
        }
        fn start(&self, session: &str) -> Value {
            self.command(
                json!({"method":"sources.sendspin.start","device_id":"capture"}),
                Some(session),
            )
        }
        fn wait(&self, what: &str, done: impl Fn(&Self) -> bool) {
            let deadline = Instant::now() + DEADLINE;
            while !done(self) {
                assert!(
                    Instant::now() < deadline,
                    "deadline: {what}: {} {}",
                    self.state(),
                    self.query("sources.sendspin.get")
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        /// Serves the peer's clock replies until the owner reports the wanted fact.
        fn serve(&self, peer: &mut peer::PlayerPeer, what: &str, done: impl Fn(&Self) -> bool) {
            let mut checked = Instant::now();
            peer.serve_until(what, || {
                // An IPC round trip per poll would starve the peer's clock replies.
                if checked.elapsed() < Duration::from_millis(20) {
                    return false;
                }
                checked = Instant::now();
                done(self)
            });
        }
        fn start_local(&self, session: &str, takeover: bool) -> Value {
            self.command(
                json!({"method":"sessions.start","source":"local_file","device_id":"capture",
                    "takeover":takeover,"seekable":true}),
                Some(session),
            )
        }
        fn takeover(&self, session: &str) -> Value {
            self.command(
                json!({"method":"sources.sendspin.start","device_id":"capture","takeover":true}),
                Some(session),
            )
        }
        /// Unmutes the prepared local session, so its file plays.
        fn play_local(&self) {
            self.wait("the local file is prepared", |bench| {
                bench.state()["transport"] == "buffering"
            });
            self.apply(json!({"method":"mute.set","muted":false}));
        }
        /// Drops every connection that waits in the listener and reports whether one waited.
        fn drain_listener(&self) -> bool {
            self.listener.set_nonblocking(true).unwrap();
            let mut waited = false;
            while self.listener.accept().is_ok() {
                waited = true;
            }
            self.listener.set_nonblocking(false).unwrap();
            waited
        }
        /// Fails when the owner connects to its server again within 1.5 seconds.
        /// The first retry of a lost connection would come after 250 ms.
        fn assert_no_reconnect(&self) {
            self.drain_listener();
            std::thread::sleep(Duration::from_millis(1500));
            assert!(!self.drain_listener(), "the retired source connected again");
            assert_eq!(self.phase(), "disconnected");
        }
        /// Shuts the owner down and returns every byte that reached the output.
        fn capture(mut self) -> Vec<u8> {
            self.shutdown();
            self.gate.take().unwrap().finish()
        }
        /// Starts a session, connects the peer, and unmutes once the source is prepared.
        fn connect(&self, session: &str) -> peer::PlayerPeer {
            assert_eq!(self.start(session)["status"], "accepted");
            self.attach()
        }
        /// Connects the peer to an accepted session, and unmutes once the source is prepared.
        fn attach(&self) -> peer::PlayerPeer {
            let unmute = json!({"method":"mute.set","muted":false});
            // No peer has accepted yet, so the source cannot be prepared.
            let early = self.command(unmute.clone(), None);
            assert_eq!(early["status"], "rejected", "{early}");
            assert_eq!(early["error"]["code"], "SAFETY_POLICY_BLOCKED");
            // The peer's accept blocks, so a missing connection must fail here first.
            let mut pending = [rustix::event::PollFd::new(
                &self.listener,
                rustix::event::PollFlags::IN,
            )];
            let timeout = rustix::event::Timespec::try_from(DEADLINE).unwrap();
            assert_eq!(
                rustix::event::poll(&mut pending, Some(&timeout)).unwrap(),
                1,
                "the owner never connected to its server: {}",
                self.query("sources.sendspin.get")
            );
            let mut peer = peer::PlayerPeer::accept(&self.listener);
            peer.activate_player();
            self.serve(&mut peer, "the source awaits its first stream", |bench| {
                bench.phase() == "awaiting_stream"
            });
            assert_eq!(self.state()["transport"], "buffering");
            self.apply(unmute);
            peer
        }
        /// Sends a zero preamble and the pattern as 25 ms chunks, 200 ms ahead of the server clock.
        /// Returns the sent frame count and the anchor silence that the scheduler placed before it.
        fn stream(&self, peer: &mut peer::PlayerPeer) -> (u64, u64) {
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
            let count = |playback: &Value, key: &str| playback[key].as_u64().unwrap();
            // The output accepts every sent frame that was not dropped, plus the inserted and the idle silence.
            self.serve(peer, "the output accepts every scheduled frame", |bench| {
                let playback = bench.state()["sendspin_playback"].take();
                let timing = &playback["timing"];
                playback["submitted_frames"].is_u64()
                    && count(&playback, "submitted_frames") + count(timing, "dropped_frames")
                        == frames
                            + count(timing, "inserted_silence_frames")
                            + count(timing, "idle_silence_frames")
            });
            let playback = self.state()["sendspin_playback"].take();
            let timing = &playback["timing"];
            // An early stream loses nothing, so the output accepted exactly the silence and the sent frames.
            assert_eq!(timing["dropped_frames"], 0, "{playback}");
            let pad = count(timing, "inserted_silence_frames");
            assert!(pad <= ANCHOR_PAD_FRAMES, "anchor pad {pad}");
            // The output ran on idle silence while the stream waited for its anchor.
            let idle = count(timing, "idle_silence_frames");
            assert!(idle > 0, "{playback}");
            assert_eq!(
                playback["submitted_frames"],
                frames + pad + idle,
                "{playback}"
            );
            assert_eq!(timing["qualification"], "unqualified");
            assert_eq!(timing["correction"], "none");
            assert_eq!(timing["anchored"], true);
            assert!(timing["start_error_us"].is_i64(), "{playback}");
            (frames, pad)
        }
        /// Ends the session with `sessions.stop` or `sessions.pause` and waits for retirement.
        fn end(&self, method: &str) {
            self.apply(json!({"method":method}));
            self.wait("the connection retires", |bench| {
                bench.phase() == "disconnected"
            });
            let state = self.state();
            assert_eq!(state["transport"], "idle");
            assert_eq!(state["session_id"], Value::Null);
            assert_eq!(state["source"], Value::Null);
            assert_eq!(state["muted"], true);
        }
        /// Nothing else writes this output, so the capture holds silence and the sent bytes.
        /// Idle silence, the anchor pad, and the preamble precede the pattern. Idle silence may follow it.
        /// The owner has exited, so the playback worker has closed and flushed the file.
        /// The comparison is exact, so a dropped, repeated, moved, or split pattern frame fails it.
        fn assert_capture_is_exact(&self, pad: u64) {
            assert!(self.exited, "read the capture only after the owner exits");
            let capture = fs::read(self.directory.join("capture.pcm")).unwrap();
            let pattern = pattern();
            // The first and the last pattern byte are nonzero.
            let first = capture.iter().position(|byte| *byte != 0).unwrap();
            let last = capture.iter().rposition(|byte| *byte != 0).unwrap();
            assert!(
                first.is_multiple_of(4) && first >= (pad as usize + PREAMBLE_FRAMES) * 4,
                "{first} bytes of silence precede the pattern"
            );
            assert!(
                capture[first..=last] == pattern,
                "captured {} bytes of pattern; sent {}",
                last + 1 - first,
                pattern.len()
            );
        }
        /// Shuts the owner down and waits for its exit.
        /// A clean retirement ends far sooner than the runtime's ten second fallback.
        fn shutdown(&mut self) {
            let receipt = self.command(json!({"method":"owner.shutdown"}), None);
            assert_eq!(receipt["status"], "applied", "{receipt}");
            let asked = Instant::now();
            let status = loop {
                if let Some(status) = self.owner.try_wait().unwrap() {
                    break status;
                }
                assert!(
                    asked.elapsed() < EXIT_DEADLINE,
                    "the owner did not exit promptly: {}",
                    fs::read_to_string(self.directory.join("owner.log")).unwrap()
                );
                std::thread::sleep(Duration::from_millis(5));
            };
            assert!(status.success(), "{status:?}");
            self.exited = true;
        }
    }
    impl Drop for Bench {
        fn drop(&mut self) {
            // The owner is this bench's own child. A failed test keeps its artifacts.
            let _ = self.owner.kill();
            let _ = self.owner.wait();
            if !std::thread::panicking() {
                let _ = fs::remove_dir_all(&self.directory);
            }
        }
    }

    fn plays_the_stream_and_reports_unqualified_timing() {
        let mut bench = Bench::launch(server_public());
        let capabilities = bench.query("capabilities.get");
        assert!(
            capabilities["sources"]
                .as_array()
                .unwrap()
                .contains(&json!("sendspin"))
        );
        let mut peer = bench.connect("player-1");
        let (_, pad) = bench.stream(&mut peer);
        let state = bench.state();
        assert_eq!(state["source"], "sendspin");
        assert_eq!(state["session_id"], "player-1");
        assert_eq!(state["transport"], "playing");
        assert_eq!(state["seekable"], false);
        assert_eq!(state["audible_position_frames"], Value::Null);
        assert_eq!(state["sendspin_playback"]["phase"], "streaming");
        let resume = bench.command(json!({"method":"sessions.resume"}), None);
        assert_eq!(
            resume["error"]["code"], "CAPABILITY_UNSUPPORTED",
            "{resume}"
        );
        peer.end_stream();
        bench.serve(&mut peer, "the stream end returns to buffering", |bench| {
            bench.state()["transport"] == "buffering"
        });
        // A closed server would start a reconnect, and a new output open truncates the capture.
        bench.end("sessions.stop");
        assert_eq!(bench.state()["error"], Value::Null);
        drop(peer);
        bench.shutdown();
        bench.assert_capture_is_exact(pad);
    }
    fn stop_retires_the_connection_and_a_second_start_plays() {
        let mut bench = Bench::launch(server_public());
        let mut peer = bench.connect("player-1");
        bench.stream(&mut peer);
        bench.end("sessions.stop");
        // The owner cancelled the connection, so the peer's socket has ended.
        drop(peer);
        // The file device truncates on open, so this capture belongs to the second session only.
        let mut peer = bench.connect("player-2");
        assert_eq!(
            bench.query("sources.sendspin.get")["connection_generation"],
            2
        );
        let (_, pad) = bench.stream(&mut peer);
        bench.end("sessions.stop");
        drop(peer);
        bench.shutdown();
        bench.assert_capture_is_exact(pad);
    }
    fn pause_stops_the_stream_and_leaves_the_owner_reusable() {
        let mut bench = Bench::launch(server_public());
        let mut peer = bench.connect("player-1");
        let (frames, pad) = bench.stream(&mut peer);
        bench.end("sessions.pause");
        // The closed source keeps its cumulative facts.
        let playback = bench.state()["sendspin_playback"].take();
        assert_eq!(playback["phase"], "closed");
        let idle = playback["timing"]["idle_silence_frames"].as_u64().unwrap();
        assert_eq!(
            playback["submitted_frames"],
            frames + pad + idle,
            "{playback}"
        );
        assert_eq!(playback["timing"]["qualification"], "unqualified");
        drop(peer);
        let mut peer = bench.connect("player-2");
        let (_, pad) = bench.stream(&mut peer);
        bench.shutdown();
        bench.assert_capture_is_exact(pad);
    }
    fn a_wrong_server_key_is_denied_without_audio() {
        let mut bench = Bench::launch([22; 32]);
        assert_eq!(bench.start("player-1")["status"], "accepted");
        bench.wait("the denied start fails the session", |bench| {
            bench.state()["transport"] == "failed"
        });
        bench.wait("the denied connection retires", |bench| {
            bench.phase() == "disconnected"
        });
        let state = bench.state();
        assert_eq!(state["error"], "AUTH_DENIED");
        assert_eq!(state["session_id"], Value::Null);
        assert_eq!(state["muted"], true);
        bench.listener.set_nonblocking(true).unwrap();
        assert!(
            bench.listener.accept().is_err(),
            "a denied start opens no connection"
        );
        bench.shutdown();
        let capture = fs::read(bench.directory.join("capture.pcm")).unwrap_or_default();
        assert!(capture.is_empty(), "a denied start writes no audio");
    }
    fn shutdown_during_a_stream_exits_cleanly() {
        let mut bench = Bench::launch(server_public());
        let mut peer = bench.connect("player-1");
        bench.stream(&mut peer);
        assert_eq!(bench.state()["transport"], "playing");
        bench.shutdown();
    }
    /// Out of Sendspin: a local start with takeover retires the stream, and the server sees the end.
    fn a_local_start_takes_the_output_and_the_retired_stream_stays_silent() {
        let bench = Bench::gated(true);
        let mut peer = bench.connect("player-1");
        bench.stream(&mut peer);
        // These chunks are due after the takeover. A stream that kept its timeline would play them.
        let due = peer.server_now_us() + 1_000_000;
        let retired = retired_pattern(19200);
        for (index, chunk) in retired.chunks(CHUNK_FRAMES * 4).enumerate() {
            peer.send_chunk(due + index as i64 * 25_000, chunk);
            let paced = Instant::now() + Duration::from_millis(5);
            peer.serve_until("chunk pacing", || Instant::now() >= paced);
        }
        assert_eq!(bench.state()["transport"], "playing");

        let implicit = bench.start_local("local-1", false);
        assert_eq!(implicit["error"]["code"], "SOURCE_BUSY", "{implicit}");
        assert_eq!(bench.state()["source"], "sendspin");
        let explicit = bench.start_local("local-1", true);
        assert_eq!(explicit["status"], "accepted", "{explicit}");
        let state = bench.state();
        assert_eq!(state["source"], "local_file");
        assert_eq!(state["session_id"], "local-1");
        assert_eq!(state["muted"], true);
        assert_eq!(state["sendspin_playback"], Value::Null);

        // The closed connection tells the server that this player left.
        peer.serve_to_close("the retired connection ends");
        bench.wait("the retired connection retires", |bench| {
            bench.phase() == "disconnected"
        });
        bench.play_local();
        bench.wait("the local file plays to its end", |bench| {
            bench.state()["transport"] == "idle"
        });
        // The retired chunks fall due inside this wait.
        bench.assert_no_reconnect();
        let state = bench.state();
        assert_eq!(state["error"], Value::Null, "{state}");
        assert_ne!(state["source"], "sendspin");
        assert_eq!(
            bench.query("sources.sendspin.get")["connection_generation"],
            1
        );

        let capture = bench.capture();
        let kinds = classify(&capture);
        assert!(!kinds.contains(&Frame::Retired), "a retired chunk played");
        assert!(frames_of(&capture, &kinds, Frame::Stream) == pattern());
        assert!(frames_of(&capture, &kinds, Frame::Local) == local_pattern());
        let handover = kinds.iter().position(|kind| *kind == Frame::Local).unwrap();
        assert!(
            !kinds[handover..].contains(&Frame::Stream),
            "the stream played into the local session"
        );
    }
    /// Into Sendspin: a start with takeover stops local playback, and only then the stream plays.
    fn a_sendspin_start_takes_the_output_from_local_playback() {
        let bench = Bench::gated(false);
        assert_eq!(bench.start_local("local-1", false)["status"], "accepted");
        bench.play_local();
        // The closed gate holds the local source in the middle of its file.
        let gate = bench.gate.as_ref().unwrap();
        bench.wait("local playback fills the closed gate", |_| {
            gate.held_bytes() >= 32768
        });
        assert_eq!(bench.state()["transport"], "playing");

        let implicit = bench.start("player-1");
        assert_eq!(implicit["error"]["code"], "SOURCE_BUSY", "{implicit}");
        assert_eq!(bench.state()["source"], "local_file");
        let explicit = bench.takeover("player-1");
        assert_eq!(explicit["status"], "accepted", "{explicit}");
        let state = bench.state();
        assert_eq!(state["source"], "sendspin");
        assert_eq!(state["session_id"], "player-1");
        assert_eq!(state["muted"], true);
        assert_eq!(state["playback"], Value::Null);
        gate.open();

        let mut peer = bench.attach();
        bench.stream(&mut peer);
        assert_eq!(bench.state()["transport"], "playing");
        bench.end("sessions.stop");
        drop(peer);

        let capture = bench.capture();
        let kinds = classify(&capture);
        let local = frames_of(&capture, &kinds, Frame::Local);
        // The takeover cut the file short, and what played is its unbroken start.
        assert!(!local.is_empty() && local.len() < LOCAL_FRAMES * 4);
        assert!(local_pattern().starts_with(&local));
        assert!(frames_of(&capture, &kinds, Frame::Stream) == pattern());
        let handover = kinds
            .iter()
            .position(|kind| *kind == Frame::Stream)
            .unwrap();
        assert!(
            !kinds[handover..].contains(&Frame::Local),
            "local audio played into the Sendspin session"
        );
    }
    /// A takeover while the server is away ends the retries, so no attempt opens the output again.
    fn a_takeover_during_a_reconnect_ends_the_retries() {
        let bench = Bench::gated(true);
        let peer = bench.connect("player-1");
        peer.abandon();
        bench.wait("the lost server starts a reconnect", |bench| {
            bench.phase() == "reconnecting"
        });
        let explicit = bench.start_local("local-1", true);
        assert_eq!(explicit["status"], "accepted", "{explicit}");
        bench.wait("the reconnecting source retires", |bench| {
            bench.phase() == "disconnected"
        });
        bench.play_local();
        bench.assert_no_reconnect();
        let state = bench.state();
        assert_eq!(state["transport"], "idle", "{state}");
        assert_eq!(state["error"], Value::Null, "{state}");

        let capture = bench.capture();
        let kinds = classify(&capture);
        assert!(frames_of(&capture, &kinds, Frame::Local) == local_pattern());
        assert!(!kinds.contains(&Frame::Stream) && !kinds.contains(&Frame::Retired));
    }
    /// A stop right after a takeover cancels both sources. A shutdown in a handover exits in time.
    fn a_stop_or_a_shutdown_during_a_handover_ends_cleanly() {
        let bench = Bench::gated(true);
        assert_eq!(bench.start_local("local-1", false)["status"], "accepted");
        assert_eq!(bench.takeover("player-1")["status"], "accepted");
        // No peer accepted, so this stop cancels the connection attempt.
        bench.end("sessions.stop");
        assert_eq!(bench.state()["error"], Value::Null);
        bench.drain_listener();

        // The owner is reusable in both directions.
        let mut peer = bench.connect("player-2");
        bench.stream(&mut peer);
        let explicit = bench.start_local("local-2", true);
        assert_eq!(explicit["status"], "accepted", "{explicit}");
        // The Sendspin connection still retires and the local file still prepares.
        let capture = bench.capture();
        let kinds = classify(&capture);
        assert!(frames_of(&capture, &kinds, Frame::Stream) == pattern());
        assert!(!kinds.contains(&Frame::Local), "a muted session played");
    }
}
