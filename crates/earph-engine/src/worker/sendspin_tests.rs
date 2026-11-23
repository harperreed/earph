// ABOUTME: Plays a real Sendspin peer through the real worker and its scheduler into an ALSA file capture.
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
    path::{Path, PathBuf},
    process::Command,
    time::{Instant, SystemTime, UNIX_EPOCH},
};
#[allow(dead_code, reason = "this test uses a subset of the shared peer")]
#[path = "../../tests/support/sendspin_player_peer.rs"]
mod peer;

const CHUNK_FRAMES: usize = 1200;
const PREAMBLE_FRAMES: usize = 4800;
const PATTERN_FRAMES: usize = 43200;
/// The scheduler pads an anchor with at most 5 ms of silence.
const ANCHOR_PAD_FRAMES: u64 = 240;
/// Eight chunks. A part lasts 200 ms.
const PART_FRAMES: u64 = 9600;
const PART_US: i64 = 200_000;
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
    // The declared lead time and buffer need are admitted values, not measurements.
    assert_eq!(
        peer.activate_player(),
        serde_json::json!({"available":true,"output_delay_ms":0,"required_lead_time_ms":150,"min_buffer_ms":200,"supported_commands":[]})
    );
    peer.serve_until("the source awaits its first stream", || {
        bench.observed().phase == SendspinTransportPhase::AwaitingStream
    });
    peer.start_stream();
    peer
}
/// Sends PCM as 25 ms chunks, then waits until the scheduler placed all `sent` frames.
/// The output accepts every sent frame that was not dropped, plus the inserted silence.
/// A stream end discards queued audio, so callers confirm submission before they end a stream.
fn stream(peer: &mut peer::PlayerPeer, bench: &Bench, first_timestamp: i64, pcm: &[u8], sent: u64) {
    for (index, chunk) in pcm.chunks(CHUNK_FRAMES * 4).enumerate() {
        peer.send_chunk(first_timestamp + index as i64 * 25_000, chunk);
        // The transport ends a connection that holds more than 32 undrained events.
        let paced = Instant::now() + Duration::from_millis(5);
        peer.serve_until("chunk pacing", || Instant::now() >= paced);
    }
    placed(peer, bench, sent);
}
/// Waits until the scheduler placed all `sent` frames.
fn placed(peer: &mut peer::PlayerPeer, bench: &Bench, sent: u64) {
    peer.serve_until("the output accepts every scheduled frame", || {
        let observed = bench.observed();
        observed.submitted_frames + observed.timing.dropped_frames
            == sent + observed.timing.inserted_silence_frames
    });
}
/// Closes the peer, joins both workers, and returns the capture.
fn finish(peer: peer::PlayerPeer, mut bench: Bench, directory: &Path) -> Vec<u8> {
    peer.close();
    wait("the source closes after its peer", || {
        bench.observed().phase == SendspinTransportPhase::Closed
    });
    assert_eq!(bench.retire(), Some(DriverError::PeerClosed));
    drop(bench.worker);
    fs::read(directory.join("capture.pcm")).unwrap()
}
/// Returns the capture after its leading inserted silence.
fn after_silence(capture: &[u8], frames: u64) -> &[u8] {
    let (silence, rest) = capture.split_at(frames as usize * 4);
    assert!(silence.iter().all(|byte| *byte == 0));
    rest
}
/// Returns the capture without its silence, and the count of silent frames. No pattern frame is silent.
fn without_silence(capture: &[u8]) -> (Vec<u8>, u64) {
    let (frames, rest) = capture.as_chunks::<4>();
    assert!(rest.is_empty(), "the capture holds whole frames");
    let audible: Vec<u8> = frames
        .iter()
        .filter(|frame| **frame != [0; 4])
        .flatten()
        .copied()
        .collect();
    let silent = (capture.len() - audible.len()) as u64 / 4;
    (audible, silent)
}
/// Splits the pattern into four parts of eight chunks each.
fn parts() -> [Vec<u8>; 4] {
    let pattern = pattern();
    let part = |index: usize| {
        let bytes = PART_FRAMES as usize * 4;
        pattern[index * bytes..(index + 1) * bytes].to_vec()
    };
    [part(0), part(1), part(2), part(3)]
}
/// Asserts that the capture is exactly these parts plus the silence that the scheduler inserted.
fn assert_capture(capture: &[u8], parts: &[&[u8]], timing: SendspinTiming) {
    let (audible, silent) = without_silence(capture);
    assert!(
        audible == parts.concat(),
        "captured {} audible bytes; expected {}",
        audible.len(),
        parts.concat().len()
    );
    assert_eq!(silent, timing.inserted_silence_frames);
    assert_eq!(timing.dropped_frames, 0);
}

/// The file device never underruns, so this seam stands in for the native failure.
/// It fails this many writes as underruns do. Recovery reprepares the real ALSA output.
static UNDERRUNS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
struct UnderrunSeam {
    inner: Box<dyn Output>,
    underran: bool,
}
pub(super) fn underrun_seam(inner: Box<dyn Output>) -> Box<dyn Output> {
    Box::new(UnderrunSeam {
        inner,
        underran: false,
    })
}
impl Output for UnderrunSeam {
    fn audio_storage_bytes(&self) -> Result<u64, EngineError> {
        self.inner.audio_storage_bytes()
    }
    fn is_running(&self) -> bool {
        self.inner.is_running()
    }
    fn observation(&self) -> Result<earph_core::session::BackendObservation, EngineError> {
        self.inner.observation()
    }
    fn write(
        &mut self,
        block: &PcmBlock,
        offset: usize,
        frames: usize,
    ) -> Result<Option<usize>, EngineError> {
        let armed = |left: usize| left.checked_sub(1);
        if UNDERRUNS
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, armed)
            .is_ok()
        {
            self.underran = true;
            return Err(EngineError::new(
                ErrorCode::OutputFault,
                "test seam: the output underran",
            ));
        }
        self.inner.write(block, offset, frames)
    }
    fn progress(&mut self) -> Result<output::Counters, EngineError> {
        self.inner.progress()
    }
    fn suspend(&mut self, epochs: Epochs) -> Result<output::Counters, EngineError> {
        self.inner.suspend(epochs)
    }
    fn recover(&mut self, epochs: Epochs) -> Result<bool, EngineError> {
        if !std::mem::take(&mut self.underran) {
            return self.inner.recover(epochs);
        }
        // A native recovery prepares the stream again. Suspend does that for a healthy stream.
        self.inner.suspend(epochs).map(|_| true)
    }
    fn finish(&mut self) -> Result<Option<output::Counters>, EngineError> {
        self.inner.finish()
    }
    fn wait(&mut self) -> Result<(), EngineError> {
        self.inner.wait()
    }
    fn stop(&mut self) -> Result<(), EngineError> {
        self.inner.stop()
    }
}
/// Returns after the worker applied these settings.
fn apply(bench: &mut Bench, settings: Settings) {
    bench
        .worker
        .send(Action::Settings(settings.clone()))
        .unwrap();
    // The worker reads the second copy only after it applied the first.
    bench.worker.send(Action::Settings(settings)).unwrap();
    wait("the playback worker applies the settings", || {
        bench.worker.queued.load(Ordering::Acquire) == 0
    });
}

/// Starts a bench whose gain is unity from its first frame.
fn unity_bench() -> Bench {
    let mut settings = settings("capture");
    settings.snapshot.gain_ramp_frames = 0;
    Bench::start(settings)
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
    let first_timestamp = peer.server_now_us() + 300_000;
    stream(&mut peer, &bench, first_timestamp, &sent, frames);
    let streaming = bench.observed();
    assert_eq!(streaming.phase, SendspinTransportPhase::Streaming);
    let timing = streaming.timing;
    assert!(timing.anchored);
    assert_eq!(
        timing.qualification,
        SendspinTimingQualification::Unqualified
    );
    assert_eq!(timing.correction, SendspinTimingCorrection::None);
    assert_eq!(timing.dropped_frames, 0);
    let pad = timing.inserted_silence_frames;
    assert!(pad <= ANCHOR_PAD_FRAMES, "anchor pad {pad}");
    // The file device has no clock and reports zero delay, so it accepts the second chunk at once.
    // That chunk leaves at most one 25 ms chunk plus the anchor pad before its target.
    let start = timing.start_error_us.expect("a measured start error");
    assert!((-31_000..=0).contains(&start), "start error {start}");
    assert!(timing.last_error_us.is_some());
    assert!(timing.max_abs_error_us >= start.unsigned_abs());
    assert!(timing.mapping_revision.is_some());
    assert!(timing.backend_delay_known);
    assert_eq!(timing.backend_delay_basis, Some("alsa_client_estimate"));
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
    let closed = bench.observed();
    assert_eq!(closed.phase, SendspinTransportPhase::Closed);
    assert_eq!(closed.submitted_frames, frames + pad);
    assert!(!closed.timing.anchored, "a closed source holds no anchor");
    assert_eq!(closed.timing.start_error_us, None);
    assert_eq!(closed.timing.dropped_frames, 0);
    assert_eq!(closed.timing.inserted_silence_frames, pad);
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

    // Nothing else writes this output, so the capture holds the anchor pad and the sent bytes.
    let capture = fs::read(directory.join("capture.pcm")).unwrap();
    let audible = after_silence(&capture, pad + PREAMBLE_FRAMES as u64);
    assert!(
        audible == pattern,
        "captured {} bytes after the silence; sent {}",
        audible.len(),
        pattern.len()
    );
}

#[test]
fn a_late_stream_drops_its_overdue_frames_and_plays_the_exact_remainder() {
    let Some(directory) = isolate(
        "worker::sendspin_tests::a_late_stream_drops_its_overdue_frames_and_plays_the_exact_remainder",
    ) else {
        return;
    };
    const LATE_US: i64 = 40_000;
    let bench = unity_bench();
    let mut peer = streaming_peer(&bench);
    let pattern = pattern();
    let stamped = Instant::now();
    let first_timestamp = peer.server_now_us() - LATE_US;
    // Transit and scheduling add lateness, so the test measures when the anchor appeared.
    let mut anchored = None;
    for (index, chunk) in pattern.chunks(CHUNK_FRAMES * 4).enumerate() {
        peer.send_chunk(first_timestamp + index as i64 * 25_000, chunk);
        let paced = Instant::now() + Duration::from_millis(5);
        peer.serve_until("chunk pacing", || {
            if anchored.is_none() && bench.observed().timing.anchored {
                anchored = Some(stamped.elapsed());
            }
            Instant::now() >= paced
        });
    }
    placed(&mut peer, &bench, PATTERN_FRAMES as u64);
    let timing = bench.observed().timing;
    let capture = finish(peer, bench, &directory);
    // The clock mapping may err by a fraction of a millisecond; allow 2 ms.
    let dropped = timing.dropped_frames;
    let least = LATE_US as u64 * 48 / 1000 - 96;
    let overdue_us = LATE_US as u64 + anchored.expect("an anchor while sending").as_micros() as u64;
    let most = overdue_us * 48 / 1000 + CHUNK_FRAMES as u64;
    assert!(
        (least..=most).contains(&dropped),
        "dropped {dropped}, expected {least}..={most}"
    );
    let played = after_silence(&capture, timing.inserted_silence_frames);
    assert!(
        played == &pattern[dropped as usize * 4..],
        "the capture is the pattern without its first {dropped} frames"
    );
}

#[test]
fn a_short_timestamp_gap_becomes_exactly_that_much_silence() {
    let Some(directory) =
        isolate("worker::sendspin_tests::a_short_timestamp_gap_becomes_exactly_that_much_silence")
    else {
        return;
    };
    const GAP_FRAMES: u64 = 480;
    let bench = unity_bench();
    let mut peer = streaming_peer(&bench);
    let pattern = pattern();
    let (before, after) = pattern.split_at(pattern.len() / 2);
    let first_timestamp = peer.server_now_us() + 100_000;
    stream(
        &mut peer,
        &bench,
        first_timestamp,
        before,
        before.len() as u64 / 4,
    );
    let chunks = (before.len() / 4 / CHUNK_FRAMES) as i64;
    // The second part starts 10 ms after the first part ends.
    stream(
        &mut peer,
        &bench,
        first_timestamp + chunks * 25_000 + 10_000,
        after,
        PATTERN_FRAMES as u64,
    );
    let timing = bench.observed().timing;
    let capture = finish(peer, bench, &directory);
    assert!(timing.anchored, "a short gap keeps the anchor");
    assert_eq!(timing.dropped_frames, 0);
    let pad = timing.inserted_silence_frames - GAP_FRAMES;
    assert!(pad <= ANCHOR_PAD_FRAMES, "anchor pad {pad}");
    let played = after_silence(&capture, pad);
    assert_eq!(played.len(), pattern.len() + GAP_FRAMES as usize * 4);
    assert!(played[..before.len()] == *before);
    assert!(after_silence(&played[before.len()..], GAP_FRAMES) == after);
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
    apply(&mut bench, muted);
    let chunks = (sent.len() / 4 / CHUNK_FRAMES) as i64;
    let frames = (PREAMBLE_FRAMES + PATTERN_FRAMES) as u64;
    stream(
        &mut peer,
        &bench,
        first_timestamp + chunks * 25_000,
        silenced,
        frames,
    );
    let streaming = bench.observed();
    assert_eq!(streaming.phase, SendspinTransportPhase::Streaming);
    assert!(streaming.timing.anchored, "a muted source stays scheduled");
    assert_eq!(streaming.timing.dropped_frames, 0);
    let pad = streaming.timing.inserted_silence_frames;
    let capture = finish(peer, bench, &directory);
    assert_eq!(capture.len() as u64, (frames + pad) * 4);
    let played = after_silence(&capture, pad + PREAMBLE_FRAMES as u64);
    let (played, muted) = played.split_at(audible.len());
    assert!(played == audible);
    assert!(
        muted.iter().all(|byte| *byte == 0),
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

#[test]
fn a_full_clear_discards_queued_audio_and_the_next_audio_anchors_again() {
    let Some(directory) = isolate(
        "worker::sendspin_tests::a_full_clear_discards_queued_audio_and_the_next_audio_anchors_again",
    ) else {
        return;
    };
    let bench = unity_bench();
    let mut peer = streaming_peer(&bench);
    let [first, cleared, second, third] = parts();
    let start = peer.server_now_us() + 100_000;
    stream(&mut peer, &bench, start, &first, PART_FRAMES);
    assert_eq!(bench.observed().timing.anchors, 1);
    // These timestamps continue the first part. Only the clear makes this audio anchor again.
    peer.clear_stream(None);
    stream(&mut peer, &bench, start + PART_US, &second, 2 * PART_FRAMES);
    let timing = bench.observed().timing;
    assert!(timing.anchored);
    assert_eq!(timing.anchors, 2, "audio after a full clear anchors again");
    // This audio is due in three seconds, so it is still queued when the clear arrives.
    for (index, chunk) in cleared.chunks(CHUNK_FRAMES * 4).enumerate() {
        peer.send_chunk(start + 3_000_000 + index as i64 * 25_000, chunk);
        let paced = Instant::now() + Duration::from_millis(5);
        peer.serve_until("chunk pacing", || Instant::now() >= paced);
    }
    peer.clear_stream(None);
    let restart = peer.server_now_us() + 150_000;
    stream(&mut peer, &bench, restart, &third, 3 * PART_FRAMES);
    let streaming = bench.observed();
    assert_eq!(streaming.phase, SendspinTransportPhase::Streaming);
    assert_eq!(streaming.timing.anchors, 3);
    assert_eq!(streaming.timing.underruns, 0);
    let capture = finish(peer, bench, &directory);
    assert_capture(&capture, &[&first, &second, &third], streaming.timing);
}

#[test]
fn a_partial_clear_keeps_submitted_audio_and_trims_its_replacement() {
    let Some(directory) = isolate(
        "worker::sendspin_tests::a_partial_clear_keeps_submitted_audio_and_trims_its_replacement",
    ) else {
        return;
    };
    let bench = unity_bench();
    let mut peer = streaming_peer(&bench);
    let [first, replacement, ..] = parts();
    let start = peer.server_now_us() + 100_000;
    stream(&mut peer, &bench, start, &first, PART_FRAMES);
    // The output already accepted the whole first part, so its second half cannot be retracted.
    // The replacement starts at the cutoff. Its first half covers that submitted audio.
    let cutoff = start + PART_US / 2;
    peer.clear_stream(Some(cutoff));
    stream(
        &mut peer,
        &bench,
        cutoff,
        &replacement,
        PART_FRAMES + PART_FRAMES / 2,
    );
    // A wrongly accepted replacement would follow here.
    let settled = Instant::now() + Duration::from_millis(100);
    peer.serve_until("the scheduler stays idle", || Instant::now() >= settled);
    let streaming = bench.observed();
    assert_eq!(streaming.phase, SendspinTransportPhase::Streaming);
    assert_eq!(
        streaming.timing.anchors, 1,
        "a partial clear keeps the anchor"
    );
    let capture = finish(peer, bench, &directory);
    let kept = &replacement[replacement.len() / 2..];
    assert_capture(&capture, &[&first, kept], streaming.timing);
}

#[test]
fn a_second_stream_after_a_stream_end_plays_exactly_and_anchors_again() {
    let Some(directory) = isolate(
        "worker::sendspin_tests::a_second_stream_after_a_stream_end_plays_exactly_and_anchors_again",
    ) else {
        return;
    };
    let bench = unity_bench();
    let mut peer = streaming_peer(&bench);
    let [first, second, ..] = parts();
    let start = peer.server_now_us() + 100_000;
    stream(&mut peer, &bench, start, &first, PART_FRAMES);
    peer.end_stream();
    peer.serve_until("the source awaits another stream", || {
        bench.observed().phase == SendspinTransportPhase::AwaitingStream
    });
    assert!(!bench.observed().timing.anchored);
    peer.start_stream();
    let restart = peer.server_now_us() + 150_000;
    stream(&mut peer, &bench, restart, &second, 2 * PART_FRAMES);
    let streaming = bench.observed();
    assert_eq!(streaming.phase, SendspinTransportPhase::Streaming);
    assert_eq!(streaming.timing.anchors, 2);
    let capture = finish(peer, bench, &directory);
    assert_capture(&capture, &[&first, &second], streaming.timing);
}

#[test]
fn a_reactivated_player_declares_again_and_plays_the_next_stream() {
    let Some(directory) = isolate(
        "worker::sendspin_tests::a_reactivated_player_declares_again_and_plays_the_next_stream",
    ) else {
        return;
    };
    let bench = unity_bench();
    let mut peer = streaming_peer(&bench);
    let [first, lost, second, _] = parts();
    let start = peer.server_now_us() + 100_000;
    stream(&mut peer, &bench, start, &first, PART_FRAMES);
    // This audio is due in three seconds, so the lost role finds it queued.
    for (index, chunk) in lost.chunks(CHUNK_FRAMES * 4).enumerate() {
        peer.send_chunk(start + 3_000_000 + index as i64 * 25_000, chunk);
        let paced = Instant::now() + Duration::from_millis(5);
        peer.serve_until("chunk pacing", || Instant::now() >= paced);
    }
    peer.activate_idle();
    peer.serve_until("the source loses its player role", || {
        bench.observed().phase == SendspinTransportPhase::Preparing
    });
    assert_eq!(
        peer.activate_player()["available"],
        true,
        "a new activation gets a new available declaration"
    );
    peer.serve_until("the source awaits a stream again", || {
        bench.observed().phase == SendspinTransportPhase::AwaitingStream
    });
    peer.start_stream();
    let restart = peer.server_now_us() + 150_000;
    stream(&mut peer, &bench, restart, &second, 2 * PART_FRAMES);
    let streaming = bench.observed();
    assert_eq!(streaming.phase, SendspinTransportPhase::Streaming);
    assert_eq!(streaming.timing.anchors, 2);
    let capture = finish(peer, bench, &directory);
    assert_capture(&capture, &[&first, &second], streaming.timing);
}

#[test]
fn a_starvation_gap_longer_than_100_ms_anchors_again_and_plays_both_sides_exactly() {
    let Some(directory) = isolate(
        "worker::sendspin_tests::a_starvation_gap_longer_than_100_ms_anchors_again_and_plays_both_sides_exactly",
    ) else {
        return;
    };
    let bench = unity_bench();
    let mut peer = streaming_peer(&bench);
    let [first, second, ..] = parts();
    let start = peer.server_now_us() + 100_000;
    stream(&mut peer, &bench, start, &first, PART_FRAMES);
    assert_eq!(bench.observed().timing.anchors, 1);
    // The server sends nothing for 300 ms of its timeline.
    stream(
        &mut peer,
        &bench,
        start + PART_US + 300_000,
        &second,
        2 * PART_FRAMES,
    );
    let streaming = bench.observed();
    assert_eq!(streaming.phase, SendspinTransportPhase::Streaming);
    assert!(streaming.timing.anchored);
    assert_eq!(streaming.timing.anchors, 2, "a long gap anchors again");
    // Each anchor pads at most 5 ms. The scheduler waits out the gap; it does not fill it.
    let pad = streaming.timing.inserted_silence_frames;
    assert!(pad <= 2 * ANCHOR_PAD_FRAMES, "anchor pads {pad}");
    let capture = finish(peer, bench, &directory);
    assert_capture(&capture, &[&first, &second], streaming.timing);
}

#[test]
fn audio_beyond_the_timeline_budget_closes_the_source_with_a_reported_failure() {
    if isolate(
        "worker::sendspin_tests::audio_beyond_the_timeline_budget_closes_the_source_with_a_reported_failure",
    )
    .is_none()
    {
        return;
    }
    let mut bench = unity_bench();
    let mut peer = streaming_peer(&bench);
    let chunk = vec![1; CHUNK_FRAMES * 4];
    // This audio is due in one minute, so nothing plays and every chunk stays charged.
    let start = peer.server_now_us() + 60_000_000;
    // Each chunk costs its payload and a 13-byte header against the wire budget.
    let fitting = earph_sendspin::player::MAX_OUTSTANDING_WIRE_BYTES / (chunk.len() + 13);
    for index in 0..fitting {
        peer.send_chunk(start + index as i64 * 25_000, &chunk);
        let paced = Instant::now() + Duration::from_millis(5);
        peer.serve_until("chunk pacing", || Instant::now() >= paced);
    }
    assert_eq!(
        bench.observed().phase,
        SendspinTransportPhase::Streaming,
        "audio within the budget keeps the source open"
    );
    peer.send_chunk(start + fitting as i64 * 25_000, &chunk);
    // The source cancels its connection, so the client ends it.
    peer.serve_to_close("the source closes its connection");
    wait("the source reports its close", || {
        bench.observed().phase == SendspinTransportPhase::Closed
    });
    let closed = bench.observed();
    assert_eq!(closed.submitted_frames, 0);
    for expected in [
        (SendspinTransportPhase::Preparing, None),
        (SendspinTransportPhase::AwaitingStream, None),
        (SendspinTransportPhase::Streaming, None),
        (
            SendspinTransportPhase::Closed,
            Some(ErrorCode::ResourceBudgetExceeded),
        ),
    ] {
        assert_eq!(bench.reported(), expected);
    }
    assert_eq!(bench.retire(), Some(DriverError::Cancelled));
}

#[test]
fn an_output_underrun_reprepares_the_output_and_anchors_again_with_gain_unchanged() {
    let Some(directory) = isolate(
        "worker::sendspin_tests::an_output_underrun_reprepares_the_output_and_anchors_again_with_gain_unchanged",
    ) else {
        return;
    };
    let mut settings = settings("capture");
    settings.snapshot.gain_ramp_frames = 0;
    let mut bench = Bench::start(settings.clone());
    let mut peer = streaming_peer(&bench);
    let [first, second, muted, _] = parts();
    let start = peer.server_now_us() + 100_000;
    stream(&mut peer, &bench, start, &first, PART_FRAMES);
    // The next write fails as a native underrun does, with part of a chunk already in the period.
    UNDERRUNS.store(1, Ordering::Release);
    stream(&mut peer, &bench, start + PART_US, &second, 2 * PART_FRAMES);
    assert_eq!(
        UNDERRUNS.load(Ordering::Acquire),
        0,
        "the seam failed a write"
    );
    let recovered = bench.observed();
    assert_eq!(recovered.phase, SendspinTransportPhase::Streaming);
    assert_eq!(recovered.timing.underruns, 1);
    assert!(recovered.timing.anchored);
    assert_eq!(recovered.timing.anchors, 2, "an underrun anchors again");
    // A muted session stays muted across the next recovery.
    settings.snapshot.muted = true;
    apply(&mut bench, settings);
    UNDERRUNS.store(1, Ordering::Release);
    stream(
        &mut peer,
        &bench,
        start + 2 * PART_US,
        &muted,
        3 * PART_FRAMES,
    );
    let streaming = bench.observed();
    assert_eq!(streaming.phase, SendspinTransportPhase::Streaming);
    assert_eq!(streaming.timing.underruns, 2);
    assert_eq!(streaming.timing.anchors, 3);
    assert_eq!(streaming.timing.dropped_frames, 0);
    let capture = finish(peer, bench, &directory);
    // Unity gain survived the first recovery: both audible parts are exact, with no frame lost.
    let (audible, silent) = without_silence(&capture);
    assert!(audible == [first, second].concat());
    assert_eq!(
        silent,
        PART_FRAMES + streaming.timing.inserted_silence_frames,
        "the muted part reaches the output as silence"
    );
}

#[test]
fn an_underrun_before_the_recovered_output_accepts_a_frame_closes_the_source() {
    if isolate(
        "worker::sendspin_tests::an_underrun_before_the_recovered_output_accepts_a_frame_closes_the_source",
    )
    .is_none()
    {
        return;
    }
    let mut bench = unity_bench();
    let mut peer = streaming_peer(&bench);
    let [first, second, ..] = parts();
    let start = peer.server_now_us() + 100_000;
    stream(&mut peer, &bench, start, &first, PART_FRAMES);
    // The first write after the recovery fails too. No real underrun does that.
    UNDERRUNS.store(2, Ordering::Release);
    peer.send_chunk(start + PART_US, &second[..CHUNK_FRAMES * 4]);
    // The source cancels its connection, so the client ends it.
    peer.serve_to_close("the source closes its connection");
    wait("the source reports its close", || {
        bench.observed().phase == SendspinTransportPhase::Closed
    });
    let closed = bench.observed();
    assert_eq!(closed.timing.underruns, 1, "the second failure is final");
    for expected in [
        (SendspinTransportPhase::Preparing, None),
        (SendspinTransportPhase::AwaitingStream, None),
        (SendspinTransportPhase::Streaming, None),
        (SendspinTransportPhase::Closed, Some(ErrorCode::OutputFault)),
    ] {
        assert_eq!(bench.reported(), expected);
    }
    assert_eq!(bench.retire(), Some(DriverError::Cancelled));
}
