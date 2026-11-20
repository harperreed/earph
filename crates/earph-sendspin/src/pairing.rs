// ABOUTME: Plans protected pairing records and reduces authenticated Pairing PSK events.
// ABOUTME: Fences local work and grants no persistence, transport, or playback authority.
use crate::noise::{Category, SENTINEL, Selection};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingError {
    Capacity,
    InvalidCatalog,
    AdmissionInvariant,
    CounterExhausted,
}
impl fmt::Display for PairingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "pairing {self:?}")
    }
}
impl std::error::Error for PairingError {}

/// The protocol label is identical to the frozen Noise credential selector's label.
pub fn psk_id(psk: &[u8; 32]) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"sendspin-psk-id-v1");
    hash.update(psk);
    hash.finalize().into()
}

#[derive(Clone, PartialEq, Eq)]
pub struct PairingRecord {
    pub server_public: [u8; 32],
    pub psk: [u8; 32],
    pub last_used_order: u64,
}
impl PairingRecord {
    pub fn psk_id(&self) -> [u8; 32] {
        psk_id(&self.psk)
    }
}
impl fmt::Debug for PairingRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairingRecord([REDACTED])")
    }
}

/// Superseded records stay in this single lookup catalog while a connection protects them.
#[derive(Clone, PartialEq, Eq)]
pub struct RecordCatalog {
    capacity: usize,
    records: Vec<PairingRecord>,
    superseded: Vec<[u8; 32]>,
}
impl fmt::Debug for RecordCatalog {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecordCatalog")
            .field("capacity", &self.capacity)
            .field("records", &"[REDACTED]")
            .finish()
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordUpdate {
    pub replacement: RecordCatalog,
}
impl RecordCatalog {
    pub fn new(capacity: usize) -> Result<Self, PairingError> {
        Self::from_committed(capacity, vec![], vec![])
    }
    /// Restore only a backend's validated committed snapshot, including superseded identifiers.
    pub fn from_committed(
        capacity: usize,
        records: Vec<PairingRecord>,
        superseded: Vec<[u8; 32]>,
    ) -> Result<Self, PairingError> {
        if capacity < 5 {
            return Err(PairingError::Capacity);
        }
        let ids: BTreeSet<_> = records.iter().map(PairingRecord::psk_id).collect();
        let stale: BTreeSet<_> = superseded.iter().copied().collect();
        if records.len() > capacity
            || ids.len() != records.len()
            || stale.len() != superseded.len()
            || !stale.is_subset(&ids)
            || records.iter().any(|r| r.psk == SENTINEL)
        {
            return Err(PairingError::InvalidCatalog);
        }
        let mut current_servers = BTreeSet::new();
        for record in &records {
            if !stale.contains(&record.psk_id()) && !current_servers.insert(record.server_public) {
                return Err(PairingError::InvalidCatalog);
            }
        }
        Ok(Self {
            capacity,
            records,
            superseded,
        })
    }
    pub fn records(&self) -> &[PairingRecord] {
        &self.records
    }
    pub fn capacity(&self) -> usize {
        self.capacity
    }
    pub fn superseded_ids(&self) -> &[[u8; 32]] {
        &self.superseded
    }
    /// The owner must also cap all open paired connections below capacity, including shared credentials.
    pub fn ensure_admission(&self, protected: &[[u8; 32]]) -> Result<(), PairingError> {
        let unique: BTreeSet<_> = protected.iter().copied().collect();
        if unique.len() >= self.capacity
            || (self.records.len() >= self.capacity
                && self.records.iter().all(|r| unique.contains(&r.psk_id())))
        {
            return Err(PairingError::AdmissionInvariant);
        }
        Ok(())
    }
    pub fn next_order(&self) -> Result<u64, PairingError> {
        self.records
            .iter()
            .map(|r| r.last_used_order)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(PairingError::CounterExhausted)
    }
    pub fn plan_replace(
        &self,
        record: PairingRecord,
        protected: &[[u8; 32]],
    ) -> Result<RecordUpdate, PairingError> {
        self.ensure_admission(protected)?;
        if record.psk == SENTINEL || self.records.iter().any(|r| r.psk_id() == record.psk_id()) {
            return Err(PairingError::InvalidCatalog);
        }
        let mut replacement = self.clone();
        for existing in &self.records {
            if existing.server_public == record.server_public {
                replacement.superseded.push(existing.psk_id());
            }
        }
        replacement.superseded.sort();
        replacement.superseded.dedup();
        let new_id = record.psk_id();
        replacement.records.push(record);
        replacement.prune(protected, Some(new_id))?;
        Ok(RecordUpdate { replacement })
    }
    pub fn plan_release(&self, protected: &[[u8; 32]]) -> Result<RecordUpdate, PairingError> {
        let mut replacement = self.clone();
        replacement.prune(protected, None)?;
        Ok(RecordUpdate { replacement })
    }
    pub fn plan_mark_used(&self, id: [u8; 32], order: u64) -> Result<RecordUpdate, PairingError> {
        let mut replacement = self.clone();
        let record = replacement
            .records
            .iter_mut()
            .find(|r| r.psk_id() == id)
            .ok_or(PairingError::InvalidCatalog)?;
        if order < record.last_used_order {
            return Err(PairingError::InvalidCatalog);
        }
        record.last_used_order = order;
        Ok(RecordUpdate { replacement })
    }
    fn prune(
        &mut self,
        protected: &[[u8; 32]],
        new_id: Option<[u8; 32]>,
    ) -> Result<(), PairingError> {
        self.records
            .retain(|r| !self.superseded.contains(&r.psk_id()) || protected.contains(&r.psk_id()));
        while self.records.len() > self.capacity {
            let victim = self
                .records
                .iter()
                .enumerate()
                .filter(|(_, r)| Some(r.psk_id()) != new_id && !protected.contains(&r.psk_id()))
                .min_by_key(|(_, r)| (r.last_used_order, r.psk_id()))
                .map(|(i, _)| i)
                .ok_or(PairingError::AdmissionInvariant)?;
            self.records.remove(victim);
        }
        self.superseded
            .retain(|id| self.records.iter().any(|r| r.psk_id() == *id));
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AttemptId {
    pub connection_generation: u64,
    pub handshake_generation: u64,
    pub pairing_index: u32,
}
pub struct PairingContext {
    pub server_public: [u8; 32],
    /// Supply only a selection confirmed by authenticated server transport traffic.
    pub selection: Selection,
    pub connection_generation: u64,
    pub handshake_generation: u64,
    /// The persistent device Pairing PSK remains available outside an attempt.
    pub pairing_psk: [u8; 32],
}
impl fmt::Debug for PairingContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairingContext([REDACTED])")
    }
}

pub struct Candidate([u8; 32]);
impl Candidate {
    /// The later credential adapter supplies a fresh CSPRNG value for each request.
    pub fn new(psk: [u8; 32]) -> Self {
        Self(psk)
    }
}
impl fmt::Debug for Candidate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Candidate([REDACTED])")
    }
}
#[derive(Clone, PartialEq, Eq)]
pub struct PairingFlight {
    pairing_index: u32,
    psk: [u8; 32],
}
impl PairingFlight {
    /// Explicit wire export. Write both messages consecutively through the single ordered writer.
    pub fn messages(&self) -> [Value; 2] {
        [
            json!({"type":"client/pair-init","payload":{"pairing_index":self.pairing_index}}),
            json!({"type":"client/pair-finalize","payload":{"long_term_psk":URL_SAFE_NO_PAD.encode(self.psk)}}),
        ]
    }
}
impl fmt::Debug for PairingFlight {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairingFlight([REDACTED])")
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AbortReason {
    MethodNotSupported,
    UserCancelled,
    AttemptTimeout,
    ConcurrentAttempt,
    PairingCodeMismatch,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoodbyeReason {
    PairingRequired,
    Unauthorized,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseReason {
    Protocol,
    Unauthorized,
    Clock,
    CounterExhausted,
    AdmissionInvariant,
    SendFailed,
    Disconnected,
    ConcurrentAttempt,
    StorageUnchanged,
    StorageRecoveryRequired,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitOutcome {
    Unchanged,
    Uncertain,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairingPhase {
    Idle,
    AwaitCandidate,
    FlightQueued,
    AwaitServerFinalize,
    Committing,
    RecordStored,
    Discarding,
    Closed,
}

pub enum PairingEvent {
    /// Authenticated server/activate payload. Validate direction and reassembly before this call.
    Activate(Value),
    CandidateReady(AttemptId, Candidate),
    /// Report completed init emission before starting the finalize write.
    PairingInitSent(AttemptId, u64),
    /// Fence cancellation before starting a write that may reveal the new PSK.
    PairingFinalizeStarted(AttemptId),
    /// Report the init emission clock, after both messages finish on the ordered writer.
    PairingFlightSent(AttemptId, u64),
    ServerFinalize(Value),
    RemoteAbort(AbortReason),
    UnexpectedPairingMessage,
    CommitSucceeded(AttemptId),
    CommitFailed(AttemptId, CommitOutcome),
    SendFailed(AttemptId),
    Cancel,
    Disconnected,
    Tick,
}
impl fmt::Debug for PairingEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairingEvent([REDACTED])")
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PairingEffect {
    RequestCandidate(AttemptId),
    SendFlight(AttemptId, PairingFlight),
    Abort(AttemptId, AbortReason),
    CommitRecord(AttemptId, RecordUpdate),
    Finishing(AttemptId),
    /// Keep this event in the owner's ordered queue and redeliver after commit completion.
    DeferUntilCommitted(AttemptId),
    Goodbye(GoodbyeReason),
    Close(CloseReason),
}
struct Staged {
    id: AttemptId,
    update: RecordUpdate,
    queued_ms: u64,
    first_sent_ms: Option<u64>,
    finalize_started: bool,
}
pub struct PairingClient {
    context: PairingContext,
    catalog: RecordCatalog,
    protected: Vec<[u8; 32]>,
    phase: PairingPhase,
    index: u32,
    active: Option<AttemptId>,
    staged: Option<Staged>,
    pending_commit: Option<(AttemptId, RecordUpdate)>,
    clock_ms: Option<u64>,
    recovery_required: bool,
    roles: Vec<String>,
}
impl fmt::Debug for PairingClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PairingClient")
            .field("phase", &self.phase)
            .field("credentials", &"[REDACTED]")
            .finish()
    }
}
impl PairingClient {
    pub fn new(
        context: PairingContext,
        catalog: RecordCatalog,
        protected: Vec<[u8; 32]>,
    ) -> Result<Self, PairingError> {
        catalog.ensure_admission(&protected)?;
        if context.pairing_psk == SENTINEL
            || (context.selection.sentinel_fallback
                && context.selection.category != Category::Sentinel)
            || catalog
                .records()
                .iter()
                .any(|r| r.psk == context.pairing_psk)
        {
            return Err(PairingError::InvalidCatalog);
        }
        catalog.next_order()?;
        Ok(Self {
            context,
            catalog,
            protected,
            phase: PairingPhase::Idle,
            index: 0,
            active: None,
            staged: None,
            pending_commit: None,
            clock_ms: None,
            recovery_required: false,
            roles: vec![],
        })
    }
    pub fn phase(&self) -> PairingPhase {
        self.phase
    }
    pub fn catalog(&self) -> &RecordCatalog {
        &self.catalog
    }
    pub fn selection(&self) -> Selection {
        self.context.selection
    }
    pub fn is_paired(&self) -> bool {
        self.context.selection.category == Category::LongTerm
    }
    pub fn recovery_required(&self) -> bool {
        self.recovery_required
    }

    /// Keep protection fixed throughout an attempt. The owner serializes all catalog writers.
    pub fn handle(&mut self, event: PairingEvent, now_ms: u64) -> Vec<PairingEffect> {
        // Acknowledged durable work must resolve even after a protocol close or clock failure.
        match &event {
            PairingEvent::CommitSucceeded(id)
                if self
                    .pending_commit
                    .as_ref()
                    .is_some_and(|(pending, _)| pending == id) =>
            {
                let (_, update) = self.pending_commit.take().expect("matched pending commit");
                self.catalog = update.replacement;
                self.active = None;
                if self.phase != PairingPhase::Closed {
                    self.phase = PairingPhase::RecordStored;
                }
                return vec![];
            }
            PairingEvent::CommitFailed(id, outcome)
                if self
                    .pending_commit
                    .as_ref()
                    .is_some_and(|(pending, _)| pending == id) =>
            {
                self.pending_commit = None;
                self.recovery_required = *outcome == CommitOutcome::Uncertain;
                return self.close(if self.recovery_required {
                    CloseReason::StorageRecoveryRequired
                } else {
                    CloseReason::StorageUnchanged
                });
            }
            PairingEvent::CommitSucceeded(_) | PairingEvent::CommitFailed(..) => return vec![],
            _ => {}
        }
        if self.phase == PairingPhase::Closed {
            return vec![];
        }
        // Superseded callbacks have no state or clock authority.
        if let PairingEvent::CandidateReady(id, _)
        | PairingEvent::PairingInitSent(id, _)
        | PairingEvent::PairingFinalizeStarted(id)
        | PairingEvent::PairingFlightSent(id, _)
        | PairingEvent::SendFailed(id) = &event
            && self.active != Some(*id)
        {
            return vec![];
        }
        if self.clock_ms.is_some_and(|previous| now_ms < previous) {
            return self.close(CloseReason::Clock);
        }
        self.clock_ms = Some(now_ms);
        if let Some((id, _)) = &self.pending_commit {
            if matches!(
                event,
                PairingEvent::Activate(_) | PairingEvent::Disconnected
            ) {
                return vec![PairingEffect::DeferUntilCommitted(*id)];
            }
            if matches!(event, PairingEvent::Cancel) {
                return vec![PairingEffect::Finishing(*id)];
            }
            if matches!(event, PairingEvent::RemoteAbort(_)) {
                return vec![PairingEffect::DeferUntilCommitted(*id)];
            }
        }
        if !matches!(
            event,
            PairingEvent::Disconnected | PairingEvent::SendFailed(_) | PairingEvent::RemoteAbort(_)
        ) && self
            .staged
            .as_ref()
            .and_then(|s| s.first_sent_ms)
            .is_some_and(|first| now_ms - first >= 120_000)
        {
            let id = self.active.expect("staged attempt has an ID");
            self.discard();
            // A new activation still owns its position in the authenticated wire queue.
            let mut effects = vec![PairingEffect::Abort(id, AbortReason::AttemptTimeout)];
            if let PairingEvent::Activate(payload) = event {
                effects.extend(self.activate(payload));
            }
            return effects;
        }
        match event {
            PairingEvent::Activate(payload) => self.activate(payload),
            PairingEvent::CandidateReady(id, candidate) => {
                if self.phase != PairingPhase::AwaitCandidate {
                    return vec![];
                }
                if candidate.0 == SENTINEL
                    || candidate.0 == self.context.pairing_psk
                    || self.catalog.records().iter().any(|r| r.psk == candidate.0)
                {
                    return vec![PairingEffect::RequestCandidate(id)];
                }
                let record = PairingRecord {
                    server_public: self.context.server_public,
                    psk: candidate.0,
                    last_used_order: match self.catalog.next_order() {
                        Ok(order) => order,
                        Err(_) => return self.close(CloseReason::CounterExhausted),
                    },
                };
                let update = match self.catalog.plan_replace(record, &self.protected) {
                    Ok(update) => update,
                    Err(_) => return self.close(CloseReason::AdmissionInvariant),
                };
                self.staged = Some(Staged {
                    id,
                    update,
                    queued_ms: now_ms,
                    first_sent_ms: None,
                    finalize_started: false,
                });
                self.phase = PairingPhase::FlightQueued;
                vec![PairingEffect::SendFlight(
                    id,
                    PairingFlight {
                        pairing_index: id.pairing_index,
                        psk: candidate.0,
                    },
                )]
            }
            PairingEvent::PairingInitSent(_, first) => {
                if self.phase != PairingPhase::FlightQueued {
                    return self.close(CloseReason::Protocol);
                }
                let stage = self.staged.as_mut().expect("queued flight has staging");
                if stage.first_sent_ms.is_some() {
                    return self.close(CloseReason::Protocol);
                }
                if first < stage.queued_ms || first > now_ms {
                    return self.close(CloseReason::Clock);
                }
                stage.first_sent_ms = Some(first);
                vec![]
            }
            PairingEvent::PairingFinalizeStarted(_) => {
                if self.phase != PairingPhase::FlightQueued {
                    return self.close(CloseReason::Protocol);
                }
                let stage = self.staged.as_mut().expect("queued flight has staging");
                if stage.first_sent_ms.is_none() || stage.finalize_started {
                    return self.close(CloseReason::Protocol);
                }
                stage.finalize_started = true;
                vec![]
            }
            PairingEvent::PairingFlightSent(_, first) => {
                if self.phase != PairingPhase::FlightQueued {
                    return self.close(CloseReason::Protocol);
                }
                let stage = self.staged.as_ref().expect("queued flight has staging");
                if stage.first_sent_ms != Some(first) || !stage.finalize_started {
                    return self.close(CloseReason::Protocol);
                }
                self.phase = PairingPhase::AwaitServerFinalize;
                if now_ms - first >= 120_000 {
                    let id = stage.id;
                    self.discard();
                    vec![PairingEffect::Abort(id, AbortReason::AttemptTimeout)]
                } else {
                    vec![]
                }
            }
            PairingEvent::ServerFinalize(payload) => {
                if self.phase == PairingPhase::Discarding {
                    return vec![];
                }
                if self.phase != PairingPhase::AwaitServerFinalize || !payload.is_object() {
                    return self.close(CloseReason::Protocol);
                }
                let stage = self.staged.take().expect("waiting for ack has staging");
                self.phase = PairingPhase::Committing;
                self.pending_commit = Some((stage.id, stage.update.clone()));
                vec![PairingEffect::CommitRecord(stage.id, stage.update)]
            }
            PairingEvent::UnexpectedPairingMessage => {
                if self.phase == PairingPhase::Discarding {
                    vec![]
                } else {
                    self.close(CloseReason::Protocol)
                }
            }
            PairingEvent::RemoteAbort(reason) => {
                if self.active.is_none() {
                    return vec![];
                }
                self.discard();
                if reason == AbortReason::ConcurrentAttempt {
                    self.close(CloseReason::ConcurrentAttempt)
                } else {
                    vec![]
                }
            }
            PairingEvent::Cancel => {
                let Some(id) = self.active else {
                    return vec![];
                };
                if self.phase == PairingPhase::AwaitCandidate
                    || self.staged.as_ref().is_some_and(|s| !s.finalize_started)
                {
                    self.discard();
                    vec![PairingEffect::Abort(id, AbortReason::UserCancelled)]
                } else {
                    vec![PairingEffect::Finishing(id)]
                }
            }
            PairingEvent::SendFailed(_) => self.close(CloseReason::SendFailed),
            PairingEvent::Disconnected => self.close(CloseReason::Disconnected),
            PairingEvent::Tick
            | PairingEvent::CommitSucceeded(_)
            | PairingEvent::CommitFailed(..) => vec![],
        }
    }
    fn activate(&mut self, payload: Value) -> Vec<PairingEffect> {
        let Some(object) = payload.as_object() else {
            return self.close(CloseReason::Protocol);
        };
        let Some(activities) = object.get("activities").and_then(Value::as_array) else {
            return self.close(CloseReason::Protocol);
        };
        let Some(activities) = activities
            .iter()
            .map(Value::as_str)
            .collect::<Option<Vec<_>>>()
        else {
            return self.close(CloseReason::Protocol);
        };
        if activities.iter().collect::<BTreeSet<_>>().len() != activities.len() {
            return self.close(CloseReason::Protocol);
        }
        let pairing = activities.contains(&"pairing");
        self.discard();
        let id = if pairing {
            self.index = match self.index.checked_add(1) {
                Some(index) => index,
                None => return self.close(CloseReason::CounterExhausted),
            };
            Some(AttemptId {
                connection_generation: self.context.connection_generation,
                handshake_generation: self.context.handshake_generation,
                pairing_index: self.index,
            })
        } else {
            None
        };
        let explicit_roles = object.get("active_roles");
        let mut roles = if let Some(value) = explicit_roles {
            let Some(values) = value.as_array() else {
                return self.close(CloseReason::Protocol);
            };
            let Some(roles) = values
                .iter()
                .map(|v| v.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
            else {
                return self.close(CloseReason::Protocol);
            };
            if roles.iter().collect::<BTreeSet<_>>().len() != roles.len() {
                return self.close(CloseReason::Protocol);
            }
            roles
        } else {
            self.roles.clone()
        };
        let paired = self.is_paired();
        let allowed = activities
            .iter()
            .all(|a| matches!(*a, "pairing" | "playback"))
            && !(paired && pairing);
        let method = object
            .get("pairing")
            .and_then(|v| v.get("method"))
            .and_then(Value::as_str);
        let format = object.get("pairing").and_then(|v| v.get("format"));
        if pairing && (method.is_none() || format.is_some_and(|value| !value.is_string())) {
            return self.close(CloseReason::Protocol);
        }
        let method_allowed = !pairing
            || (self.context.selection.category == Category::Pairing
                && method == Some("pairing_psk")
                && format.is_none());
        let playback_capable = paired && allowed;
        if !playback_capable && explicit_roles.is_none() {
            roles.clear();
        }
        let requires_unpaired = !paired && (activities.contains(&"playback") || !roles.is_empty());
        if requires_unpaired && allowed && method_allowed {
            return self.goodbye(GoodbyeReason::PairingRequired);
        }
        if !allowed || requires_unpaired {
            return self.goodbye(GoodbyeReason::Unauthorized);
        }
        self.roles = roles;
        if let Some(id) = id {
            if !method_allowed {
                return vec![PairingEffect::Abort(id, AbortReason::MethodNotSupported)];
            }
            if self.catalog.ensure_admission(&self.protected).is_err() {
                return self.close(CloseReason::AdmissionInvariant);
            }
            self.active = Some(id);
            self.phase = PairingPhase::AwaitCandidate;
            vec![PairingEffect::RequestCandidate(id)]
        } else {
            vec![]
        }
    }
    fn discard(&mut self) {
        self.staged = None;
        self.active = None;
        self.phase = PairingPhase::Discarding;
    }
    fn close(&mut self, reason: CloseReason) -> Vec<PairingEffect> {
        self.staged = None;
        self.active = None;
        self.phase = PairingPhase::Closed;
        vec![PairingEffect::Close(reason)]
    }
    fn goodbye(&mut self, reason: GoodbyeReason) -> Vec<PairingEffect> {
        let mut effects = vec![PairingEffect::Goodbye(reason)];
        effects.extend(self.close(CloseReason::Unauthorized));
        effects
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn activation_counter_exhaustion_closes_without_wrapping() {
        let mut client = PairingClient::new(
            PairingContext {
                server_public: [20; 32],
                selection: Selection {
                    category: Category::Pairing,
                    sentinel_fallback: false,
                },
                connection_generation: 1,
                handshake_generation: 1,
                pairing_psk: [7; 32],
            },
            RecordCatalog::new(5).unwrap(),
            vec![],
        )
        .unwrap();
        client.index = u32::MAX;
        assert_eq!(
            client.handle(
                PairingEvent::Activate(
                    json!({"activities":["pairing"],"pairing":{"method":"pairing_psk"}})
                ),
                0
            ),
            vec![PairingEffect::Close(CloseReason::CounterExhausted)]
        );
        assert_eq!(client.index, u32::MAX);
    }
}
