// ABOUTME: Plays a real Sendspin peer through the real worker into an ALSA file capture.
// ABOUTME: Runs in a child process so its private ALSA configuration reaches no other test.
use super::*;
use earph_core::{control::*, ids::*, safety::*, sendspin_source::*};
use earph_network::sendspin::*;
use earph_sendspin::{
    pairing::{AttemptId, Candidate, CommitOutcome, PairingRecord, RecordCatalog, RecordUpdate},
    player::{PlayerBinding, PlayerSupport},
    session::{AdmissionProposal, SessionConfig},
    timing::{Binding, TimePolicy},
};
use std::{
    fs,
    net::{IpAddr, TcpListener, UdpSocket},
    path::PathBuf,
    process::Command,
    time::{Instant, SystemTime, UNIX_EPOCH},
};
#[path = "../../tests/support/sendspin_player_peer.rs"]
mod peer;

const CHUNK_FRAMES: usize = 1200;
const PREAMBLE_FRAMES: usize = 4800;
const PATTERN_FRAMES: usize = 43200;
const DEADLINE: Duration = Duration::from_secs(10);
const CHILD_DEADLINE: Duration = Duration::from_secs(60);

fn isolate(name: &str) -> Option<PathBuf> {
    if std::env::var("EARPH_SENDSPIN_WORKER_CHILD").as_deref() == Ok(name) {
        return Some(
            std::env::var_os("EARPH_SENDSPIN_WORKER_ARTIFACTS")
                .unwrap()
                .into(),
        );
    }
    let directory = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../target/sendspin-worker")
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
    fs::write(
        &config,
        format!(
            "pcm.null {{ type null }}\npcm.capture {{ type file slave.pcm null file \"{}\" format raw }}\n",
            directory.join("capture.pcm").display()
        ),
    )
    .unwrap();
    // Log files keep a talkative child from blocking on a full pipe.
    let log = |name: &str| fs::File::create(directory.join(name)).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", name, "--nocapture"])
        .env("EARPH_SENDSPIN_WORKER_CHILD", name)
        .env("EARPH_SENDSPIN_WORKER_ARTIFACTS", &directory)
        .env("ALSA_CONFIG_PATH", config)
        .stdout(log("stdout.log"))
        .stderr(log("stderr.log"))
        .spawn()
        .unwrap();
    // A hung worker must fail this test, so the wait has a deadline.
    let deadline = Instant::now() + CHILD_DEADLINE;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            break None;
        }
        thread::sleep(Duration::from_millis(5));
    };
    assert!(
        status.is_some_and(|status| status.success()),
        "child {status:?}: {} / {}",
        fs::read_to_string(directory.join("stdout.log")).unwrap(),
        fs::read_to_string(directory.join("stderr.log")).unwrap()
    );
    None
}
/// The transport refuses loopback pins, so the peer listens on a private interface.
fn interface() -> IpAddr {
    let socket = UdpSocket::bind("0.0.0.0:0").unwrap();
    socket.connect("192.0.2.1:9").unwrap();
    let address = socket.local_addr().unwrap().ip();
    assert!(matches!(address, IpAddr::V4(ip) if ip.is_private()));
    address
}
struct Services;
impl SendspinServices for Services {
    fn playback_authorized(&self) -> bool {
        true
    }
    fn admission(&mut self, _: AdmissionProposal) -> Operation<Result<bool, DriverError>> {
        Box::pin(async { Ok(true) })
    }
    fn candidate(&mut self, _: AttemptId) -> Operation<Result<Candidate, DriverError>> {
        Box::pin(async { Err(DriverError::Service) })
    }
    fn commit(&mut self, _: AttemptId, _: RecordUpdate) -> Operation<Result<(), CommitOutcome>> {
        Box::pin(async { Err(CommitOutcome::Unchanged) })
    }
}
fn session_config() -> SessionConfig {
    SessionConfig {
        private: peer::CLIENT_PRIVATE,
        pairing_psk: peer::PAIRING_PSK,
        connection_generation: 1,
        hello: serde_json::json!({"name":"Earph fixture","supported_roles":[]}),
        catalog: RecordCatalog::from_committed(
            5,
            vec![PairingRecord {
                server_public: server_public(),
                psk: peer::RECORD_PSK,
                last_used_order: 1,
            }],
            vec![],
        )
        .unwrap(),
        protected: vec![],
        limits: roleless_session_limits(),
    }
}
fn server_public() -> [u8; 32] {
    earph_sendspin::noise::static_public_key(&peer::SERVER_PRIVATE).unwrap()
}
fn settings(endpoint: &str) -> Settings {
    let mut snapshot = earph_core::session::Session::new_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("sendspin-owner").unwrap(),
        ControlLimits::default(),
    )
    .snapshot();
    snapshot.session_id = Some(SessionId::new("sendspin-session").unwrap());
    snapshot.epochs = Epochs {
        session: 1,
        stream: 0,
    };
    snapshot.muted = false;
    snapshot.software_gain_db = 0.0;
    // The ramp ends inside the zero preamble, so every pattern frame passes at unity.
    snapshot.gain_ramp_frames = 2400;
    snapshot.output_consented = true;
    snapshot.output = Some(OutputConfiguration {
        backend: OutputBackend::Alsa,
        route: OutputRoute::System,
        endpoint: endpoint.into(),
        device_id: DeviceId::new("capture").unwrap(),
        profile_id: ProfileId::new("profile").unwrap(),
        period_frames: 480,
        buffer_frames: 1920,
        volume_mode: VolumeMode::Fixed,
        fidelity: FidelityPolicy::StrictPreservation,
    });
    Settings {
        snapshot,
        playing: true,
        pcm_processing: false,
    }
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
fn wait(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + DEADLINE;
    while !done() {
        assert!(Instant::now() < deadline, "deadline: {what}");
        thread::sleep(Duration::from_millis(1));
    }
}

/// One real transport connection and one real playback worker that owns its source.
struct Bench {
    listener: TcpListener,
    network: SendspinWorker,
    transport: SendspinObserver,
    observation: Arc<Mutex<SendspinSourceObservation>>,
    worker: Worker,
}
impl Bench {
    fn start(settings: Settings) -> Self {
        let listener = TcpListener::bind((interface(), 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let permit = SendspinConnectPermit::new(
            OwnerId::new("sendspin-owner").unwrap(),
            1,
            1,
            Instant::now() + Duration::from_secs(60),
            &format!("ws://player-fixture.test:{}/sendspin", address.port()),
            vec![address.ip()],
        )
        .unwrap();
        let domain = MonotonicDomain::new(1).unwrap();
        let mut network = SendspinWorker::default();
        let handle = network
            .start_player(
                permit.clone(),
                &permit,
                session_config(),
                ClockConfiguration {
                    domain: domain.clone(),
                    policy: TimePolicy::new(250_000, 10_000_000, 60_000_000).unwrap(),
                },
                Box::new(Services),
                DriverLimits::default(),
                PlayerSupport::new(262144).unwrap(),
            )
            .unwrap();
        let transport = handle.observer();
        let observation = Arc::new(Mutex::new(SendspinSourceObservation::new(
            SendspinTransportPhase::Preparing,
            0,
        )));
        let mut worker = Worker::spawn();
        worker
            .send(Action::StartSendspin {
                settings,
                handle,
                clock: domain,
                binding: PlayerBinding::new(Binding::new(server_public(), 1, 1), 1, 1),
                observation: observation.clone(),
                cancel: Arc::new(AtomicBool::new(false)),
            })
            .unwrap();
        Self {
            listener,
            network,
            transport,
            observation,
            worker,
        }
    }
    fn observed(&self) -> SendspinSourceObservation {
        *self.observation.lock().unwrap()
    }
    /// Returns the phase and error of the next worker event, which must be a Sendspin event.
    fn reported(&self) -> (SendspinTransportPhase, Option<ErrorCode>) {
        match self.worker.events.recv_timeout(DEADLINE) {
            Ok(Event::Sendspin(event)) => (event.observation.phase, event.error),
            _ => panic!("the Sendspin source must report its next event"),
        }
    }
    /// Joins the Network worker and returns why its connection ended.
    fn retire(&mut self) -> Option<DriverError> {
        self.network.retire().unwrap();
        let transport = self.transport.observation();
        assert!(transport.joined);
        transport.terminal
    }
}

/// Accepts the client, activates the player role, and starts one stream.
fn streaming_peer(bench: &Bench) -> peer::PlayerPeer {
    let mut peer = peer::PlayerPeer::accept(&bench.listener);
    peer.activate_player();
    peer.serve_until("the source awaits its first stream", || {
        bench.observed().phase == SendspinTransportPhase::AwaitingStream
    });
    peer.start_stream();
    peer
}
/// Sends PCM as 25 ms chunks, then waits until the output accepted `submitted` frames in total.
/// A stream end discards queued audio, so callers confirm submission before they end a stream.
fn stream(
    peer: &mut peer::PlayerPeer,
    bench: &Bench,
    first_timestamp: i64,
    pcm: &[u8],
    submitted: u64,
) {
    for (index, chunk) in pcm.chunks(CHUNK_FRAMES * 4).enumerate() {
        peer.send_chunk(first_timestamp + index as i64 * 25_000, chunk);
        // The transport ends a connection that holds more than 32 undrained events.
        let paced = Instant::now() + Duration::from_millis(5);
        peer.serve_until("chunk pacing", || Instant::now() >= paced);
    }
    peer.serve_until("the output accepts every sent frame", || {
        bench.observed().submitted_frames == submitted
    });
}
/// Returns after the worker finished every action sent before this call.
fn settle(bench: &mut Bench) {
    // The worker counts an action as read before it applies it. A second action proves the first.
    bench.worker.send(Action::StopSendspin).unwrap();
    wait("the playback worker reads its actions", || {
        bench.worker.queued.load(Ordering::Acquire) == 0
    });
}

#[test]
fn real_peer_audio_reaches_the_alsa_capture_bit_exact_and_the_source_closes_cleanly() {
    let Some(directory) = isolate(
        "worker::sendspin_tests::real_peer_audio_reaches_the_alsa_capture_bit_exact_and_the_source_closes_cleanly",
    ) else {
        return;
    };
    let mut bench = Bench::start(settings("capture"));
    let mut peer = streaming_peer(&bench);
    let pattern = pattern();
    let sent = [vec![0; PREAMBLE_FRAMES * 4], pattern.clone()].concat();
    let frames = (PREAMBLE_FRAMES + PATTERN_FRAMES) as u64;
    let first_timestamp = peer.server_now_us() + 200_000;
    stream(&mut peer, &bench, first_timestamp, &sent, frames);
    assert_eq!(bench.observed().phase, SendspinTransportPhase::Streaming);
    peer.end_stream();
    peer.serve_until("the source awaits another stream", || {
        bench.observed().phase == SendspinTransportPhase::AwaitingStream
    });
    peer.close();
    wait("the source closes after its peer", || {
        bench.observed().phase == SendspinTransportPhase::Closed
    });
    settle(&mut bench);
    assert_eq!(bench.retire(), Some(DriverError::PeerClosed));
    assert_eq!(
        bench.observed(),
        SendspinSourceObservation::new(SendspinTransportPhase::Closed, frames)
    );
    for phase in [
        SendspinTransportPhase::Preparing,
        SendspinTransportPhase::AwaitingStream,
        SendspinTransportPhase::Streaming,
        SendspinTransportPhase::AwaitingStream,
        SendspinTransportPhase::Closed,
    ] {
        assert_eq!(bench.reported(), (phase, None));
    }
    assert!(bench.worker.events.try_recv().is_err());
    // Dropping the worker joins its thread, which proves only that the thread is not hung.
    drop(bench.worker);

    // Nothing else writes this output, so the capture holds exactly the sent bytes.
    let capture = fs::read(directory.join("capture.pcm")).unwrap();
    let audible = capture.iter().position(|byte| *byte != 0).unwrap();
    assert_eq!(audible, PREAMBLE_FRAMES * 4);
    assert!(
        capture[audible..] == pattern,
        "captured {} bytes after {audible} zeros; sent {}",
        capture.len() - audible,
        pattern.len()
    );
}

#[test]
fn mute_silences_the_capture_while_the_source_still_consumes_the_stream() {
    let Some(directory) = isolate(
        "worker::sendspin_tests::mute_silences_the_capture_while_the_source_still_consumes_the_stream",
    ) else {
        return;
    };
    let mut bench = Bench::start(settings("capture"));
    let mut peer = streaming_peer(&bench);
    let pattern = pattern();
    let (audible, silenced) = pattern.split_at(pattern.len() / 2);
    let sent = [vec![0; PREAMBLE_FRAMES * 4], audible.to_vec()].concat();
    let first_timestamp = peer.server_now_us() + 200_000;
    stream(
        &mut peer,
        &bench,
        first_timestamp,
        &sent,
        sent.len() as u64 / 4,
    );
    let mut muted = settings("capture");
    muted.snapshot.muted = true;
    muted.snapshot.gain_ramp_frames = 0;
    bench.worker.send(Action::Settings(muted.clone())).unwrap();
    // The worker reads the second copy only after it applied the first.
    bench.worker.send(Action::Settings(muted)).unwrap();
    wait("the playback worker applies the mute", || {
        bench.worker.queued.load(Ordering::Acquire) == 0
    });
    let chunks = (sent.len() / 4 / CHUNK_FRAMES) as i64;
    let frames = (PREAMBLE_FRAMES + PATTERN_FRAMES) as u64;
    stream(
        &mut peer,
        &bench,
        first_timestamp + chunks * 25_000,
        silenced,
        frames,
    );
    assert_eq!(bench.observed().phase, SendspinTransportPhase::Streaming);
    peer.close();
    wait("the source closes after its peer", || {
        bench.observed().phase == SendspinTransportPhase::Closed
    });
    assert_eq!(bench.retire(), Some(DriverError::PeerClosed));
    drop(bench.worker);

    let capture = fs::read(directory.join("capture.pcm")).unwrap();
    assert_eq!(capture.len() as u64, frames * 4);
    assert!(capture[..PREAMBLE_FRAMES * 4].iter().all(|byte| *byte == 0));
    assert!(capture[PREAMBLE_FRAMES * 4..sent.len()] == *audible);
    assert!(
        capture[sent.len()..].iter().all(|byte| *byte == 0),
        "muted frames reach the output as silence"
    );
}

#[test]
fn seek_is_refused_and_pause_stop_or_cancellation_close_the_source() {
    if isolate(
        "worker::sendspin_tests::seek_is_refused_and_pause_stop_or_cancellation_close_the_source",
    )
    .is_none()
    {
        return;
    }
    // No peer accepts these connections, so each transport stays in its upgrade.
    let preparing = (SendspinTransportPhase::Preparing, None);
    let closed = (SendspinTransportPhase::Closed, None);
    let mut bench = Bench::start(settings("capture"));
    assert_eq!(bench.reported(), preparing);
    let seek = Action::Seek {
        settings: settings("capture"),
        position: 0,
    };
    bench.worker.send(seek).unwrap();
    assert_eq!(
        bench.reported(),
        (
            SendspinTransportPhase::Preparing,
            Some(ErrorCode::CapabilityUnsupported)
        ),
        "a refused seek leaves the source running"
    );
    // Pause is a safety action. This source cannot hold a stream, so it closes.
    bench
        .worker
        .send(Action::Pause(settings("capture")))
        .unwrap();
    assert_eq!(bench.reported(), closed);
    assert_eq!(bench.retire(), Some(DriverError::Cancelled));

    let mut bench = Bench::start(settings("capture"));
    assert_eq!(bench.reported(), preparing);
    let stop = StopRequest {
        request_id: RequestId::new("stop").unwrap(),
        settings: settings("capture"),
    };
    bench.worker.send(Action::Stop(stop)).unwrap();
    assert_eq!(bench.reported(), closed);
    assert!(matches!(
        bench.worker.events.recv_timeout(DEADLINE),
        Ok(Event::Stopped {
            observation: None,
            ..
        })
    ));
    assert_eq!(bench.retire(), Some(DriverError::Cancelled));

    let mut bench = Bench::start(settings("capture"));
    assert_eq!(bench.reported(), preparing);
    bench.worker.cancel_active();
    // The source reports Closed only after its output stopped without an error.
    assert_eq!(bench.reported(), closed);
    assert_eq!(bench.observed().phase, SendspinTransportPhase::Closed);
    assert_eq!(bench.retire(), Some(DriverError::Cancelled));
}

#[test]
fn a_failed_start_reports_one_closed_event_that_carries_its_error() {
    if isolate(
        "worker::sendspin_tests::a_failed_start_reports_one_closed_event_that_carries_its_error",
    )
    .is_none()
    {
        return;
    }
    let mut ramp = settings("capture");
    ramp.snapshot.gain_ramp_frames = 48001;
    for (settings, code) in [
        (settings("missing"), ErrorCode::DeviceUnavailable),
        (ramp, ErrorCode::SafetyPolicyBlocked),
    ] {
        let mut bench = Bench::start(settings);
        assert_eq!(
            bench.reported(),
            (SendspinTransportPhase::Closed, Some(code))
        );
        assert_eq!(bench.observed().phase, SendspinTransportPhase::Closed);
        settle(&mut bench);
        assert!(
            bench.worker.events.try_recv().is_err(),
            "a failed start reports once"
        );
        assert_eq!(bench.retire(), Some(DriverError::Cancelled));
    }
}
