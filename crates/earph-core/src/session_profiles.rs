// ABOUTME: Publishes saved listening intent through the existing Session transaction.
// ABOUTME: Cancels stale saves and bounds catalog, operation, and query retention.
use super::*;
use crate::{
    profile_control::*,
    profiles::{self, ProfileDeviceContext, ProfileObjectHash},
};

type Record = Arc<(CommandEnvelope, Receipt)>;
const BUSY: EngineError = EngineError::new(
    ErrorCode::SourceBusy,
    "another operation owns the profile preparation slot",
);
const CANCELLED: EngineError =
    EngineError::new(ErrorCode::OperationCancelled, "profile save was cancelled");
const STALE_PROFILE: EngineError = EngineError::new(
    ErrorCode::StaleRevision,
    "refresh the profile and Session revision",
);

#[derive(Clone, Default)]
pub(super) struct ProfileControl {
    pub(super) references: Vec<ProfileCatalogReference>,
    records: Arc<Vec<Arc<ProfileRecord>>>,
    pending: Option<ProfileSaveJob>,
    pending_receipt: Option<Record>,
    terminal: Option<Record>,
    worker_request: Option<(RequestId, ProfileSaveBinding)>,
}
impl Session {
    pub fn profile_references(&self) -> &[ProfileCatalogReference] {
        &self.profiles.references
    }
    pub fn restore_profile_records(
        &mut self,
        records: Vec<Arc<ProfileRecord>>,
    ) -> Result<(), EngineError> {
        if records.capacity() > profiles::MAX_PROFILES
            || records.len() != self.profiles.references.len()
        {
            return Err(INVALID);
        }
        for (reference, record) in self.profiles.references.iter().zip(&records) {
            record.validate()?;
            if record.reference() != *reference {
                return Err(INVALID);
            }
        }
        let mut staged = self.clone();
        staged.profiles.records = Arc::new(records);
        staged.validate_profile_storage(None)?;
        self.profiles = staged.profiles;
        Ok(())
    }
    pub fn profile_job(&self) -> Option<ProfileSaveJob> {
        self.profiles.pending.clone()
    }
    pub fn profile_worker_busy(&self) -> bool {
        self.profiles.worker_request.is_some()
    }
    fn profile_save_inspection(&self) -> ProfileSaveInspection {
        ProfileSaveInspection {
            stage: self.profiles.worker_request.as_ref().map(|_| {
                if self.profiles.pending.is_some() {
                    ProfileSaveStage::Saving
                } else {
                    ProfileSaveStage::Retiring
                }
            }),
            request_id: self
                .profiles
                .worker_request
                .as_ref()
                .map(|(id, _)| id.clone()),
            deadline_monotonic_ns: self
                .profiles
                .pending
                .as_ref()
                .map(|j| j.deadline_monotonic_ns),
        }
    }
    fn profile_binding(&self) -> ProfileSaveBinding {
        ProfileSaveBinding {
            owner_id: self.state.owner_id.clone(),
            session_id: self.state.session_id.clone(),
            epochs: self.state.epochs,
            device_epoch: self.state.device_epoch,
            expected_revision: self.state.revision,
        }
    }
    pub(super) fn profile_receipt(&self, id: &RequestId) -> Option<&Record> {
        self.profiles
            .pending_receipt
            .as_ref()
            .filter(|r| &r.0.request_id == id)
            .or_else(|| {
                self.profiles
                    .terminal
                    .as_ref()
                    .filter(|r| &r.0.request_id == id)
            })
    }
    pub(super) fn retain_profile_receipt(&mut self, record: Record) {
        if self
            .profiles
            .pending
            .as_ref()
            .is_some_and(|j| j.request_id == record.0.request_id)
        {
            self.profiles.pending_receipt = Some(record);
        }
    }
    fn settle_profile(
        &mut self,
        status: ReceiptStatus,
        error: Option<EngineError>,
        result: Option<QueryResult>,
    ) {
        self.profiles.pending = None;
        if let Some(prior) = self.profiles.pending_receipt.take() {
            let mut receipt = prior.1.clone();
            receipt.status = status;
            receipt.error = error;
            receipt.result = result;
            receipt.revision = self.state.revision;
            receipt.epochs = self.state.epochs;
            let updated = Arc::new((prior.0.clone(), receipt));
            for slot in &mut self.receipts {
                if slot.0.request_id == updated.0.request_id {
                    *slot = updated.clone();
                }
            }
            self.profiles.terminal = Some(updated);
        }
    }
    pub fn expire_profile_work(&mut self, now_ns: u64) {
        if self
            .profiles
            .pending
            .as_ref()
            .is_some_and(|j| now_ns >= j.deadline_monotonic_ns)
        {
            self.settle_profile(ReceiptStatus::Cancelled, Some(CANCELLED), None);
        }
    }
    pub(super) fn cancel_profile_work(&mut self) {
        if self.profiles.pending.is_some() {
            self.settle_profile(ReceiptStatus::Cancelled, Some(CANCELLED), None);
        }
    }
    pub(super) fn apply_profile_save(
        &mut self,
        command: &CommandEnvelope,
        now_ns: u64,
    ) -> Result<ReceiptStatus, EngineError> {
        if self.profile_worker_busy()
            || self.profiles.pending.is_some()
            || self.graph_job().is_some()
            || self.graph_activation().is_some()
            || self.graph_inspection().prepared.is_some()
            || self.graph_inspection().service.preparing_or_candidate
            || self.local_boundary.is_some()
        {
            return Err(BUSY);
        }
        if command.session_id != self.state.session_id {
            return Err(STALE);
        }
        let (id, draft, graph, expected) = match &command.payload {
            Command::ProfilesCreate { id, draft, graph } => (id, draft, graph, None),
            Command::ProfilesUpdate {
                id,
                draft,
                graph,
                expected_profile_revision,
            } => (id, draft, graph, Some(*expected_profile_revision)),
            _ => return Err(STATE),
        };
        let existing = self.profiles.records.iter().find(|r| &r.profile.id == id);
        let revision = match (expected, existing) {
            (None, None) if self.profiles.records.len() < profiles::MAX_PROFILES => 1,
            (None, None) => return Err(BUDGET),
            (None, Some(_)) => return Err(INVALID),
            (Some(expected), Some(record)) if expected == record.profile.revision => {
                increment(expected)?
            }
            _ => return Err(STALE_PROFILE),
        };
        draft.document(
            id.clone(),
            revision,
            ProfileObjectHash::try_from("0".repeat(64))?,
        )?;
        if !draft.corrections.is_empty() {
            return Err(EngineError::new(
                ErrorCode::CapabilityUnsupported,
                "correction object import is unavailable",
            ));
        }
        self.profiles.pending = Some(ProfileSaveJob {
            request_id: command.request_id.clone(),
            binding: self.profile_binding(),
            id: id.clone(),
            revision,
            draft: draft.clone(),
            graph: graph.clone(),
            deadline_monotonic_ns: now_ns.checked_add(PROFILE_SAVE_DEADLINE_NS).ok_or(BUDGET)?,
        });
        self.profiles.worker_request = Some((command.request_id.clone(), self.profile_binding()));
        self.validate_profile_storage(Some(command))?;
        Ok(ReceiptStatus::Accepted)
    }
    pub fn complete_profile_save(
        &mut self,
        id: &RequestId,
        binding: &ProfileSaveBinding,
        now_ns: u64,
        result: Result<Arc<ProfileRecord>, EngineError>,
        commit: impl FnOnce(&Session) -> Result<(), EngineError>,
    ) -> Result<(), EngineError> {
        // A completion proves compiler and asset resources have retired, even after cancellation.
        if self
            .profiles
            .worker_request
            .as_ref()
            .is_some_and(|(request, expected)| request == id && expected == binding)
        {
            self.profiles.worker_request = None;
        }
        self.expire_profile_work(now_ns);
        let job = self
            .profiles
            .pending
            .as_ref()
            .filter(|j| &j.request_id == id && &j.binding == binding)
            .cloned()
            .ok_or(STALE)?;
        let result = (|| {
            if now_ns < self.state.monotonic_ns {
                return Err(STALE);
            }
            if self.profile_binding() != *binding {
                return Err(STALE_PROFILE);
            }
            let record = result?;
            record.validate()?;
            if record.profile.as_ref()
                != &job
                    .draft
                    .document(job.id.clone(), job.revision, record.profile.graph_sha256)?
                || record.graph != job.graph
            {
                return Err(INVALID);
            }
            let mut staged = self.clone();
            staged.profiles.pending = None;
            let records = Arc::make_mut(&mut staged.profiles.records);
            if let Some(slot) = records.iter_mut().find(|r| r.profile.id == job.id) {
                *slot = record.clone();
            } else {
                records.try_reserve_exact(1).map_err(|_| BUDGET)?;
                records.push(record.clone());
            }
            records.sort_by(|a, b| a.profile.id.cmp(&b.profile.id));
            staged.profiles.references = records.iter().map(|r| r.reference()).collect();
            staged.state.revision = increment(staged.state.revision)?;
            staged.state.event_sequence = staged.next_session_event_sequence(false)?;
            staged.state.monotonic_ns = now_ns;
            staged.settle_profile(
                ReceiptStatus::Applied,
                None,
                Some(QueryResult::ProfileSaved(record)),
            );
            staged.validate_profile_storage(None)?;
            staged.validate_profile_response(
                staged.profiles.terminal.as_ref().ok_or(INVALID)?.as_ref(),
            )?;
            let checkpoint = serde_json::to_vec(&staged.checkpoint()).map_err(|_| INVALID)?;
            if checkpoint.len() > 2 * 1024 * 1024 {
                return Err(BUDGET);
            }
            // Count both old and staged owners conservatively across the commit boundary.
            if self
                .profile_storage_bytes()
                .checked_add(staged.profile_storage_bytes())
                .and_then(|n| n.checked_add(self.profile_environment_bytes().ok()?))
                .is_none_or(|n| n > 256 * 1024 * 1024)
            {
                return Err(BUDGET);
            }
            commit(&staged)?;
            staged.log(Some(id.clone()));
            *self = staged;
            Ok(())
        })();
        if let Err(error) = result {
            self.settle_profile(
                if matches!(
                    error.code,
                    ErrorCode::StaleRevision
                        | ErrorCode::StaleSession
                        | ErrorCode::OperationCancelled
                ) {
                    ReceiptStatus::Cancelled
                } else {
                    ReceiptStatus::Failed
                },
                Some(error),
                None,
            );
        }
        result
    }
    pub(super) fn profile_query(
        &self,
        command: &Command,
    ) -> Result<Option<QueryResult>, EngineError> {
        if self.profiles.references.len() != self.profiles.records.len() {
            return Err(INVALID);
        }
        Ok(match command {
            Command::ProfilesGet { id } => Some(QueryResult::Profile(
                self.profiles
                    .records
                    .iter()
                    .find(|r| &r.profile.id == id)
                    .cloned()
                    .ok_or(INVALID)?,
            )),
            Command::ProfilesList { offset, limit } => {
                if *limit == 0 || *limit > MAX_PROFILE_PAGE {
                    return Err(BUDGET);
                }
                let entries = self
                    .profiles
                    .records
                    .iter()
                    .skip(*offset)
                    .take(*limit)
                    .map(|r| ProfileSummary {
                        id: r.profile.id.clone(),
                        revision: r.profile.revision,
                        name: r.profile.name.clone(),
                        profile_sha256: r.profile_sha256,
                    })
                    .collect();
                let next = offset.saturating_add(*limit);
                Some(QueryResult::Profiles(ProfilePage {
                    entries,
                    total: self.profiles.records.len(),
                    next_offset: (next < self.profiles.records.len()).then_some(next),
                    operation: self.profile_save_inspection(),
                }))
            }
            Command::ProfilesResolve {
                manual_profile_id,
                output,
            } => {
                let documents: Vec<_> = self
                    .profiles
                    .records
                    .iter()
                    .map(|r| (*r.profile).clone())
                    .collect();
                Some(QueryResult::ProfileResolution(profiles::resolve_profile(
                    &documents,
                    &[],
                    manual_profile_id.as_ref(),
                    Some(&ProfileDeviceContext {
                        backend: output.backend,
                        route: output.route,
                        device_id: output.device_id.clone(),
                        verified_identity_sha256: None,
                    }),
                )?))
            }
            _ => None,
        })
    }
    pub(super) fn validate_profile_response(
        &self,
        record: &(CommandEnvelope, Receipt),
    ) -> Result<(), EngineError> {
        if serde_json::to_vec(&serde_json::json!({"receipt":record.1}))
            .map_err(|_| INVALID)?
            .len()
            > 1024 * 1024
        {
            return Err(BUDGET);
        }
        Ok(())
    }
    pub(super) fn validate_profile_storage(
        &self,
        command: Option<&CommandEnvelope>,
    ) -> Result<(), EngineError> {
        validate_catalog(
            &self.profiles.references,
            self.profiles.references.capacity(),
        )?;
        if self.profiles.records.capacity() > profiles::MAX_PROFILES {
            return Err(BUDGET);
        }
        let bytes = self
            .profile_storage_bytes()
            .checked_add(command.map_or(Ok(0), |c| c.profile_capacity_bytes().map(|n| n as u64))?)
            .ok_or(BUDGET)?;
        if bytes > MAX_PROFILE_STORAGE_BYTES
            || bytes
                .checked_add(self.profile_environment_bytes()?)
                .is_none_or(|n| n > 256 * 1024 * 1024)
        {
            return Err(BUDGET);
        }
        Ok(())
    }
    pub(super) fn validate_profile_receipt_admission(
        &self,
        command: &CommandEnvelope,
        receipt: &Receipt,
    ) -> Result<(), EngineError> {
        if !command.payload.is_profile() {
            return Ok(());
        }
        let record = Arc::new((command.clone(), receipt.clone()));
        self.validate_profile_response(&record)?;
        let mut staged = self.clone();
        if staged.receipts.len() == staged.limits.receipts {
            staged.receipts.pop_front();
        }
        staged.receipts.push_back(record.clone());
        staged.retain_profile_receipt(record);
        staged.validate_profile_storage(None)?;
        if self
            .profile_storage_bytes()
            .checked_add(staged.profile_storage_bytes())
            .and_then(|n| n.checked_add(self.profile_environment_bytes().ok()?))
            .is_none_or(|n| n > 256 * 1024 * 1024)
        {
            return Err(BUDGET);
        }
        Ok(())
    }
    /// Conservative retained payload capacity, including repeated Arc owners; excludes allocator overhead.
    pub fn profile_storage_bytes(&self) -> u64 {
        self.profile_storage_capacity()
            .map_or(u64::MAX, |n| n as u64)
    }
    /// Known graph, adapter audio, and source-period storage; excludes opaque native and decoder allocations.
    pub fn profile_environment_bytes(&self) -> Result<u64, EngineError> {
        let mut bytes = self.graph_inspection().service.resident_bytes;
        if let Some(playback) = &self.state.playback {
            bytes = bytes
                .checked_add(
                    playback
                        .backend
                        .as_ref()
                        .map_or(0, |b| b.audio_storage_bytes),
                )
                .ok_or(BUDGET)?;
            if let Some(output) = &self.state.output {
                bytes = bytes
                    .checked_add(playback.format.storage_bytes(output.period_frames as u64)? as u64)
                    .ok_or(BUDGET)?;
            }
        }
        Ok(bytes)
    }
    fn profile_storage_capacity(&self) -> Result<usize, EngineError> {
        if self.profiles.references.is_empty()
            && self.profiles.records.is_empty()
            && self.profiles.pending.is_none()
            && self.profiles.pending_receipt.is_none()
            && self.profiles.terminal.is_none()
            && self.profiles.worker_request.is_none()
            && !self.receipts.iter().any(|r| r.0.payload.is_profile())
        {
            return Ok(0);
        }
        let mut bytes = size_of::<ProfileControl>()
            + self.profiles.references.capacity() * size_of::<ProfileCatalogReference>()
            + self.profiles.records.capacity() * size_of::<Arc<ProfileRecord>>();
        if let Some((id, _)) = &self.profiles.worker_request {
            bytes = bytes.checked_add(id.capacity_bytes() + 512).ok_or(BUDGET)?;
        }
        for reference in &self.profiles.references {
            bytes = bytes
                .checked_add(reference.id.capacity_bytes())
                .ok_or(BUDGET)?;
        }
        for record in self.profiles.records.iter() {
            bytes = bytes
                .checked_add(record.retained_capacity_bytes()?)
                .ok_or(BUDGET)?;
        }
        for record in self
            .receipts
            .iter()
            .chain(self.profiles.pending_receipt.iter())
            .chain(self.profiles.terminal.iter())
        {
            if record.0.payload.is_profile() {
                bytes = bytes
                    .checked_add(record.0.profile_capacity_bytes()?)
                    .ok_or(BUDGET)?;
                let result = match &record.1.result {
                    Some(QueryResult::Profile(r) | QueryResult::ProfileSaved(r)) => {
                        r.retained_capacity_bytes()?
                    }
                    Some(QueryResult::Profiles(page)) => {
                        size_of::<ProfilePage>()
                            + page
                                .operation
                                .request_id
                                .as_ref()
                                .map_or(0, RequestId::capacity_bytes)
                            + page.entries.capacity() * size_of::<ProfileSummary>()
                            + page
                                .entries
                                .iter()
                                .map(|s| s.id.capacity_bytes() + s.name.capacity())
                                .sum::<usize>()
                    }
                    Some(QueryResult::ProfileResolution(
                        profiles::ProfileResolution::Selected { profile, .. },
                    )) => profile.id.capacity_bytes(),
                    Some(QueryResult::ProfileResolution(
                        profiles::ProfileResolution::SelectionRequired { candidates, .. },
                    )) => {
                        candidates.capacity() * size_of::<profiles::ProfileRevision>()
                            + candidates
                                .iter()
                                .map(|r| r.id.capacity_bytes())
                                .sum::<usize>()
                    }
                    _ => 0,
                };
                bytes = bytes
                    .checked_add(result + size_of::<Receipt>() + 128)
                    .ok_or(BUDGET)?;
            }
        }
        if let Some(job) = &self.profiles.pending {
            bytes = bytes
                .checked_add(
                    job.draft.retained_capacity_bytes()?
                        + job.graph.graph.retained_capacity_bytes()?
                        + size_of::<ProfileSaveJob>()
                        + 1024,
                )
                .ok_or(BUDGET)?;
        }
        Ok(bytes)
    }
}
