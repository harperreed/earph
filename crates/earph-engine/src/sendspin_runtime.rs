// ABOUTME: Runs the Sendspin player connection that a Core session owns, and connects again after a lost server.
// ABOUTME: Loads credentials, starts Network, feeds the worker, and retires Network before credentials.
use crate::{
    sendspin_credentials::{
        AdmissionPolicy, BridgeError, CredentialSupervisor, Retirement, SessionHandoff,
        SessionRequest, StoreConfiguration,
    },
    worker::{Action, Settings, Worker},
};
use earph_core::{
    EngineError, ErrorCode,
    ids::OwnerId,
    sendspin_control::SendspinConnection,
    sendspin_source::{SendspinSourceEvent, SendspinSourceObservation, SendspinTransportPhase},
    session::Session,
};
use earph_network::sendspin::{
    ClockConfiguration, DriverError, DriverLimits, MonotonicDomain, Operation,
    SendspinConnectPermit, SendspinObserver, SendspinWorker, roleless_session_limits,
};
use earph_sendspin::{
    player::{PlayerBinding, PlayerSupport},
    timing::{Binding, TimePolicy},
};
use std::{
    fmt,
    net::IpAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    task::{Context, Poll, Waker},
    time::{Duration, Instant},
};

/// The permit and the admission policy expire together. Each connection attempt gets new ones.
const LIFETIME: Duration = Duration::from_secs(12 * 60 * 60);
/// The advertised buffer must fit the transport's event byte limit.
const BUFFER_CAPACITY: usize = 98304;
/// The supervisor bounds each native operation. This bound covers a supervisor that never answers.
const LOAD_DEADLINE: Duration = Duration::from_secs(30);
/// A worker that owns the source reports its close within one tick of a transport end.
const ORPHAN_GRACE: Duration = Duration::from_secs(2);
/// Network closes within its own one second ceiling unless a commit is still pending.
const RETIREMENT_DEADLINE: Duration = Duration::from_secs(10);

/// The first attempt after a lost connection waits this long.
const RECONNECT_DELAY: Duration = Duration::from_millis(250);
/// The delay doubles after each failed attempt and stops growing here.
const RECONNECT_DELAY_LIMIT: Duration = Duration::from_secs(10);

/// Launch configuration for the one Sendspin server. IPC never supplies any of these values.
pub struct SendspinConfiguration {
    /// `ws://IP:PORT/PATH`.
    pub server_url: String,
    pub pinned_address: IpAddr,
    pub server_public_key: [u8; 32],
    pub credentials: StoreConfiguration,
}
impl fmt::Debug for SendspinConfiguration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SendspinConfiguration([REDACTED])")
    }
}

fn bridge_code(error: BridgeError) -> ErrorCode {
    match error {
        BridgeError::Cancelled => ErrorCode::OperationCancelled,
        // No identity exists yet. Provisioning and pairing are separate work.
        BridgeError::Store(earph_credentials::StoreError::Missing) => ErrorCode::AuthRequired,
        // Locked, invalid, or uncertain credentials all deny this connection.
        _ => ErrorCode::AuthDenied,
    }
}
fn driver_code(error: DriverError) -> ErrorCode {
    match error {
        DriverError::Expired | DriverError::Authentication | DriverError::Admission => {
            ErrorCode::AuthDenied
        }
        DriverError::Budget => ErrorCode::ResourceBudgetExceeded,
        _ => ErrorCode::AssetInvalid,
    }
}

/// True for a transport end that a later connection can heal:
/// a closed or lost socket, an unreachable server, and a server that stopped answering.
/// A refused authentication, a protocol violation, an expired permit, and a cancel never retry.
fn heals(error: DriverError) -> bool {
    matches!(
        error,
        DriverError::PeerClosed
            | DriverError::Lost
            | DriverError::Connect
            | DriverError::Upgrade
            | DriverError::Deadline
    )
}
/// Returns the wait before the next attempt. The delay has no random part, so tests can predict it.
fn reconnect_delay(failures: u32) -> Duration {
    RECONNECT_DELAY
        .saturating_mul(1 << failures.min(16))
        .min(RECONNECT_DELAY_LIMIT)
}

/// The reconnect facts of one session. They outlive each connection of that session.
struct Reconnect {
    /// The generation of the session's connection fence.
    generation: u64,
    /// Lost connections that this runtime restored.
    restored: u64,
    /// Attempts that failed in a row since the server last knew this player.
    failures: u32,
    /// True from a lost connection until the server knows this player again.
    lost: bool,
    /// When the next attempt may begin.
    due: Instant,
}
/// Writes the reconnect facts into an observation of the worker's source.
/// One source serves one connection, so it knows neither the count nor an earlier loss.
fn stamped(
    reconnect: Option<&Reconnect>,
    mut observation: SendspinSourceObservation,
) -> SendspinSourceObservation {
    if let Some(reconnect) = reconnect {
        observation.reconnects = reconnect.restored;
        if reconnect.lost && observation.phase == SendspinTransportPhase::Preparing {
            observation.phase = SendspinTransportPhase::Reconnecting;
        }
    }
    observation
}

enum Stage {
    Loading {
        load: Operation<Result<SessionHandoff, BridgeError>>,
        since: Instant,
    },
    Running {
        observer: SendspinObserver,
        /// When the transport ended while the worker had not reported its closed source.
        orphaned: Option<Instant>,
    },
    RetiringNetwork,
    RetiringSupervisor,
}
struct Connection {
    fence: SendspinConnection,
    stage: Stage,
    // Rust drops fields in declaration order. Network joins while its supervisor still lives.
    network: SendspinWorker,
    supervisor: CredentialSupervisor,
    policy: AdmissionPolicy,
    expires: Instant,
    cancel: Arc<AtomicBool>,
    /// The transport ended in a way that a new connection can heal. The session keeps its fence.
    lost: bool,
    shared: Arc<Mutex<SendspinSourceObservation>>,
    /// The latest progress that Core adopted.
    adopted: Option<SendspinSourceObservation>,
    retiring_since: Option<Instant>,
    /// Network kept an unresolved commit, or the supervisor reported an unknown outcome.
    /// The supervisor reports one for a native operation that outlived its timeout,
    /// for a lost completion, and for a failed supervisor thread.
    uncertain: bool,
}

pub(crate) struct SendspinRuntime {
    configuration: SendspinConfiguration,
    owner: OwnerId,
    connection: Option<Connection>,
    /// Present once the server knew this player in the current session. Only then a loss retries.
    reconnect: Option<Reconnect>,
    /// An uncertain credential outcome refuses every later start in this owner process.
    /// Explicit storage recovery is separate work, so only a restart clears this latch.
    recovery_required: bool,
}
impl SendspinRuntime {
    /// Validates the launch target with the transport's own permit rules.
    pub fn new(configuration: SendspinConfiguration, owner: OwnerId) -> Result<Self, EngineError> {
        SendspinConnectPermit::new(
            owner.clone(),
            1,
            1,
            Instant::now() + LIFETIME,
            &configuration.server_url,
            vec![configuration.pinned_address],
        )
        .map_err(|_| {
            EngineError::new(
                ErrorCode::AssetInvalid,
                "Sendspin needs a ws://IP:PORT/PATH server and a permitted pinned address",
            )
        })?;
        Ok(Self {
            configuration,
            owner,
            connection: None,
            reconnect: None,
            recovery_required: false,
        })
    }
    pub fn is_idle(&self) -> bool {
        self.connection.is_none()
    }
    /// True when a retirement outlived its bound. The owner may then stop waiting for it.
    pub fn retirement_overdue(&self) -> bool {
        self.connection
            .as_ref()
            .and_then(|connection| connection.retiring_since)
            .is_some_and(|since| since.elapsed() > RETIREMENT_DEADLINE)
    }
    /// Passes one worker event to Core under the fences of the connection that produced it.
    /// A source that closed on a healable transport end reaches Core as `reconnecting`.
    pub fn route(&mut self, session: &mut Session, mut event: SendspinSourceEvent, now_ns: u64) {
        let Some(connection) = self.connection.as_mut() else {
            return;
        };
        let fence = connection.fence;
        if fence.epochs != event.epochs {
            return;
        }
        match event.observation.phase {
            SendspinTransportPhase::AwaitingStream | SendspinTransportPhase::Streaming => {
                let reconnect = self.reconnect.get_or_insert(Reconnect {
                    generation: fence.generation,
                    restored: 0,
                    failures: 0,
                    lost: false,
                    due: Instant::now(),
                });
                if reconnect.lost {
                    reconnect.lost = false;
                    reconnect.restored += 1;
                    reconnect.failures = 0;
                }
            }
            SendspinTransportPhase::Closed => {
                let terminal = match &connection.stage {
                    Stage::Running { observer, .. } => observer.observation().terminal,
                    _ => None,
                };
                // A source that failed for its own reason reports another code and never retries.
                if let Some(reconnect) = self.reconnect.as_mut()
                    && let Some(terminal) = terminal.filter(|terminal| heals(*terminal))
                    && event.error.is_none_or(|code| code == driver_code(terminal))
                    && session.sendspin_connection() == Some(fence)
                {
                    reconnect.lost = true;
                    connection.lost = true;
                    // The session stays open, and an open phase carries no error.
                    event.error = None;
                    event.observation.phase = SendspinTransportPhase::Reconnecting;
                }
            }
            _ => {}
        }
        event.observation = stamped(self.reconnect.as_ref(), event.observation);
        // Core refuses an event that the current session no longer owns.
        let _ = session.advance_sendspin(fence.generation, event, now_ns);
    }
    /// Runs one step that never blocks. The owner loop calls it on every iteration.
    pub fn poll(&mut self, session: &mut Session, worker: &mut Worker, now_ns: u64) {
        let wanted = session.sendspin_connection();
        // Reconnect facts belong to one session.
        if self
            .reconnect
            .as_ref()
            .map(|reconnect| reconnect.generation)
            != wanted.map(|fence| fence.generation)
        {
            self.reconnect = None;
        }
        let restored = self.reconnect.as_ref().map_or(0, |facts| facts.restored);
        if self.connection.is_none()
            && let Some(fence) = wanted
            // A lost connection waits for its delay. A stop meanwhile releases the session at once.
            && self
                .reconnect
                .as_ref()
                .is_none_or(|reconnect| Instant::now() >= reconnect.due)
        {
            match self.begin(fence) {
                Ok(connection) => self.connection = Some(connection),
                // Credentials that fail now deny the session, as they do at its start.
                Err(code) => {
                    close(session, fence, (0, restored), Some(code), now_ns);
                    let _ = session.complete_sendspin_retirement(fence.generation, now_ns);
                }
            }
        }
        let Some(connection) = self.connection.as_mut() else {
            // A session that ended before this poll left no workers to join.
            if let Some(generation) = session.retiring_sendspin_connection() {
                let _ = session.complete_sendspin_retirement(generation, now_ns);
            }
            return;
        };
        let fence = connection.fence;
        if wanted == Some(fence) && !connection.lost {
            let failure = match &mut connection.stage {
                Stage::Loading { load, since } => {
                    match load.as_mut().poll(&mut Context::from_waker(Waker::noop())) {
                        Poll::Pending if since.elapsed() < LOAD_DEADLINE => None,
                        Poll::Pending => Some(Some(ErrorCode::DeviceUnavailable)),
                        Poll::Ready(Err(error)) => Some(Some(bridge_code(error))),
                        Poll::Ready(Ok(handoff)) => connection
                            .start(&self.configuration, &self.owner, handoff, session, worker)
                            .err()
                            .map(Some),
                    }
                }
                Stage::Running { observer, orphaned } => {
                    let observed = connection.shared.lock().map(|shared| *shared).ok();
                    let observed =
                        observed.map(|observed| stamped(self.reconnect.as_ref(), observed));
                    if let Some(observed) = observed
                        && connection.adopted != Some(observed)
                        && session.observe_sendspin(fence.generation, observed).is_ok()
                    {
                        connection.adopted = Some(observed);
                    }
                    // A start action that the worker dropped unread never reports a closed source.
                    match observer.observation().terminal {
                        Some(error) if orphaned.is_some_and(|at| at.elapsed() > ORPHAN_GRACE) => {
                            Some(
                                (!matches!(
                                    error,
                                    DriverError::PeerClosed | DriverError::Cancelled
                                ))
                                .then(|| driver_code(error)),
                            )
                        }
                        Some(_) => {
                            orphaned.get_or_insert_with(Instant::now);
                            None
                        }
                        None => None,
                    }
                }
                Stage::RetiringNetwork | Stage::RetiringSupervisor => None,
            };
            if let Some(error) = failure {
                let frames = connection.adopted.map_or(0, |seen| seen.submitted_frames);
                close(session, fence, (frames, restored), error, now_ns);
            }
        }
        let released = session.sendspin_connection() != Some(fence);
        if (released || connection.lost) && connection.retire() {
            if connection.uncertain {
                self.recovery_required = true;
                session.require_sendspin_credential_recovery();
                if !released {
                    // The latch refuses every later attempt, so the session ends here.
                    let denied = Some(ErrorCode::AuthDenied);
                    close(session, fence, (0, restored), denied, now_ns);
                }
            }
            if session.sendspin_connection() == Some(fence) {
                // The session keeps its fence. The next attempt gets a new permit and fresh credentials.
                if let Some(reconnect) = self.reconnect.as_mut() {
                    reconnect.due = Instant::now() + reconnect_delay(reconnect.failures);
                    reconnect.failures = reconnect.failures.saturating_add(1);
                }
            } else {
                let _ = session.complete_sendspin_retirement(fence.generation, now_ns);
            }
            self.connection = None;
        }
    }
    /// Spawns the credential supervisor and requests the committed document.
    fn begin(&self, fence: SendspinConnection) -> Result<Connection, ErrorCode> {
        if self.recovery_required {
            return Err(ErrorCode::AuthDenied);
        }
        let expires = Instant::now() + LIFETIME;
        let policy =
            AdmissionPolicy::paired_playback(self.configuration.server_public_key, expires);
        let store = &self.configuration.credentials;
        let mut supervisor = CredentialSupervisor::spawn(StoreConfiguration {
            helper: store.helper.clone(),
            native: store.native.clone(),
            lock_path: store.lock_path.clone(),
            operation_timeout: store.operation_timeout,
        })
        .map_err(bridge_code)?;
        let load = supervisor.load(
            SessionRequest {
                connection_generation: fence.generation,
                hello: serde_json::json!({"name":"Earph","supported_roles":[]}),
                protected: vec![],
                limits: roleless_session_limits(),
            },
            policy.clone(),
        );
        Ok(Connection {
            fence,
            stage: Stage::Loading {
                load,
                since: Instant::now(),
            },
            network: SendspinWorker::default(),
            supervisor,
            policy,
            expires,
            cancel: Arc::new(AtomicBool::new(false)),
            lost: false,
            shared: Arc::new(Mutex::new(SendspinSourceObservation::new(
                SendspinTransportPhase::Preparing,
                0,
            ))),
            adopted: None,
            retiring_since: None,
            uncertain: false,
        })
    }
}
/// Reports a closed source to Core on behalf of a worker that never owned it.
fn close(
    session: &mut Session,
    fence: SendspinConnection,
    (submitted_frames, reconnects): (u64, u64),
    error: Option<ErrorCode>,
    now_ns: u64,
) {
    let mut observation =
        SendspinSourceObservation::new(SendspinTransportPhase::Closed, submitted_frames);
    observation.reconnects = reconnects;
    let _ = session.advance_sendspin(
        fence.generation,
        SendspinSourceEvent {
            epochs: fence.epochs,
            observation,
            error,
        },
        now_ns,
    );
}
impl Connection {
    /// Starts the Network connection and hands its handle to the playback worker.
    fn start(
        &mut self,
        configuration: &SendspinConfiguration,
        owner: &OwnerId,
        handoff: SessionHandoff,
        session: &Session,
        worker: &mut Worker,
    ) -> Result<(), ErrorCode> {
        let generation = self.fence.generation;
        let permit = SendspinConnectPermit::new(
            owner.clone(),
            generation,
            generation,
            self.expires,
            &configuration.server_url,
            vec![configuration.pinned_address],
        )
        .map_err(driver_code)?;
        let domain = MonotonicDomain::new(generation).map_err(driver_code)?;
        let policy = TimePolicy::new(250_000, 10_000_000, 60_000_000)
            .map_err(|_| ErrorCode::InternalFault)?;
        let support =
            PlayerSupport::new(BUFFER_CAPACITY).map_err(|_| ErrorCode::ResourceBudgetExceeded)?;
        // A one millisecond join wait keeps each retirement poll inside the owner loop's own tick.
        let limits = DriverLimits {
            retire_ms: 1,
            ..DriverLimits::default()
        };
        let handle = self
            .network
            .start_player(
                permit.clone(),
                &permit,
                handoff.config,
                ClockConfiguration {
                    domain: domain.clone(),
                    policy,
                },
                Box::new(handoff.services),
                limits,
                support,
            )
            .map_err(driver_code)?;
        self.stage = Stage::Running {
            observer: handle.observer(),
            orphaned: None,
        };
        let binding = PlayerBinding::new(
            Binding::new(configuration.server_public_key, generation, generation),
            self.fence.epochs.session,
            generation,
        );
        let mut snapshot = session.snapshot();
        // The source reports its events under these epochs, and Core fences on them.
        snapshot.epochs = self.fence.epochs;
        // A refused send drops the handle, which cancels the connection.
        worker
            .send(Action::StartSendspin {
                settings: Settings {
                    pcm_processing: false,
                    snapshot,
                    playing: true,
                },
                handle,
                clock: domain,
                binding,
                observation: self.shared.clone(),
                cancel: self.cancel.clone(),
            })
            .map_err(|error| error.code)
    }
    /// Advances retirement by one step that never blocks. Returns true after both workers joined.
    fn retire(&mut self) -> bool {
        if self.retiring_since.is_none() {
            self.retiring_since = Some(Instant::now());
            self.policy.revoke();
            // The flag closes the worker's source without a queued action.
            self.cancel.store(true, Ordering::Release);
        }
        if !matches!(self.stage, Stage::RetiringSupervisor) {
            self.stage = Stage::RetiringNetwork;
            // Network joins first, so its session consumes an acknowledged commit result.
            match self.network.retire() {
                Err(DriverError::CleanupTimeout) => return false,
                Err(DriverError::StorageUncertain) => self.uncertain = true,
                _ => {}
            }
            self.stage = Stage::RetiringSupervisor;
        }
        // A zero timeout polls the supervisor thread without a wait.
        let _ = self.supervisor.retire(Duration::ZERO);
        let observed = self.supervisor.observation();
        self.uncertain |= observed.uncertain;
        observed.retirement == Retirement::Joined
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use earph_core::{
        control::{ControlLimits, ReceiptStatus, decode_command},
        ids::ZoneId,
        sendspin_control::SendspinPhase,
        session::TransportState,
    };
    use serde_json::json;

    fn submit(session: &mut Session, id: &str, target: Option<&str>, payload: serde_json::Value) {
        let bytes = serde_json::to_vec(&json!({
            "schema_version":1,"request_id":id,"zone_id":"main","owner_id":"owner",
            "session_id":target,"expected_revision":session.snapshot().revision,"payload":payload
        }))
        .unwrap();
        let receipt = session.submit(decode_command(&bytes, &ControlLimits::default()).unwrap());
        assert_ne!(receipt.status, ReceiptStatus::Rejected, "{id}: {receipt:?}");
    }
    /// Returns an owner with an approved output, and a runtime whose credential helper is missing.
    fn fixture() -> (Session, SendspinRuntime) {
        let owner = OwnerId::new("owner").unwrap();
        let mut session = Session::new_owner(
            ZoneId::new("main").unwrap(),
            owner.clone(),
            ControlLimits::default(),
        );
        session.enable_local_playback();
        session.enable_sendspin();
        let directory = std::env::temp_dir().join(format!("earph-sendspin-{}", std::process::id()));
        let runtime = SendspinRuntime::new(
            SendspinConfiguration {
                server_url: "ws://10.1.2.3:9876/sendspin".into(),
                pinned_address: "10.1.2.3".parse().unwrap(),
                server_public_key: [8; 32],
                credentials: StoreConfiguration {
                    helper: directory.join("missing-helper"),
                    native: earph_credentials::NativeConfig::SecretService {
                        address: "unix:path=/nonexistent/earph-test-bus".into(),
                    },
                    lock_path: directory.join("missing-directory").join("lock"),
                    operation_timeout: Duration::from_millis(200),
                },
            },
            owner,
        )
        .unwrap();
        submit(
            &mut session,
            "select",
            None,
            json!({"method":"output.select","output":{
            "backend":"alsa","route":"system","endpoint":"null","device_id":"dac",
            "profile_id":"profile","period_frames":64,"buffer_frames":256,
            "volume_mode":"software","fidelity":"compatible_conversion"}}),
        );
        submit(
            &mut session,
            "consent",
            None,
            json!({"method":"output.consent","device_id":"dac","profile_id":"profile"}),
        );
        (session, runtime)
    }
    fn start(session: &mut Session, id: &str) {
        submit(
            session,
            id,
            Some(id),
            json!({"method":"sources.sendspin.start","device_id":"dac"}),
        );
    }
    /// Polls as the owner loop does until Core reports a disconnected source.
    /// No poll may ever start a Network connection in these tests.
    fn settle(session: &mut Session, runtime: &mut SendspinRuntime, worker: &mut Worker) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while session.sendspin_snapshot().phase != SendspinPhase::Disconnected {
            assert!(Instant::now() < deadline, "the runtime never retired");
            let now = session.snapshot().monotonic_ns + 1;
            runtime.poll(session, worker, now);
            assert!(!matches!(
                runtime
                    .connection
                    .as_ref()
                    .map(|connection| &connection.stage),
                Some(Stage::Running { .. })
            ));
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(runtime.is_idle());
        assert!(
            worker.events.try_recv().is_err(),
            "the worker saw no action"
        );
    }
    /// Puts the runtime in its loading stage without a poll that could finish the load.
    fn loading(session: &Session, runtime: &mut SendspinRuntime) {
        let fence = session.sendspin_connection().unwrap();
        runtime.connection = Some(runtime.begin(fence).unwrap());
        assert!(matches!(
            runtime.connection.as_ref().unwrap().stage,
            Stage::Loading { .. }
        ));
    }

    #[test]
    fn the_reconnect_delay_doubles_from_250_ms_and_stops_at_10_s() {
        let delays: Vec<u128> = (0..8)
            .map(|failures| reconnect_delay(failures).as_millis())
            .collect();
        assert_eq!(delays, [250, 500, 1000, 2000, 4000, 8000, 10000, 10000]);
        assert_eq!(reconnect_delay(u32::MAX), RECONNECT_DELAY_LIMIT);
    }
    #[test]
    fn only_a_transport_end_that_a_new_connection_heals_retries() {
        use DriverError::*;
        for error in [PeerClosed, Lost, Connect, Upgrade, Deadline] {
            assert!(heals(error), "{error:?}");
        }
        for error in [
            Authentication,
            Admission,
            Expired,
            Protocol,
            TlsUnsupported,
            Cancelled,
            Budget,
            Busy,
            Service,
            StorageUncertain,
            CleanupTimeout,
            Internal,
            Clock,
        ] {
            assert!(!heals(error), "{error:?}");
        }
    }
    /// The worker's source counts no reconnect, and its new source starts in `preparing`.
    #[test]
    fn observations_carry_the_reconnect_count_and_stay_reconnecting_until_the_server_returns() {
        let observed = |phase| SendspinSourceObservation::new(phase, 7);
        let mut reconnect = Reconnect {
            generation: 1,
            restored: 3,
            failures: 2,
            lost: true,
            due: Instant::now(),
        };
        let preparing = stamped(
            Some(&reconnect),
            observed(SendspinTransportPhase::Preparing),
        );
        assert_eq!(preparing.phase, SendspinTransportPhase::Reconnecting);
        assert_eq!(preparing.reconnects, 3);
        assert_eq!(preparing.submitted_frames, 7);
        reconnect.lost = false;
        for phase in [
            SendspinTransportPhase::Preparing,
            SendspinTransportPhase::Streaming,
            SendspinTransportPhase::Closed,
        ] {
            let seen = stamped(Some(&reconnect), observed(phase));
            assert_eq!((seen.phase, seen.reconnects), (phase, 3));
        }
        assert_eq!(
            stamped(None, observed(SendspinTransportPhase::Preparing)),
            observed(SendspinTransportPhase::Preparing)
        );
    }
    /// A helper that cannot run fails the load. No Network connection and no worker action exist.
    #[test]
    fn unavailable_credentials_fail_the_session_and_retire_without_a_connection() {
        let (mut session, mut runtime) = fixture();
        start(&mut session, "player");
        let mut worker = Worker::spawn();
        settle(&mut session, &mut runtime, &mut worker);
        let state = session.snapshot();
        assert_eq!(state.transport, TransportState::Failed);
        assert_eq!(state.error, Some(ErrorCode::AuthDenied));
        assert_eq!(state.session_id, None);
        assert!(!session.sendspin_snapshot().credential_recovery_required);
    }
    #[test]
    fn a_stop_during_the_credential_load_drops_the_load_and_disconnects() {
        let (mut session, mut runtime) = fixture();
        start(&mut session, "player");
        loading(&session, &mut runtime);
        submit(
            &mut session,
            "stop",
            Some("player"),
            json!({"method":"sessions.stop"}),
        );
        let mut worker = Worker::spawn();
        settle(&mut session, &mut runtime, &mut worker);
        let state = session.snapshot();
        assert_eq!(state.transport, TransportState::Idle);
        assert_eq!(state.error, None, "a dropped load reports no failure");
        assert_eq!(state.session_id, None);
    }
    /// The owner loop polls between commands. Core must still disconnect without that poll.
    #[test]
    fn a_session_that_ends_before_any_poll_still_disconnects() {
        let (mut session, mut runtime) = fixture();
        start(&mut session, "player");
        submit(
            &mut session,
            "stop",
            Some("player"),
            json!({"method":"sessions.stop"}),
        );
        let mut worker = Worker::spawn();
        settle(&mut session, &mut runtime, &mut worker);
    }
    /// The test sets the uncertain outcome that a real supervisor or Network join reports.
    #[test]
    fn an_uncertain_credential_outcome_latches_recovery_and_refuses_later_starts() {
        let (mut session, mut runtime) = fixture();
        start(&mut session, "player-1");
        loading(&session, &mut runtime);
        runtime.connection.as_mut().unwrap().uncertain = true;
        submit(
            &mut session,
            "stop",
            Some("player-1"),
            json!({"method":"sessions.stop"}),
        );
        let mut worker = Worker::spawn();
        settle(&mut session, &mut runtime, &mut worker);
        assert!(session.sendspin_snapshot().credential_recovery_required);
        start(&mut session, "player-2");
        let now = session.snapshot().monotonic_ns + 1;
        runtime.poll(&mut session, &mut worker, now);
        // The latch refuses the start before any supervisor exists.
        assert!(runtime.is_idle());
        assert_eq!(session.snapshot().error, Some(ErrorCode::AuthDenied));
        settle(&mut session, &mut runtime, &mut worker);
        assert!(session.sendspin_snapshot().credential_recovery_required);
    }
}
