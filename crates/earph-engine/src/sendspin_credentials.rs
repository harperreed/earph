// ABOUTME: Bridges native credential ownership to explicit Sendspin policies.
// ABOUTME: Retains blocking work and uncertainty outside Network futures and Engine startup.
use earph_credentials::{CredentialStore, NativeConfig, StoreError};
use earph_network::sendspin::{DriverError, DriverLimits, Operation, SendspinServices};
use earph_sendspin::{
    credentials::CommittedCredentials,
    pairing::{AttemptId, Candidate, CommitOutcome, RecordCatalog, RecordUpdate},
    session::{AdmissionProposal, SessionConfig, SessionLimits},
};
use serde_json::Value;
use std::{
    fmt,
    future::Future,
    io::{self, Write},
    path::PathBuf,
    pin::Pin,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    task::{Context, Poll},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tokio::sync::oneshot;

pub struct StoreConfiguration {
    pub helper: PathBuf,
    pub native: NativeConfig,
    pub lock_path: PathBuf,
    pub operation_timeout: Duration,
}
impl fmt::Debug for StoreConfiguration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("StoreConfiguration([REDACTED])")
    }
}
pub struct SessionRequest {
    pub connection_generation: u64,
    pub hello: Value,
    pub protected: Vec<[u8; 32]>,
    pub limits: SessionLimits,
}
impl fmt::Debug for SessionRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionRequest([REDACTED])")
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BridgeError {
    Store(StoreError),
    Busy,
    Uncertain,
    Cancelled,
    InvalidRequest,
    CleanupTimeout,
    Failed,
}
impl fmt::Display for BridgeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sendspin credentials {self:?}")
    }
}
impl std::error::Error for BridgeError {}

/// Caller-controlled activity authority. A transport permit does not provide this authority.
#[derive(Clone)]
pub struct AdmissionPolicy {
    idle: bool,
    pairing: bool,
    playback_server: Option<[u8; 32]>,
    expires: Option<Instant>,
    revoked: Arc<AtomicBool>,
}
impl fmt::Debug for AdmissionPolicy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AdmissionPolicy([REDACTED])")
    }
}
impl Default for AdmissionPolicy {
    fn default() -> Self {
        Self::deny()
    }
}
impl AdmissionPolicy {
    pub fn deny() -> Self {
        Self {
            idle: false,
            pairing: false,
            playback_server: None,
            expires: None,
            revoked: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn new(idle: bool, pairing: bool, expires: Instant) -> Self {
        Self {
            idle,
            pairing,
            playback_server: None,
            expires: Some(expires),
            revoked: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn revoke(&self) {
        self.revoked.store(true, Ordering::Release);
    }
    /// Native policy for one already paired server; grants no source or output authority.
    pub fn paired_playback(server_public: [u8; 32], expires: Instant) -> Self {
        Self {
            idle: true,
            pairing: false,
            playback_server: Some(server_public),
            expires: Some(expires),
            revoked: Arc::new(AtomicBool::new(false)),
        }
    }
    fn live(&self) -> bool {
        !self.revoked.load(Ordering::Acquire) && self.expires.is_some_and(|e| Instant::now() < e)
    }
    fn permits(&self, roles: &[String], activities: &[String]) -> bool {
        self.live()
            && match (roles, activities) {
                ([], []) => self.idle,
                ([], [activity]) if activity == "pairing" => self.pairing,
                ([], [activity]) if activity == "playback" => self.playback_server.is_some(),
                ([role], []) if role == "player@v1" => self.idle && self.playback_server.is_some(),
                ([role], [activity]) if role == "player@v1" && activity == "playback" => {
                    self.playback_server.is_some()
                }
                _ => false,
            }
    }
}

pub struct SessionHandoff {
    pub config: SessionConfig,
    pub services: CredentialServices,
}
impl fmt::Debug for SessionHandoff {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionHandoff([REDACTED])")
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingKind {
    Load,
    Admission,
    Candidate,
    Commit,
    Recovery,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retirement {
    Running,
    Retiring,
    Exited,
    Joined,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryRead {
    ValidDocument,
    Missing,
    Refused(StoreError),
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommitResult {
    pub attempt: AttemptId,
    pub result: Result<(), CommitOutcome>,
}
/// Fixed classes and operation ownership only. This value never contains native diagnostics or identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CredentialObservation {
    pub busy: bool,
    pub lease_active: bool,
    pub operation: Option<u64>,
    pub pending: Option<PendingKind>,
    pub native_operation: Option<u64>,
    pub uncertain: bool,
    pub cancelled: bool,
    pub retirement: Retirement,
    pub last_error: Option<BridgeError>,
    pub last_commit: Option<CommitResult>,
}
struct State {
    observation: CredentialObservation,
    next: u64,
    stopping: bool,
    cancel: Option<Arc<AtomicBool>>,
}
struct Shared(Mutex<State>);
impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.0.lock().unwrap_or_else(|p| {
            let mut state = p.into_inner();
            state.observation.uncertain = true;
            state
        })
    }
    fn reserve(
        self: &Arc<Self>,
        kind: PendingKind,
        load: bool,
    ) -> Result<Arc<Reservation>, BridgeError> {
        let mut state = self.lock();
        if state.stopping {
            return Err(BridgeError::Cancelled);
        }
        if state.observation.uncertain && kind != PendingKind::Recovery {
            return Err(BridgeError::Uncertain);
        }
        if state.observation.busy || (load && state.observation.lease_active) {
            return Err(BridgeError::Busy);
        }
        let operation = state.next;
        state.next = operation.checked_add(1).ok_or(BridgeError::Failed)?;
        let cancel = Arc::new(AtomicBool::new(false));
        state.cancel = Some(cancel.clone());
        state.observation.busy = true;
        state.observation.operation = Some(operation);
        state.observation.pending = Some(kind);
        state.observation.cancelled = false;
        Ok(Arc::new(Reservation {
            shared: self.clone(),
            cancel,
            kind,
        }))
    }
    fn settled(&self, reservation: &Reservation, native: Option<u64>) {
        let mut state = self.lock();
        state.observation.pending = None;
        state.observation.native_operation = native;
        state.observation.uncertain |= native.is_some();
        state.observation.cancelled = reservation.cancel.load(Ordering::Acquire);
        state.cancel = None;
    }
    fn stop(&self) {
        let mut state = self.lock();
        state.stopping = true;
        if state.observation.retirement == Retirement::Running {
            state.observation.retirement = Retirement::Retiring;
        }
        if state.observation.pending != Some(PendingKind::Commit)
            && let Some(cancel) = &state.cancel
        {
            cancel.store(true, Ordering::Release);
        }
    }
}
/// The slot remains reserved until both the producer and its completion receiver release it.
struct Reservation {
    shared: Arc<Shared>,
    cancel: Arc<AtomicBool>,
    kind: PendingKind,
}
impl Drop for Reservation {
    fn drop(&mut self) {
        let mut state = self.shared.lock();
        state.observation.busy = false;
        state.observation.operation = None;
    }
}
#[derive(Clone, Copy)]
enum CompletionRefusal {
    Cancelled,
    Policy,
}
struct Completion<T, E> {
    receiver: Option<oneshot::Receiver<Result<T, E>>>,
    reservation: Option<Arc<Reservation>>,
    lost: fn() -> Result<T, E>,
    refused: fn(CompletionRefusal) -> Result<T, E>,
    policy: Option<AdmissionPolicy>,
    playback_lease: Option<Arc<Lease>>,
}
impl<T, E> Completion<T, E> {
    fn refusal(&self) -> Option<CompletionRefusal> {
        let reservation = self.reservation.as_ref().unwrap();
        if reservation.kind == PendingKind::Commit {
            return None;
        }
        if reservation.cancel.load(Ordering::Acquire) || reservation.shared.lock().stopping {
            return Some(CompletionRefusal::Cancelled);
        }
        self.policy
            .as_ref()
            .filter(|policy| !policy.live())
            .map(|_| CompletionRefusal::Policy)
            .or_else(|| {
                self.playback_lease
                    .as_ref()
                    .filter(|lease| !lease.playback_authorized())
                    .map(|_| CompletionRefusal::Policy)
            })
    }
    fn cancel_receiver(&mut self) {
        let reservation = self.reservation.as_ref().unwrap();
        reservation.cancel.store(true, Ordering::Release);
        reservation.shared.lock().observation.cancelled = true;
        self.receiver.take();
    }
}
impl<T, E> Future for Completion<T, E> {
    type Output = Result<T, E>;
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let result = match Pin::new(
            this.receiver
                .as_mut()
                .expect("completion polled after result"),
        )
        .poll(cx)
        {
            Poll::Ready(result) => result,
            Poll::Pending => {
                if matches!(this.refusal(), Some(CompletionRefusal::Cancelled)) {
                    this.cancel_receiver();
                    this.reservation.take();
                    return Poll::Ready((this.refused)(CompletionRefusal::Cancelled));
                }
                return Poll::Pending;
            }
        };
        this.receiver.take();
        let mut result = result.unwrap_or_else(|_| {
            let mut state = this.reservation.as_ref().unwrap().shared.lock();
            state.observation.uncertain = true;
            state.observation.last_error = Some(BridgeError::Failed);
            (this.lost)()
        });
        // A computed value remains cancellable until its receiver actually delivers it.
        // Preserve terminal provider errors. Fence every successful cancellable result here.
        if result.is_ok()
            && let Some(refusal) = this.refusal()
        {
            this.cancel_receiver();
            result = (this.refused)(refusal);
        }
        this.reservation.take();
        Poll::Ready(result)
    }
}
impl<T, E> Drop for Completion<T, E> {
    fn drop(&mut self) {
        if self.receiver.is_some()
            && let Some(reservation) = &self.reservation
        {
            if reservation.kind == PendingKind::Commit {
                reservation.shared.lock().observation.uncertain = true;
            } else {
                reservation.cancel.store(true, Ordering::Release);
            }
        }
    }
}
struct Work<T> {
    reservation: Arc<Reservation>,
    reply: oneshot::Sender<T>,
}
impl<T> Work<T> {
    fn finish(self, value: T, native: Option<u64>) {
        self.reservation.shared.settled(&self.reservation, native);
        let shared = self.reservation.shared.clone();
        let commit = self.reservation.kind == PendingKind::Commit;
        // Release producer ownership before publishing the result to the next service poll.
        drop(self.reservation);
        if self.reply.send(value).is_err() && commit {
            shared.lock().observation.uncertain = true;
        }
    }
}
fn completion<T, E>(
    reservation: Arc<Reservation>,
    lost: fn() -> Result<T, E>,
    refused: fn(CompletionRefusal) -> Result<T, E>,
    policy: Option<AdmissionPolicy>,
) -> (Work<Result<T, E>>, Completion<T, E>) {
    let (reply, receiver) = oneshot::channel();
    (
        Work {
            reservation: reservation.clone(),
            reply,
        },
        Completion {
            receiver: Some(receiver),
            reservation: Some(reservation),
            lost,
            refused,
            policy,
            playback_lease: None,
        },
    )
}
struct Lease {
    shared: Arc<Shared>,
    generation: u64,
    policy: AdmissionPolicy,
    playback_record: Option<([u8; 32], [u8; 32])>,
}
impl Lease {
    fn playback_authorized(&self) -> bool {
        let state = self.shared.lock();
        self.policy.live()
            && self.playback_record.is_some()
            && state.observation.lease_active
            && !state.stopping
            && !state.observation.uncertain
    }
    fn permits(&self, proposal: &AdmissionProposal) -> bool {
        if !self.policy.permits(proposal.roles(), proposal.activities()) {
            return false;
        }
        if proposal.activities() != ["playback"] {
            return true;
        }
        self.playback_authorized()
            && proposal.connection_generation() == self.generation
            && proposal.paired_identity().is_some_and(|identity| {
                self.playback_record == Some((identity.server_public(), identity.credential_id()))
            })
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        self.shared.lock().observation.lease_active = false;
    }
}
pub struct CredentialServices {
    lease: Arc<Lease>,
    sender: SyncSender<Command>,
}
impl fmt::Debug for CredentialServices {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CredentialServices([REDACTED])")
    }
}
enum Command {
    Load(
        SessionRequest,
        AdmissionPolicy,
        SyncSender<Command>,
        Work<Result<SessionHandoff, BridgeError>>,
    ),
    Admission(
        AdmissionProposal,
        Arc<Lease>,
        Work<Result<bool, DriverError>>,
    ),
    Candidate(Arc<Lease>, Work<Result<Candidate, DriverError>>),
    Commit(
        AttemptId,
        RecordUpdate,
        Arc<Lease>,
        Work<Result<(), CommitOutcome>>,
    ),
    Recovery(Work<Result<RecoveryRead, BridgeError>>),
}
fn send<T, E>(
    sender: &SyncSender<Command>,
    command: Command,
    future: Completion<T, E>,
    rejected: Result<T, E>,
) -> Operation<Result<T, E>>
where
    T: Send + 'static,
    E: Send + 'static,
{
    match sender.try_send(command) {
        Ok(()) => Box::pin(future),
        Err(error) => {
            let (command, disconnected) = match error {
                TrySendError::Full(command) => (command, false),
                TrySendError::Disconnected(command) => (command, true),
            };
            // No command reached the supervisor. Disarm receiver-loss handling before releasing either owner.
            let mut future = future;
            future.receiver.take();
            if let Some(reservation) = &future.reservation {
                reservation.shared.settled(reservation, None);
                if disconnected {
                    let mut state = reservation.shared.lock();
                    state.observation.uncertain = true;
                    state.observation.last_error = Some(BridgeError::Failed);
                }
            }
            drop(command);
            drop(future);
            Box::pin(async move { rejected })
        }
    }
}
fn rejected_commit(shared: &Shared, error: BridgeError) -> CommitOutcome {
    let observation = shared.lock().observation;
    if error == BridgeError::Uncertain
        || observation.uncertain
        || observation.native_operation.is_some()
        || observation.pending == Some(PendingKind::Commit)
    {
        CommitOutcome::Uncertain
    } else {
        CommitOutcome::Unchanged
    }
}
fn driver_error(error: BridgeError) -> DriverError {
    match error {
        BridgeError::Busy => DriverError::Busy,
        BridgeError::Uncertain => DriverError::StorageUncertain,
        BridgeError::Cancelled => DriverError::Cancelled,
        _ => DriverError::Service,
    }
}
impl SendspinServices for CredentialServices {
    fn playback_authorized(&self) -> bool {
        self.lease.playback_authorized()
    }
    fn admission(&mut self, proposal: AdmissionProposal) -> Operation<Result<bool, DriverError>> {
        let reservation = match self.lease.shared.reserve(PendingKind::Admission, false) {
            Ok(r) => r,
            Err(e) => return Box::pin(async move { Err(driver_error(e)) }),
        };
        let (work, mut future) = completion(
            reservation,
            || Err(DriverError::Internal),
            |refusal| match refusal {
                CompletionRefusal::Cancelled => Err(DriverError::Cancelled),
                CompletionRefusal::Policy => Ok(false),
            },
            Some(self.lease.policy.clone()),
        );
        if proposal.activities() == ["playback"] {
            future.playback_lease = Some(self.lease.clone());
        }
        send(
            &self.sender,
            Command::Admission(proposal, self.lease.clone(), work),
            future,
            Err(DriverError::Service),
        )
    }

    fn candidate(&mut self, id: AttemptId) -> Operation<Result<Candidate, DriverError>> {
        if id.connection_generation != self.lease.generation {
            return Box::pin(async { Err(DriverError::Admission) });
        }
        let reservation = match self.lease.shared.reserve(PendingKind::Candidate, false) {
            Ok(r) => r,
            Err(e) => return Box::pin(async move { Err(driver_error(e)) }),
        };
        let (work, future) = completion(
            reservation,
            || Err(DriverError::Internal),
            |refusal| {
                Err(match refusal {
                    CompletionRefusal::Cancelled => DriverError::Cancelled,
                    CompletionRefusal::Policy => DriverError::Admission,
                })
            },
            Some(self.lease.policy.clone()),
        );
        send(
            &self.sender,
            Command::Candidate(self.lease.clone(), work),
            future,
            Err(DriverError::Service),
        )
    }
    fn commit(
        &mut self,
        id: AttemptId,
        update: RecordUpdate,
    ) -> Operation<Result<(), CommitOutcome>> {
        let catalog = &update.replacement;
        if catalog.capacity() != 5
            || catalog.records().len() > 5
            || catalog.superseded_ids().len() > 5
        {
            let outcome = rejected_commit(&self.lease.shared, BridgeError::InvalidRequest);
            return Box::pin(async move { Err(outcome) });
        }
        // Compact caller allocation before admitting the whole replacement into the bounded slot.
        let replacement = RecordCatalog::from_committed(
            5,
            catalog.records().to_vec(),
            catalog.superseded_ids().to_vec(),
        );
        let update = match replacement {
            Ok(replacement) => RecordUpdate { replacement },
            Err(_) => {
                let outcome = rejected_commit(&self.lease.shared, BridgeError::InvalidRequest);
                return Box::pin(async move { Err(outcome) });
            }
        };
        let reservation = match self.lease.shared.reserve(PendingKind::Commit, false) {
            Ok(r) => r,
            Err(error) => {
                let outcome = rejected_commit(&self.lease.shared, error);
                return Box::pin(async move { Err(outcome) });
            }
        };
        if id.connection_generation != self.lease.generation {
            self.lease.shared.settled(&reservation, None);
            drop(reservation);
            let outcome = rejected_commit(&self.lease.shared, BridgeError::InvalidRequest);
            return Box::pin(async move { Err(outcome) });
        }
        let (work, future) = completion(
            reservation,
            || Err(CommitOutcome::Uncertain),
            |_| Err(CommitOutcome::Uncertain),
            None,
        );
        let shared = self.lease.shared.clone();
        let future = send(
            &self.sender,
            Command::Commit(id, update, self.lease.clone(), work),
            future,
            Err(CommitOutcome::Unchanged),
        );
        Box::pin(async move {
            let result = future.await;
            let observation = shared.lock().observation;
            if observation.uncertain || observation.native_operation.is_some() {
                Err(CommitOutcome::Uncertain)
            } else {
                result
            }
        })
    }
}

pub struct CredentialSupervisor {
    shared: Arc<Shared>,
    sender: SyncSender<Command>,
    thread: Option<JoinHandle<()>>,
    exited: Receiver<()>,
}
impl fmt::Debug for CredentialSupervisor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("CredentialSupervisor")
            .field(&self.observation())
            .finish()
    }
}
impl CredentialSupervisor {
    /// Starts one dormant owner. Native opening and loading occur only after an explicit request.
    pub fn spawn(configuration: StoreConfiguration) -> Result<Self, BridgeError> {
        let shared = Arc::new(Shared(Mutex::new(State {
            observation: CredentialObservation {
                busy: false,
                lease_active: false,
                operation: None,
                pending: None,
                native_operation: None,
                uncertain: false,
                cancelled: false,
                retirement: Retirement::Running,
                last_error: None,
                last_commit: None,
            },
            next: 1,
            stopping: false,
            cancel: None,
        })));
        let (sender, receiver) = mpsc::sync_channel(1);
        let (done, exited) = mpsc::sync_channel(1);
        let owned = shared.clone();
        let thread = thread::Builder::new()
            .name("earph-sendspin-credentials".into())
            .spawn(move || {
                struct Exit {
                    shared: Arc<Shared>,
                    done: SyncSender<()>,
                }
                impl Drop for Exit {
                    fn drop(&mut self) {
                        let mut state = self.shared.lock();
                        state.observation.retirement = Retirement::Exited;
                        if thread::panicking() {
                            state.observation.uncertain = true;
                            state.observation.last_error = Some(BridgeError::Failed);
                        }
                        drop(state);
                        let _ = self.done.try_send(());
                    }
                }
                let _exit = Exit {
                    shared: owned.clone(),
                    done,
                };
                supervise(configuration, receiver, owned);
            })
            .map_err(|_| BridgeError::Failed)?;
        Ok(Self {
            shared,
            sender,
            thread: Some(thread),
            exited,
        })
    }
    pub fn observation(&self) -> CredentialObservation {
        self.shared.lock().observation
    }
    pub fn load(
        &mut self,
        mut request: SessionRequest,
        policy: AdmissionPolicy,
    ) -> Operation<Result<SessionHandoff, BridgeError>> {
        if request.connection_generation == 0
            || request.protected.len() > 5
            || DriverLimits::default().validate(request.limits).is_err()
        {
            return Box::pin(async { Err(BridgeError::InvalidRequest) });
        }
        request.hello = match bounded_hello(request.hello) {
            Ok(hello) => hello,
            Err(error) => return Box::pin(async move { Err(error) }),
        };
        request.protected = request.protected.into_boxed_slice().into_vec();
        let reservation = match self.shared.reserve(PendingKind::Load, true) {
            Ok(r) => r,
            Err(e) => return Box::pin(async move { Err(e) }),
        };
        let (work, future) = completion(
            reservation,
            || Err(BridgeError::Failed),
            |_| Err(BridgeError::Cancelled),
            None,
        );
        send(
            &self.sender,
            Command::Load(request, policy, self.sender.clone(), work),
            future,
            Err(BridgeError::Failed),
        )
    }
    /// A diagnostic read cannot acknowledge native completion or clear either uncertainty latch.
    pub fn recover(&mut self) -> Operation<Result<RecoveryRead, BridgeError>> {
        let reservation = match self.shared.reserve(PendingKind::Recovery, false) {
            Ok(r) => r,
            Err(e) => return Box::pin(async move { Err(e) }),
        };
        let (work, future) = completion(
            reservation,
            || Err(BridgeError::Failed),
            |_| Err(BridgeError::Cancelled),
            None,
        );
        send(
            &self.sender,
            Command::Recovery(work),
            future,
            Err(BridgeError::Failed),
        )
    }
    /// Bounded retirement keeps the original thread and native lock after a timeout.
    pub fn retire(&mut self, timeout: Duration) -> Result<(), BridgeError> {
        self.shared.stop();
        if self.thread.is_none() {
            return Ok(());
        }
        let deadline = Instant::now()
            .checked_add(timeout)
            .ok_or(BridgeError::InvalidRequest)?;
        while !self.thread.as_ref().unwrap().is_finished() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(BridgeError::CleanupTimeout);
            }
            let step = remaining.min(Duration::from_millis(1));
            if matches!(
                self.exited.recv_timeout(step),
                Err(mpsc::RecvTimeoutError::Disconnected)
            ) {
                thread::sleep(step);
            }
        }
        let result = self
            .thread
            .take()
            .unwrap()
            .join()
            .map_err(|_| BridgeError::Failed);
        self.shared.lock().observation.retirement = Retirement::Joined;
        result
    }
}
impl Drop for CredentialSupervisor {
    fn drop(&mut self) {
        self.shared.stop();
        if let Some(thread) = self.thread.take() {
            // OS entropy calls are not interruptible. Final ownership release may wait indefinitely.
            let _ = thread.join();
            self.shared.lock().observation.retirement = Retirement::Joined;
        }
    }
}
fn bounded_hello(value: Value) -> Result<Value, BridgeError> {
    fn within_depth(value: &Value, depth: usize, remaining: &mut usize) -> bool {
        if depth > 8 || *remaining == 0 {
            return false;
        }
        *remaining -= 1;
        match value {
            Value::Array(values) => values.iter().all(|v| within_depth(v, depth + 1, remaining)),
            Value::Object(values) => values
                .values()
                .all(|v| within_depth(v, depth + 1, remaining)),
            _ => true,
        }
    }
    if !within_depth(&value, 0, &mut 16384)
        || !value["name"].as_str().is_some_and(|name| name.len() <= 64)
        || !value["supported_roles"]
            .as_array()
            .is_some_and(Vec::is_empty)
    {
        return Err(BridgeError::InvalidRequest);
    }
    struct Bounded(Vec<u8>);
    impl Write for Bounded {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            if bytes.len() > 16384 - self.0.len() {
                return Err(io::Error::other("hello budget"));
            }
            self.0.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let mut bytes = Bounded(Vec::new());
    serde_json::to_writer(&mut bytes, &value).map_err(|_| BridgeError::InvalidRequest)?;
    serde_json::from_slice(&bytes.0).map_err(|_| BridgeError::InvalidRequest)
}
struct Envelope {
    private: [u8; 32],
    pairing_psk: [u8; 32],
    last_server: Option<[u8; 32]>,
}
fn fresh_candidate(
    cancel: &AtomicBool,
    fill: impl FnOnce(&mut [u8; 32]) -> Result<(), ()>,
) -> Result<Candidate, DriverError> {
    if cancel.load(Ordering::Acquire) {
        return Err(DriverError::Cancelled);
    }
    let mut bytes = [0; 32];
    fill(&mut bytes).map_err(|_| DriverError::Service)?;
    if cancel.load(Ordering::Acquire) {
        return Err(DriverError::Cancelled);
    }
    Ok(Candidate::new(bytes))
}
fn commit_outcome(
    result: Result<u64, StoreError>,
    native: Option<u64>,
    latched: bool,
) -> Result<(), CommitOutcome> {
    if native.is_some() || latched {
        return Err(CommitOutcome::Uncertain);
    }
    match result {
        Ok(_) => Ok(()),
        Err(
            StoreError::Missing
            | StoreError::Locked
            | StoreError::Unavailable
            | StoreError::Ambiguous
            | StoreError::InvalidDocument
            | StoreError::Cancelled,
        ) => Err(CommitOutcome::Unchanged),
        Err(_) => Err(CommitOutcome::Uncertain),
    }
}
fn supervise(configuration: StoreConfiguration, receiver: Receiver<Command>, shared: Arc<Shared>) {
    let mut store: Option<Result<CredentialStore, StoreError>> = None;
    let mut envelope: Option<Envelope> = None;
    loop {
        let command = match receiver.recv_timeout(Duration::from_millis(5)) {
            Ok(c) => c,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if shared.lock().stopping {
                    break;
                }
                continue;
            }
        };
        match command {
            Command::Load(request, policy, sender, work) => {
                envelope = None;
                let store = store.get_or_insert_with(|| {
                    CredentialStore::open(
                        configuration.helper.clone(),
                        configuration.native.clone(),
                        &configuration.lock_path,
                        configuration.operation_timeout,
                    )
                });
                let result = match store {
                    Ok(store) => store.load(&work.reservation.cancel),
                    Err(error) => Err(*error),
                };
                let native = store
                    .as_ref()
                    .ok()
                    .and_then(CredentialStore::uncertain_operation);
                let result = result.map_err(BridgeError::Store).and_then(|document| {
                    if work.reservation.cancel.load(Ordering::Acquire) {
                        return Err(BridgeError::Cancelled);
                    }
                    envelope = Some(Envelope {
                        private: *document.static_private(),
                        pairing_psk: *document.pairing_psk(),
                        last_server: document.last_playback_server(),
                    });
                    let config = SessionConfig {
                        private: *document.static_private(),
                        pairing_psk: *document.pairing_psk(),
                        catalog: document.catalog().clone(),
                        connection_generation: request.connection_generation,
                        hello: request.hello,
                        protected: request.protected.into_boxed_slice().into_vec(),
                        limits: request.limits,
                    };
                    shared.lock().observation.lease_active = true;
                    let lease = Arc::new(Lease {
                        shared: shared.clone(),
                        generation: config.connection_generation,
                        playback_record: policy.playback_server.and_then(|server| {
                            document
                                .catalog()
                                .records()
                                .iter()
                                .find(|record| record.server_public == server)
                                .map(|record| (record.server_public, record.psk_id()))
                        }),
                        policy,
                    });
                    Ok(SessionHandoff {
                        config,
                        services: CredentialServices { lease, sender },
                    })
                });
                if let Err(error) = result {
                    let mut state = shared.lock();
                    state.observation.last_error = Some(error);
                    state.observation.uncertain |=
                        error == BridgeError::Store(StoreError::Uncertain);
                    drop(state);
                    work.finish(Err(error), native);
                } else {
                    work.finish(result, native);
                }
            }
            Command::Admission(proposal, lease, work) => {
                let result = if work.reservation.cancel.load(Ordering::Acquire) {
                    Err(DriverError::Cancelled)
                } else {
                    Ok(lease.permits(&proposal))
                };
                drop(lease);
                work.finish(result, None);
            }
            Command::Candidate(lease, work) => {
                let result = if !lease.policy.permits(&[], &["pairing".into()]) {
                    Err(DriverError::Admission)
                } else {
                    fresh_candidate(&work.reservation.cancel, |bytes| {
                        getrandom::fill(bytes).map_err(|_| ())
                    })
                };
                let result = result.and_then(|candidate| {
                    if lease.policy.live() {
                        Ok(candidate)
                    } else {
                        Err(DriverError::Admission)
                    }
                });
                drop(lease);
                work.finish(result, None);
            }
            Command::Commit(id, update, lease, work) => {
                // Acknowledged work ignores service cancellation, revocation, and retirement.
                let result = match (&mut store, &envelope) {
                    (Some(Ok(store)), Some(envelope)) => {
                        let document = CommittedCredentials::new(
                            envelope.private,
                            envelope.pairing_psk,
                            update.replacement,
                            envelope.last_server,
                        );
                        match document {
                            Ok(document) => store.commit(&document, &AtomicBool::new(false)),
                            Err(_) => Err(StoreError::InvalidDocument),
                        }
                    }
                    _ => Err(StoreError::Uncertain),
                };
                let native = store
                    .as_ref()
                    .and_then(|s| s.as_ref().ok())
                    .and_then(CredentialStore::uncertain_operation);
                let result = commit_outcome(result, native, false);
                {
                    let mut state = shared.lock();
                    state.observation.last_commit = Some(CommitResult {
                        attempt: id,
                        result,
                    });
                    state.observation.uncertain |= result == Err(CommitOutcome::Uncertain);
                }
                drop(lease);
                work.finish(result, native);
            }
            Command::Recovery(work) => {
                let result = match &mut store {
                    Some(Ok(store)) => match store.recover(&work.reservation.cancel) {
                        Ok(_) => Ok(RecoveryRead::ValidDocument),
                        Err(StoreError::Missing) => Ok(RecoveryRead::Missing),
                        Err(error) => Ok(RecoveryRead::Refused(error)),
                    },
                    Some(Err(error)) => Err(BridgeError::Store(*error)),
                    None => Err(BridgeError::InvalidRequest),
                };
                let native = store
                    .as_ref()
                    .and_then(|s| s.as_ref().ok())
                    .and_then(CredentialStore::uncertain_operation);
                work.finish(result, native);
            }
        }
        if shared.lock().stopping {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn activity_authority_denies_roles_unknown_and_duplicate_activities() {
        let policy = AdmissionPolicy::new(true, true, Instant::now() + Duration::from_secs(1));
        for (roles, activities) in [
            (vec!["player@v1".into()], vec![]),
            (vec![], vec!["playback".into()]),
            (vec![], vec!["pairing".into(), "pairing".into()]),
        ] {
            assert!(!policy.permits(&roles, &activities));
        }
    }
    #[test]
    fn entropy_failure_and_cancellation_never_produce_a_candidate() {
        let cancel = AtomicBool::new(false);
        assert_eq!(
            fresh_candidate(&cancel, |_| Err(())).err(),
            Some(DriverError::Service)
        );
        cancel.store(true, Ordering::Release);
        assert_eq!(
            fresh_candidate(&cancel, |_| panic!("cancelled draw ran")).err(),
            Some(DriverError::Cancelled)
        );
        cancel.store(false, Ordering::Release);
        assert_eq!(
            fresh_candidate(&cancel, |bytes| {
                bytes.fill(44);
                cancel.store(true, Ordering::Release);
                Ok(())
            })
            .err(),
            Some(DriverError::Cancelled)
        );
    }
    #[test]
    fn any_native_pending_or_prior_latch_overrides_every_commit_result() {
        for result in [
            Ok(7),
            Err(StoreError::Missing),
            Err(StoreError::Locked),
            Err(StoreError::Unavailable),
            Err(StoreError::Ambiguous),
            Err(StoreError::InvalidDocument),
            Err(StoreError::Cancelled),
            Err(StoreError::Busy),
            Err(StoreError::Uncertain),
        ] {
            assert_eq!(
                commit_outcome(result, Some(8), false),
                Err(CommitOutcome::Uncertain)
            );
            assert_eq!(
                commit_outcome(result, None, true),
                Err(CommitOutcome::Uncertain)
            );
        }
    }
    #[test]
    fn oversized_hello_is_refused_before_creating_native_ownership() {
        let mut owner = CredentialSupervisor::spawn(StoreConfiguration {
            helper: PathBuf::from("/missing"),
            native: NativeConfig::SecretService {
                address: "test".into(),
            },
            lock_path: PathBuf::from("/missing/lock"),
            operation_timeout: Duration::from_secs(1),
        })
        .unwrap();
        let mut operation=owner.load(SessionRequest{connection_generation:1,hello:serde_json::json!({"name":"fixture","supported_roles":[],"oversize":"a".repeat(16385)}),protected:vec![],limits:earph_network::sendspin::roleless_session_limits()},AdmissionPolicy::deny());
        assert!(matches!(
            operation
                .as_mut()
                .poll(&mut Context::from_waker(std::task::Waker::noop())),
            Poll::Ready(Err(BridgeError::InvalidRequest))
        ));
        owner.retire(Duration::from_secs(1)).unwrap();
    }
    #[test]
    fn exit_notification_cannot_join_a_thread_that_has_not_finished() {
        let mut owner = CredentialSupervisor::spawn(StoreConfiguration {
            helper: PathBuf::from("/missing"),
            native: NativeConfig::SecretService {
                address: "test".into(),
            },
            lock_path: PathBuf::from("/missing/lock"),
            operation_timeout: Duration::from_secs(1),
        })
        .unwrap();
        owner.retire(Duration::from_secs(1)).unwrap();
        let (done, exited) = mpsc::sync_channel(1);
        let (ready, started) = mpsc::sync_channel(1);
        owner.exited = exited;
        owner.thread = Some(thread::spawn(move || {
            done.send(()).unwrap();
            ready.send(()).unwrap();
            thread::sleep(Duration::from_millis(100));
        }));
        started.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(
            owner.retire(Duration::from_millis(2)),
            Err(BridgeError::CleanupTimeout)
        );
        assert!(owner.thread.is_some());
        owner.retire(Duration::from_secs(1)).unwrap();
    }
    #[test]
    fn retirement_refusal_cannot_hide_an_existing_uncertainty_latch() {
        let mut owner = CredentialSupervisor::spawn(StoreConfiguration {
            helper: PathBuf::from("/missing"),
            native: NativeConfig::SecretService {
                address: "test".into(),
            },
            lock_path: PathBuf::from("/missing/lock"),
            operation_timeout: Duration::from_secs(1),
        })
        .unwrap();
        let lease = Arc::new(Lease {
            shared: owner.shared.clone(),
            generation: 1,
            policy: AdmissionPolicy::deny(),
            playback_record: None,
        });
        let mut services = CredentialServices {
            lease,
            sender: owner.sender.clone(),
        };
        owner.shared.lock().observation.uncertain = true;
        owner.retire(Duration::from_secs(1)).unwrap();
        let mut operation = services.commit(
            AttemptId {
                connection_generation: 1,
                handshake_generation: 2,
                pairing_index: 1,
            },
            RecordUpdate {
                replacement: earph_sendspin::pairing::RecordCatalog::new(5).unwrap(),
            },
        );
        assert_eq!(
            operation
                .as_mut()
                .poll(&mut Context::from_waker(std::task::Waker::noop())),
            Poll::Ready(Err(CommitOutcome::Uncertain))
        );
    }
    #[test]
    fn invalid_catalog_is_refused_before_reserving_supervisor_work() {
        let mut owner = CredentialSupervisor::spawn(StoreConfiguration {
            helper: PathBuf::from("/missing"),
            native: NativeConfig::SecretService {
                address: "test".into(),
            },
            lock_path: PathBuf::from("/missing/lock"),
            operation_timeout: Duration::from_secs(1),
        })
        .unwrap();
        let lease = Arc::new(Lease {
            shared: owner.shared.clone(),
            generation: 1,
            policy: AdmissionPolicy::deny(),
            playback_record: None,
        });
        let mut services = CredentialServices {
            lease,
            sender: owner.sender.clone(),
        };
        let mut operation = services.commit(
            AttemptId {
                connection_generation: 1,
                handshake_generation: 2,
                pairing_index: 1,
            },
            RecordUpdate {
                replacement: earph_sendspin::pairing::RecordCatalog::new(6).unwrap(),
            },
        );
        assert_eq!(
            operation
                .as_mut()
                .poll(&mut Context::from_waker(std::task::Waker::noop())),
            Poll::Ready(Err(CommitOutcome::Unchanged))
        );
        assert!(!owner.observation().busy);
        owner.retire(Duration::from_secs(1)).unwrap();
    }
    #[test]
    fn retired_owner_revokes_an_unconsumed_cancellable_completion() {
        let mut owner = CredentialSupervisor::spawn(StoreConfiguration {
            helper: PathBuf::from("/missing"),
            native: NativeConfig::SecretService {
                address: "test".into(),
            },
            lock_path: PathBuf::from("/missing/lock"),
            operation_timeout: Duration::from_secs(1),
        })
        .unwrap();
        let reservation = owner.shared.reserve(PendingKind::Admission, false).unwrap();
        let (work, mut future) = completion(
            reservation,
            || Err(DriverError::Internal),
            |_| Err(DriverError::Cancelled),
            None,
        );
        work.finish(Ok(true), None);
        owner.retire(Duration::from_secs(1)).unwrap();
        assert_eq!(
            Pin::new(&mut future).poll(&mut Context::from_waker(std::task::Waker::noop())),
            Poll::Ready(Err(DriverError::Cancelled))
        );
    }
    #[test]
    fn candidate_completion_rechecks_caller_revocation_before_delivery() {
        let mut owner = CredentialSupervisor::spawn(StoreConfiguration {
            helper: PathBuf::from("/missing"),
            native: NativeConfig::SecretService {
                address: "test".into(),
            },
            lock_path: PathBuf::from("/missing/lock"),
            operation_timeout: Duration::from_secs(1),
        })
        .unwrap();
        let policy = AdmissionPolicy::new(false, true, Instant::now() + Duration::from_secs(3));
        let lease = Arc::new(Lease {
            shared: owner.shared.clone(),
            generation: 1,
            policy: policy.clone(),
            playback_record: None,
        });
        let mut services = CredentialServices {
            lease,
            sender: owner.sender.clone(),
        };
        let mut operation = services.candidate(AttemptId {
            connection_generation: 1,
            handshake_generation: 2,
            pairing_index: 1,
        });
        let end = Instant::now() + Duration::from_secs(2);
        while owner.observation().pending.is_some() {
            assert!(Instant::now() < end);
            thread::sleep(Duration::from_millis(1));
        }
        policy.revoke();
        assert!(matches!(
            operation
                .as_mut()
                .poll(&mut Context::from_waker(std::task::Waker::noop())),
            Poll::Ready(Err(DriverError::Admission))
        ));
        owner.retire(Duration::from_secs(1)).unwrap();
    }
}
