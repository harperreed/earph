// ABOUTME: Owns ordered encrypted client sessions, pairing, admission, and key rotation.
// ABOUTME: Emits bounded transport writes without granting clock or native output authority.
use crate::{
    message::{self, Complete, Direction, Envelope, MessageLimits, Outgoing, Reassembler},
    noise::{self, Category, Credential, Responder},
    pairing::{
        AbortReason, ActivationError, AttemptId, Candidate, CommitOutcome, FlightAuthorization,
        FlightPart, GoodbyeReason, PairingClient, PairingContext, PairingEffect, PairingEvent,
        RecordCatalog, RecordUpdate,
    },
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    fmt,
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionError {
    Protocol,
    Budget,
    Authentication,
    Deadline,
    Clock,
    Closed,
    Busy,
    Suppressed,
    Write,
    Counter,
    Storage,
}
impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sendspin session {self:?}")
    }
}
impl std::error::Error for SessionError {}
impl From<message::MessageError> for SessionError {
    fn from(value: message::MessageError) -> Self {
        match value {
            message::MessageError::Budget => Self::Budget,
            message::MessageError::Deadline => Self::Deadline,
            _ => Self::Protocol,
        }
    }
}
impl From<noise::ChannelError> for SessionError {
    fn from(value: noise::ChannelError) -> Self {
        match value {
            noise::ChannelError::Budget => Self::Budget,
            noise::ChannelError::Authentication => Self::Authentication,
            _ => Self::Protocol,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SessionLimits {
    pub messages: MessageLimits,
    pub noise: noise::Limits,
    /// Aggregate retained input bytes, including ciphertext awaiting an epoch and fragment bodies.
    pub pending_input_bytes: usize,
    /// Complete cleartext transport-message bytes, including the JSON envelope.
    pub cleartext_message_bytes: usize,
    /// Aggregate producer bodies, control queue, and outstanding transport write bytes.
    pub pending_output_bytes: usize,
    /// Caps retained message metadata even when every body is empty.
    pub pending_messages: usize,
    /// Absolute caller-clock milliseconds for bring-up, rotation, and admission.
    pub exchange_ms: u64,
}
impl Default for SessionLimits {
    fn default() -> Self {
        Self {
            messages: MessageLimits::default(),
            noise: noise::Limits::default(),
            pending_input_bytes: 64 * 1024 * 1024,
            cleartext_message_bytes: 100_000,
            pending_output_bytes: 64 * 1024 * 1024,
            pending_messages: 4096,
            exchange_ms: 30_000,
        }
    }
}
pub struct SessionConfig {
    pub private: [u8; 32],
    pub pairing_psk: [u8; 32],
    pub connection_generation: u64,
    pub hello: Value,
    pub catalog: RecordCatalog,
    pub protected: Vec<[u8; 32]>,
    pub limits: SessionLimits,
}
impl fmt::Debug for SessionConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionConfig([REDACTED])")
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    Text,
    Binary,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionPhase {
    SendingInit,
    AwaitServerInit,
    AwaitHandshake,
    AwaitServerHello,
    AwaitInitialActivation,
    AwaitAdmission,
    Active,
    Quiet,
    AwaitNewActivation,
    Closing,
    Closed,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteId {
    owner: u64,
    serial: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionId {
    owner: u64,
    generation: u64,
    serial: u64,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteResult {
    Complete,
    Failed,
    Uncertain,
}
pub struct TransportWrite {
    pub id: WriteId,
    pub kind: TransportKind,
    pub bytes: Vec<u8>,
}
impl fmt::Debug for TransportWrite {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TransportWrite")
            .field("id", &self.id)
            .field("kind", &self.kind)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}
pub struct ApplicationMessage {
    pub id: u8,
    pub body: Vec<u8>,
}
impl ApplicationMessage {
    pub fn json(kind: &str, payload: Value) -> Self {
        Self {
            id: 0,
            body: serde_json::to_vec(&json!({"type":kind,"payload":payload}))
                .expect("JSON value serializes"),
        }
    }
}
impl fmt::Debug for ApplicationMessage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApplicationMessage")
            .field("id", &self.id)
            .field("bytes", &self.body.len())
            .finish()
    }
}
#[derive(Debug)]
pub struct SendRejected {
    pub error: SessionError,
    pub message: ApplicationMessage,
}
/// Constructed only after new-epoch authenticated traffic proves possession of a bound matched credential.
#[derive(Clone, Copy)]
pub struct PairedIdentity {
    server_public: [u8; 32],
    credential_id: [u8; 32],
    generation: u64,
}
impl PairedIdentity {
    pub fn server_public(&self) -> [u8; 32] {
        self.server_public
    }
    pub fn credential_id(&self) -> [u8; 32] {
        self.credential_id
    }
    pub fn handshake_generation(&self) -> u64 {
        self.generation
    }
}
impl fmt::Debug for PairedIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairedIdentity([REDACTED])")
    }
}
pub struct AdmissionProposal {
    id: AdmissionId,
    activities: Vec<String>,
    roles: Vec<String>,
    identity: Option<PairedIdentity>,
}
impl AdmissionProposal {
    pub fn id(&self) -> AdmissionId {
        self.id
    }
    pub fn roles(&self) -> &[String] {
        &self.roles
    }
    pub fn activities(&self) -> &[String] {
        &self.activities
    }
    pub fn paired_identity(&self) -> Option<PairedIdentity> {
        self.identity
    }
}
impl fmt::Debug for AdmissionProposal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AdmissionProposal")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}
pub struct ActivationNotice {
    pub handshake_generation: u64,
    pub roles: Vec<String>,
    pub removed_roles: Vec<String>,
    pub activities: Vec<String>,
}
impl fmt::Debug for ActivationNotice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ActivationNotice")
            .field("handshake_generation", &self.handshake_generation)
            .finish_non_exhaustive()
    }
}
pub enum SessionEvent {
    Unsent(ApplicationMessage),
    Admission(AdmissionProposal),
    Activated(ActivationNotice),
    Message(Complete),
    RequestCandidate(AttemptId),
    CommitRecord(AttemptId, RecordUpdate),
    Finishing(AttemptId),
    Closed,
}
impl fmt::Debug for SessionEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Unsent(_) => "SessionEvent::Unsent([REDACTED])",
            Self::Admission(_) => "SessionEvent::Admission",
            Self::Activated(_) => "SessionEvent::Activated",
            Self::Message(_) => "SessionEvent::Message([REDACTED])",
            Self::RequestCandidate(_) => "SessionEvent::RequestCandidate",
            Self::CommitRecord(..) => "SessionEvent::CommitRecord([REDACTED])",
            Self::Finishing(_) => "SessionEvent::Finishing",
            Self::Closed => "SessionEvent::Closed",
        })
    }
}

#[derive(Clone, Copy)]
enum Purpose {
    Init,
    InitialReply,
    Hello,
    Application,
    PairInit(AttemptId),
    PairFinalize(AttemptId),
    PairAbort {
        id: AttemptId,
        activation_response: bool,
    },
    Goodbye,
    RotationReply,
}
struct Queued {
    kind: TransportKind,
    body: Vec<u8>,
    purpose: Purpose,
}
struct Current {
    outgoing: Outgoing,
    purpose: Purpose,
}
struct Outstanding {
    id: WriteId,
    purpose: Purpose,
    last: bool,
    bytes: usize,
}
struct RawInput {
    kind: TransportKind,
    bytes: Vec<u8>,
    arrival: u64,
}
struct PendingAdmission {
    id: AdmissionId,
    body: Vec<u8>,
    releases_rotation: bool,
}
static OWNER: AtomicU64 = AtomicU64::new(1);
pub struct ClientSession {
    owner: u64,
    serial: u64,
    admission_serial: u64,
    connection_generation: u64,
    generation: u64,
    private: [u8; 32],
    pairing_psk: [u8; 32],
    hello: Vec<u8>,
    client_init: Vec<u8>,
    initial_catalog: Option<RecordCatalog>,
    protected: Vec<[u8; 32]>,
    pairing: Option<PairingClient>,
    limits: SessionLimits,
    phase: SessionPhase,
    clock: u64,
    deadline: Option<u64>,
    quiet: bool,
    channel: Option<Responder>,
    pending_channel: Option<Responder>,
    reassembler: Reassembler,
    raw: VecDeque<RawInput>,
    messages: VecDeque<Complete>,
    input_bytes: usize,
    queue: VecDeque<Queued>,
    current: Option<Current>,
    application: Option<Outgoing>,
    outstanding: Option<Outstanding>,
    admission: Option<PendingAdmission>,
}
impl fmt::Debug for ClientSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientSession")
            .field("phase", &self.phase())
            .field("handshake_generation", &self.generation)
            .finish_non_exhaustive()
    }
}
impl ClientSession {
    pub fn new(mut config: SessionConfig, now_ms: u64) -> Result<Self, SessionError> {
        let limits = config.limits;
        if config.pairing_psk == noise::SENTINEL
            || config
                .catalog
                .records()
                .iter()
                .any(|r| r.psk == config.pairing_psk)
            || limits.messages.record_plaintext_bytes < 4
            || limits.messages.fragment_count == 0
            || limits.pending_messages == 0
            || limits.exchange_ms == 0
        {
            return Err(SessionError::Protocol);
        }
        config
            .catalog
            .ensure_admission(&config.protected)
            .map_err(|_| SessionError::Storage)?;
        let hello = config.hello.as_object_mut().ok_or(SessionError::Protocol)?;
        if !hello
            .get("name")
            .and_then(Value::as_str)
            .is_some_and(|s| s.len() <= 64)
            || !hello
                .get("supported_roles")
                .and_then(Value::as_array)
                .is_some_and(|roles| roles.iter().all(Value::is_string))
        {
            return Err(SessionError::Protocol);
        }
        hello.insert("supported_pair_methods".into(), json!({"pairing_psk":{}}));
        hello.insert("unpaired_access".into(), json!({"enabled":false}));
        let hello = json!({"type":"client/hello","payload":config.hello});
        if !within_depth(&hello, 1) {
            return Err(SessionError::Budget);
        }
        let hello = message::bounded_json(&hello, 16_384)?;
        let client_init = message::encode(
            "client/init",
            json!({"version":1,"suite":"25519_ChaChaPoly_SHA256","client_id":URL_SAFE_NO_PAD.encode(noise::static_public_key(&config.private)?)}),
            limits.noise.init_bytes.min(65535),
        )?;
        if client_init.len() > limits.pending_output_bytes
            || hello.len() > limits.pending_output_bytes
        {
            return Err(SessionError::Budget);
        }
        let deadline = now_ms
            .checked_add(limits.exchange_ms)
            .ok_or(SessionError::Counter)?;
        let owner = OWNER
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| SessionError::Counter)?;
        let queue = VecDeque::from([Queued {
            kind: TransportKind::Text,
            body: client_init.clone(),
            purpose: Purpose::Init,
        }]);
        Ok(Self {
            owner,
            serial: 0,
            admission_serial: 0,
            connection_generation: config.connection_generation,
            generation: 1,
            private: config.private,
            pairing_psk: config.pairing_psk,
            hello,
            client_init,
            initial_catalog: Some(config.catalog),
            protected: config.protected,
            pairing: None,
            limits,
            phase: SessionPhase::SendingInit,
            clock: now_ms,
            deadline: Some(deadline),
            quiet: false,
            channel: None,
            pending_channel: None,
            reassembler: Reassembler::new(limits.messages),
            raw: VecDeque::new(),
            messages: VecDeque::new(),
            input_bytes: 0,
            queue,
            current: None,
            application: None,
            outstanding: None,
            admission: None,
        })
    }
    pub fn phase(&self) -> SessionPhase {
        if self.quiet && self.phase == SessionPhase::Active {
            SessionPhase::Quiet
        } else {
            self.phase
        }
    }
    pub fn handshake_generation(&self) -> u64 {
        self.generation
    }
    pub fn handshake_hash(&self) -> Option<[u8; 32]> {
        self.channel.as_ref().and_then(Responder::handshake_hash)
    }
    pub fn roles(&self) -> &[String] {
        self.pairing.as_ref().map_or(&[], PairingClient::roles)
    }
    pub fn catalog(&self) -> &RecordCatalog {
        self.pairing
            .as_ref()
            .map(PairingClient::catalog)
            .or(self.initial_catalog.as_ref())
            .expect("catalog always owned")
    }
    fn check_now(&mut self, now: u64) -> Result<(), SessionError> {
        if self.phase == SessionPhase::Closed {
            return Err(SessionError::Closed);
        }
        if now < self.clock {
            return Err(SessionError::Clock);
        }
        self.clock = now;
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            return Err(SessionError::Deadline);
        }
        self.reassembler.check_deadline(now)?;
        Ok(())
    }
    fn outcome<T>(&mut self, result: Result<T, SessionError>) -> Result<T, SessionError> {
        if result.is_err() {
            self.close();
        }
        result
    }
    /// Supply complete WebSocket messages, with final-byte arrival sampled before this call.
    pub fn receive(
        &mut self,
        kind: TransportKind,
        bytes: &[u8],
        arrival_ms: u64,
    ) -> Result<Vec<SessionEvent>, SessionError> {
        let result = self.receive_inner(kind, bytes, arrival_ms);
        self.outcome(result)
    }
    fn receive_inner(
        &mut self,
        kind: TransportKind,
        bytes: &[u8],
        arrival: u64,
    ) -> Result<Vec<SessionEvent>, SessionError> {
        self.check_now(arrival)?;
        let limit = if kind == TransportKind::Binary {
            self.limits.noise.record_bytes.min(65535)
        } else {
            self.limits.cleartext_message_bytes
        };
        if bytes.len() > limit
            || self
                .input_bytes
                .checked_add(self.reassembler.retained_bytes())
                .and_then(|n| n.checked_add(bytes.len()))
                .is_none_or(|n| n > self.limits.pending_input_bytes)
            || self.raw.len() + self.messages.len() + usize::from(self.admission.is_some())
                >= self.limits.pending_messages
        {
            return Err(SessionError::Budget);
        }
        if self.buffer_raw() {
            self.input_bytes += bytes.len();
            self.raw.push_back(RawInput {
                kind,
                bytes: bytes.to_vec(),
                arrival,
            });
            return Ok(vec![]);
        }
        let mut events = vec![];
        self.process_raw(kind, bytes, arrival, &mut events)?;
        self.drain(&mut events)?;
        Ok(events)
    }
    fn buffer_raw(&self) -> bool {
        self.outstanding.as_ref().is_some_and(|w| {
            matches!(
                w.purpose,
                Purpose::Init | Purpose::InitialReply | Purpose::RotationReply
            )
        }) || self
            .queue
            .iter()
            .any(|w| matches!(w.purpose, Purpose::InitialReply))
    }
    fn process_raw(
        &mut self,
        kind: TransportKind,
        bytes: &[u8],
        arrival: u64,
        events: &mut Vec<SessionEvent>,
    ) -> Result<(), SessionError> {
        if self.phase == SessionPhase::AwaitServerInit {
            if kind != TransportKind::Text || bytes.len() > self.limits.noise.init_bytes.min(65535)
            {
                return Err(SessionError::Protocol);
            }
            let channel =
                Responder::new(&self.private, &self.client_init, bytes, self.limits.noise)?;
            self.channel = Some(channel);
            self.phase = SessionPhase::AwaitHandshake;
            return Ok(());
        }
        if self.phase == SessionPhase::AwaitHandshake {
            if kind != TransportKind::Text {
                return Err(SessionError::Protocol);
            }
            let envelope = Envelope::parse(bytes)?;
            if envelope.kind != "noise/handshake" {
                return Err(SessionError::Protocol);
            }
            let raw = decode_handshake(&envelope.payload, self.limits.noise)?;
            let credentials = self.credentials();
            let reply = self
                .channel
                .as_mut()
                .ok_or(SessionError::Protocol)?
                .receive_handshake(&raw, &credentials)?;
            self.enqueue(
                TransportKind::Text,
                handshake_envelope(&reply, self.limits.pending_output_bytes)?,
                Purpose::InitialReply,
            )?;
            return Ok(());
        }
        if kind != TransportKind::Binary {
            return Err(SessionError::Protocol);
        }
        let plaintext = self
            .channel
            .as_mut()
            .ok_or(SessionError::Protocol)?
            .decrypt(bytes)?;
        if let Some(message) = self.reassembler.push(&plaintext, arrival)? {
            if message.id == 0 {
                let envelope = Envelope::parse(&message.body)?;
                if envelope.direction() == Some(Direction::Client) {
                    return Err(SessionError::Protocol);
                }
                if envelope.kind == "noise/handshake" {
                    if self.quiet
                        || !matches!(
                            self.phase,
                            SessionPhase::Active | SessionPhase::AwaitAdmission
                        )
                    {
                        return Err(SessionError::Protocol);
                    }
                    // Fence sends immediately. Earlier authenticated acknowledgments retain their wire order.
                    self.quiet = true;
                    let deadline = arrival
                        .checked_add(self.limits.exchange_ms)
                        .ok_or(SessionError::Counter)?;
                    self.deadline = Some(
                        self.deadline
                            .map_or(deadline, |existing| existing.min(deadline)),
                    );
                    self.return_unstarted(events);
                    self.queue.clear();
                } else if self.quiet
                    && (self.pending_channel.is_some()
                        || self.phase == SessionPhase::Active
                        || self
                            .admission
                            .as_ref()
                            .is_some_and(|a| !a.releases_rotation))
                {
                    return Err(SessionError::Protocol);
                }
            } else if message.id == 12 {
                return Err(SessionError::Protocol);
            }
            if self
                .input_bytes
                .checked_add(message.body.len())
                .is_none_or(|n| n > self.limits.pending_input_bytes)
            {
                return Err(SessionError::Budget);
            }
            self.input_bytes += message.body.len();
            self.messages.push_back(message);
        }
        self.drain_messages(events)
    }
    fn blocked(&self) -> bool {
        self.admission.is_some()
            || self
                .pairing
                .as_ref()
                .is_some_and(PairingClient::commit_pending)
            || self.outstanding.as_ref().is_some_and(|w| {
                matches!(
                    w.purpose,
                    Purpose::Hello | Purpose::PairFinalize(_) | Purpose::RotationReply
                )
            })
            || self
                .queue
                .iter()
                .any(|w| matches!(w.purpose, Purpose::Hello))
    }
    fn drain(&mut self, events: &mut Vec<SessionEvent>) -> Result<(), SessionError> {
        while !self.buffer_raw() {
            let Some(raw) = self.raw.pop_front() else {
                break;
            };
            self.input_bytes -= raw.bytes.len();
            self.process_raw(raw.kind, &raw.bytes, raw.arrival, events)?;
        }
        self.drain_messages(events)
    }
    fn drain_messages(&mut self, events: &mut Vec<SessionEvent>) -> Result<(), SessionError> {
        while !self.blocked() {
            let Some(message) = self.messages.pop_front() else {
                break;
            };
            self.input_bytes -= message.body.len();
            self.dispatch(message, events)?;
            if matches!(self.phase, SessionPhase::Closed | SessionPhase::Closing) {
                break;
            }
        }
        Ok(())
    }
    fn dispatch(
        &mut self,
        message: Complete,
        events: &mut Vec<SessionEvent>,
    ) -> Result<(), SessionError> {
        if self.phase == SessionPhase::Closing {
            return Ok(());
        }
        let envelope = if message.id == 0 {
            Some(Envelope::parse(&message.body)?)
        } else {
            None
        };
        if self.phase == SessionPhase::AwaitServerHello {
            let envelope = envelope.ok_or(SessionError::Protocol)?;
            if envelope.kind != "server/hello"
                || !envelope
                    .payload
                    .get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|s| s.len() <= 64)
            {
                return Err(SessionError::Protocol);
            }
            if envelope.payload.get("languages").is_some_and(|v| {
                !v.as_array().is_some_and(|a| {
                    !a.is_empty() && a.iter().all(|v| v.as_str().is_some_and(|s| !s.is_empty()))
                })
            }) {
                return Err(SessionError::Protocol);
            }
            let channel = self.channel.as_ref().ok_or(SessionError::Protocol)?;
            let context = PairingContext {
                server_public: channel.server_public(),
                selection: channel.selection().ok_or(SessionError::Protocol)?,
                connection_generation: self.connection_generation,
                handshake_generation: self.generation,
                pairing_psk: self.pairing_psk,
            };
            let mut pairing = PairingClient::new(
                context,
                self.initial_catalog
                    .as_ref()
                    .ok_or(SessionError::Protocol)?
                    .clone(),
                self.protected.clone(),
            )
            .map_err(|_| SessionError::Storage)?;
            pairing
                .protect(channel.matched_id())
                .map_err(|_| SessionError::Storage)?;
            self.pairing = Some(pairing);
            self.initial_catalog = None;
            self.enqueue(TransportKind::Binary, self.hello.clone(), Purpose::Hello)?;
            return Ok(());
        }
        if matches!(
            self.phase,
            SessionPhase::AwaitInitialActivation | SessionPhase::AwaitNewActivation
        ) && envelope
            .as_ref()
            .is_none_or(|e| e.kind != "server/activate")
        {
            return Err(SessionError::Protocol);
        }
        if let Some(envelope) = envelope {
            if envelope.direction() == Some(Direction::Client) {
                return Err(SessionError::Protocol);
            }
            match envelope.kind.as_str() {
                "server/activate" => self.activation(envelope.payload, message.body, events),
                "noise/handshake" => self.rotation(envelope.payload),
                "server/init" | "server/error" | "server/hello" => Err(SessionError::Protocol),
                "server/pair-finalize" => {
                    self.pair_event(PairingEvent::ServerFinalize(envelope.payload), events)
                }
                "pair/abort" => {
                    let reason = parse_abort(&envelope.payload)?;
                    self.pair_event(PairingEvent::RemoteAbort(reason), events)
                }
                "server/pair-init" | "server/pair-auth" | "server/pair-confirm" => {
                    self.pair_event(PairingEvent::UnexpectedPairingMessage, events)
                }
                kind => {
                    if !matches!(self.phase, SessionPhase::Active) {
                        return Err(SessionError::Protocol);
                    }
                    if message::direction(kind).is_some() {
                        events.push(SessionEvent::Message(message));
                    }
                    Ok(())
                }
            }
        } else {
            if self.phase != SessionPhase::Active {
                return Err(SessionError::Protocol);
            }
            if message.id == 4 {
                events.push(SessionEvent::Message(message));
            }
            Ok(())
        }
    }
    fn activation(
        &mut self,
        payload: Value,
        body: Vec<u8>,
        events: &mut Vec<SessionEvent>,
    ) -> Result<(), SessionError> {
        let pairing = self.pairing.as_ref().ok_or(SessionError::Protocol)?;
        match pairing.validate_activation(&payload) {
            Err(ActivationError::Protocol) => return Err(SessionError::Protocol),
            Err(ActivationError::Goodbye(_)) => {
                self.phase = SessionPhase::Closing;
                return self.pair_event(PairingEvent::Activate(payload), events);
            }
            Err(ActivationError::Method) => {
                self.phase = if self.phase == SessionPhase::AwaitNewActivation {
                    SessionPhase::AwaitNewActivation
                } else if self.quiet {
                    SessionPhase::Active
                } else {
                    SessionPhase::AwaitInitialActivation
                };
                return self.pair_event(PairingEvent::Activate(payload), events);
            }
            Ok(checked) => {
                self.retire_pairing_flight();
                self.admission_serial = self
                    .admission_serial
                    .checked_add(1)
                    .ok_or(SessionError::Counter)?;
                let id = AdmissionId {
                    owner: self.owner,
                    generation: self.generation,
                    serial: self.admission_serial,
                };
                let identity = self.identity();
                let proposal = AdmissionProposal {
                    id,
                    activities: checked.activities,
                    roles: checked.roles,
                    identity,
                };
                self.input_bytes += body.len();
                self.admission = Some(PendingAdmission {
                    id,
                    body,
                    releases_rotation: self.phase == SessionPhase::AwaitNewActivation,
                });
                self.phase = SessionPhase::AwaitAdmission;
                if self.deadline.is_none() {
                    self.deadline = Some(
                        self.clock
                            .checked_add(self.limits.exchange_ms)
                            .ok_or(SessionError::Counter)?,
                    );
                }
                events.push(SessionEvent::Admission(proposal));
            }
        }
        Ok(())
    }
    fn identity(&self) -> Option<PairedIdentity> {
        let channel = self.channel.as_ref()?;
        if channel.phase() != noise::Phase::Established
            || channel.selection()?.category != Category::LongTerm
        {
            return None;
        }
        Some(PairedIdentity {
            server_public: channel.server_public(),
            credential_id: channel.matched_id()?,
            generation: self.generation,
        })
    }
    /// Admission accepts protocol activities only. It grants no clock readiness or output lease.
    pub fn admit(
        &mut self,
        id: AdmissionId,
        accepted: bool,
        now: u64,
    ) -> Result<Vec<SessionEvent>, SessionError> {
        if self.admission.as_ref().is_none_or(|a| a.id != id) {
            return Ok(vec![]);
        }
        let result = self.admit_inner(accepted, now);
        self.outcome(result)
    }
    fn admit_inner(&mut self, accepted: bool, now: u64) -> Result<Vec<SessionEvent>, SessionError> {
        self.check_now(now)?;
        let admission = self.admission.take().ok_or(SessionError::Protocol)?;
        self.input_bytes -= admission.body.len();
        let payload = Envelope::parse(&admission.body)?.payload;
        let releases_rotation = admission.releases_rotation;
        drop(admission.body);
        let mut events = vec![];
        if !accepted {
            self.phase = SessionPhase::Closing;
            self.enqueue(
                TransportKind::Binary,
                message::encode(
                    "client/goodbye",
                    json!({"reason":"another_server"}),
                    self.limits.pending_output_bytes,
                )?,
                Purpose::Goodbye,
            )?;
            return Ok(events);
        }
        let pairing = self.pairing.as_ref().ok_or(SessionError::Protocol)?;
        let checked = pairing
            .validate_activation(&payload)
            .map_err(|_| SessionError::Protocol)?;
        let removed_roles = pairing
            .roles()
            .iter()
            .filter(|r| !checked.roles.contains(r))
            .cloned()
            .collect();
        let activities = checked.activities;
        self.pair_event(PairingEvent::Activate(payload), &mut events)?;
        if self.phase == SessionPhase::Closed || self.phase == SessionPhase::Closing {
            return Ok(events);
        }
        if self.quiet && !releases_rotation {
            // Earlier authenticated messages retain their order before the queued rotation.
            self.phase = SessionPhase::Active;
        } else {
            self.phase = SessionPhase::Active;
            self.quiet = false;
            self.deadline = None;
        }
        events.push(SessionEvent::Activated(ActivationNotice {
            handshake_generation: self.generation,
            roles: self.roles().to_vec(),
            removed_roles,
            activities,
        }));
        self.drain(&mut events)?;
        Ok(events)
    }
    fn rotation(&mut self, payload: Value) -> Result<(), SessionError> {
        if !self.quiet || self.pending_channel.is_some() || self.reassembler.is_assembling() {
            return Err(SessionError::Protocol);
        }
        let old = self.channel.as_ref().ok_or(SessionError::Protocol)?;
        let mut pending = Responder::rehandshake(
            &self.private,
            old.server_public(),
            old.handshake_hash().ok_or(SessionError::Protocol)?,
            self.limits.noise,
        )?;
        let raw = decode_handshake(&payload, self.limits.noise)?;
        let reply = pending.receive_handshake(&raw, &self.credentials())?;
        self.pairing
            .as_mut()
            .ok_or(SessionError::Protocol)?
            .protect(pending.matched_id())
            .map_err(|_| SessionError::Storage)?;
        self.retire_pairing_flight();
        self.pending_channel = Some(pending);
        self.phase = SessionPhase::Quiet;
        self.enqueue(
            TransportKind::Binary,
            handshake_envelope(&reply, self.limits.pending_output_bytes)?,
            Purpose::RotationReply,
        )
    }
    fn return_unstarted(&mut self, events: &mut Vec<SessionEvent>) {
        if self.application.as_ref().is_some_and(|a| !a.started()) {
            let (id, body) = self
                .application
                .take()
                .expect("unstarted application exists")
                .into_parts();
            events.push(SessionEvent::Unsent(ApplicationMessage { id, body }));
        }
    }
    fn credentials(&self) -> Vec<Credential> {
        let mut credentials = Vec::with_capacity(self.catalog().records().len() + 1);
        credentials.push(Credential::Pairing {
            psk: self.pairing_psk,
        });
        credentials.extend(
            self.catalog()
                .records()
                .iter()
                .map(|r| Credential::LongTerm {
                    psk: r.psk,
                    server_public: r.server_public,
                }),
        );
        credentials
    }
    /// Returns unsent producer data on suppression or backpressure. No encryption starts here.
    pub fn send(&mut self, message: ApplicationMessage, now: u64) -> Result<(), SendRejected> {
        let result = (|| {
            self.check_now(now)?;
            if self.phase != SessionPhase::Active || self.quiet {
                return Err(SessionError::Suppressed);
            }
            if self.application.is_some() {
                return Err(SessionError::Busy);
            }
            if message.id == 0 {
                let envelope = Envelope::parse(&message.body)?;
                if envelope.direction() == Some(Direction::Server)
                    || matches!(
                        envelope.kind.as_str(),
                        "noise/handshake"
                            | "client/init"
                            | "client/hello"
                            | "client/pair-init"
                            | "client/pair-finalize"
                            | "pair/abort"
                    )
                {
                    return Err(SessionError::Protocol);
                }
            } else if message.id != 12 {
                return Err(SessionError::Protocol);
            }
            Outgoing::validate(message.id, message.body.len(), self.limits.messages)?;
            let record_bytes =
                Outgoing::first_record_len(message.body.len(), self.limits.messages)?
                    .checked_add(16)
                    .ok_or(SessionError::Budget)?;
            self.check_output(
                message
                    .body
                    .len()
                    .checked_add(record_bytes)
                    .ok_or(SessionError::Budget)?,
            )?;
            Ok(())
        })();
        if let Err(error) = result {
            if !matches!(
                error,
                SessionError::Busy | SessionError::Suppressed | SessionError::Closed
            ) {
                self.close();
            }
            return Err(SendRejected { error, message });
        }
        self.application = Some(
            Outgoing::new(message.id, message.body, self.limits.messages)
                .expect("validated outgoing message"),
        );
        Ok(())
    }
    fn output_bytes(&self) -> usize {
        self.queue.iter().map(|q| q.body.len()).sum::<usize>()
            + self.current.as_ref().map_or(0, |c| c.outgoing.body_bytes())
            + self.application.as_ref().map_or(0, Outgoing::body_bytes)
            + self.outstanding.as_ref().map_or(0, |w| w.bytes)
    }
    fn check_output(&self, additional: usize) -> Result<(), SessionError> {
        if self
            .output_bytes()
            .checked_add(additional)
            .is_none_or(|n| n > self.limits.pending_output_bytes)
        {
            Err(SessionError::Budget)
        } else {
            Ok(())
        }
    }
    fn enqueue(
        &mut self,
        kind: TransportKind,
        body: Vec<u8>,
        purpose: Purpose,
    ) -> Result<(), SessionError> {
        self.check_output(body.len())?;
        if self.queue.len() >= self.limits.pending_messages {
            return Err(SessionError::Budget);
        }
        self.queue.push_back(Queued {
            kind,
            body,
            purpose,
        });
        Ok(())
    }
    /// Obtains exactly one write. Do not request another until reporting its complete transport result.
    pub fn next_write(&mut self, now: u64) -> Result<Option<TransportWrite>, SessionError> {
        if self.phase == SessionPhase::Closed {
            return Ok(None);
        }
        let result = self.next_write_inner(now);
        self.outcome(result)
    }
    fn next_write_inner(&mut self, now: u64) -> Result<Option<TransportWrite>, SessionError> {
        self.check_now(now)?;
        if self.outstanding.is_some() {
            return Ok(None);
        }
        let started = self.application.as_ref().is_some_and(Outgoing::started);
        let (kind, bytes, purpose, last) = if started
            || (self.queue.is_empty()
                && self.phase == SessionPhase::Active
                && !self.quiet
                && self.application.is_some())
        {
            let record_bytes = self
                .application
                .as_ref()
                .and_then(Outgoing::next_record_len)
                .ok_or(SessionError::Protocol)?
                .checked_add(16)
                .ok_or(SessionError::Budget)?;
            self.check_output(record_bytes)?;
            let application = self.application.as_mut().expect("application exists");
            let plain = application.next_record().ok_or(SessionError::Protocol)?;
            let last = !application.has_more();
            let record = self
                .channel
                .as_mut()
                .ok_or(SessionError::Protocol)?
                .encrypt(&plain)?;
            (TransportKind::Binary, record, Purpose::Application, last)
        } else {
            while self.current.is_none() {
                let Some(queued) = self.queue.pop_front() else {
                    return Ok(None);
                };
                let flight = match queued.purpose {
                    Purpose::PairInit(id) => Some((id, FlightPart::Init)),
                    Purpose::PairFinalize(id) => Some((id, FlightPart::Finalize)),
                    _ => None,
                };
                if let Some((id, part)) = flight {
                    if self.quiet || self.admission.is_some() || self.phase == SessionPhase::Closing
                    {
                        continue;
                    }
                    let (authorization, effects) = self
                        .pairing
                        .as_mut()
                        .ok_or(SessionError::Protocol)?
                        .authorize_flight_start(id, part, now);
                    let mut events = vec![];
                    self.apply_pair_effects(effects, &mut events)?;
                    if !events.is_empty() {
                        return Err(SessionError::Protocol);
                    }
                    if authorization != FlightAuthorization::Authorized {
                        continue;
                    }
                }
                if let Purpose::PairAbort {
                    id,
                    activation_response,
                } = queued.purpose
                    && (!self.attempt_current(id) || (self.quiet && !activation_response))
                {
                    continue;
                }
                if queued.kind == TransportKind::Text {
                    let count = queued.body.len();
                    self.check_output(count)?;
                    return self
                        .issue(queued.kind, queued.body, queued.purpose, true)
                        .map(Some);
                }
                self.current = Some(Current {
                    outgoing: Outgoing::new(0, queued.body, self.limits.messages)?,
                    purpose: queued.purpose,
                });
            }
            let record_bytes = self
                .current
                .as_ref()
                .and_then(|c| c.outgoing.next_record_len())
                .ok_or(SessionError::Protocol)?
                .checked_add(16)
                .ok_or(SessionError::Budget)?;
            self.check_output(record_bytes)?;
            let current = self.current.as_mut().expect("current control exists");
            let plain = current
                .outgoing
                .next_record()
                .ok_or(SessionError::Protocol)?;
            let last = !current.outgoing.has_more();
            let purpose = current.purpose;
            let record = self
                .channel
                .as_mut()
                .ok_or(SessionError::Protocol)?
                .encrypt(&plain)?;
            (TransportKind::Binary, record, purpose, last)
        };
        self.check_output(bytes.len())?;
        self.issue(kind, bytes, purpose, last).map(Some)
    }
    fn issue(
        &mut self,
        kind: TransportKind,
        bytes: Vec<u8>,
        purpose: Purpose,
        last: bool,
    ) -> Result<TransportWrite, SessionError> {
        self.serial = self.serial.checked_add(1).ok_or(SessionError::Counter)?;
        let id = WriteId {
            owner: self.owner,
            serial: self.serial,
        };
        self.outstanding = Some(Outstanding {
            id,
            purpose,
            last,
            bytes: bytes.len(),
        });
        Ok(TransportWrite { id, kind, bytes })
    }
    pub fn complete_write(
        &mut self,
        id: WriteId,
        result: WriteResult,
        now: u64,
    ) -> Result<Vec<SessionEvent>, SessionError> {
        if self.outstanding.as_ref().is_none_or(|w| w.id != id) {
            return Ok(vec![]);
        }
        let result = self.complete_write_inner(result, now);
        self.outcome(result)
    }
    fn complete_write_inner(
        &mut self,
        result: WriteResult,
        now: u64,
    ) -> Result<Vec<SessionEvent>, SessionError> {
        self.check_now(now)?;
        if result != WriteResult::Complete {
            return Err(SessionError::Write);
        }
        let write = self.outstanding.take().ok_or(SessionError::Write)?;
        let mut events = vec![];
        if write.last {
            if matches!(write.purpose, Purpose::Application) {
                self.application = None;
            } else {
                self.current = None;
            }
            match write.purpose {
                Purpose::Init => self.phase = SessionPhase::AwaitServerInit,
                Purpose::InitialReply => self.phase = SessionPhase::AwaitServerHello,
                Purpose::Hello => self.phase = SessionPhase::AwaitInitialActivation,
                Purpose::PairInit(id) => {
                    self.pair_event(PairingEvent::PairingInitSent(id, now), &mut events)?;
                }
                Purpose::PairFinalize(id) => {
                    let effects = self
                        .pairing
                        .as_mut()
                        .ok_or(SessionError::Protocol)?
                        .complete_flight_write(id, now);
                    self.apply_pair_effects(effects, &mut events)?;
                }
                Purpose::RotationReply => {
                    let generation = self
                        .generation
                        .checked_add(1)
                        .ok_or(SessionError::Counter)?;
                    let pending = self.pending_channel.take().ok_or(SessionError::Protocol)?;
                    self.pairing
                        .as_mut()
                        .ok_or(SessionError::Protocol)?
                        .transition_handshake(
                            pending.selection().ok_or(SessionError::Protocol)?,
                            generation,
                        )
                        .map_err(|_| SessionError::Storage)?;
                    let mut protected = self.protected.clone();
                    if let Some(id) = pending.matched_id()
                        && !protected.contains(&id)
                    {
                        protected.push(id);
                    }
                    self.pairing
                        .as_mut()
                        .ok_or(SessionError::Protocol)?
                        .replace_protection(protected)
                        .map_err(|_| SessionError::Storage)?;
                    self.channel = Some(pending);
                    self.generation = generation;
                    self.phase = SessionPhase::AwaitNewActivation;
                }
                Purpose::Goodbye => {
                    self.close();
                    events.push(SessionEvent::Closed);
                    return Ok(events);
                }
                Purpose::Application | Purpose::PairAbort { .. } => {}
            }
        }
        self.drain(&mut events)?;
        Ok(events)
    }
    fn prune_queued_pairing(&mut self) {
        let pairing = self.pairing.as_ref();
        self.queue.retain(|queued| match queued.purpose {
            Purpose::PairInit(id) => {
                pairing.is_some_and(|p| p.queued_write_is_current(id, FlightPart::Init))
            }
            Purpose::PairFinalize(id) => {
                pairing.is_some_and(|p| p.queued_write_is_current(id, FlightPart::Finalize))
            }
            _ => true,
        });
    }
    fn retire_pairing_flight(&mut self) {
        if let Some(pairing) = &mut self.pairing {
            pairing.retire_unacknowledged_attempt();
        }
        self.prune_queued_pairing();
    }
    fn pair_event(
        &mut self,
        event: PairingEvent,
        events: &mut Vec<SessionEvent>,
    ) -> Result<(), SessionError> {
        let effects = self
            .pairing
            .as_mut()
            .ok_or(SessionError::Protocol)?
            .handle(event, self.clock);
        self.apply_pair_effects(effects, events)
    }
    fn apply_pair_effects(
        &mut self,
        effects: Vec<PairingEffect>,
        events: &mut Vec<SessionEvent>,
    ) -> Result<(), SessionError> {
        self.prune_queued_pairing();
        for effect in effects {
            match effect {
                PairingEffect::RequestCandidate(id) => {
                    events.push(SessionEvent::RequestCandidate(id))
                }
                PairingEffect::SendFlight(id, flight) => {
                    let [init, finalize] = flight.messages();
                    let init = message::bounded_json(&init, self.limits.pending_output_bytes)?;
                    let finalize =
                        message::bounded_json(&finalize, self.limits.pending_output_bytes)?;
                    self.enqueue(TransportKind::Binary, init, Purpose::PairInit(id))?;
                    self.enqueue(TransportKind::Binary, finalize, Purpose::PairFinalize(id))?;
                }
                PairingEffect::Abort(id, reason) => {
                    let activation_response =
                        self.quiet && self.phase == SessionPhase::AwaitNewActivation;
                    if !self.attempt_current(id) || (self.quiet && !activation_response) {
                        continue;
                    }
                    self.enqueue(
                        TransportKind::Binary,
                        message::encode(
                            "pair/abort",
                            json!({"reason":abort_name(reason)}),
                            self.limits.pending_output_bytes,
                        )?,
                        Purpose::PairAbort {
                            id,
                            activation_response,
                        },
                    )?;
                }
                PairingEffect::CommitRecord(id, update) => {
                    events.push(SessionEvent::CommitRecord(id, update))
                }
                PairingEffect::Finishing(id) => events.push(SessionEvent::Finishing(id)),
                PairingEffect::Goodbye(reason) => {
                    self.phase = SessionPhase::Closing;
                    self.queue.clear();
                    self.return_unstarted(events);
                    self.enqueue(TransportKind::Binary,message::encode("client/goodbye",json!({"reason":match reason{GoodbyeReason::PairingRequired=>"pairing_required",GoodbyeReason::Unauthorized=>"unauthorized"}}),self.limits.pending_output_bytes)?,Purpose::Goodbye)?;
                }
                PairingEffect::Close(_) => {
                    if self.phase != SessionPhase::Closing {
                        return Err(SessionError::Protocol);
                    }
                }
                PairingEffect::DeferUntilCommitted(_) => return Err(SessionError::Protocol),
            }
        }
        Ok(())
    }
    pub fn candidate(
        &mut self,
        id: AttemptId,
        candidate: Candidate,
        now: u64,
    ) -> Result<Vec<SessionEvent>, SessionError> {
        if self.phase == SessionPhase::Closed
            || self.quiet
            || !self.attempt_current(id)
            || self.pairing.as_ref().is_none_or(|p| !p.owns_candidate(id))
        {
            return Ok(vec![]);
        }
        let result = (|| {
            self.check_now(now)?;
            let mut events = vec![];
            self.pair_event(PairingEvent::CandidateReady(id, candidate), &mut events)?;
            Ok(events)
        })();
        self.outcome(result)
    }
    fn attempt_current(&self, id: AttemptId) -> bool {
        id.connection_generation == self.connection_generation
            && id.handshake_generation == self.generation
    }
    pub fn commit_succeeded(
        &mut self,
        id: AttemptId,
        now: u64,
    ) -> Result<Vec<SessionEvent>, SessionError> {
        self.commit_result(id, None, now)
    }
    pub fn commit_failed(
        &mut self,
        id: AttemptId,
        outcome: CommitOutcome,
        now: u64,
    ) -> Result<Vec<SessionEvent>, SessionError> {
        self.commit_result(id, Some(outcome), now)
    }
    fn commit_result(
        &mut self,
        id: AttemptId,
        failure: Option<CommitOutcome>,
        now: u64,
    ) -> Result<Vec<SessionEvent>, SessionError> {
        let Some(pairing) = self.pairing.as_mut() else {
            return Ok(vec![]);
        };
        if !pairing.owns_commit(id) {
            return Ok(vec![]);
        }
        let event = if let Some(outcome) = failure {
            PairingEvent::CommitFailed(id, outcome)
        } else {
            PairingEvent::CommitSucceeded(id)
        };
        let effects = pairing.handle(event, now);
        if effects.iter().any(|e| matches!(e, PairingEffect::Close(_))) {
            self.close();
            return Err(SessionError::Storage);
        }
        if self.phase == SessionPhase::Closed {
            return Ok(vec![]);
        }
        let result = (|| {
            self.check_now(now)?;
            let mut events = vec![];
            self.drain(&mut events)?;
            Ok(events)
        })();
        self.outcome(result)
    }
    pub fn cancel_pairing(&mut self, now: u64) -> Result<Vec<SessionEvent>, SessionError> {
        let result = (|| {
            self.check_now(now)?;
            let mut events = vec![];
            self.pair_event(PairingEvent::Cancel, &mut events)?;
            Ok(events)
        })();
        self.outcome(result)
    }
    pub fn tick(&mut self, now: u64) -> Result<Vec<SessionEvent>, SessionError> {
        if self.phase == SessionPhase::Closed {
            return Ok(vec![]);
        }
        let result = (|| {
            self.check_now(now)?;
            let mut events = vec![];
            // The exchange deadline remains active while old pairing work awaits ordered retirement.
            if self.pairing.is_some() && !self.quiet {
                self.pair_event(PairingEvent::Tick, &mut events)?;
            }
            Ok(events)
        })();
        self.outcome(result)
    }
    /// Drops crypto and uncommitted work. An acknowledged storage operation remains completable.
    pub fn close(&mut self) {
        self.phase = SessionPhase::Closed;
        self.channel = None;
        self.pending_channel = None;
        self.reassembler.close();
        self.raw.clear();
        self.messages.clear();
        self.input_bytes = 0;
        self.queue.clear();
        self.current = None;
        self.application = None;
        self.outstanding = None;
        self.admission = None;
        if let Some(pairing) = &mut self.pairing {
            pairing.handle(PairingEvent::Disconnected, self.clock);
        }
    }
}
impl Drop for ClientSession {
    fn drop(&mut self) {
        self.close();
    }
}
fn handshake_encoded_limit(limits: noise::Limits) -> usize {
    let raw = limits
        .record_bytes
        .min(65535)
        .min(limits.handshake_payload_bytes.min(65487) + 48);
    raw.div_ceil(3) * 4
}
fn decode_handshake(payload: &Value, limits: noise::Limits) -> Result<Vec<u8>, SessionError> {
    let data = payload
        .get("data")
        .and_then(Value::as_str)
        .ok_or(SessionError::Protocol)?;
    if data.len() > handshake_encoded_limit(limits) {
        return Err(SessionError::Budget);
    }
    let raw = URL_SAFE_NO_PAD
        .decode(data)
        .map_err(|_| SessionError::Protocol)?;
    if raw.len() > limits.record_bytes.min(65535)
        || raw.len() > limits.handshake_payload_bytes.min(65487) + 48
    {
        return Err(SessionError::Budget);
    }
    Ok(raw)
}
fn handshake_envelope(record: &[u8], limit: usize) -> Result<Vec<u8>, SessionError> {
    Ok(message::encode(
        "noise/handshake",
        json!({"data":URL_SAFE_NO_PAD.encode(record)}),
        limit,
    )?)
}
fn within_depth(value: &Value, level: usize) -> bool {
    if level > 10 {
        return false;
    }
    match value {
        Value::Object(object) => object
            .values()
            .all(|v| !v.is_object() && !v.is_array() || within_depth(v, level + 1)),
        Value::Array(values) => values
            .iter()
            .all(|v| !v.is_object() && !v.is_array() || within_depth(v, level + 1)),
        _ => true,
    }
}
fn abort_name(reason: AbortReason) -> &'static str {
    match reason {
        AbortReason::MethodNotSupported => "method_not_supported",
        AbortReason::UserCancelled => "user_cancelled",
        AbortReason::AttemptTimeout => "attempt_timeout",
        AbortReason::ConcurrentAttempt => "concurrent_attempt",
        AbortReason::PairingCodeMismatch => "pairing_code_mismatch",
    }
}
fn parse_abort(payload: &Value) -> Result<AbortReason, SessionError> {
    match payload.get("reason").and_then(Value::as_str) {
        Some("method_not_supported") => Ok(AbortReason::MethodNotSupported),
        Some("user_cancelled") => Ok(AbortReason::UserCancelled),
        Some("attempt_timeout") => Ok(AbortReason::AttemptTimeout),
        Some("concurrent_attempt") => Ok(AbortReason::ConcurrentAttempt),
        Some("pairing_code_mismatch") => Ok(AbortReason::PairingCodeMismatch),
        _ => Err(SessionError::Protocol),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> SessionConfig {
        SessionConfig {
            private: [9; 32],
            pairing_psk: [7; 32],
            connection_generation: 1,
            hello: json!({"name":"Fixture","supported_roles":[]}),
            catalog: RecordCatalog::new(5).unwrap(),
            protected: vec![],
            limits: SessionLimits::default(),
        }
    }
    #[test]
    fn generation_exhaustion_closes_before_installing_or_resetting() {
        let mut client = ClientSession::new(config(), 0).unwrap();
        let write = client.next_write(0).unwrap().unwrap();
        client.generation = u64::MAX;
        client.outstanding.as_mut().unwrap().purpose = Purpose::RotationReply;
        assert_eq!(
            client
                .complete_write(write.id, WriteResult::Complete, 1)
                .unwrap_err(),
            SessionError::Counter
        );
        assert_eq!(client.generation, u64::MAX);
        assert_eq!(client.phase(), SessionPhase::Closed);
    }
    #[test]
    fn local_hello_checks_depth_and_size_before_emission() {
        let mut config = config();
        config.hello["device_info"] = json!({"product_name":"x".repeat(17_000)});
        assert!(matches!(
            ClientSession::new(config, 0),
            Err(SessionError::Budget)
        ));
        let mut config = self::config();
        let mut nested = json!({});
        for _ in 0..9 {
            nested = json!({"nested":nested});
        }
        config.hello["deep"] = nested;
        assert!(matches!(
            ClientSession::new(config, 0),
            Err(SessionError::Budget)
        ));
        let mut config = self::config();
        config.hello["supported_pair_methods"] = json!({"static_pairing_code":{}});
        config.hello["unpaired_access"] = json!({"enabled":true});
        let client = ClientSession::new(config, 0).unwrap();
        let hello = Envelope::parse(&client.hello).unwrap();
        assert_eq!(
            hello.payload["supported_pair_methods"],
            json!({"pairing_psk":{}})
        );
        assert_eq!(hello.payload["unpaired_access"], json!({"enabled":false}));
    }
}
