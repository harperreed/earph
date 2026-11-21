// ABOUTME: Serializes WebSocket messages and complete session write receipts.
// ABOUTME: Retains one socket and acknowledged service work until retirement.
use earph_sendspin::{
    message::ReceiveStamp,
    session::{TransportKind, TransportWrite},
};
use futures_util::Sink;
use std::{
    pin::Pin,
    task::{Context, Poll},
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Error, Message},
};

struct PendingWrite {
    write: TransportWrite,
    started: bool,
}
impl PendingWrite {
    fn poll<S: AsyncRead + AsyncWrite + Unpin>(
        &mut self,
        socket: &mut WebSocketStream<S>,
        cx: &mut Context<'_>,
        closed: bool,
    ) -> Poll<Result<(), Error>> {
        if closed {
            return Poll::Ready(Err(Error::AlreadyClosed));
        }
        if !self.started {
            std::task::ready!(Pin::new(&mut *socket).poll_ready(cx))?;
            self.start_ready(socket)?;
        }
        Pin::new(socket).poll_flush(cx)
    }
    fn start_ready<S: AsyncRead + AsyncWrite + Unpin>(
        &mut self,
        socket: &mut WebSocketStream<S>,
    ) -> Result<(), Error> {
        let bytes = std::mem::take(&mut self.write.bytes);
        let message = match self.write.kind {
            TransportKind::Binary => Message::Binary(bytes.into()),
            TransportKind::Text => Message::Text(
                String::from_utf8(bytes)
                    .map_err(|_| Error::AlreadyClosed)?
                    .into(),
            ),
        };
        Pin::new(&mut *socket).start_send(message)?;
        self.started = true;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use earph_sendspin::{pairing::RecordCatalog, session::*};
    use serde_json::json;
    use std::{
        future::poll_fn,
        task::{Poll, Waker},
    };
    use tokio::io::AsyncReadExt;
    use tokio_tungstenite::tungstenite::protocol::Role;

    fn session() -> ClientSession {
        ClientSession::new(
            SessionConfig {
                private: [5; 32],
                pairing_psk: [7; 32],
                connection_generation: 1,
                hello: json!({"name":"fixture","supported_roles":[]}),
                catalog: RecordCatalog::new(5).unwrap(),
                protected: vec![],
                limits: SessionLimits::default(),
            },
            0,
        )
        .unwrap()
    }
    #[test]
    fn buffered_write_cannot_acknowledge_session_before_complete_flush() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let (transport, mut consumer) = tokio::io::duplex(1);
                let mut socket =
                    WebSocketStream::from_raw_socket(transport, Role::Client, None).await;
                let mut session = session();
                let mut pending = PendingWrite {
                    write: session.next_write(0).unwrap().unwrap(),
                    started: false,
                };
                let mut cx = Context::from_waker(Waker::noop());
                assert!(
                    pending.poll(&mut socket, &mut cx, false).is_pending(),
                    "buffered start_send is not complete flush"
                );
                assert!(pending.started);
                assert_eq!(session.phase(), SessionPhase::SendingInit);
                assert!(session.next_write(1).unwrap().is_none());
                assert_eq!(session.handshake_hash(), None);
                let drain = async move {
                    let mut bytes = Vec::new();
                    consumer.read_to_end(&mut bytes).await.unwrap();
                    bytes
                };
                let send = async {
                    poll_fn(|cx| pending.poll(&mut socket, cx, false))
                        .await
                        .unwrap();
                    session
                        .complete_write(pending.write.id, WriteResult::Complete, 2)
                        .unwrap();
                    assert_eq!(session.phase(), SessionPhase::AwaitServerInit);
                    assert!(
                        session
                            .complete_write(pending.write.id, WriteResult::Complete, 2)
                            .unwrap()
                            .is_empty()
                    );
                    drop(socket);
                };
                let (_, bytes) = futures_util::future::join(send, drain).await;
                assert!(bytes.len() > 1);
            });
    }
    #[test]
    fn observed_close_never_produces_a_complete_receipt() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let (transport, _consumer) = tokio::io::duplex(1);
                let mut socket =
                    WebSocketStream::from_raw_socket(transport, Role::Client, None).await;
                let mut session = session();
                let mut pending = PendingWrite {
                    write: session.next_write(0).unwrap().unwrap(),
                    started: false,
                };
                let mut cx = Context::from_waker(Waker::noop());
                assert!(matches!(
                    pending.poll(&mut socket, &mut cx, true),
                    Poll::Ready(Err(_))
                ));
                assert!(
                    session
                        .complete_write(pending.write.id, WriteResult::Uncertain, 1)
                        .is_err()
                );
                assert_eq!(session.phase(), SessionPhase::Closed);
            });
    }
    #[test]
    fn actual_peer_close_preserves_uncertain_receipt_even_when_adapter_flush_succeeds() {
        use futures_util::StreamExt;
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let (transport, peer) = tokio::io::duplex(1);
                let mut peer = WebSocketStream::from_raw_socket(peer, Role::Server, None).await;
                let mut socket =
                    WebSocketStream::from_raw_socket(transport, Role::Client, None).await;
                let mut session = session();
                let mut pending = PendingWrite {
                    write: session.next_write(0).unwrap().unwrap(),
                    started: false,
                };
                assert!(
                    pending
                        .poll(&mut socket, &mut Context::from_waker(Waker::noop()), false)
                        .is_pending()
                );
                let remote = async move {
                    peer.close(None).await.unwrap();
                    assert!(matches!(peer.next().await, Some(Ok(Message::Text(_)))));
                    assert!(matches!(peer.next().await, Some(Ok(Message::Close(_)))));
                };
                let local = async {
                    assert!(matches!(socket.next().await, Some(Ok(Message::Close(_)))));
                    assert!(
                        pending
                            .poll(&mut socket, &mut Context::from_waker(Waker::noop()), true)
                            .is_ready()
                    );
                    assert!(
                        session
                            .complete_write(pending.write.id, WriteResult::Uncertain, 1)
                            .is_err()
                    );
                    assert_eq!(session.phase(), SessionPhase::Closed);
                    // The original issued bytes drain on this socket, followed by the queued reply.
                    flush_close_reply(&mut socket, Instant::now() + Duration::from_secs(1))
                        .await
                        .unwrap();
                    assert!(
                        session
                            .complete_write(pending.write.id, WriteResult::Complete, 2)
                            .unwrap()
                            .is_empty()
                    );
                    assert_eq!(session.phase(), SessionPhase::Closed);
                    drop(socket);
                };
                tokio::time::timeout(
                    Duration::from_secs(2),
                    futures_util::future::join(local, remote),
                )
                .await
                .unwrap();
            });
    }
    #[test]
    fn a_blocked_close_reply_expires_without_acknowledging_its_pending_write() {
        use futures_util::StreamExt;
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let (transport, peer) = tokio::io::duplex(1);
                let mut socket =
                    WebSocketStream::from_raw_socket(transport, Role::Client, None).await;
                let mut peer = WebSocketStream::from_raw_socket(peer, Role::Server, None).await;
                let mut session = session();
                let mut pending = PendingWrite {
                    write: session.next_write(0).unwrap().unwrap(),
                    started: false,
                };
                assert!(
                    pending
                        .poll(&mut socket, &mut Context::from_waker(Waker::noop()), false)
                        .is_pending()
                );
                let (release, gate) = tokio::sync::oneshot::channel();
                let remote = async move {
                    peer.close(None).await.unwrap();
                    gate.await.unwrap();
                };
                let local = async {
                    assert!(matches!(socket.next().await, Some(Ok(Message::Close(_)))));
                    assert!(
                        session
                            .complete_write(pending.write.id, WriteResult::Uncertain, 1)
                            .is_err()
                    );
                    assert_eq!(
                        flush_close_reply(&mut socket, Instant::now() + Duration::from_millis(20))
                            .await,
                        Err(DriverError::Deadline)
                    );
                    assert_eq!(session.phase(), SessionPhase::Closed);
                    assert!(
                        session
                            .complete_write(pending.write.id, WriteResult::Complete, 2)
                            .unwrap()
                            .is_empty()
                    );
                    release.send(()).unwrap();
                    drop(socket);
                };
                tokio::time::timeout(
                    Duration::from_secs(2),
                    futures_util::future::join(local, remote),
                )
                .await
                .unwrap();
            });
    }
}

use super::{
    ClockConfiguration, ClockObservation, DriverError, DriverLimits, Operation,
    SendspinConnectPermit, SendspinServices, clock::ClockDriver,
};
use earph_sendspin::{
    pairing::{AttemptId, Candidate, CommitOutcome},
    session::{
        AdmissionId, ApplicationMessage, ClientSession, SessionConfig, SessionError, SessionEvent,
        SessionPhase, WriteResult,
    },
};
use futures_util::Stream;
use std::{
    collections::VecDeque,
    future::{Future, poll_fn},
    net::SocketAddr,
    sync::{Arc, Mutex, MutexGuard, mpsc},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tokio::net::TcpStream;
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RetirementState {
    Starting,
    Connected,
    Closing,
    AwaitCommit,
    Exited,
    Joined,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceKind {
    Admission,
    Candidate,
    Commit,
}
#[derive(Debug, Clone)]
pub struct Observation {
    pub connection_generation: u64,
    pub phase: SessionPhase,
    pub handshake_generation: u64,
    /// Unavailable: the reducer owns this deadline and exposes no getter.
    pub session_deadline_ms: Option<u64>,
    pub transport_deadline_ms: Option<u64>,
    pub service_deadline_ms: Option<u64>,
    pub retirement: RetirementState,
    pub terminal: Option<DriverError>,
    pub joined: bool,
    pub command_entries: usize,
    pub producer_bytes: usize,
    pub producer_high_water: usize,
    pub event_entries: usize,
    pub event_bytes: usize,
    pub event_high_water: usize,
    pub pending_write: bool,
    pub pending_admission: bool,
    pub pending_candidate: bool,
    pub pending_commit: bool,
    pub writes_completed: u64,
    pub recovery_required: bool,
    pub received_messages: u64,
    pub last_arrival: Option<ReceiveStamp>,
    pub clock: Option<ClockObservation>,
}
struct State {
    observation: Observation,
    commands: VecDeque<Command>,
    events: VecDeque<SessionEvent>,
    cancelled: bool,
}
impl State {
    fn discard_commands(&mut self) {
        let bytes = self
            .commands
            .iter()
            .map(|command| match command {
                Command::Send(message) => message.body.capacity(),
                Command::CancelPairing => 0,
            })
            .sum::<usize>();
        self.observation.producer_bytes -= bytes;
        self.observation.command_entries -= self.commands.len();
        self.commands.clear();
    }
}
struct ProducerAdmission<'a> {
    shared: &'a Shared,
    bytes: usize,
    queued: bool,
}
impl Drop for ProducerAdmission<'_> {
    fn drop(&mut self) {
        if !self.queued {
            let mut state = self.shared.lock();
            state.observation.producer_bytes -= self.bytes;
            state.observation.command_entries -= 1;
        }
    }
}
struct Shared {
    state: Mutex<State>,
    limits: DriverLimits,
}
impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }
    fn cancel(&self) {
        self.lock().cancelled = true;
    }
    fn terminal(&self, error: DriverError) {
        let mut s = self.lock();
        s.observation.terminal.get_or_insert(error);
    }
    fn admit_producer(&self, bytes: usize) -> Result<ProducerAdmission<'_>, DriverError> {
        let mut state = self.lock();
        if state.cancelled || state.observation.terminal.is_some() {
            return Err(DriverError::Cancelled);
        }
        let total = state
            .observation
            .producer_bytes
            .checked_add(bytes)
            .ok_or(DriverError::Budget)?;
        if state.observation.command_entries >= self.limits.commands
            || total > self.limits.producer_bytes
        {
            return Err(DriverError::Budget);
        }
        state.observation.producer_bytes = total;
        state.observation.command_entries += 1;
        state.observation.producer_high_water = state.observation.producer_high_water.max(total);
        Ok(ProducerAdmission {
            shared: self,
            bytes,
            queued: false,
        })
    }
    fn pop_command(&self) -> Option<(Command, ProducerAdmission<'_>)> {
        let command = self.lock().commands.pop_front()?;
        let bytes = match &command {
            Command::Send(message) => message.body.capacity(),
            Command::CancelPairing => 0,
        };
        Some((
            command,
            ProducerAdmission {
                shared: self,
                bytes,
                queued: false,
            },
        ))
    }
    fn arrival(&self, arrival: ReceiveStamp) -> Result<(), DriverError> {
        let mut s = self.lock();
        s.observation.received_messages = s
            .observation
            .received_messages
            .checked_add(1)
            .ok_or(DriverError::Budget)?;
        s.observation.last_arrival = Some(arrival);
        Ok(())
    }
    fn publish(&self, event: SessionEvent) -> Result<(), DriverError> {
        let bytes = event_bytes(&event);
        let mut s = self.lock();
        let n = s
            .observation
            .event_bytes
            .checked_add(bytes)
            .ok_or(DriverError::Budget)?;
        if s.observation.event_entries >= self.limits.events || n > self.limits.event_bytes {
            return Err(DriverError::Budget);
        }
        s.observation.event_bytes = n;
        s.observation.event_entries += 1;
        s.observation.event_high_water = s.observation.event_high_water.max(n);
        s.events.push_back(event);
        Ok(())
    }
}
fn event_bytes(e: &SessionEvent) -> usize {
    match e {
        SessionEvent::Unsent(m) => m.body.capacity(),
        SessionEvent::Message(m) => m.body.capacity(),
        SessionEvent::Activated(n) => n
            .roles
            .iter()
            .chain(&n.removed_roles)
            .chain(&n.activities)
            .map(String::capacity)
            .sum(),
        _ => 0,
    }
}
/// Payload credits remain charged until this lease drops, including while held by a consumer.
pub struct EventLease {
    event: SessionEvent,
    shared: Arc<Shared>,
    bytes: usize,
}
impl EventLease {
    pub fn event(&self) -> &SessionEvent {
        &self.event
    }
}
impl std::fmt::Debug for EventLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EventLease([REDACTED])")
    }
}
impl Drop for EventLease {
    fn drop(&mut self) {
        let mut s = self.shared.lock();
        s.observation.event_entries -= 1;
        s.observation.event_bytes -= self.bytes;
    }
}
#[derive(Debug)]
pub struct ProducerRejected {
    pub error: DriverError,
    pub message: ApplicationMessage,
}
enum Command {
    Send(ApplicationMessage),
    CancelPairing,
}
struct Job {
    shared: Arc<Shared>,
    thread: JoinHandle<()>,
    done: mpsc::Receiver<()>,
}
#[derive(Default)]
pub struct SendspinWorker {
    active: Option<Job>,
    recovery_required: bool,
}
pub struct SendspinHandle {
    shared: Arc<Shared>,
}
#[derive(Clone)]
pub struct SendspinObserver {
    shared: Arc<Shared>,
}
impl SendspinWorker {
    pub fn start(
        &mut self,
        permit: SendspinConnectPermit,
        current: &SendspinConnectPermit,
        mut config: SessionConfig,
        clock: ClockConfiguration,
        services: Box<dyn SendspinServices>,
        limits: DriverLimits,
    ) -> Result<SendspinHandle, DriverError> {
        if self.active.is_some() {
            return Err(DriverError::Busy);
        }
        if self.recovery_required {
            return Err(DriverError::StorageUncertain);
        }
        if &permit != current || config.connection_generation != permit.connection {
            return Err(DriverError::Admission);
        }
        if permit.expires <= Instant::now() {
            return Err(DriverError::Expired);
        }
        limits.validate(config.limits)?;
        if config
            .hello
            .get("supported_roles")
            .and_then(serde_json::Value::as_array)
            .is_none_or(|r| !r.is_empty())
            || config
                .hello
                .as_object()
                .is_some_and(|hello| hello.keys().any(|key| key.ends_with("_support")))
        {
            return Err(DriverError::Admission);
        }
        if config.catalog.capacity() != 5
            || config.catalog.records().len() > 5
            || config.catalog.superseded_ids().len() > 5
            || config.protected.len() > 5
        {
            return Err(DriverError::Budget);
        }
        config.catalog = config.catalog.clone();
        config.protected = config.protected.into_boxed_slice().into_vec();
        let session =
            ClientSession::new(config, clock.domain.milliseconds()).map_err(session_error)?;
        let shared = Arc::new(Shared {
            limits,
            state: Mutex::new(State {
                commands: VecDeque::new(),
                events: VecDeque::new(),
                cancelled: false,
                observation: Observation {
                    connection_generation: permit.connection,
                    phase: session.phase(),
                    handshake_generation: session.handshake_generation(),
                    session_deadline_ms: None,
                    transport_deadline_ms: None,
                    service_deadline_ms: None,
                    retirement: RetirementState::Starting,
                    terminal: None,
                    joined: false,
                    command_entries: 0,
                    producer_bytes: 0,
                    producer_high_water: 0,
                    event_entries: 0,
                    event_bytes: 0,
                    event_high_water: 0,
                    pending_write: false,
                    pending_admission: false,
                    pending_candidate: false,
                    pending_commit: false,
                    writes_completed: 0,
                    recovery_required: false,
                    received_messages: 0,
                    last_arrival: None,
                    clock: None,
                },
            }),
        });
        let owned = shared.clone();
        let (done_tx, done) = mpsc::channel();
        let thread = thread::Builder::new()
            .name("earph-sendspin-network".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let runtime = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                        .map_err(|_| DriverError::Internal)?;
                    runtime.block_on(run(
                        permit,
                        session,
                        ClockDriver::new(clock),
                        services,
                        owned.clone(),
                    ))
                }));
                if !matches!(result, Ok(Ok(()))) {
                    owned.terminal(match result {
                        Ok(Err(e)) => e,
                        _ => DriverError::Internal,
                    });
                }
                let mut s = owned.lock();
                if s.observation.pending_commit {
                    s.observation.recovery_required = true;
                }
                s.observation.retirement = RetirementState::Exited;
                s.observation.phase = SessionPhase::Closed;
                s.discard_commands();
                drop(s);
                let _ = done_tx.send(());
            })
            .map_err(|_| DriverError::Internal)?;
        self.active = Some(Job {
            shared: shared.clone(),
            thread,
            done,
        });
        Ok(SendspinHandle { shared })
    }
    /// One bounded wait; an incomplete owner stays Busy until a later successful join.
    pub fn retire(&mut self) -> Result<(), DriverError> {
        let Some(job) = &self.active else {
            return Ok(());
        };
        job.shared.cancel();
        match job
            .done
            .recv_timeout(Duration::from_millis(job.shared.limits.retire_ms))
        {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => return Err(DriverError::CleanupTimeout),
        }
        let job = self.active.take().expect("owned job");
        job.thread.join().map_err(|_| DriverError::Internal)?;
        let mut s = job.shared.lock();
        s.observation.joined = true;
        s.observation.retirement = RetirementState::Joined;
        self.recovery_required |= s.observation.recovery_required;
        if s.observation.pending_commit {
            return Err(DriverError::StorageUncertain);
        }
        Ok(())
    }
    /// Call only after fresh caller-owned storage recovery resolves the previous uncertain document.
    pub fn acknowledge_recovery(&mut self) -> Result<(), DriverError> {
        if self.active.is_some() {
            return Err(DriverError::Busy);
        }
        self.recovery_required = false;
        Ok(())
    }
}
impl Drop for SendspinWorker {
    fn drop(&mut self) {
        if let Some(job) = self.active.take() {
            job.shared.cancel();
            let _ = job.thread.join();
            let mut s = job.shared.lock();
            s.observation.joined = true;
            s.observation.retirement = RetirementState::Joined;
        }
    }
}
impl SendspinHandle {
    pub fn observer(&self) -> SendspinObserver {
        SendspinObserver {
            shared: self.shared.clone(),
        }
    }
    pub fn cancel(&self) {
        self.shared.cancel();
    }
    /// Only caller-generated core JSON traffic is admitted by this roleless unit.
    pub fn try_send(&self, message: ApplicationMessage) -> Result<(), ProducerRejected> {
        let mut admission = match self.shared.admit_producer(message.body.capacity()) {
            Ok(admission) => admission,
            Err(error) => return Err(ProducerRejected { error, message }),
        };
        // Reservations bound all concurrent input bodies and their transient decoded payload copies.
        // Parsing owns no state lock, so cancellation and commit completion remain independent.
        let valid = message.id == 0
            && earph_sendspin::message::Envelope::parse(&message.body)
                .is_ok_and(|e| matches!(e.kind.as_str(), "client/goodbye"));
        let mut s = self.shared.lock();
        let error = if s.cancelled || s.observation.terminal.is_some() {
            Some(DriverError::Cancelled)
        } else if !valid {
            Some(DriverError::Protocol)
        } else {
            None
        };
        if let Some(error) = error {
            return Err(ProducerRejected { error, message });
        }
        s.commands.push_back(Command::Send(message));
        admission.queued = true;
        Ok(())
    }
    pub fn cancel_pairing(&self) -> Result<(), DriverError> {
        let mut s = self.shared.lock();
        if s.cancelled || s.observation.terminal.is_some() {
            return Err(DriverError::Cancelled);
        }
        if s.observation.command_entries >= self.shared.limits.commands {
            return Err(DriverError::Budget);
        }
        s.commands.push_back(Command::CancelPairing);
        s.observation.command_entries += 1;
        Ok(())
    }
}
impl Drop for SendspinHandle {
    fn drop(&mut self) {
        self.cancel();
    }
}
impl SendspinObserver {
    pub fn observation(&self) -> Observation {
        self.shared.lock().observation.clone()
    }
    pub fn next_event(&self) -> Option<EventLease> {
        let event = self.shared.lock().events.pop_front()?;
        let bytes = event_bytes(&event);
        Some(EventLease {
            event,
            shared: self.shared.clone(),
            bytes,
        })
    }
}
fn session_error(e: SessionError) -> DriverError {
    match e {
        SessionError::Authentication => DriverError::Authentication,
        SessionError::Budget | SessionError::Counter => DriverError::Budget,
        SessionError::Deadline => DriverError::Deadline,
        SessionError::Storage => DriverError::Service,
        _ => DriverError::Protocol,
    }
}
fn websocket_error(e: Error) -> DriverError {
    match e {
        Error::Capacity(_) | Error::WriteBufferFull(_) => DriverError::Budget,
        _ => DriverError::Protocol,
    }
}
fn millis(start: Instant, deadline: Instant) -> u64 {
    u64::try_from(deadline.saturating_duration_since(start).as_millis()).unwrap_or(u64::MAX)
}
fn control(shared: &Shared, expires: Instant) -> Result<(), DriverError> {
    if shared.lock().cancelled {
        return Err(DriverError::Cancelled);
    }
    if Instant::now() >= expires {
        return Err(DriverError::Expired);
    }
    Ok(())
}
async fn controlled<F: Future>(
    future: F,
    deadline: Instant,
    expires: Instant,
    shared: &Shared,
) -> Result<F::Output, DriverError> {
    let mut future = std::pin::pin!(future);
    loop {
        control(shared, expires)?;
        if Instant::now() >= deadline {
            return Err(DriverError::Deadline);
        }
        let timer = tokio::time::sleep(Duration::from_millis(5));
        let mut timer = std::pin::pin!(timer);
        if let Some(result) = poll_fn(|cx| {
            if let Poll::Ready(v) = future.as_mut().poll(cx) {
                return Poll::Ready(Some(v));
            }
            if timer.as_mut().poll(cx).is_ready() {
                return Poll::Ready(None);
            }
            Poll::Pending
        })
        .await
        {
            return Ok(result);
        }
    }
}
fn websocket_config() -> tokio_tungstenite::tungstenite::protocol::WebSocketConfig {
    tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
        .read_buffer_size(4096)
        .write_buffer_size(0)
        .max_write_buffer_size(131072)
        .max_message_size(Some(100000))
        .max_frame_size(Some(100000))
}
async fn flush_close_reply<S: AsyncRead + AsyncWrite + Unpin>(
    socket: &mut WebSocketStream<S>,
    deadline: Instant,
) -> Result<(), DriverError> {
    tokio::time::timeout_at(
        deadline.into(),
        poll_fn(|cx| Pin::new(&mut *socket).poll_flush(cx)),
    )
    .await
    .map_err(|_| DriverError::Deadline)?
    .map_err(websocket_error)
}
struct Service<T, I> {
    id: I,
    future: Operation<T>,
    deadline: Instant,
}
struct Pump {
    clock: ClockDriver,
    session: ClientSession,
    services: Box<dyn SendspinServices>,
    shared: Arc<Shared>,
    start: Instant,
    admission: Option<Service<Result<bool, DriverError>, AdmissionId>>,
    candidate: Option<Service<Result<Candidate, DriverError>, AttemptId>>,
    commit: Option<Service<Result<(), CommitOutcome>, AttemptId>>,
}
impl Pump {
    fn events(&mut self, events: Vec<SessionEvent>) -> Result<(), DriverError> {
        // Acknowledged storage takes ownership before ordinary event admission can fail.
        let mut ordinary = Vec::new();
        for event in events {
            match event {
                SessionEvent::CommitRecord(id, update) => {
                    if self.commit.is_some() {
                        return Err(DriverError::Service);
                    }
                    if update.replacement.capacity() != 5
                        || update.replacement.records().len() > 5
                        || update.replacement.superseded_ids().len() > 5
                    {
                        return Err(DriverError::Budget);
                    }
                    self.shared.lock().observation.pending_commit = true;
                    let future = self.services.commit(id, update);
                    self.commit = Some(Service {
                        id,
                        future,
                        deadline: Instant::now()
                            + Duration::from_millis(self.shared.limits.service_ms),
                    });
                }
                e => ordinary.push(e),
            }
        }
        let context = self.session.clock_context();
        let mut visible = Vec::new();
        for event in ordinary {
            if let SessionEvent::Message(message) = &event
                && self.clock.consume(context, message)?
            {
                continue;
            }
            visible.push(event);
        }
        self.sync_clock()?;
        for event in visible {
            match event {
                SessionEvent::Admission(proposal) => {
                    if !proposal.roles().is_empty()
                        || proposal.activities().iter().any(|a| a != "pairing")
                    {
                        return Err(DriverError::Admission);
                    }
                    if self.admission.is_some() {
                        return Err(DriverError::Service);
                    }
                    let id = proposal.id();
                    let future = self.services.admission(proposal);
                    self.admission = Some(Service {
                        id,
                        future,
                        deadline: Instant::now()
                            + Duration::from_millis(self.shared.limits.service_ms),
                    });
                }
                SessionEvent::RequestCandidate(id) => {
                    if self.candidate.is_some() {
                        return Err(DriverError::Service);
                    }
                    let future = self.services.candidate(id);
                    self.candidate = Some(Service {
                        id,
                        future,
                        deadline: Instant::now()
                            + Duration::from_millis(self.shared.limits.service_ms),
                    });
                }
                SessionEvent::Closed => return Err(DriverError::Protocol),
                event => self.shared.publish(event)?,
            }
        }
        Ok(())
    }
    fn sync_clock(&mut self) -> Result<(), DriverError> {
        let now = self.clock.domain.sample()?;
        let observation = self.clock.advance(self.session.clock_context(), now)?;
        self.shared.lock().observation.clock = observation;
        Ok(())
    }
    fn update(&self, pending: bool, transport_deadline: Option<Instant>) {
        let mut s = self.shared.lock();
        let o = &mut s.observation;
        o.phase = self.session.phase();
        o.handshake_generation = self.session.handshake_generation();
        o.pending_write = pending;
        o.pending_admission = self.admission.is_some();
        o.pending_candidate = self.candidate.is_some();
        o.pending_commit = self.commit.is_some();
        o.transport_deadline_ms = transport_deadline.map(|d| millis(self.start, d));
        o.service_deadline_ms = self
            .admission
            .as_ref()
            .map(|s| s.deadline)
            .into_iter()
            .chain(self.candidate.as_ref().map(|s| s.deadline))
            .chain(self.commit.as_ref().map(|s| s.deadline))
            .min()
            .map(|d| millis(self.start, d));
    }
}
enum Step {
    Tick,
    Read(
        Option<Result<Message, Error>>,
        Result<ReceiveStamp, DriverError>,
    ),
    ClockWrite(Result<PendingWrite, DriverError>),
    Written(Result<(), Error>),
    Admission(AdmissionId, Result<bool, DriverError>),
    Candidate(AttemptId, Result<Candidate, DriverError>),
    Commit(AttemptId, Result<(), CommitOutcome>),
}
async fn run(
    permit: SendspinConnectPermit,
    session: ClientSession,
    clock: ClockDriver,
    services: Box<dyn SendspinServices>,
    shared: Arc<Shared>,
) -> Result<(), DriverError> {
    let start = Instant::now();
    let l = shared.limits;
    let connect_deadline = start + Duration::from_millis(l.connect_ms);
    let mut stream = None;
    shared.lock().observation.transport_deadline_ms = Some(millis(start, connect_deadline));
    for address in &permit.pins {
        control(&shared, permit.expires)?;
        let result = controlled(
            TcpStream::connect(SocketAddr::new(*address, permit.port)),
            connect_deadline,
            permit.expires,
            &shared,
        )
        .await?;
        if let Ok(s) = result {
            stream = Some(s);
            break;
        }
    }
    let stream = stream.ok_or(DriverError::Connect)?;
    let upgrade_deadline = Instant::now() + Duration::from_millis(l.upgrade_ms);
    shared.lock().observation.transport_deadline_ms = Some(millis(start, upgrade_deadline));
    let (socket, _) = controlled(
        tokio_tungstenite::client_async_with_config(
            permit.endpoint.as_str(),
            stream,
            Some(websocket_config()),
        ),
        upgrade_deadline,
        permit.expires,
        &shared,
    )
    .await?
    .map_err(|_| DriverError::Upgrade)?;
    control(&shared, permit.expires)?;
    let mut socket = socket;
    shared.lock().observation.retirement = RetirementState::Connected;
    let mut p = Pump {
        clock,
        session,
        services,
        shared: shared.clone(),
        start,
        admission: None,
        candidate: None,
        commit: None,
    };
    let mut pending: Option<(PendingWrite, Instant)> = None;
    let mut ping_due = Instant::now() + Duration::from_millis(l.ping_ms);
    let mut pong_due = None;
    let mut control_flush = false;
    let mut terminal = loop {
        if let Err(e) = control(&shared, permit.expires) {
            break e;
        }
        let now = Instant::now();
        if pending.as_ref().is_some_and(|(_, d)| now >= *d)
            || p.admission.as_ref().is_some_and(|s| now >= s.deadline)
            || p.candidate.as_ref().is_some_and(|s| now >= s.deadline)
            || p.commit.as_ref().is_some_and(|s| now >= s.deadline)
            || pong_due.is_some_and(|d| now >= d)
        {
            break DriverError::Deadline;
        }
        if let Err(error) = p
            .session
            .tick(p.clock.domain.milliseconds())
            .map_err(session_error)
            .and_then(|events| p.events(events))
        {
            break error;
        }
        if pending.is_none() {
            match p.session.next_write(p.clock.domain.milliseconds()) {
                Ok(Some(write)) => {
                    pending = Some((
                        PendingWrite {
                            write,
                            started: false,
                        },
                        Instant::now() + Duration::from_millis(l.write_ms),
                    ))
                }
                Ok(None) => {}
                Err(e) => break session_error(e),
            }
        }
        let transport_deadline = pending
            .as_ref()
            .map(|(_, d)| *d)
            .into_iter()
            .chain(pong_due)
            .chain(Some(permit.expires))
            .min();
        p.update(pending.is_some(), transport_deadline);
        if let Some((c, credit)) = shared.pop_command() {
            // Session validation shares the same input and parse-copy allowance as pending enqueues.
            let result = match c {
                Command::Send(m) => match p.session.send(m, p.clock.domain.milliseconds()) {
                    Ok(()) => Ok(()),
                    Err(e) => shared.publish(SessionEvent::Unsent(e.message)),
                },
                Command::CancelPairing => p
                    .session
                    .cancel_pairing(p.clock.domain.milliseconds())
                    .map_err(session_error)
                    .and_then(|e| p.events(e)),
            };
            drop(credit);
            if let Err(e) = result {
                break e;
            }
        }
        if let Err(error) = p.sync_clock() {
            break error;
        }
        let timer = tokio::time::sleep(Duration::from_millis(5));
        let mut timer = std::pin::pin!(timer);
        let step = poll_fn(|cx| {
            if let Some(s) = &mut p.commit
                && let Poll::Ready(v) = s.future.as_mut().poll(cx)
            {
                return Poll::Ready(Step::Commit(s.id, v));
            }
            if let Some(s) = &mut p.admission
                && let Poll::Ready(v) = s.future.as_mut().poll(cx)
            {
                return Poll::Ready(Step::Admission(s.id, v));
            }
            if let Some(s) = &mut p.candidate
                && let Poll::Ready(v) = s.future.as_mut().poll(cx)
            {
                return Poll::Ready(Step::Candidate(s.id, v));
            }
            // Observe closure before accepting this adapter's ConnectionClosed-to-Ok flush mapping.
            if let Poll::Ready(v) = Pin::new(&mut socket).poll_next(cx) {
                return Poll::Ready(Step::Read(v, p.clock.domain.sample()));
            }
            if let Some((w, _)) = &mut pending {
                if let Poll::Ready(v) = w.poll(&mut socket, cx, false) {
                    return Poll::Ready(Step::Written(v));
                }
            } else {
                match poll_clock_write(&mut socket, cx, &mut p.session, &mut p.clock) {
                    Poll::Ready(Ok(Some(write))) => {
                        return Poll::Ready(Step::ClockWrite(Ok(write)));
                    }
                    Poll::Ready(Err(error)) => return Poll::Ready(Step::ClockWrite(Err(error))),
                    _ => {}
                }
                if !control_flush && pong_due.is_none() && Instant::now() >= ping_due {
                    match Pin::new(&mut socket).poll_ready(cx) {
                        Poll::Ready(Ok(())) => {
                            if let Err(e) = Pin::new(&mut socket)
                                .start_send(Message::Ping(b"earph".to_vec().into()))
                            {
                                return Poll::Ready(Step::Written(Err(e)));
                            }
                            control_flush = true;
                            pong_due = Some(Instant::now() + Duration::from_millis(l.pong_ms));
                            ping_due = Instant::now() + Duration::from_millis(l.ping_ms);
                        }
                        Poll::Ready(Err(e)) => return Poll::Ready(Step::Written(Err(e))),
                        Poll::Pending => {}
                    }
                }
                if control_flush && let Poll::Ready(v) = Pin::new(&mut socket).poll_flush(cx) {
                    control_flush = false;
                    if let Err(e) = v {
                        return Poll::Ready(Step::Written(Err(e)));
                    }
                }
            }
            if timer.as_mut().poll(cx).is_ready() {
                return Poll::Ready(Step::Tick);
            }
            Poll::Pending
        })
        .await;
        let result = match step {
            Step::Tick => Ok(()),
            Step::ClockWrite(result) => match result {
                Ok(write) => {
                    pending = Some((write, Instant::now() + Duration::from_millis(l.write_ms)));
                    Ok(())
                }
                Err(error) => Err(error),
            },
            Step::Written(result) => match result {
                Ok(()) => {
                    if let Some((w, _)) = pending.take() {
                        let next_count =
                            { shared.lock().observation.writes_completed.checked_add(1) };
                        if let Some(count) = next_count {
                            let events = p
                                .session
                                .complete_write(
                                    w.write.id,
                                    WriteResult::Complete,
                                    p.clock.domain.milliseconds(),
                                )
                                .map_err(session_error);
                            shared.lock().observation.writes_completed = count;
                            events.and_then(|e| p.events(e))
                        } else {
                            let _ = p.session.complete_write(
                                w.write.id,
                                WriteResult::Uncertain,
                                p.clock.domain.milliseconds(),
                            );
                            Err(DriverError::Budget)
                        }
                    } else {
                        Ok(())
                    }
                }
                Err(e) => Err(websocket_error(e)),
            },
            Step::Read(Some(Ok(message)), arrival) => arrival.and_then(|stamp| {
                let recorded = if matches!(&message, Message::Text(_) | Message::Binary(_)) {
                    shared.arrival(stamp)
                } else {
                    Ok(())
                };
                recorded.and_then(|_| match message {
                    Message::Text(text) => p
                        .session
                        .receive(TransportKind::Text, text.as_bytes(), stamp)
                        .map_err(session_error)
                        .and_then(|e| p.events(e)),
                    Message::Binary(bytes) => {
                        if bytes.len() > 65535 {
                            Err(DriverError::Budget)
                        } else {
                            p.session
                                .receive(TransportKind::Binary, &bytes, stamp)
                                .map_err(session_error)
                                .and_then(|e| p.events(e))
                        }
                    }
                    Message::Ping(_) => {
                        control_flush = true;
                        Ok(())
                    }
                    Message::Pong(bytes) => {
                        if bytes.as_ref() == b"earph" {
                            pong_due = None;
                        }
                        Ok(())
                    }
                    Message::Close(_) => Err(DriverError::PeerClosed),
                    Message::Frame(_) => Err(DriverError::Protocol),
                })
            }),
            Step::Read(Some(Err(e)), _) => Err(websocket_error(e)),
            Step::Read(None, _) => Err(DriverError::Protocol),
            Step::Admission(id, result) => {
                p.admission = None;
                result
                    .and_then(|accepted| {
                        p.session
                            .admit(id, accepted, p.clock.domain.milliseconds())
                            .map_err(session_error)
                    })
                    .and_then(|e| p.events(e))
            }
            Step::Candidate(id, result) => {
                p.candidate = None;
                result
                    .and_then(|candidate| {
                        p.session
                            .candidate(id, candidate, p.clock.domain.milliseconds())
                            .map_err(session_error)
                    })
                    .and_then(|e| p.events(e))
            }
            Step::Commit(id, result) => {
                p.commit = None;
                let events = match result {
                    Ok(()) => p
                        .session
                        .commit_succeeded(id, p.clock.domain.milliseconds()),
                    Err(outcome) => {
                        if outcome == CommitOutcome::Uncertain {
                            shared.lock().observation.recovery_required = true;
                        }
                        p.session
                            .commit_failed(id, outcome, p.clock.domain.milliseconds())
                    }
                };
                events.map_err(session_error).and_then(|e| p.events(e))
            }
        };
        if let Err(e) = result {
            break e;
        }
    };
    if terminal == DriverError::PeerClosed {
        shared.cancel();
    } else {
        shared.terminal(terminal);
    }
    let uncertain = pending.is_some();
    if let Some((w, _)) = pending.take() {
        let _ = p.session.complete_write(
            w.write.id,
            WriteResult::Uncertain,
            p.clock.domain.milliseconds(),
        );
    }
    if let Ok(now) = p.clock.domain.sample()
        && let Ok(observation) = p.clock.retire(now)
    {
        shared.lock().observation.clock = observation;
    }
    p.session.close();
    p.admission = None;
    p.candidate = None;
    shared.lock().observation.retirement = if p.commit.is_some() {
        RetirementState::AwaitCommit
    } else {
        RetirementState::Closing
    };
    p.update(false, None);
    if terminal == DriverError::PeerClosed {
        let deadline = Instant::now() + Duration::from_millis(l.close_ms);
        shared.lock().observation.transport_deadline_ms = Some(millis(start, deadline));
        // This only drains existing frames and the queued Close. It creates no session receipt.
        if let Err(error) = flush_close_reply(&mut socket, deadline).await {
            terminal = error;
        }
        shared.terminal(terminal);
    } else if !uncertain && matches!(terminal, DriverError::Cancelled | DriverError::Expired) {
        let deadline = Instant::now() + Duration::from_millis(l.close_ms);
        shared.lock().observation.transport_deadline_ms = Some(millis(start, deadline));
        let _ = tokio::time::timeout_at(deadline.into(), socket.close(None)).await;
    }
    // Drop the sole socket before waiting for acknowledged durable work. No reconnect or retransmit exists.
    drop(socket);
    if let Some(mut commit) = p.commit.take() {
        // The service deadline closes transport, but cannot cancel or retry an acknowledged storage operation.
        let result = commit.future.as_mut().await;
        let callback = match result {
            Ok(()) => p
                .session
                .commit_succeeded(commit.id, p.clock.domain.milliseconds()),
            Err(outcome) => {
                if outcome == CommitOutcome::Uncertain {
                    shared.lock().observation.recovery_required = true;
                }
                p.session
                    .commit_failed(commit.id, outcome, p.clock.domain.milliseconds())
            }
        };
        let _ = callback;
    }
    p.update(false, None);
    Ok(())
}

#[cfg(test)]
mod resource_tests {
    use super::*;
    fn shared() -> Arc<Shared> {
        Arc::new(Shared {
            limits: DriverLimits {
                events: 1,
                event_bytes: 4,
                ..DriverLimits::default()
            },
            state: Mutex::new(State {
                commands: VecDeque::new(),
                events: VecDeque::new(),
                cancelled: false,
                observation: Observation {
                    connection_generation: 1,
                    phase: SessionPhase::Active,
                    handshake_generation: 1,
                    session_deadline_ms: None,
                    transport_deadline_ms: None,
                    service_deadline_ms: None,
                    retirement: RetirementState::Connected,
                    terminal: None,
                    joined: false,
                    command_entries: 0,
                    producer_bytes: 0,
                    producer_high_water: 0,
                    event_entries: 0,
                    event_bytes: 0,
                    event_high_water: 0,
                    pending_write: false,
                    pending_admission: false,
                    pending_candidate: false,
                    pending_commit: false,
                    writes_completed: 0,
                    recovery_required: false,
                    received_messages: 0,
                    last_arrival: None,
                    clock: None,
                },
            }),
        })
    }
    fn event() -> SessionEvent {
        SessionEvent::Unsent(ApplicationMessage {
            id: 0,
            body: vec![42; 4],
        })
    }
    #[test]
    fn producer_admission_rejects_small_oversized_malformed_and_blocked_input_first() {
        let mut owned = shared();
        Arc::get_mut(&mut owned).unwrap().limits.producer_bytes = 32;
        let handle = SendspinHandle { shared: owned };
        for body in [
            br#"{"type":"client/goodbye","payload":{"client_transmitted":1}}"#.to_vec(),
            vec![b'{'; 64],
        ] {
            let original = body.clone();
            let rejected = handle
                .try_send(ApplicationMessage { id: 0, body })
                .unwrap_err();
            assert_eq!(rejected.error, DriverError::Budget);
            assert_eq!(rejected.message.body, original);
        }
        let mut owned = shared();
        Arc::get_mut(&mut owned).unwrap().limits.commands = 1;
        let handle = SendspinHandle { shared: owned };
        handle
            .try_send(ApplicationMessage::json(
                "client/goodbye",
                serde_json::json!({"client_transmitted":1}),
            ))
            .unwrap();
        let rejected = handle
            .try_send(ApplicationMessage {
                id: 0,
                body: vec![b'{'],
            })
            .unwrap_err();
        assert_eq!(rejected.error, DriverError::Budget);
        assert_eq!(rejected.message.body, b"{");
        handle.cancel();
        let rejected = handle
            .try_send(ApplicationMessage {
                id: 0,
                body: vec![b'{'],
            })
            .unwrap_err();
        assert_eq!(rejected.error, DriverError::Cancelled);
        assert_eq!(rejected.message.body, b"{");
    }
    #[test]
    fn malformed_admitted_input_returns_its_credit_before_the_next_producer() {
        let handle = SendspinHandle { shared: shared() };
        assert_eq!(
            handle
                .try_send(ApplicationMessage {
                    id: 0,
                    body: vec![b'{']
                })
                .unwrap_err()
                .error,
            DriverError::Protocol
        );
        assert_eq!(handle.observer().observation().producer_bytes, 0);
        assert_eq!(handle.observer().observation().command_entries, 0);
        handle
            .try_send(ApplicationMessage::json(
                "client/goodbye",
                serde_json::json!({"client_transmitted":1}),
            ))
            .unwrap();
        assert_eq!(handle.observer().observation().command_entries, 1);
    }
    #[test]
    fn pending_producer_credit_survives_queue_cleanup_and_allows_cancellation() {
        let handle = SendspinHandle { shared: shared() };
        let admission = handle.shared.admit_producer(8).unwrap();
        handle
            .try_send(ApplicationMessage::json(
                "client/goodbye",
                serde_json::json!({"client_transmitted":1}),
            ))
            .unwrap();
        handle.shared.lock().discard_commands();
        assert_eq!(handle.observer().observation().producer_bytes, 8);
        assert_eq!(handle.observer().observation().command_entries, 1);
        handle.cancel();
        drop(admission);
        assert_eq!(handle.observer().observation().producer_bytes, 0);
        assert_eq!(handle.observer().observation().command_entries, 0);
    }
    #[test]
    fn concurrent_small_producers_share_entry_and_byte_admission() {
        let mut owned = shared();
        let limits = &mut Arc::get_mut(&mut owned).unwrap().limits;
        limits.commands = 2;
        limits.producer_bytes = 128;
        let handle = SendspinHandle { shared: owned };
        let admitted = std::thread::scope(|scope| {
            let jobs = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        handle
                            .try_send(ApplicationMessage {
                                id: 0,
                                body:
                                    br#"{"type":"client/goodbye","payload":{"client_transmitted":1}}"#
                                        .to_vec(),
                            })
                            .is_ok()
                    })
                })
                .collect::<Vec<_>>();
            jobs.into_iter()
                .map(|job| usize::from(job.join().unwrap()))
                .sum::<usize>()
        });
        assert_eq!(admitted, 2);
        assert_eq!(handle.observer().observation().command_entries, 2);
        assert!(handle.observer().observation().producer_bytes <= 128);
        handle.shared.lock().discard_commands();
        assert_eq!(handle.observer().observation().producer_bytes, 0);
    }
    #[test]
    fn dequeued_input_keeps_its_validation_credit_until_transfer() {
        let mut owned = shared();
        Arc::get_mut(&mut owned).unwrap().limits.producer_bytes = 64;
        let handle = SendspinHandle { shared: owned };
        handle
            .try_send(ApplicationMessage {
                id: 0,
                body: br#"{"type":"client/goodbye","payload":{"client_transmitted":1}}"#.to_vec(),
            })
            .unwrap();
        let (command, credit) = handle.shared.pop_command().unwrap();
        assert_eq!(handle.observer().observation().command_entries, 1);
        assert_eq!(
            handle
                .try_send(ApplicationMessage {
                    id: 0,
                    body: vec![b'{'; 16]
                })
                .unwrap_err()
                .error,
            DriverError::Budget
        );
        drop(command);
        drop(credit);
        assert_eq!(handle.observer().observation().producer_bytes, 0);
        assert_eq!(handle.observer().observation().command_entries, 0);
    }
    #[test]
    fn caller_stamped_time_returns_without_queue_admission() {
        let handle = SendspinHandle { shared: shared() };
        let message =
            ApplicationMessage::json("client/time", serde_json::json!({"client_transmitted":123}));
        let expected = message.body.clone();
        let rejected = handle.try_send(message).unwrap_err();
        assert_eq!(rejected.error, DriverError::Protocol);
        assert_eq!(rejected.message.body, expected);
        assert_eq!(handle.observer().observation().command_entries, 0);
    }
    #[test]
    fn unsupported_core_messages_return_without_queue_admission() {
        let handle = SendspinHandle { shared: shared() };
        for kind in ["client/ping", "client/state", "client/command"] {
            let rejected = handle
                .try_send(ApplicationMessage::json(kind, serde_json::json!({})))
                .unwrap_err();
            assert_eq!(rejected.error, DriverError::Protocol);
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&rejected.message.body).unwrap()["type"],
                kind
            );
            assert_eq!(handle.observer().observation().command_entries, 0);
        }
    }
    #[test]
    fn consumer_lease_retains_aggregate_byte_and_entry_credit() {
        let shared = shared();
        let observer = SendspinObserver {
            shared: shared.clone(),
        };
        shared.publish(event()).unwrap();
        let lease = observer.next_event().unwrap();
        assert_eq!(observer.observation().event_entries, 1);
        assert_eq!(observer.observation().event_bytes, 4);
        assert_eq!(shared.publish(event()), Err(DriverError::Budget));
        assert!(format!("{lease:?}").contains("REDACTED"));
        drop(lease);
        assert_eq!(observer.observation().event_bytes, 0);
        shared.publish(event()).unwrap();
        drop(observer.next_event().unwrap());
        assert_eq!(observer.observation().event_entries, 0);
        assert_eq!(observer.observation().event_high_water, 4);
    }
    #[test]
    fn a_large_producer_capacity_cannot_evade_aggregate_accounting() {
        let shared = shared();
        let handle = SendspinHandle { shared };
        let mut body = Vec::with_capacity(1048577);
        body.extend_from_slice(br#"{"type":"client/goodbye","payload":{}}"#);
        let rejected = handle
            .try_send(ApplicationMessage { id: 0, body })
            .unwrap_err();
        assert_eq!(rejected.error, DriverError::Budget);
        assert!(rejected.message.body.capacity() > 1048576);
    }
    #[test]
    fn arrival_counter_exhaustion_cannot_wrap_or_replace_the_last_sample() {
        let shared = shared();
        shared.lock().observation.received_messages = u64::MAX;
        shared.lock().observation.last_arrival = Some(ReceiveStamp::new(17).unwrap());
        assert_eq!(
            shared.arrival(ReceiveStamp::new(19).unwrap()),
            Err(DriverError::Budget)
        );
        assert_eq!(
            shared.lock().observation.last_arrival,
            Some(ReceiveStamp::new(17).unwrap())
        );
    }
}

#[cfg(test)]
#[path = "../../tests/support/sendspin_flush.rs"]
mod flush_tests;

/// The socket accepts readiness before T1 sampling, serialization, or Noise nonce consumption.
fn poll_clock_write<S: AsyncRead + AsyncWrite + Unpin>(
    socket: &mut WebSocketStream<S>,
    cx: &mut Context<'_>,
    session: &mut ClientSession,
    clock: &mut ClockDriver,
) -> Poll<Result<Option<PendingWrite>, DriverError>> {
    if !clock.has_offer() || !session.probe_write_ready() {
        return Poll::Ready(Ok(None));
    }
    std::task::ready!(Pin::new(&mut *socket).poll_ready(cx)).map_err(websocket_error)?;
    let t1 = clock.domain.sample()?;
    let Some(token) = clock.offered_at(t1)? else {
        return Poll::Ready(Ok(None));
    };
    let Some(write) = session.time_probe_write(t1).map_err(session_error)? else {
        return Poll::Ready(Ok(None));
    };
    let mut pending = PendingWrite {
        write,
        started: false,
    };
    if let Err(error) = pending.start_ready(socket) {
        let _ = session.complete_write(
            pending.write.id,
            WriteResult::Uncertain,
            clock.domain.milliseconds(),
        );
        return Poll::Ready(Err(websocket_error(error)));
    }
    if let Err(error) = clock.started(token, t1) {
        let _ = session.complete_write(
            pending.write.id,
            WriteResult::Uncertain,
            clock.domain.milliseconds(),
        );
        return Poll::Ready(Err(error));
    }
    Poll::Ready(Ok(Some(pending)))
}

#[cfg(test)]
#[path = "../../tests/support/sendspin_clock_writer.rs"]
mod clock_writer_tests;
