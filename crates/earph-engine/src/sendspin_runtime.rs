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
    sendspin_source::{
        SendspinConnectionEnd, SendspinSourceEvent, SendspinSourceObservation,
        SendspinTransportPhase,
    },
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

/// The transport sends a WebSocket Ping this often. The specification names Ping and Pong as the liveness check and sets no time.
/// Two seconds is the time that one clock burst of eight probes takes when no reply comes.
/// The advertised buffer holds about half a second of audio, so a server that streams sends frames far more often.
const LIVENESS_PING_MS: u64 = 2000;
/// A server that sends no frame at all for this long after a Ping is lost. Any frame counts, not only a Pong.
/// Three seconds is what the transport already gives a server to accept a connection and to answer the upgrade.
/// A dead network without a reset is therefore found within five seconds. Admitted, not measured.
const LIVENESS_REPLY_MS: u64 = 3000;

/// The first attempt after a lost connection waits this long.
const RECONNECT_DELAY: Duration = Duration::from_millis(250);
/// The delay doubles after each failed attempt and stops growing here.
const RECONNECT_DELAY_LIMIT: Duration = Duration::from_secs(10);
/// A connection that the server kept at least this long ends a run of failed attempts.
/// A shorter one keeps the grown delay, so a server that accepts and drops cannot force fast retries.
const RECONNECT_STABLE: Duration = Duration::from_secs(30);

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
        DriverError::Expired
        | DriverError::Authentication
        | DriverError::Admission
        | DriverError::Unpaired
        | DriverError::UpgradeRefused => ErrorCode::AuthDenied,
        DriverError::Budget => ErrorCode::ResourceBudgetExceeded,
        _ => ErrorCode::AssetInvalid,
    }
}

/// Why a connection ended, with the WebSocket close code or the HTTP status that came with it.
type Ended = (SendspinConnectionEnd, Option<u16>);
/// Names a transport end for the source observation. `None` for an end that the observation does not explain.
fn connection_end(
    terminal: DriverError,
    close_code: Option<u16>,
    upgrade_status: Option<u16>,
) -> Option<Ended> {
    Some(match terminal {
        DriverError::PeerClosed => (SendspinConnectionEnd::PeerClosed, close_code),
        DriverError::Lost => (SendspinConnectionEnd::Lost, None),
        DriverError::Connect => (SendspinConnectionEnd::ConnectFailed, None),
        DriverError::Upgrade => (SendspinConnectionEnd::UpgradeFailed, upgrade_status),
        DriverError::Deadline => (SendspinConnectionEnd::Deadline, None),
        DriverError::Unpaired => (SendspinConnectionEnd::Unpaired, None),
        DriverError::UpgradeRefused => (SendspinConnectionEnd::UpgradeRefused, upgrade_status),
        _ => return None,
    })
}
/// Reads the terminal reason of a connection and its name for the source observation.
fn observed_end(observer: &SendspinObserver) -> (Option<DriverError>, Option<Ended>) {
    let observed = observer.observation();
    let ended = observed.terminal.and_then(|terminal| {
        connection_end(terminal, observed.close_code, observed.upgrade_status)
    });
    (observed.terminal, ended)
}

/// True for a transport end that a later connection can heal:
/// a closed or lost socket, an unreachable server, and a server that stopped answering.
/// A server that unpaired this player, an upgrade refused with HTTP 401 or 403, a refused authentication,
/// a protocol violation, an expired permit, and a cancel never retry.
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
    /// Attempts that ended in a row without a connection that lasted `RECONNECT_STABLE`.
    failures: u32,
    /// True from a lost connection until the server knows this player again.
    lost: bool,
    /// When the server last came to know this player on the current connection.
    accepted_at: Option<Instant>,
    /// When the next attempt may begin.
    due: Instant,
    /// Attempts that began since the server last knew this player.
    attempts: u64,
    /// Why the latest connection or attempt ended.
    ended: Option<Ended>,
}
impl Reconnect {
    /// Starts the facts when the server first knows this player.
    fn new(generation: u64, now: Instant) -> Self {
        Self {
            generation,
            restored: 0,
            failures: 0,
            lost: false,
            accepted_at: Some(now),
            due: now,
            attempts: 0,
            ended: None,
        }
    }
    /// Records that the server knows this player. After a loss this restores the connection.
    fn accept(&mut self, now: Instant) {
        if self.lost {
            self.lost = false;
            self.restored += 1;
            self.attempts = 0;
            self.accepted_at = Some(now);
        }
    }
    /// Counts an attempt that begins while the server is lost.
    fn attempt(&mut self) {
        if self.lost {
            self.attempts = self.attempts.saturating_add(1);
        }
    }
    /// Records a healable end and its reason. Only a connection that lasted clears the failed attempts.
    fn lose(&mut self, now: Instant, ended: Option<Ended>) {
        let lasted = self
            .accepted_at
            .take()
            .is_some_and(|at| now.saturating_duration_since(at) >= RECONNECT_STABLE);
        if lasted {
            self.failures = 0;
        }
        self.lost = true;
        self.ended = ended;
    }
    /// Sets when the next attempt may begin and returns the wait.
    fn schedule(&mut self, now: Instant) -> Duration {
        let delay = reconnect_delay(self.failures);
        self.due = now + delay;
        self.failures = self.failures.saturating_add(1);
        delay
    }
}
/// Decides the end of a connection whose worker never reported its closed source.
/// `None` keeps the session for a reconnect, as a reported close with the same transport end does.
/// Otherwise the session closes: cleanly after a peer close or a cancel, else with a failure.
fn orphan_failure(error: DriverError, accepted: bool) -> Option<Option<ErrorCode>> {
    if accepted && heals(error) {
        return None;
    }
    let clean = matches!(error, DriverError::PeerClosed | DriverError::Cancelled);
    Some((!clean).then(|| driver_code(error)))
}
/// Writes the reconnect facts into an observation of the worker's source.
/// One source serves one connection, so it knows neither the count nor an earlier loss.
fn stamped(
    reconnect: Option<&Reconnect>,
    mut observation: SendspinSourceObservation,
) -> SendspinSourceObservation {
    if let Some(reconnect) = reconnect {
        observation.reconnects = reconnect.restored;
        observation.reconnect_attempts = reconnect.attempts;
        if let Some((end, code)) = reconnect.ended {
            observation.last_end = Some(end);
            observation.last_end_code = code;
        }
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
        // A lost connection already reached Core as `reconnecting`. Its source has nothing more to say.
        if fence.epochs != event.epochs || connection.lost {
            return;
        }
        let now = Instant::now();
        // The end of a connection that closes the session. A healed end goes into the reconnect facts.
        let mut closed = None;
        match event.observation.phase {
            SendspinTransportPhase::AwaitingStream | SendspinTransportPhase::Streaming => {
                self.reconnect
                    .get_or_insert(Reconnect::new(fence.generation, now))
                    .accept(now);
            }
            SendspinTransportPhase::Closed => {
                // Every attempt of a session shares its epochs. A source marks its own observation closed
                // before it reports, so a close that left this one open belongs to an earlier attempt.
                if connection
                    .shared
                    .lock()
                    .is_ok_and(|shared| shared.phase != SendspinTransportPhase::Closed)
                {
                    return;
                }
                let (terminal, ended) = match &connection.stage {
                    Stage::Running { observer, .. } => observed_end(observer),
                    _ => (None, None),
                };
                // A source that failed for its own reason reports another code and never retries.
                let terminal = terminal.filter(|terminal| {
                    event
                        .error
                        .is_none_or(|code| code == driver_code(*terminal))
                });
                if let Some(reconnect) = self.reconnect.as_mut()
                    && terminal.is_some_and(heals)
                    && session.sendspin_connection() == Some(fence)
                {
                    reconnect.lose(now, ended);
                    connection.lost = true;
                    // The session stays open, and an open phase carries no error.
                    event.error = None;
                    event.observation.phase = SendspinTransportPhase::Reconnecting;
                } else if terminal.is_some() {
                    closed = ended;
                }
            }
            _ => {}
        }
        event.observation = stamped(self.reconnect.as_ref(), event.observation);
        if let Some((end, code)) = closed {
            event.observation.last_end = Some(end);
            event.observation.last_end_code = code;
        }
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
                Ok(connection) => {
                    self.connection = Some(connection);
                    if let Some(reconnect) = self.reconnect.as_mut() {
                        reconnect.attempt();
                    }
                }
                // Credentials that fail now deny the session, as they do at its start.
                Err(code) => {
                    close(session, fence, (0, restored), None, Some(code), now_ns);
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
            let mut orphan_heals = false;
            let mut orphan_ended = None;
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
                    let (terminal, ended) = observed_end(observer);
                    match terminal {
                        Some(error) if orphaned.is_some_and(|at| at.elapsed() > ORPHAN_GRACE) => {
                            orphan_ended = ended;
                            let failure = orphan_failure(error, self.reconnect.is_some());
                            orphan_heals = failure.is_none();
                            failure
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
                close(
                    session,
                    fence,
                    (frames, restored),
                    orphan_ended,
                    error,
                    now_ns,
                );
            }
            if orphan_heals && let Some(reconnect) = self.reconnect.as_mut() {
                // The worker reported no close, so the runtime reports the lost server itself.
                reconnect.lose(Instant::now(), orphan_ended);
                connection.lost = true;
                let event = SendspinSourceEvent {
                    epochs: fence.epochs,
                    observation: stamped(
                        Some(reconnect),
                        SendspinSourceObservation::new(SendspinTransportPhase::Reconnecting, 0),
                    ),
                    error: None,
                };
                let _ = session.advance_sendspin(fence.generation, event, now_ns);
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
                    close(session, fence, (0, restored), None, denied, now_ns);
                }
            }
            if session.sendspin_connection() == Some(fence) {
                // The session keeps its fence. The next attempt gets a new permit and fresh credentials.
                if let Some(reconnect) = self.reconnect.as_mut() {
                    reconnect.schedule(Instant::now());
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
    ended: Option<Ended>,
    error: Option<ErrorCode>,
    now_ns: u64,
) {
    let mut observation =
        SendspinSourceObservation::new(SendspinTransportPhase::Closed, submitted_frames);
    observation.reconnects = reconnects;
    if let Some((end, code)) = ended {
        observation.last_end = Some(end);
        observation.last_end_code = code;
    }
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
            ping_ms: LIVENESS_PING_MS,
            pong_ms: LIVENESS_REPLY_MS,
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
            Unpaired,
            UpgradeRefused,
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
    /// A server that accepts the player and drops it at once must not get an attempt every 250 ms.
    #[test]
    fn a_short_connection_keeps_the_grown_delay_and_a_lasting_one_resets_it() {
        let mut now = Instant::now();
        let mut reconnect = Reconnect::new(1, now);
        let mut delays = Vec::new();
        for _ in 0..4 {
            // The server drops the player one second after it accepted it.
            now += Duration::from_secs(1);
            reconnect.lose(now, None);
            assert!(reconnect.lost);
            let delay = reconnect.schedule(now);
            assert_eq!(reconnect.due, now + delay);
            delays.push(delay.as_millis());
            now += delay;
            reconnect.accept(now);
            assert!(!reconnect.lost);
        }
        assert_eq!(delays, [250, 500, 1000, 2000]);
        assert_eq!(reconnect.restored, 4);
        // Attempts that never reach the server keep growing the delay as well.
        reconnect.lose(now, None);
        assert_eq!(reconnect.schedule(now).as_millis(), 4000);
        reconnect.lose(now, None);
        assert_eq!(reconnect.schedule(now).as_millis(), 8000);
        reconnect.accept(now);
        // One millisecond short of the stable time changes nothing.
        reconnect.lose(now + RECONNECT_STABLE - Duration::from_millis(1), None);
        assert_eq!(reconnect.schedule(now).as_millis(), 10000);
        reconnect.accept(now);
        reconnect.lose(now + RECONNECT_STABLE, None);
        assert_eq!(reconnect.schedule(now).as_millis(), 250);
    }
    /// A user must see why the player retries, and how often it tried.
    #[test]
    fn a_lost_connection_shows_its_end_and_counts_the_attempts_until_the_server_returns() {
        let now = Instant::now();
        let mut reconnect = Reconnect::new(1, now);
        let observed = |reconnect: &Reconnect| {
            let preparing = SendspinSourceObservation::new(SendspinTransportPhase::Preparing, 0);
            let seen = stamped(Some(reconnect), preparing);
            (seen.reconnect_attempts, seen.last_end, seen.last_end_code)
        };
        // An attempt counts only while the server is lost. The first connection is no attempt.
        reconnect.attempt();
        assert_eq!(observed(&reconnect), (0, None, None));
        reconnect.lose(now, Some((SendspinConnectionEnd::PeerClosed, Some(1000))));
        let closed = Some(SendspinConnectionEnd::PeerClosed);
        assert_eq!(observed(&reconnect), (0, closed, Some(1000)));
        reconnect.attempt();
        reconnect.lose(now, Some((SendspinConnectionEnd::UpgradeFailed, Some(503))));
        reconnect.attempt();
        let failed = Some(SendspinConnectionEnd::UpgradeFailed);
        assert_eq!(observed(&reconnect), (2, failed, Some(503)));
        // The restored connection starts the count again and keeps the last end readable.
        reconnect.accept(now);
        assert_eq!(observed(&reconnect), (0, failed, Some(503)));
        assert_eq!(reconnect.restored, 1);
    }
    #[test]
    fn each_explained_transport_end_keeps_only_the_code_that_belongs_to_it() {
        use DriverError::*;
        use SendspinConnectionEnd as End;
        let end = |error| connection_end(error, Some(1001), Some(403));
        assert_eq!(end(PeerClosed), Some((End::PeerClosed, Some(1001))));
        assert_eq!(end(Lost), Some((End::Lost, None)));
        assert_eq!(end(Connect), Some((End::ConnectFailed, None)));
        assert_eq!(end(Upgrade), Some((End::UpgradeFailed, Some(403))));
        assert_eq!(end(Deadline), Some((End::Deadline, None)));
        assert_eq!(end(Unpaired), Some((End::Unpaired, None)));
        assert_eq!(end(UpgradeRefused), Some((End::UpgradeRefused, Some(403))));
        for error in [
            Cancelled,
            Authentication,
            Admission,
            Expired,
            Protocol,
            Clock,
        ] {
            assert_eq!(end(error), None, "{error:?}");
        }
    }
    /// The worker can take longer than the grace time to report its close, as with a slow output open.
    #[test]
    fn an_orphaned_connection_retries_a_healable_end_once_the_server_knew_the_player() {
        use DriverError::*;
        for error in [PeerClosed, Lost, Connect, Upgrade, Deadline] {
            assert_eq!(orphan_failure(error, true), None, "{error:?}");
        }
        // A server that never knew this player gets no retry.
        assert_eq!(orphan_failure(PeerClosed, false), Some(None));
        assert_eq!(
            orphan_failure(Connect, false),
            Some(Some(ErrorCode::AssetInvalid))
        );
        // An end that no connection heals closes the session as before.
        assert_eq!(orphan_failure(Cancelled, true), Some(None));
        assert_eq!(
            orphan_failure(Authentication, true),
            Some(Some(ErrorCode::AuthDenied))
        );
        // A refusal ends the session, however long the server knew this player.
        for refusal in [Unpaired, UpgradeRefused] {
            assert_eq!(
                orphan_failure(refusal, true),
                Some(Some(ErrorCode::AuthDenied)),
                "{refusal:?}"
            );
        }
        assert_eq!(
            orphan_failure(Protocol, true),
            Some(Some(ErrorCode::AssetInvalid))
        );
    }
    /// After an orphaned attempt, its slow source can still report a close while the next attempt runs.
    #[test]
    fn a_close_from_an_earlier_source_does_not_end_the_next_attempt() {
        let (mut session, mut runtime) = fixture();
        start(&mut session, "player");
        loading(&session, &mut runtime);
        let fence = session.sendspin_connection().unwrap();
        let closed = SendspinSourceEvent {
            epochs: fence.epochs,
            observation: SendspinSourceObservation::new(SendspinTransportPhase::Closed, 0),
            error: None,
        };
        // This attempt's own observation is still open, so the close is not its own.
        let now = session.snapshot().monotonic_ns + 1;
        runtime.route(&mut session, closed, now);
        assert_eq!(session.sendspin_connection(), Some(fence));
        assert!(session.snapshot().session_id.is_some());
        // The source of this attempt marks its observation before it reports.
        let shared = runtime.connection.as_ref().unwrap().shared.clone();
        shared.lock().unwrap().phase = SendspinTransportPhase::Closed;
        runtime.route(&mut session, closed, now);
        assert_eq!(session.sendspin_connection(), None);
        let mut worker = Worker::spawn();
        settle(&mut session, &mut runtime, &mut worker);
    }
    /// The worker's source counts no reconnect, and its new source starts in `preparing`.
    #[test]
    fn observations_carry_the_reconnect_count_and_stay_reconnecting_until_the_server_returns() {
        let observed = |phase| SendspinSourceObservation::new(phase, 7);
        let mut reconnect = Reconnect {
            restored: 3,
            lost: true,
            ..Reconnect::new(1, Instant::now())
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
