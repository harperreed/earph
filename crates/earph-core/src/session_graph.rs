// ABOUTME: Applies graph jobs and one-use activation tokens through Session.
// ABOUTME: Keeps graph receipts separate from pending transport operations.
use super::*;
use crate::graph::GraphProcessingRequest;
use std::collections::BTreeMap;

type Record = Arc<(CommandEnvelope, Receipt)>;
const BUSY: EngineError = EngineError::new(
    ErrorCode::SourceBusy,
    "another graph operation owns the preparation slot",
);
const BLOCKED: EngineError = EngineError::new(
    ErrorCode::SafetyPolicyBlocked,
    "prepare and activate the configured graph before unmuting",
);

#[derive(Clone)]
struct Job {
    request: GraphJobRequest,
    record: Option<Record>,
}
#[derive(Clone)]
struct Ready {
    prepared: GraphPrepared,
    intent: Arc<GraphIntent>,
    record: Option<Record>,
}
#[derive(Clone)]
struct Activation {
    request: GraphActivationRequest,
    intent: Arc<GraphIntent>,
    record: Option<Record>,
    readiness: Arc<BTreeMap<QueueEntryId, crate::queue::PreparationStatus>>,
}
#[derive(Clone, Default)]
pub(super) struct GraphControl {
    pub(super) configured: Option<Arc<GraphIntent>>,
    pub(super) active: Option<Arc<GraphPreview>>,
    job: Option<Job>,
    ready: Option<Ready>,
    activation: Option<Activation>,
    serial: u64,
    terminal: Option<Record>,
    pub(super) recovery_required: bool,
    service: GraphServiceObservation,
}
impl Session {
    pub(super) fn defer_graph_readiness(
        &mut self,
        id: &QueueEntryId,
        status: crate::queue::PreparationStatus,
    ) -> Result<bool, EngineError> {
        if !matches!(status, crate::queue::PreparationStatus::Ready { .. })
            || self.graph.activation.is_none()
        {
            return Ok(false);
        }
        let entries = self.queue.entries();
        if !entries.iter().any(|entry| &entry.id == id) {
            return Err(EngineError::new(
                ErrorCode::AssetInvalid,
                "invalid queue entry",
            ));
        }
        let activation = self.graph.activation.as_mut().ok_or(STALE)?;
        let readiness = Arc::make_mut(&mut activation.readiness);
        readiness.retain(|id, _| entries.iter().any(|entry| &entry.id == id));
        readiness.insert(id.clone(), status);
        Ok(true)
    }
    pub(super) fn next_session_event_sequence(&self, terminal: bool) -> Result<u64, EngineError> {
        if !terminal
            && self.graph.activation.is_some()
            && self.state.event_sequence.checked_add(2).is_none()
        {
            return Err(EngineError::new(
                ErrorCode::ResourceBudgetExceeded,
                "graph activation reserves the final event slot",
            ));
        }
        increment(self.state.event_sequence)
    }
    pub(super) fn clear_deferred_graph_readiness(&mut self, id: &QueueEntryId) {
        if let Some(activation) = self.graph.activation.as_mut()
            && activation.readiness.contains_key(id)
        {
            Arc::make_mut(&mut activation.readiness).remove(id);
        }
    }
    pub fn graph_inspection(&self) -> GraphInspection {
        GraphInspection {
            service: self.graph.service,
            recovery_required: self.graph.recovery_required,
            graph_epoch: self.state.graph_epoch,
            active: self.graph.active.clone(),
            configured: self.graph.configured.clone(),
            pending_request_id: self
                .graph
                .job
                .as_ref()
                .map(|j| j.request.request_id.clone())
                .or_else(|| {
                    self.graph
                        .activation
                        .as_ref()
                        .map(|a| a.request.request_id.clone())
                }),
            pending_stage: self
                .graph
                .job
                .as_ref()
                .map(|j| {
                    if j.request.validate_only {
                        GraphOperationStage::Validating
                    } else {
                        GraphOperationStage::Preparing
                    }
                })
                .or_else(|| {
                    self.graph
                        .activation
                        .as_ref()
                        .map(|_| GraphOperationStage::Activating)
                }),
            prepared: self.graph.ready.as_ref().map(|r| r.prepared.clone()),
        }
    }
    pub fn graph_job(&self) -> Option<GraphJobRequest> {
        self.graph.job.as_ref().map(|j| j.request.clone())
    }
    pub(super) fn bind_profile_graph_job(
        &mut self,
        binding: Arc<crate::profile_control::ProfileApplicationBinding>,
    ) -> Result<(), EngineError> {
        self.graph
            .job
            .as_mut()
            .ok_or(STALE)?
            .request
            .profile_application = Some(binding);
        Ok(())
    }
    pub fn graph_activation(&self) -> Option<GraphActivationRequest> {
        self.graph.activation.as_ref().map(|a| a.request.clone())
    }
    pub fn observe_graph_resources(
        &mut self,
        observation: GraphServiceObservation,
        now_ns: u64,
    ) -> Result<(), EngineError> {
        if observation == self.graph.service {
            return Ok(());
        }
        if now_ns < self.state.monotonic_ns {
            return Err(STALE);
        }
        let sequence = match self.next_session_event_sequence(false) {
            Ok(sequence) => sequence,
            Err(_) if self.graph.activation.is_some() => return Ok(()),
            Err(error) => return Err(error),
        };
        self.graph.service = observation;
        self.state.event_sequence = sequence;
        self.state.monotonic_ns = now_ns;
        self.log(None);
        Ok(())
    }
    pub fn submit_graph(&mut self, command: CommandEnvelope, now_ns: u64) -> Receipt {
        self.submit_with_graph_effects(command, now_ns, |_| Err(STATE), |_| Ok(()))
    }
    fn graph_binding(&self) -> Result<GraphBinding, EngineError> {
        Ok(GraphBinding {
            owner_id: self.state.owner_id.clone().ok_or(STATE)?,
            session_id: self.state.session_id.clone().ok_or(STATE)?,
            epochs: self.state.epochs,
            device_epoch: self.state.device_epoch,
            expected_revision: self.state.revision,
        })
    }
    pub(super) fn live_graph_receipt(&self, id: &RequestId) -> Option<&Record> {
        self.graph
            .job
            .as_ref()
            .and_then(|j| j.record.as_ref())
            .filter(|r| &r.0.request_id == id)
            .or_else(|| {
                self.graph
                    .ready
                    .as_ref()
                    .and_then(|r| r.record.as_ref())
                    .filter(|r| &r.0.request_id == id)
            })
            .or_else(|| {
                self.graph
                    .terminal
                    .as_ref()
                    .filter(|r| &r.0.request_id == id)
            })
            .or_else(|| {
                self.graph
                    .activation
                    .as_ref()
                    .and_then(|a| a.record.as_ref())
                    .filter(|r| &r.0.request_id == id)
            })
    }
    pub(super) fn graph_profile_records(&self) -> impl Iterator<Item = &Record> {
        [
            self.graph.job.as_ref().and_then(|j| j.record.as_ref()),
            self.graph.ready.as_ref().and_then(|r| r.record.as_ref()),
            self.graph
                .activation
                .as_ref()
                .and_then(|a| a.record.as_ref()),
            self.graph.terminal.as_ref(),
        ]
        .into_iter()
        .flatten()
    }
    pub(super) fn graph_profile_metadata_bytes(&self) -> Result<usize, EngineError> {
        let budget = crate::profile_control::BUDGET;
        let mut bytes = 0_usize;
        if let Some(application) = self
            .graph
            .job
            .as_ref()
            .and_then(|j| j.request.profile_application.as_ref())
        {
            bytes = size_of::<crate::profile_control::ProfileApplicationBinding>()
                + application.profile.id.capacity_bytes();
        }
        for preview in [
            self.graph.ready.as_ref().map(|r| &r.prepared.preview),
            self.graph
                .activation
                .as_ref()
                .map(|a| &a.request.prepared.preview),
            self.graph.active.as_ref(),
        ]
        .into_iter()
        .flatten()
        .filter(|p| p.profile_application.is_some())
        {
            bytes = bytes
                .checked_add(preview.retained_capacity_bytes()?)
                .ok_or(budget)?;
        }
        Ok(bytes)
    }
    pub(super) fn replay_graph_record(
        &self,
        command: &CommandEnvelope,
        record: &Record,
    ) -> Receipt {
        if command == &record.0 {
            record.1.clone()
        } else {
            self.receipt(
                command,
                ReceiptStatus::Rejected,
                Some(EngineError::new(
                    ErrorCode::AssetInvalid,
                    "request ID reused for different content",
                )),
            )
        }
    }
    pub(super) fn retain_graph_record(&mut self, record: Record) {
        if let Some(job) = &mut self.graph.job
            && job.request.request_id == record.0.request_id
        {
            job.record = Some(record.clone());
        }
        if let Some(activation) = &mut self.graph.activation
            && activation.request.request_id == record.0.request_id
        {
            activation.record = Some(record);
        }
    }
    fn settle_graph_record(
        &mut self,
        record: &mut Option<Record>,
        status: ReceiptStatus,
        error: Option<EngineError>,
        result: Option<QueryResult>,
    ) {
        if let Some(prior) = record.take() {
            let mut updated = (*prior).clone();
            updated.1.status = status;
            updated.1.error = error;
            updated.1.result = result;
            updated.1.revision = self.state.revision;
            updated.1.epochs = self.state.epochs;
            let updated = Arc::new(updated);
            // One terminal graph record covers completion after ordinary history rotation.
            // Keep the full ordinary receipt window; never evict it for this extra record.
            self.graph.terminal = Some(updated.clone());
            if let Some(slot) = self
                .receipts
                .iter_mut()
                .find(|r| r.0.request_id == updated.0.request_id)
            {
                *slot = updated.clone();
            }
            *record = Some(updated);
        }
    }
    pub(super) fn invalidate_graph_work(&mut self, error: EngineError) {
        let status = if matches!(
            error.code,
            ErrorCode::OperationCancelled | ErrorCode::StaleRevision | ErrorCode::StaleSession
        ) {
            ReceiptStatus::Cancelled
        } else {
            ReceiptStatus::Failed
        };
        if let Some(mut job) = self.graph.job.take() {
            self.settle_graph_record(&mut job.record, status, Some(error), None);
        }
        self.graph.ready = None;
        if let Some(mut activation) = self.graph.activation.take() {
            self.settle_graph_record(&mut activation.record, status, Some(error), None);
        }
    }
    pub fn expire_graph_work(&mut self, now_ns: u64) {
        if self
            .graph
            .job
            .as_ref()
            .is_some_and(|j| now_ns >= j.request.deadline_monotonic_ns)
            && let Some(mut job) = self.graph.job.take()
        {
            self.settle_graph_record(
                &mut job.record,
                ReceiptStatus::Failed,
                Some(EngineError::new(
                    ErrorCode::OperationCancelled,
                    "graph preparation deadline expired",
                )),
                None,
            );
        }
        if self
            .graph
            .ready
            .as_ref()
            .is_some_and(|r| now_ns >= r.prepared.expires_monotonic_ns)
        {
            self.graph.ready = None;
        }
    }
    pub(super) fn validate_graph_control(&self, command: &Command) -> Result<(), EngineError> {
        if (self.local_boundary.is_some()
            && matches!(
                command,
                Command::Resume
                    | Command::GraphPrepare { .. }
                    | Command::GraphValidate { .. }
                    | Command::GraphCommit { .. }
                    | Command::ProfilesPrepare { .. }
                    | Command::ProfilesApply { .. }
            ))
            || (self.local_boundary == Some(LocalBoundary::Stop)
                && matches!(command, Command::Pause | Command::Seek { .. }))
        {
            return Err(EngineError::new(
                ErrorCode::SourceBusy,
                "local playback transition awaits worker acknowledgement",
            ));
        }
        if self.graph.activation.is_some()
            && !matches!(
                command,
                Command::Mute { muted: true }
                    | Command::Stop
                    | Command::OwnerShutdown
                    | Command::VolumeCaps { .. }
            )
        {
            return Err(if matches!(command, Command::Mute { muted: false }) {
                BLOCKED
            } else {
                BUSY
            });
        }
        Ok(())
    }
    pub(super) fn validate_graph_unmute(&self, state: &SessionSnapshot) -> Result<(), EngineError> {
        if self.graph.recovery_required
            || self.graph.activation.is_some()
            || self.graph.configured.is_some() && self.graph.active.is_none()
        {
            return Err(BLOCKED);
        }
        if let Some(active) = &self.graph.active {
            active.validate_headroom(
                state.software_gain_db,
                state.volume_policy.software_cap_db(),
            )?;
        }
        Ok(())
    }
    pub(super) fn apply_graph_command(
        &mut self,
        command: &CommandEnvelope,
        now_ns: u64,
    ) -> Result<ReceiptStatus, EngineError> {
        if now_ns < self.state.monotonic_ns {
            return Err(EngineError::new(
                ErrorCode::AssetInvalid,
                "non-monotonic graph time",
            ));
        }
        match &command.payload {
            Command::GraphPrepare { graph, processing }
            | Command::GraphValidate { graph, processing } => {
                if self.graph.ready.as_ref().is_some_and(|r| {
                    r.prepared.preview.binding.expected_revision != self.state.revision
                }) {
                    self.graph.ready = None;
                }
                if self.graph.job.is_some()
                    || self.graph.ready.is_some()
                    || self.graph.activation.is_some()
                {
                    return Err(BUSY);
                }
                if self.state.source != Some(SourceKind::LocalFile)
                    || self.state.playback.is_none()
                    || self.local_boundary.is_some()
                    || !matches!(
                        self.state.transport,
                        TransportState::Buffering
                            | TransportState::Playing
                            | TransportState::Paused
                    )
                    || !self.state.output_consented
                {
                    return Err(STATE);
                }
                let output = self.state.output.as_ref().ok_or(STATE)?;
                if matches!(processing, GraphProcessingRequest::Float64 { .. })
                    && output.fidelity == FidelityPolicy::StrictPreservation
                {
                    return Err(EngineError::new(
                        ErrorCode::FidelityPolicyConflict,
                        "strict output cannot activate floating processing",
                    ));
                }
                let deadline = now_ns
                    .checked_add(GRAPH_PREPARATION_DEADLINE_NS)
                    .ok_or(STATE)?;
                self.graph.job = Some(Job {
                    request: GraphJobRequest {
                        profile_application: None,
                        request_id: command.request_id.clone(),
                        binding: self.graph_binding()?,
                        intent: Arc::new(GraphIntent {
                            graph: graph.clone(),
                            processing: *processing,
                        }),
                        validate_only: matches!(command.payload, Command::GraphValidate { .. }),
                        deadline_monotonic_ns: deadline,
                    },
                    record: None,
                });
            }
            Command::GraphCommit { token, graph_hash }
            | Command::ProfilesApply {
                token, graph_hash, ..
            } => {
                let ready = self.graph.ready.as_ref().ok_or(STALE)?;
                let application = match &command.payload {
                    Command::ProfilesApply {
                        id,
                        expected_profile_revision,
                        profile_sha256,
                        ..
                    } => {
                        let reference = crate::profile_control::ProfileCatalogReference {
                            id: id.clone(),
                            revision: *expected_profile_revision,
                            profile_sha256: *profile_sha256,
                        };
                        let binding = self.authorized_profile_application_binding(&reference)?;
                        if ready.prepared.preview.profile_application.as_ref() != Some(&binding) {
                            return Err(EngineError::new(
                                ErrorCode::StaleRevision,
                                "prepared profile policy or immutable revision changed",
                            ));
                        }
                        Some(binding)
                    }
                    _ if ready.prepared.preview.profile_application.is_some() => return Err(STALE),
                    _ => None,
                };
                if &ready.prepared.token != token
                    || &ready.prepared.preview.graph_hash != graph_hash
                {
                    return Err(STALE);
                }
                let binding = self.graph_binding()?;
                if ready.prepared.preview.binding.expected_revision != binding.expected_revision {
                    return Err(EngineError::new(
                        ErrorCode::StaleRevision,
                        "prepared graph revision changed",
                    ));
                }
                if ready.prepared.preview.binding != binding {
                    return Err(STALE);
                }
                if !self.state.output_consented {
                    return Err(BLOCKED);
                }
                ready.prepared.preview.validate_headroom(
                    application
                        .as_ref()
                        .map_or(self.state.software_gain_db, |a| a.software_gain_db),
                    application
                        .as_ref()
                        .map_or(self.state.volume_policy.software_cap_db(), |a| {
                            a.effective_policy.software_cap_db()
                        }),
                )?;
                let epochs = self.state.epochs.next_stream()?;
                let revision = increment(self.state.revision)?;
                increment(revision)?;
                increment(self.state.graph_epoch)?;
                increment(increment(self.state.event_sequence)?)?;
                let ready = self.graph.ready.take().ok_or(STALE)?;
                self.graph.activation = Some(Activation {
                    request: GraphActivationRequest {
                        request_id: command.request_id.clone(),
                        previous_binding: binding,
                        epochs,
                        prepared: ready.prepared,
                    },
                    intent: ready.intent,
                    record: None,
                    readiness: Arc::new(BTreeMap::new()),
                });
                self.state.revision = revision;
                self.state.epochs = epochs;
                self.state.muted = true;
                if let Some(application) = application {
                    self.state.output_consented = false;
                    self.state.volume_policy.disarm();
                    self.state.software_gain_db = application.software_gain_db;
                }
                self.state.transport = TransportState::Reconfiguring;
                self.state.audible_position_frames = None;
            }
            _ => return Err(STATE),
        }
        self.state.event_sequence = increment(self.state.event_sequence)?;
        self.state.monotonic_ns = now_ns;
        self.log(Some(command.request_id.clone()));
        Ok(ReceiptStatus::Accepted)
    }
    pub fn complete_graph_preparation(
        &mut self,
        id: &RequestId,
        binding: &GraphBinding,
        now_ns: u64,
        result: Result<Arc<GraphPreview>, GraphDiagnostic>,
    ) -> Result<(), EngineError> {
        self.expire_graph_work(now_ns);
        if self
            .graph
            .job
            .as_ref()
            .is_none_or(|j| &j.request.request_id != id || &j.request.binding != binding)
        {
            return Err(STALE);
        }
        let mut job = self.graph.job.take().ok_or(STALE)?;
        let result = (|| -> Result<_, GraphDiagnostic> {
            if now_ns < self.state.monotonic_ns {
                return Err(EngineError::new(
                    ErrorCode::AssetInvalid,
                    "non-monotonic graph completion",
                )
                .into());
            }
            let current = self.graph_binding().map_err(GraphDiagnostic::from)?;
            if current.expected_revision != binding.expected_revision {
                return Err(EngineError::new(
                    ErrorCode::StaleRevision,
                    "graph preparation revision changed",
                )
                .into());
            }
            if &current != binding {
                return Err(STALE.into());
            }
            let preview = result?;
            preview.validate().map_err(GraphDiagnostic::from)?;
            if preview.profile_application != job.request.profile_application {
                return Err(STALE.into());
            }
            if preview.profile_application.is_some()
                && preview.backend != self.state.playback.as_ref().and_then(|p| p.backend.clone())
            {
                return Err(STALE.into());
            }
            let application = if let Some(application) = &job.request.profile_application {
                let current = self
                    .authorized_profile_application_binding(&application.profile)
                    .map_err(GraphDiagnostic::from)?;
                if &current != application {
                    return Err(STALE.into());
                }
                Some(current)
            } else {
                None
            };
            if !job.request.validate_only {
                preview
                    .validate_headroom(
                        application
                            .as_ref()
                            .map_or(self.state.software_gain_db, |a| a.software_gain_db),
                        application
                            .as_ref()
                            .map_or(self.state.volume_policy.software_cap_db(), |a| {
                                a.effective_policy.software_cap_db()
                            }),
                    )
                    .map_err(GraphDiagnostic::from)?;
            }
            if &preview.binding != binding
                || self.state.playback.as_ref().map(|p| p.format) != Some(preview.source_format)
                || self.state.output.as_ref().map(|o| o.period_frames as usize)
                    != Some(preview.quantum_frames)
            {
                return Err(STALE.into());
            }
            let sequence = increment(self.state.event_sequence).map_err(GraphDiagnostic::from)?;
            let prepared = if job.request.validate_only {
                None
            } else {
                let serial = increment(self.graph.serial).map_err(GraphDiagnostic::from)?;
                let token = PreparedGraphId::new(format!("graph-{serial}"))
                    .map_err(GraphDiagnostic::from)?;
                let expires = now_ns
                    .checked_add(GRAPH_TOKEN_TTL_NS)
                    .ok_or_else(|| GraphDiagnostic::from(STATE))?;
                Some((
                    serial,
                    GraphPrepared {
                        token,
                        expires_monotonic_ns: expires,
                        preview: preview.clone(),
                    },
                ))
            };
            if let Some((_, prepared)) = &prepared
                && prepared.preview.profile_application.is_some()
            {
                let mut staged = self.clone();
                let mut record = job.record.clone();
                staged.settle_graph_record(
                    &mut record,
                    ReceiptStatus::Applied,
                    None,
                    Some(QueryResult::GraphPrepared(prepared.clone())),
                );
                staged.graph.ready = Some(Ready {
                    prepared: prepared.clone(),
                    intent: job.request.intent.clone(),
                    record,
                });
                staged
                    .validate_profile_storage(None)
                    .map_err(GraphDiagnostic::from)?;
                if let Some(record) = &staged.graph.terminal {
                    staged
                        .validate_profile_response(record)
                        .map_err(GraphDiagnostic::from)?;
                }
            }
            Ok((preview, prepared, sequence))
        })();
        match result {
            Ok((preview, prepared, sequence)) => {
                if let Some((serial, prepared)) = prepared {
                    self.settle_graph_record(
                        &mut job.record,
                        ReceiptStatus::Applied,
                        None,
                        Some(QueryResult::GraphPrepared(prepared.clone())),
                    );
                    self.graph.ready = Some(Ready {
                        prepared,
                        intent: job.request.intent,
                        record: job.record,
                    });
                    self.graph.serial = serial;
                } else {
                    self.settle_graph_record(
                        &mut job.record,
                        ReceiptStatus::Applied,
                        None,
                        Some(QueryResult::GraphPreview(preview)),
                    );
                }
                self.state.monotonic_ns = now_ns;
                self.state.event_sequence = sequence;
                self.log(Some(id.clone()));
                Ok(())
            }
            Err(diagnostic) => {
                let error = diagnostic.error();
                self.settle_graph_record(
                    &mut job.record,
                    ReceiptStatus::Failed,
                    Some(error),
                    Some(QueryResult::GraphDiagnostic(diagnostic)),
                );
                Err(error)
            }
        }
    }
    pub fn complete_graph_activation(
        &mut self,
        ack: GraphActivationAck,
        now_ns: u64,
        persist: impl FnOnce(&Session) -> Result<(), EngineError>,
    ) -> Result<(), EngineError> {
        let GraphActivationAck {
            request_id,
            epochs,
            graph_hash,
            activation_frame,
            observation,
            source_total_frames,
        } = ack;
        let id = &request_id;
        let hash = graph_hash.as_str();
        let activation = self.graph.activation.as_ref().ok_or(STALE)?;
        if &activation.request.request_id != id
            || activation.request.epochs != epochs
            || self.state.epochs != epochs
            || activation.request.previous_binding.device_epoch != self.state.device_epoch
            || activation.request.prepared.preview.graph_hash != hash
        {
            return Err(STALE);
        }
        if now_ns < self.state.monotonic_ns {
            return Err(EngineError::new(
                ErrorCode::AssetInvalid,
                "non-monotonic graph activation",
            ));
        }
        if observation.source_position_frames > source_total_frames
            || observation.format != activation.request.prepared.preview.source_format
        {
            let error = EngineError::new(
                ErrorCode::AssetInvalid,
                "invalid activated source readiness",
            );
            self.fail_graph_activation(id, error);
            return Err(error);
        }
        if let Some(application) = &activation.request.prepared.preview.profile_application {
            let observed_identity = observation.backend.as_ref().and_then(|backend| {
                (backend.backend == OutputBackend::CoreAudio)
                    .then_some(backend.device_identity_sha256)
                    .flatten()
            });
            let valid = self
                .profile_application_binding(&application.profile)
                .is_ok_and(|current| current == *application)
                && observed_identity == application.device_identity_sha256;
            if !valid {
                let error = EngineError::new(
                    ErrorCode::StaleRevision,
                    "profile activation policy or saved revision changed",
                );
                self.fail_graph_activation(id, error);
                return Err(error);
            }
        }
        let counters = (|| {
            Ok::<_, EngineError>((
                increment(self.state.graph_epoch)?,
                increment(self.state.revision)?,
                increment(self.state.event_sequence)?,
            ))
        })();
        let (graph_epoch, revision, event_sequence) = match counters {
            Ok(values) => values,
            Err(error) => {
                self.fail_graph_activation(id, error);
                return Err(error);
            }
        };
        let mut staged = self.clone();
        for (entry, status) in &*activation.readiness {
            if staged.queue.entries().iter().any(|item| &item.id == entry)
                && let Err(error) = staged.queue.update_status(entry, *status)
            {
                self.fail_graph_activation(id, error);
                return Err(error);
            }
        }
        if let Err(error) = staged.queue.update_status(
            &observation.source_entry_id,
            crate::queue::PreparationStatus::Ready {
                format: observation.format,
                total_frames: source_total_frames,
            },
        ) {
            self.fail_graph_activation(id, error);
            return Err(error);
        }
        let mut activated = staged.graph.activation.take().ok_or(STALE)?;
        staged.graph.active = Some(activated.request.prepared.preview.clone());
        staged.graph.configured = Some(activated.intent);
        if let Some(application) = &activated.request.prepared.preview.profile_application {
            staged.state.configured_profile = Some(application.profile.clone());
            staged.state.volume_policy = application.effective_policy.clone();
            staged.state.volume_policy.disarm();
            staged.state.software_gain_db = application.software_gain_db;
            staged.state.output.as_mut().ok_or(STALE)?.profile_id = application.profile.id.clone();
            staged.state.output_consented = false;
        } else if staged.state.configured_profile.take().is_some() {
            staged.state.volume_policy = staged.state.local_volume_policy.clone();
            staged.state.volume_policy.disarm();
        }
        staged.graph.recovery_required = false;
        staged.state.graph_epoch = graph_epoch;
        staged.state.revision = revision;
        staged.state.event_sequence = event_sequence;
        staged.state.monotonic_ns = now_ns;
        staged.state.transport = if staged.wants_playback() {
            TransportState::Buffering
        } else {
            TransportState::Paused
        };
        staged.state.muted = true;
        staged.state.active_entry_id = Some(observation.source_entry_id.clone());
        staged.state.reserved_next_entry_id = staged
            .queue
            .entries()
            .iter()
            .position(|entry| entry.id == observation.source_entry_id)
            .and_then(|index| staged.queue.entries().get(index + 1))
            .map(|entry| entry.id.clone());
        staged.state.decoded_position_frames = observation.source_position_frames;
        staged.state.audible_position_frames = observation.audible_source_position_frames;
        staged.state.playback = Some(observation);
        if staged.state.configured_profile.is_some()
            && let Err(error) = staged.validate_profile_storage(None)
        {
            self.fail_graph_activation(id, error);
            return Err(error);
        }
        if let Err(error) = persist(&staged) {
            self.fail_graph_activation(id, error);
            return Err(error);
        }
        staged.settle_graph_record(&mut activated.record, ReceiptStatus::Applied, None, None);
        if let Some(mut record) = activated.record.take() {
            Arc::make_mut(&mut record).1.activation_frame = Some(activation_frame);
            if let Some(slot) = staged
                .receipts
                .iter_mut()
                .find(|r| r.0.request_id == record.0.request_id)
            {
                *slot = record.clone();
            }
            staged.graph.terminal = Some(record);
        }
        staged.log(Some(id.clone()));
        *self = staged;
        Ok(())
    }
    pub fn fail_graph_activation(&mut self, id: &RequestId, error: EngineError) {
        if self
            .graph
            .activation
            .as_ref()
            .is_some_and(|a| &a.request.request_id == id)
            && let Some(mut activation) = self.graph.activation.take()
        {
            self.state.muted = true;
            self.state.transport = TransportState::Failed;
            self.local_boundary = None;
            self.state.output_consented = false;
            self.state.error = Some(error.code);
            self.graph.active = None;
            self.graph.recovery_required = true;
            if let Ok(epochs) = self.state.epochs.next_stream() {
                self.state.epochs = epochs;
            }
            self.settle_graph_record(
                &mut activation.record,
                ReceiptStatus::Failed,
                Some(error),
                None,
            );
            self.finish_pending(ReceiptStatus::Failed, Some(error), None);
            if let Ok(sequence) = increment(self.state.event_sequence) {
                self.state.event_sequence = sequence;
                self.log(Some(id.clone()));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{audio::ChannelLayout, graph::*, safety::VolumeMode};

    include!("session_profile_associations_tests.rs");
    include!("session_fixed_profiles_tests.rs");

    fn fixture() -> Session {
        let mut s = Session::new_owner(
            ZoneId::new("main").unwrap(),
            OwnerId::new("owner").unwrap(),
            ControlLimits::default(),
        );
        s.enable_local_playback();
        s.state.session_id = Some(SessionId::new("source").unwrap());
        s.state.source = Some(SourceKind::LocalFile);
        s.state.transport = TransportState::Paused;
        s.state.epochs = Epochs {
            session: 1,
            stream: 0,
        };
        s.state.output_id = Some(DeviceId::new("dac").unwrap());
        s.state.output = Some(OutputConfiguration {
            backend: OutputBackend::Alsa,
            route: OutputRoute::System,
            endpoint: "null".into(),
            device_id: DeviceId::new("dac").unwrap(),
            profile_id: ProfileId::new("profile").unwrap(),
            period_frames: 64,
            buffer_frames: 256,
            volume_mode: VolumeMode::Software,
            fidelity: FidelityPolicy::CompatibleConversion,
        });
        s.state.output_consented = true;
        let entry = s.queue.append("unit.wav").unwrap();
        s.state.active_entry_id = Some(entry.clone());
        s.playback_intent = PlaybackIntent::Paused;
        let format =
            PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Signed16).unwrap();
        s.state.playback = Some(PlaybackObservation {
            processing: None,
            format,
            submitted_frames: 0,
            consumed_frames: None,
            backend_delay_frames: None,
            consumption_basis: "unit reducer",
            source_entry_id: entry,
            source_position_frames: 0,
            audible_entry_id: None,
            audible_source_position_frames: None,
            input_peak: None,
            output_peak: None,
            modified: false,
            backend: None,
        });
        s
    }
    fn command(s: &Session, id: &str, payload: Command) -> CommandEnvelope {
        CommandEnvelope {
            schema_version: 1,
            request_id: RequestId::new(id).unwrap(),
            zone_id: s.state.zone_id.clone(),
            owner_id: s.state.owner_id.clone(),
            session_id: s.state.session_id.clone(),
            expected_revision: Some(s.state.revision),
            payload,
        }
    }
    fn prepare(s: &Session, id: &str) -> CommandEnvelope {
        let port = PortFormat {
            domain: SampleDomain::Pcm,
            sample_rate_hz: 48000,
            channels: ChannelLayout::Stereo,
            precision: PcmRepresentation::Float64,
            max_quantum_frames: 64,
        };
        let input = NodeId::new("in").unwrap();
        let output = NodeId::new("out").unwrap();
        command(
            s,
            id,
            Command::GraphPrepare {
                graph: Arc::new(GraphDocument {
                    schema_version: 1,
                    nodes: vec![
                        Node {
                            id: input.clone(),
                            kind: NodeKind::Input,
                            format: port,
                        },
                        Node {
                            id: output.clone(),
                            kind: NodeKind::Output,
                            format: port,
                        },
                    ],
                    edges: vec![Edge {
                        from: input,
                        to: output.clone(),
                        from_port: 0,
                        to_port: 0,
                    }],
                    output,
                }),
                processing: GraphProcessingRequest::Float64 {
                    quantization: QuantizationPolicy::RoundTiesAwaySaturateNoDither,
                },
            },
        )
    }
    fn preview(s: &Session, bound: f64) -> Arc<GraphPreview> {
        let job = s.graph_job().unwrap();
        let native = s.state.playback.as_ref().unwrap().format;
        let float =
            PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Float64).unwrap();
        Arc::new(GraphPreview {
            profile_application: job.profile_application,
            backend: s.state.playback.as_ref().and_then(|p| p.backend.clone()),
            graph_hash: "a".repeat(64),
            kernel_hash: "b".repeat(64),
            implementation_version: "test-contract".into(),
            binding: job.binding,
            source_format: native,
            native_output_format: native,
            processing_input_format: float,
            processing_output_format: float,
            quantum_frames: 64,
            timing: GraphTiming::SameRate {
                latency_frames: 0,
                tail_frames: 0,
                tail_policy: crate::graph_control::GraphTailPolicy::AuthoredFiniteSupport,
                terminal_output_bounds: vec![0.0; 2],
                node_tails: vec![],
            },
            preservation: PreservationDecision::ProcessFloat64,
            transformations: vec![],
            resources: GraphResidentUsage::default(),
            channel_peak_bounds: vec![bound; 2],
            required_headroom_db: (20.0 * bound.log10()).max(0.0),
            transition: GraphTransition::MutedRebuffer,
        })
    }
    fn ready(s: &mut Session, id: &str, bound: f64) -> GraphPrepared {
        let c = prepare(s, id);
        assert_eq!(s.submit_graph(c, 10).status, ReceiptStatus::Accepted);
        let p = preview(s, bound);
        s.complete_graph_preparation(&RequestId::new(id).unwrap(), &p.binding.clone(), 20, Ok(p))
            .unwrap();
        s.graph_inspection().prepared.unwrap()
    }
    fn commit(s: &Session, id: &str, p: &GraphPrepared) -> CommandEnvelope {
        command(
            s,
            id,
            Command::GraphCommit {
                token: p.token.clone(),
                graph_hash: p.preview.graph_hash.clone(),
            },
        )
    }
    fn profile_fixture() -> Session {
        let mut s = fixture();
        let format = s.state.playback.as_ref().unwrap().format;
        s.state.playback.as_mut().unwrap().backend = Some(BackendObservation {
            backend: OutputBackend::Alsa,
            device_identity_sha256: None,
            audio_storage_bytes: 256,
            client_format: format.into(),
            device_format: Some(format.into()),
            measurement_boundary: "unit owner observation",
            preservation: PreservationState::Unverified,
        });
        let graph = match prepare(&s, "graph").payload {
            Command::GraphPrepare { graph, processing } => {
                Arc::new(GraphIntent { graph, processing })
            }
            _ => unreachable!(),
        };
        let draft = Arc::new(serde_json::from_value(serde_json::json!({
            "name":"Headphones", "source_policy":"local_file", "fidelity":"compatible_conversion",
            "output":{"backend":"alsa","route":"system","device_id":"dac","manual_setup":"headphones"},
            "volume":{"mode":"software","software_cap_millidecibels":-10000,"hardware_cap_millidecibels":null},
            "latency":{"mode":"balanced","max_resident_bytes":268435456,"max_preparation_bytes":268435456,"max_latency_frames":null},
            "corrections":[]
        })).unwrap());
        let save = command(
            &s,
            "save-profile",
            Command::ProfilesCreate {
                id: ProfileId::new("headphones").unwrap(),
                draft,
                graph,
            },
        );
        assert_eq!(s.submit_graph(save, 1).status, ReceiptStatus::Accepted);
        let job = s.profile_job().unwrap();
        let hash = crate::profiles::ProfileObjectHash::try_from("c".repeat(64)).unwrap();
        let record = Arc::new(crate::profile_control::ProfileRecord {
            profile: Arc::new(
                job.draft
                    .document(job.id.clone(), job.revision, hash)
                    .unwrap(),
            ),
            profile_sha256: hash,
            graph: job.graph.clone(),
            corrections: Arc::new(vec![]),
            correction_preview: None,
        });
        s.complete_profile_save(&job.request_id, &job.binding, 2, Ok(record), |_| Ok(()))
            .unwrap();
        s
    }
    fn profile_prepare(s: &Session, request_id: &str) -> CommandEnvelope {
        command(
            s,
            request_id,
            serde_json::from_value(serde_json::json!({
                "method":"profiles.prepare", "id":"headphones", "expected_profile_revision":1,
                "profile_sha256":"c".repeat(64)
            }))
            .unwrap(),
        )
    }
    #[test]
    fn profile_preparation_uses_saved_graph_without_publishing_selection() {
        let mut s = profile_fixture();
        let caps = command(
            &s,
            "caps",
            Command::VolumeCaps {
                software_millidecibels: -30000,
                hardware_millidecibels: None,
            },
        );
        assert_eq!(s.submit_graph(caps, 3).status, ReceiptStatus::Applied);
        let c = profile_prepare(&s, "prepare-profile");
        let before = s.checkpoint();
        let receipt = s.submit_graph(c, 4);
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        let job = s.graph_job().unwrap();
        assert_eq!(job.intent, s.profile_records().unwrap()[0].graph);
        assert_eq!(s.checkpoint().revision, before.revision);
        assert_eq!(s.snapshot().software_gain_db, -40.0);
        assert!(s.snapshot().output_consented);
        let observation = serde_json::to_value(s.snapshot()).unwrap();
        assert!(observation["configured_profile"].is_null());
    }
    #[test]
    fn profile_preparation_keeps_current_authorization_and_output_guards() {
        for (case, expected) in [
            ("consent", ErrorCode::SafetyPolicyBlocked),
            ("device", ErrorCode::SourceBusy),
            ("fixed", ErrorCode::CapabilityUnsupported),
            ("fidelity", ErrorCode::CapabilityUnsupported),
        ] {
            let mut s = profile_fixture();
            match case {
                "consent" => s.state.output_consented = false,
                "device" => {
                    s.state.output.as_mut().unwrap().device_id = DeviceId::new("other").unwrap()
                }
                "fixed" => s.state.output.as_mut().unwrap().volume_mode = VolumeMode::Fixed,
                "fidelity" => {
                    s.state.output.as_mut().unwrap().fidelity = FidelityPolicy::StrictPreservation
                }
                _ => unreachable!(),
            }
            let c = profile_prepare(&s, case);
            let receipt = s.submit_graph(c, 3);
            assert_eq!(receipt.error.unwrap().code, expected, "{case}");
            assert!(s.graph_job().is_none());
        }
    }
    #[test]
    fn profile_preparation_requires_observed_backend_facts() {
        let mut s = profile_fixture();
        s.state.playback.as_mut().unwrap().backend = None;
        let c = profile_prepare(&s, "missing-backend");
        assert_eq!(
            s.submit_graph(c, 3).error.unwrap().code,
            ErrorCode::DeviceUnavailable
        );
        assert!(s.graph_job().is_none());
    }
    fn profile_ready(s: &mut Session) -> GraphPrepared {
        let c = profile_prepare(s, "prepare-profile");
        assert_eq!(
            s.submit_graph(c.clone(), 10).status,
            ReceiptStatus::Accepted
        );
        let preview = preview(s, 1.0);
        s.complete_graph_preparation(&c.request_id, &preview.binding.clone(), 20, Ok(preview))
            .unwrap();
        s.graph_inspection().prepared.unwrap()
    }
    fn profile_apply(s: &Session, id: &str, prepared: &GraphPrepared) -> CommandEnvelope {
        command(s, id, serde_json::from_value(serde_json::json!({
            "method":"profiles.apply", "id":"headphones", "expected_profile_revision":1,
            "profile_sha256":"c".repeat(64), "token":prepared.token, "graph_hash":prepared.preview.graph_hash,
        })).unwrap())
    }
    fn profile_ack(s: &Session, c: &CommandEnvelope, p: &GraphPrepared) -> GraphActivationAck {
        GraphActivationAck {
            request_id: c.request_id.clone(),
            epochs: s.state.epochs,
            graph_hash: p.preview.graph_hash.clone(),
            activation_frame: 64,
            observation: s.state.playback.clone().unwrap(),
            source_total_frames: 48000,
        }
    }
    #[test]
    fn profile_application_publishes_exact_selection_only_after_persisted_ack() {
        let mut s = profile_fixture();
        let caps = command(
            &s,
            "caps",
            Command::VolumeCaps {
                software_millidecibels: -30000,
                hardware_millidecibels: None,
            },
        );
        assert_eq!(s.submit_graph(caps, 3).status, ReceiptStatus::Applied);
        let p = profile_ready(&mut s);
        assert_eq!(
            p.preview
                .profile_application
                .as_ref()
                .unwrap()
                .effective_policy
                .software_cap_db(),
            -30.0
        );
        let c = profile_apply(&s, "apply-profile", &p);
        let receipt = s.submit_graph(c.clone(), 21);
        assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
        assert!(s.state.configured_profile.is_none());
        assert!(s.checkpoint().configured_profile.is_none());
        assert!(s.state.muted);
        assert!(!s.state.output_consented);
        let ack = profile_ack(&s, &c, &p);
        s.complete_graph_activation(ack, 22, |staged| {
            assert_eq!(
                staged.state.configured_profile.as_ref().unwrap().revision,
                1
            );
            assert_eq!(
                staged.state.output.as_ref().unwrap().profile_id.as_str(),
                "headphones"
            );
            assert_eq!(staged.state.local_volume_policy.software_cap_db(), -30.0);
            assert_eq!(staged.state.volume_policy.software_cap_db(), -30.0);
            assert_eq!(staged.state.software_gain_db, -40.0);
            assert!(staged.state.muted);
            assert!(!staged.state.output_consented);
            Ok(())
        })
        .unwrap();
        let applied = s.submit_graph(c, 23);
        assert_eq!(applied.status, ReceiptStatus::Applied);
        assert_eq!(applied.activation_frame, Some(64));
    }
    #[test]
    fn profile_tokens_cannot_cross_commit_or_ignore_reference_changes() {
        let mut s = profile_fixture();
        let p = profile_ready(&mut s);
        let wrong = commit(&s, "ordinary-commit", &p);
        assert_eq!(
            s.submit_graph(wrong, 21).error.unwrap().code,
            ErrorCode::StaleSession
        );
        let mut stale = profile_apply(&s, "stale-profile", &p);
        if let Command::ProfilesApply {
            expected_profile_revision,
            ..
        } = &mut stale.payload
        {
            *expected_profile_revision = 2;
        }
        assert_eq!(
            s.submit_graph(stale, 22).error.unwrap().code,
            ErrorCode::StaleRevision
        );
        assert!(s.graph_inspection().prepared.is_some());
        let caps = command(
            &s,
            "change-caps",
            Command::VolumeCaps {
                software_millidecibels: -50000,
                hardware_millidecibels: None,
            },
        );
        s.submit_graph(caps, 23);
        let stale = profile_apply(&s, "old-caps", &p);
        assert_eq!(
            s.submit_graph(stale, 24).error.unwrap().code,
            ErrorCode::StaleRevision
        );
    }
    #[test]
    fn failed_profile_checkpoint_never_publishes_selection_or_consent() {
        let mut s = profile_fixture();
        let p = profile_ready(&mut s);
        let c = profile_apply(&s, "apply-profile", &p);
        assert_eq!(
            s.submit_graph(c.clone(), 21).status,
            ReceiptStatus::Accepted
        );
        let ack = profile_ack(&s, &c, &p);
        let error = EngineError::new(ErrorCode::InternalFault, "checkpoint failed");
        assert_eq!(
            s.complete_graph_activation(ack, 22, |_| Err(error)),
            Err(error)
        );
        assert!(s.state.configured_profile.is_none());
        assert!(s.graph_inspection().recovery_required);
        assert!(!s.state.output_consented);
        assert_eq!(s.submit_graph(c, 23).status, ReceiptStatus::Failed);
    }
    #[test]
    fn local_caps_do_not_ratchet_through_profile_effective_caps() {
        let mut s = profile_fixture();
        let caps = command(
            &s,
            "local-caps",
            Command::VolumeCaps {
                software_millidecibels: -5000,
                hardware_millidecibels: None,
            },
        );
        assert_eq!(s.submit_graph(caps, 3).status, ReceiptStatus::Applied);
        assert_eq!(s.state.local_volume_policy.software_cap_db(), -5.0);
        let p = profile_ready(&mut s);
        assert_eq!(
            p.preview
                .profile_application
                .as_ref()
                .unwrap()
                .effective_policy
                .software_cap_db(),
            -10.0
        );
        let c = profile_apply(&s, "apply", &p);
        assert_eq!(
            s.submit_graph(c.clone(), 21).status,
            ReceiptStatus::Accepted
        );
        let ack = profile_ack(&s, &c, &p);
        s.complete_graph_activation(ack, 22, |_| Ok(())).unwrap();
        let caps = command(
            &s,
            "lower-local",
            Command::VolumeCaps {
                software_millidecibels: -30000,
                hardware_millidecibels: None,
            },
        );
        s.submit_graph(caps, 23);
        let caps = command(
            &s,
            "restore-local",
            Command::VolumeCaps {
                software_millidecibels: -5000,
                hardware_millidecibels: None,
            },
        );
        s.submit_graph(caps, 24);
        assert_eq!(s.state.local_volume_policy.software_cap_db(), -5.0);
        assert_eq!(s.state.volume_policy.software_cap_db(), -10.0);
        assert_eq!(s.state.software_gain_db, -40.0);
    }
    #[test]
    fn profile_application_requires_exact_command_session() {
        let mut s = profile_fixture();
        let p = profile_ready(&mut s);
        let mut c = profile_apply(&s, "wrong-session", &p);
        c.session_id = Some(SessionId::new("another-session").unwrap());
        assert_eq!(
            s.submit_graph(c, 21).error.unwrap().code,
            ErrorCode::StaleSession
        );
        assert!(s.graph_inspection().prepared.is_some());
    }
    #[test]
    fn profile_preparation_cannot_resurrect_after_stop_or_emergency_mute() {
        for stop in [false, true] {
            let mut s = profile_fixture();
            let c = profile_prepare(&s, "prepare-profile");
            assert_eq!(
                s.submit_graph(c.clone(), 10).status,
                ReceiptStatus::Accepted
            );
            let preview = preview(&s, 1.0);
            let safety = command(
                &s,
                "safety",
                if stop {
                    Command::Stop
                } else {
                    Command::Mute { muted: true }
                },
            );
            let receipt = s.submit_graph(safety, 11);
            assert!(matches!(
                receipt.status,
                ReceiptStatus::Accepted | ReceiptStatus::Applied
            ));
            assert!(
                s.complete_graph_preparation(
                    &c.request_id,
                    &preview.binding.clone(),
                    12,
                    Ok(preview)
                )
                .is_err()
            );
            assert!(s.graph_inspection().prepared.is_none());
            assert!(s.state.configured_profile.is_none());
            assert!(s.state.muted);
        }
    }
    #[test]
    fn prepared_profile_receipts_charge_their_retained_preview() {
        let mut s = profile_fixture();
        let c = profile_prepare(&s, "prepare-profile");
        assert_eq!(
            s.submit_graph(c.clone(), 10).status,
            ReceiptStatus::Accepted
        );
        let before = s.profile_storage_bytes();
        let preview = preview(&s, 1.0);
        let encoded = serde_json::to_vec(&preview).unwrap().len() as u64;
        s.complete_graph_preparation(&c.request_id, &preview.binding.clone(), 20, Ok(preview))
            .unwrap();
        assert!(
            s.profile_storage_bytes() >= before + encoded,
            "prepared receipt metadata escaped retention admission"
        );
    }
    #[test]
    fn profile_cap_change_during_activation_cannot_publish_the_candidate() {
        let mut s = profile_fixture();
        let p = profile_ready(&mut s);
        let c = profile_apply(&s, "apply-profile", &p);
        assert_eq!(
            s.submit_graph(c.clone(), 21).status,
            ReceiptStatus::Accepted
        );
        let ack = profile_ack(&s, &c, &p);
        let caps = command(
            &s,
            "lower-caps",
            Command::VolumeCaps {
                software_millidecibels: -50000,
                hardware_millidecibels: None,
            },
        );
        assert_eq!(s.submit_graph(caps, 22).status, ReceiptStatus::Applied);
        assert_eq!(
            s.complete_graph_activation(ack, 23, |_| panic!("stale profile cannot persist")),
            Err(EngineError::new(
                ErrorCode::StaleRevision,
                "profile activation policy or saved revision changed"
            ))
        );
        assert!(s.state.configured_profile.is_none());
        assert!(s.state.muted);
        assert!(!s.state.output_consented);
    }
    #[test]
    fn selected_profile_update_waits_for_actual_stop_then_revokes_the_old_selection() {
        let mut s = profile_fixture();
        let p = profile_ready(&mut s);
        let c = profile_apply(&s, "apply-profile", &p);
        s.submit_graph(c.clone(), 21);
        let ack = profile_ack(&s, &c, &p);
        s.complete_graph_activation(ack, 22, |_| Ok(())).unwrap();
        let record = s.profile_records().unwrap()[0].clone();
        let draft = Arc::new(serde_json::from_value(serde_json::json!({
            "name":"Speakers", "source_policy":"local_file", "fidelity":"compatible_conversion",
            "output":{"backend":"alsa","route":"system","device_id":"dac","manual_setup":"speakers"},
            "volume":{"mode":"software","software_cap_millidecibels":-5000,"hardware_cap_millidecibels":null},
            "latency":{"mode":"balanced","max_resident_bytes":268435456,"max_preparation_bytes":268435456,"max_latency_frames":null}, "corrections":[]
        })).unwrap());
        let payload = Command::ProfilesUpdate {
            id: record.profile.id.clone(),
            expected_profile_revision: 1,
            draft,
            graph: record.graph.clone(),
        };
        let update = command(&s, "update-live", payload.clone());
        assert_eq!(
            s.submit_graph(update, 23).error.unwrap().code,
            ErrorCode::SourceBusy
        );
        let stop = command(&s, "stop", Command::Stop);
        assert_eq!(s.submit_graph(stop, 24).status, ReceiptStatus::Accepted);
        let update = command(&s, "update-pending-stop", payload.clone());
        assert_eq!(
            s.submit_graph(update, 25).error.unwrap().code,
            ErrorCode::SourceBusy
        );
        s.advance(EngineEvent {
            epochs: s.state.epochs,
            monotonic_ns: 26,
            kind: EngineEventKind::LocalStopped { observation: None },
        })
        .unwrap();
        let output = s.snapshot().output.unwrap();
        let consent = command(
            &s,
            "consent-after-stop",
            Command::OutputConsent {
                device_id: output.device_id,
                profile_id: output.profile_id,
            },
        );
        assert_eq!(s.submit_graph(consent, 26).status, ReceiptStatus::Applied);
        assert!(s.snapshot().output_consented);
        let update = command(&s, "update-stopped", payload);
        assert_eq!(s.submit_graph(update, 27).status, ReceiptStatus::Accepted);
        let job = s.profile_job().unwrap();
        let replacement = Arc::new(crate::profile_control::ProfileRecord {
            profile: Arc::new(
                job.draft
                    .document(job.id.clone(), job.revision, record.profile.graph_sha256)
                    .unwrap(),
            ),
            profile_sha256: crate::profiles::ProfileObjectHash::try_from("d".repeat(64)).unwrap(),
            graph: job.graph.clone(),
            corrections: Arc::new(vec![]),
            correction_preview: None,
        });
        s.complete_profile_save(
            &job.request_id,
            &job.binding,
            28,
            Ok(replacement),
            |staged| {
                assert!(staged.state.configured_profile.is_none());
                assert!(staged.checkpoint().configured_profile.is_none());
                assert!(staged.state.muted);
                assert!(!staged.state.output_consented);
                assert!(staged.graph.recovery_required);
                Ok(())
            },
        )
        .unwrap();
        assert!(!s.snapshot().output_consented);
        assert!(s.snapshot().configured_profile.is_none());
        assert!(s.checkpoint().configured_profile.is_none());
    }
    #[test]
    fn profile_restore_verifies_selected_graph_and_independent_caps_without_authority() {
        let mut s = profile_fixture();
        let p = profile_ready(&mut s);
        let c = profile_apply(&s, "apply-profile", &p);
        s.submit_graph(c.clone(), 21);
        let ack = profile_ack(&s, &c, &p);
        s.complete_graph_activation(ack, 22, |_| Ok(())).unwrap();
        let checkpoint = s.checkpoint();
        let records = s.profile_records().unwrap().as_ref().clone();
        let mut missing_local = checkpoint.clone();
        missing_local.local_volume_policy = None;
        assert!(
            Session::restore_owner(
                s.state.zone_id.clone(),
                OwnerId::new("missing-local").unwrap(),
                ControlLimits::default(),
                missing_local
            )
            .is_err(),
            "a selected-profile checkpoint cannot relabel effective caps as local caps"
        );
        let mut restored = Session::restore_owner(
            s.state.zone_id.clone(),
            OwnerId::new("fresh-owner").unwrap(),
            ControlLimits::default(),
            checkpoint.clone(),
        )
        .unwrap();
        restored.restore_profile_records(records.clone()).unwrap();
        assert_eq!(restored.state.volume_policy.software_cap_db(), -10.0);
        assert_eq!(restored.state.local_volume_policy.software_cap_db(), 0.0);
        assert!(!restored.state.output_consented);
        assert!(restored.state.muted);
        assert!(restored.graph.active.is_none());
        let mut tampered = checkpoint;
        tampered.volume_policy = VolumePolicy::new(0.0, None).unwrap();
        let mut restored = Session::restore_owner(
            s.state.zone_id.clone(),
            OwnerId::new("fresh-owner").unwrap(),
            ControlLimits::default(),
            tampered,
        )
        .unwrap();
        assert!(restored.restore_profile_records(records).is_err());
    }
    #[test]
    fn preparation_is_revision_neutral_bounded_and_exactly_replayed() {
        let mut s = fixture();
        let c = prepare(&s, "prepare");
        let accepted = s.submit_graph(c.clone(), 10);
        assert_eq!(accepted.status, ReceiptStatus::Accepted);
        assert_eq!(s.state.revision, 0);
        assert!(s.state.operation.is_none());
        assert_eq!(s.submit_graph(c.clone(), 11), accepted);
        let competitor = prepare(&s, "competitor");
        assert_eq!(
            s.submit_graph(competitor, 12).error.unwrap().code,
            ErrorCode::SourceBusy
        );
        let p = preview(&s, 1.0);
        s.complete_graph_preparation(&c.request_id, &p.binding.clone(), 20, Ok(p))
            .unwrap();
        assert_eq!(s.submit_graph(c, 21).status, ReceiptStatus::Applied);
        assert_eq!(s.state.revision, 0);
    }
    #[test]
    fn pending_stop_cannot_prepare_from_the_old_playback_observation() {
        let mut s = fixture();
        let stop = command(&s, "stop", Command::Stop);
        assert_eq!(
            s.submit_graph(stop.clone(), 1).status,
            ReceiptStatus::Accepted
        );
        let c = prepare(&s, "prepare-after-stop");
        assert_eq!(s.submit_graph(c, 2).status, ReceiptStatus::Rejected);
        assert!(s.graph_job().is_none());
        assert_eq!(s.submit_graph(stop, 3).status, ReceiptStatus::Accepted);
    }
    #[test]
    fn pending_stop_cannot_resume_or_prepare_after_receipt_rotation() {
        let mut s = fixture();
        s.state.seekable = true;
        let entry = s.state.active_entry_id.clone().unwrap();
        s.queue
            .update_status(
                &entry,
                crate::queue::PreparationStatus::Ready {
                    format: s.state.playback.as_ref().unwrap().format,
                    total_frames: 1000,
                },
            )
            .unwrap();
        let stop = command(&s, "stop", Command::Stop);
        assert_eq!(s.submit_graph(stop, 1).status, ReceiptStatus::Accepted);
        for i in 0..1025 {
            let query = command(&s, &format!("poll-{i}"), Command::GraphInspect {});
            assert_eq!(s.submit_graph(query, 2).status, ReceiptStatus::Applied);
        }
        for payload in [
            Command::Resume,
            Command::Pause,
            Command::Seek { position_frames: 0 },
        ] {
            let c = command(&s, &format!("closing-{payload:?}"), payload);
            let receipt = s.submit_graph(c, 3);
            assert_eq!(receipt.status, ReceiptStatus::Rejected);
            assert_eq!(receipt.error.unwrap().code, ErrorCode::SourceBusy);
        }
        let c = prepare(&s, "prepare-after-closing-resume");
        assert_eq!(s.submit_graph(c, 4).status, ReceiptStatus::Rejected);
        assert!(s.graph_job().is_none());
    }
    #[test]
    fn graph_preparation_waits_for_pause_and_seek_worker_acknowledgements() {
        for payload in [
            Command::Pause,
            Command::Seek {
                position_frames: 50,
            },
        ] {
            let mut s = fixture();
            s.state.seekable = true;
            let entry = s.state.active_entry_id.clone().unwrap();
            s.queue
                .update_status(
                    &entry,
                    crate::queue::PreparationStatus::Ready {
                        format: s.state.playback.as_ref().unwrap().format,
                        total_frames: 1000,
                    },
                )
                .unwrap();
            let request = command(&s, "reposition", payload);
            assert_eq!(
                s.submit_graph(request.clone(), 1).status,
                ReceiptStatus::Accepted
            );
            let c = prepare(&s, "before-ack");
            assert_eq!(
                s.submit_graph(c, 2).error.unwrap().code,
                ErrorCode::SourceBusy
            );
            let resume = command(&s, "early-resume", Command::Resume);
            assert_eq!(
                s.submit_graph(resume, 3).error.unwrap().code,
                ErrorCode::SourceBusy
            );
            s.advance(EngineEvent {
                epochs: s.state.epochs,
                monotonic_ns: 4,
                kind: EngineEventKind::LocalPaused {
                    observation: s.state.playback.as_ref().unwrap().clone(),
                },
            })
            .unwrap();
            assert_eq!(s.submit_graph(request, 5).status, ReceiptStatus::Applied);
            let c = prepare(&s, "after-ack");
            assert_eq!(s.submit_graph(c, 6).status, ReceiptStatus::Accepted);
        }
    }
    #[test]
    fn proven_source_completion_cancels_activation_without_an_output_fault() {
        let mut s = fixture();
        let p = ready(&mut s, "prepare", 1.0);
        let c = commit(&s, "commit", &p);
        s.submit_graph(c.clone(), 21);
        let mut observation = s.state.playback.clone().unwrap();
        observation.source_position_frames = 1000;
        observation.submitted_frames = 1000;
        let epochs = s.state.epochs;
        let cursor = s.state.event_sequence;
        s.advance(EngineEvent {
            epochs,
            monotonic_ns: 22,
            kind: EngineEventKind::LocalCompleted {
                observation: observation.clone(),
            },
        })
        .unwrap();
        assert_eq!(s.state.transport, TransportState::Idle);
        assert!(s.state.muted && s.state.output_consented);
        assert!(s.state.session_id.is_none() && s.graph_activation().is_none());
        assert!(!s.graph_inspection().recovery_required);
        assert_eq!(s.state.playback.as_ref(), Some(&observation));
        assert_eq!(s.state.error, None);
        let events = s.events_since(cursor).events;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].request_id.as_ref(), Some(&c.request_id));
        let receipt = s.submit_graph(c, 23);
        assert_eq!(receipt.status, ReceiptStatus::Failed);
        assert_eq!(
            receipt.error.unwrap().code,
            ErrorCode::CapabilityUnsupported
        );
        let before = s.state.clone();
        assert!(
            s.advance(EngineEvent {
                epochs,
                monotonic_ns: 24,
                kind: EngineEventKind::LocalCompleted { observation },
            })
            .is_err()
        );
        assert_eq!(s.state, before);
    }
    #[test]
    fn proven_completion_settles_repositioning_as_ended_without_revoking_consent() {
        for payload in [
            Command::Pause,
            Command::Seek {
                position_frames: 50,
            },
        ] {
            let mut s = fixture();
            s.state.seekable = true;
            let entry = s.state.active_entry_id.clone().unwrap();
            s.queue
                .update_status(
                    &entry,
                    crate::queue::PreparationStatus::Ready {
                        format: s.state.playback.as_ref().unwrap().format,
                        total_frames: 1000,
                    },
                )
                .unwrap();
            let c = command(&s, "reposition", payload);
            assert_eq!(s.submit_graph(c.clone(), 1).status, ReceiptStatus::Accepted);
            s.advance(EngineEvent {
                epochs: s.state.epochs,
                monotonic_ns: 2,
                kind: EngineEventKind::LocalCompleted {
                    observation: s.state.playback.clone().unwrap(),
                },
            })
            .unwrap();
            let receipt = s.submit_graph(c, 3);
            assert_eq!(receipt.status, ReceiptStatus::Failed);
            assert_eq!(
                receipt.error.unwrap().code,
                ErrorCode::CapabilityUnsupported
            );
            assert_eq!(s.state.transport, TransportState::Idle);
            assert!(s.state.output_consented && s.state.muted);
            assert!(s.local_boundary.is_none());
        }
    }
    #[test]
    fn proved_completion_retires_the_owner_when_a_terminal_counter_is_exhausted() {
        for counter in ["revision", "events", "stream"] {
            let mut s = fixture();
            match counter {
                "revision" => s.state.revision = u64::MAX,
                "events" => s.state.event_sequence = u64::MAX,
                "stream" => s.state.epochs.stream = u64::MAX,
                _ => unreachable!(),
            }
            s.advance(EngineEvent {
                epochs: s.state.epochs,
                monotonic_ns: 1,
                kind: EngineEventKind::LocalCompleted {
                    observation: s.state.playback.clone().unwrap(),
                },
            })
            .unwrap();
            assert_eq!(s.state.transport, TransportState::Idle, "{counter}");
            assert!(s.state.muted && s.state.output_consented, "{counter}");
            assert!(s.shutdown_requested(), "{counter}");
            assert_eq!(s.state.error, Some(ErrorCode::ResourceBudgetExceeded));
            assert!(s.state.source.is_none() && s.state.session_id.is_none());
            assert!(!s.graph_inspection().recovery_required);
        }
    }
    #[test]
    fn source_completion_correlates_graph_and_transport_without_wrapping_events() {
        for last_slot in [false, true] {
            let mut s = fixture();
            let resume = command(&s, "resume-before-graph", Command::Resume);
            assert_eq!(
                s.submit_graph(resume.clone(), 1).status,
                ReceiptStatus::Accepted
            );
            let p = ready(&mut s, "prepare-during-resume", 1.0);
            if last_slot {
                s.state.event_sequence = u64::MAX - 2;
            }
            let c = commit(&s, "graph-during-resume", &p);
            assert_eq!(
                s.submit_graph(c.clone(), 21).status,
                ReceiptStatus::Accepted
            );
            let cursor = s.state.event_sequence;
            s.advance(EngineEvent {
                epochs: s.state.epochs,
                monotonic_ns: 22,
                kind: EngineEventKind::LocalCompleted {
                    observation: s.state.playback.clone().unwrap(),
                },
            })
            .unwrap();
            let events = s.events_since(cursor).events;
            assert_eq!(events[0].request_id.as_ref(), Some(&c.request_id));
            assert_eq!(events.len(), if last_slot { 1 } else { 2 });
            if !last_slot {
                assert_eq!(events[1].request_id.as_ref(), Some(&resume.request_id));
                assert_eq!(events[1].sequence, events[0].sequence + 1);
            } else {
                assert!(s.shutdown_requested());
            }
            assert_eq!(s.submit_graph(c, 23).status, ReceiptStatus::Failed);
            assert_eq!(s.submit_graph(resume, 24).status, ReceiptStatus::Applied);
            assert!(s.state.muted && s.state.output_consented);
        }
    }
    #[test]
    fn source_terminal_facts_retire_when_the_last_counter_is_consumed() {
        for terminal in ["complete", "failure", "stopped"] {
            let counters: &[&str] = if terminal == "stopped" {
                &["revision", "events"]
            } else {
                &["revision", "events", "stream"]
            };
            for &counter in counters {
                let mut s = fixture();
                match counter {
                    "revision" => s.state.revision = u64::MAX - 1,
                    "events" => s.state.event_sequence = u64::MAX - 1,
                    "stream" => s.state.epochs.stream = u64::MAX - 1,
                    _ => unreachable!(),
                }
                let observation = s.state.playback.clone().unwrap();
                let kind = match terminal {
                    "complete" => EngineEventKind::LocalCompleted { observation },
                    "failure" => EngineEventKind::LocalFailed {
                        code: ErrorCode::OutputFault,
                    },
                    "stopped" => EngineEventKind::LocalStopped {
                        observation: Some(observation),
                    },
                    _ => unreachable!(),
                };
                s.advance(EngineEvent {
                    epochs: s.state.epochs,
                    monotonic_ns: 1,
                    kind,
                })
                .unwrap();
                assert!(s.shutdown_requested(), "{terminal}/{counter}");
                assert!(s.state.muted, "{terminal}/{counter}");
                assert_eq!(s.state.output_consented, terminal != "failure");
            }
        }
    }
    #[test]
    fn failed_source_settles_activation_with_recovery_and_request_correlation() {
        let mut s = fixture();
        let p = ready(&mut s, "prepare-before-fault", 1.0);
        let c = commit(&s, "commit-before-fault", &p);
        assert_eq!(
            s.submit_graph(c.clone(), 21).status,
            ReceiptStatus::Accepted
        );
        let cursor = s.state.event_sequence;
        s.advance(EngineEvent {
            epochs: s.state.epochs,
            monotonic_ns: 22,
            kind: EngineEventKind::LocalFailed {
                code: ErrorCode::OutputFault,
            },
        })
        .unwrap();
        assert!(s.graph_inspection().recovery_required);
        assert!(s.state.muted && !s.state.output_consented);
        assert!(s.graph_activation().is_none());
        let events = s.events_since(cursor).events;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].request_id.as_ref(), Some(&c.request_id));
        assert_eq!(s.submit_graph(c, 23).status, ReceiptStatus::Failed);
    }
    #[test]
    fn failed_source_closes_even_when_counters_cannot_advance() {
        for counter in ["revision", "events", "device", "stream"] {
            let mut s = fixture();
            match counter {
                "revision" => s.state.revision = u64::MAX,
                "events" => s.state.event_sequence = u64::MAX,
                "device" => s.state.device_epoch = u64::MAX,
                "stream" => s.state.epochs.stream = u64::MAX,
                _ => unreachable!(),
            }
            s.advance(EngineEvent {
                epochs: s.state.epochs,
                monotonic_ns: 1,
                kind: EngineEventKind::LocalFailed {
                    code: ErrorCode::OutputFault,
                },
            })
            .unwrap();
            assert!(s.shutdown_requested(), "{counter}");
            assert_eq!(s.state.transport, TransportState::Failed);
            assert!(s.state.muted && !s.state.output_consented);
            assert_eq!(s.state.error, Some(ErrorCode::OutputFault));
        }
    }
    #[test]
    fn normal_completion_settles_activation_even_after_revision_exhaustion() {
        let mut s = fixture();
        s.state.revision = u64::MAX - 2;
        let p = ready(&mut s, "prepare", 1.0);
        let c = commit(&s, "commit", &p);
        assert_eq!(
            s.submit_graph(c.clone(), 21).status,
            ReceiptStatus::Accepted
        );
        let mute = command(&s, "mute", Command::Mute { muted: true });
        assert_eq!(s.submit_graph(mute, 22).status, ReceiptStatus::Applied);
        s.advance(EngineEvent {
            epochs: s.state.epochs,
            monotonic_ns: 23,
            kind: EngineEventKind::LocalCompleted {
                observation: s.state.playback.clone().unwrap(),
            },
        })
        .unwrap();
        assert!(s.graph_activation().is_none());
        let receipt = s.submit_graph(c, 24);
        assert_eq!(receipt.status, ReceiptStatus::Failed);
        assert_eq!(
            receipt.error.unwrap().code,
            ErrorCode::ResourceBudgetExceeded
        );
        assert!(s.shutdown_requested());
    }
    #[test]
    fn a_new_owner_reserves_startup_capacity_and_preserves_ordinary_revisions() {
        for (revision, expected) in [
            (123, 123),
            (u64::MAX - 64, u64::MAX - 64),
            (u64::MAX - 63, 0),
            (u64::MAX - 1, 0),
            (u64::MAX, 0),
        ] {
            let mut s = fixture();
            s.state.revision = revision;
            let queue = s.queue_snapshot();
            let restored = Session::restore_owner(
                ZoneId::new("main").unwrap(),
                OwnerId::new("new-owner").unwrap(),
                ControlLimits::default(),
                s.checkpoint(),
            )
            .unwrap();
            assert_eq!(restored.state.revision, expected);
            assert_eq!(restored.queue_snapshot(), queue);
            assert!(restored.state.muted && !restored.state.output_consented);
        }
    }
    #[test]
    fn activation_completion_keeps_its_final_event_slot() {
        for source in [
            "mute",
            "resource",
            "queue",
            "progress",
            "reservation",
            "ready",
        ] {
            let mut s = fixture();
            let p = ready(&mut s, "prepare", 1.0);
            s.state.event_sequence = u64::MAX - 2;
            let c = commit(&s, "commit", &p);
            s.submit_graph(c.clone(), 21);
            let cursor = s.state.event_sequence;
            match source {
                "mute" => {
                    let c = command(&s, "mute", Command::Mute { muted: true });
                    assert_eq!(s.submit_graph(c, 22).status, ReceiptStatus::Rejected);
                }
                "resource" => {
                    s.observe_graph_resources(
                        GraphServiceObservation {
                            resident_bytes: 1,
                            pending_retirements: 0,
                            preparing_or_candidate: true,
                        },
                        22,
                    )
                    .unwrap();
                }
                "queue" => {
                    let entry = s.state.active_entry_id.clone().unwrap();
                    assert!(
                        s.set_queue_preparation(
                            s.state.epochs,
                            &entry,
                            crate::queue::PreparationStatus::Unprepared
                        )
                        .is_err()
                    );
                }
                "progress" => {
                    assert!(
                        s.advance(EngineEvent {
                            epochs: s.state.epochs,
                            monotonic_ns: 22,
                            kind: EngineEventKind::LocalProgress {
                                observation: s.state.playback.as_ref().unwrap().clone(),
                            }
                        })
                        .is_err()
                    );
                }
                "reservation" => {
                    let before = s.snapshot();
                    s.advance(EngineEvent {
                        epochs: s.state.epochs,
                        monotonic_ns: 22,
                        kind: EngineEventKind::LocalReserved {
                            active: before.active_entry_id.clone().unwrap(),
                            next: None,
                        },
                    })
                    .unwrap();
                    assert_eq!(s.snapshot(), before);
                }
                "ready" => {
                    let before = s.queue_snapshot();
                    let entry = s.state.active_entry_id.clone().unwrap();
                    s.set_queue_preparation(
                        s.state.epochs,
                        &entry,
                        crate::queue::PreparationStatus::Ready {
                            format: s.state.playback.as_ref().unwrap().format,
                            total_frames: 48000,
                        },
                    )
                    .unwrap();
                    assert_eq!(s.queue_snapshot(), before);
                }
                _ => unreachable!(),
            }
            assert_eq!(s.state.event_sequence, cursor, "{source}");
            s.fail_graph_activation(
                &c.request_id,
                EngineError::new(ErrorCode::OutputFault, "test failure"),
            );
            let events = s.events_since(cursor).events;
            assert_eq!(events.len(), 1, "{source}");
            assert_eq!(events[0].request_id.as_ref(), Some(&c.request_id));
            assert_eq!(events[0].transport, TransportState::Failed);
        }
    }
    #[test]
    fn stopped_source_settles_activation_before_consuming_its_terminal_event_slot() {
        let mut s = fixture();
        let p = ready(&mut s, "prepare", 1.0);
        s.state.event_sequence = u64::MAX - 2;
        let c = commit(&s, "commit", &p);
        s.submit_graph(c.clone(), 21);
        s.advance(EngineEvent {
            epochs: s.state.epochs,
            monotonic_ns: 22,
            kind: EngineEventKind::LocalStopped {
                observation: s.state.playback.clone(),
            },
        })
        .unwrap();
        assert!(s.graph_activation().is_none());
        assert_eq!(s.submit_graph(c, 23).status, ReceiptStatus::Cancelled);
    }
    #[test]
    fn activation_reserves_a_revision_before_consuming_the_token() {
        let mut s = fixture();
        s.state.revision = u64::MAX - 1;
        let p = ready(&mut s, "prepare", 1.0);
        let before = s.state.clone();
        let c = commit(&s, "commit", &p);
        assert_eq!(s.submit_graph(c, 21).status, ReceiptStatus::Rejected);
        assert_eq!(s.state, before);
        assert_eq!(s.graph_inspection().prepared.unwrap().token, p.token);
        assert!(s.graph_activation().is_none());
    }
    #[test]
    fn activation_failure_publishes_one_terminal_event() {
        let mut s = fixture();
        let p = ready(&mut s, "prepare", 1.0);
        let c = commit(&s, "commit", &p);
        s.submit_graph(c.clone(), 21);
        let cursor = s.state.event_sequence;
        let error = EngineError::new(ErrorCode::OutputFault, "test activation failure");
        s.fail_graph_activation(&c.request_id, error);
        let events = s.events_since(cursor).events;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].request_id.as_ref(), Some(&c.request_id));
        assert_eq!(events[0].transport, TransportState::Failed);
        s.fail_graph_activation(&c.request_id, error);
        assert_eq!(s.events_since(cursor).events.len(), 1);
    }
    #[test]
    fn counter_exhaustion_after_emergency_mute_settles_activation_failure() {
        let mut s = fixture();
        s.state.revision = u64::MAX - 2;
        let p = ready(&mut s, "prepare", 1.0);
        let c = commit(&s, "commit", &p);
        assert_eq!(
            s.submit_graph(c.clone(), 21).status,
            ReceiptStatus::Accepted
        );
        let mute = command(&s, "mute", Command::Mute { muted: true });
        assert_eq!(s.submit_graph(mute, 22).status, ReceiptStatus::Applied);
        assert!(
            s.complete_graph_activation(
                GraphActivationAck {
                    source_total_frames: 48000,
                    request_id: c.request_id.clone(),
                    epochs: s.state.epochs,
                    graph_hash: p.preview.graph_hash.clone(),
                    activation_frame: 0,
                    observation: s.state.playback.as_ref().unwrap().clone()
                },
                23,
                |_| panic!("exhausted counters cannot persist activation")
            )
            .is_err()
        );
        assert_eq!(s.submit_graph(c, 24).status, ReceiptStatus::Failed);
        assert!(s.graph_activation().is_none());
        assert!(s.graph_inspection().recovery_required);
        assert!(!s.state.output_consented);
    }
    #[test]
    fn bounded_semantic_graph_rejection_reserves_the_request_identity() {
        let mut s = fixture();
        let mut c = prepare(&s, "bad-matrix");
        let Command::GraphPrepare { graph, .. } = &mut c.payload else {
            unreachable!()
        };
        Arc::make_mut(graph).nodes[0].kind = NodeKind::Matrix {
            input_channels: ChannelLayout::Stereo,
            coefficients: vec![FiniteGraphScalar::new(1.0).unwrap(); 3],
        };
        assert_eq!(
            s.submit_graph(c.clone(), 1).error.unwrap().code,
            ErrorCode::GraphInvalid
        );
        let reused = command(&s, "bad-matrix", Command::Mute { muted: true });
        assert_eq!(
            s.submit_graph(reused, 2).error.unwrap().code,
            ErrorCode::AssetInvalid
        );
        assert_eq!(
            s.submit_graph(c, 3).error.unwrap().code,
            ErrorCode::GraphInvalid
        );
    }
    #[test]
    fn readiness_revision_and_token_expiry_cannot_be_bypassed() {
        let mut s = fixture();
        let p = ready(&mut s, "prepare", 1.0);
        let c = command(&s, "mute", Command::Mute { muted: true });
        s.submit_graph(c, 21);
        let c = commit(&s, "stale", &p);
        assert_eq!(
            s.submit_graph(c, 22).error.unwrap().code,
            ErrorCode::StaleRevision
        );
        let mut s = fixture();
        let p = ready(&mut s, "prepare", 1.0);
        let c = commit(&s, "expired", &p);
        assert_eq!(
            s.submit_graph(c, p.expires_monotonic_ns)
                .error
                .unwrap()
                .code,
            ErrorCode::StaleSession
        );
        assert!(s.graph_inspection().prepared.is_none());
    }
    #[test]
    fn late_preparation_cannot_resurrect_after_stop_or_revision_change() {
        for stop in [false, true] {
            let mut s = fixture();
            let c = prepare(&s, "prepare");
            s.submit_graph(c.clone(), 10);
            let p = preview(&s, 1.0);
            let mutation = command(
                &s,
                "change",
                if stop {
                    Command::Stop
                } else {
                    Command::Mute { muted: true }
                },
            );
            s.submit_graph(mutation, 15);
            assert!(
                s.complete_graph_preparation(&c.request_id, &p.binding.clone(), 20, Ok(p))
                    .is_err()
            );
            assert!(s.graph_inspection().prepared.is_none());
            assert_ne!(s.submit_graph(c, 21).status, ReceiptStatus::Applied);
        }
    }
    #[test]
    fn activation_stays_muted_and_only_exact_acknowledgement_applies_once() {
        let mut s = fixture();
        let p = ready(&mut s, "prepare", 1.0);
        let c = commit(&s, "commit", &p);
        assert_eq!(
            s.submit_graph(c.clone(), 21).status,
            ReceiptStatus::Accepted
        );
        assert_eq!(s.state.transport, TransportState::Reconfiguring);
        assert!(s.state.muted);
        assert_eq!(s.state.graph_epoch, 0);
        assert!(s.state.operation.is_none());
        let unmute = command(&s, "unsafe-unmute", Command::Mute { muted: false });
        assert_eq!(
            s.submit_graph(unmute, 22).error.unwrap().code,
            ErrorCode::SafetyPolicyBlocked
        );
        let mute = command(&s, "emergency-mute", Command::Mute { muted: true });
        s.submit_graph(mute, 23);
        let epochs = s.state.epochs;
        assert!(
            s.complete_graph_activation(
                GraphActivationAck {
                    source_total_frames: 48000,
                    request_id: RequestId::new("wrong").unwrap(),
                    epochs,
                    graph_hash: p.preview.graph_hash.clone(),
                    activation_frame: 0,
                    observation: s.state.playback.as_ref().unwrap().clone()
                },
                24,
                |_| Ok(())
            )
            .is_err()
        );
        s.complete_graph_activation(
            GraphActivationAck {
                source_total_frames: 48000,
                request_id: c.request_id.clone(),
                epochs,
                graph_hash: p.preview.graph_hash.clone(),
                activation_frame: 37,
                observation: s.state.playback.as_ref().unwrap().clone(),
            },
            25,
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(s.state.graph_epoch, 1);
        assert_eq!(s.graph.active.as_ref().unwrap().binding, p.preview.binding);
        assert!(s.state.muted);
        assert!(s.graph_inspection().configured.is_some());
        assert_eq!(s.submit_graph(c.clone(), 26).status, ReceiptStatus::Applied);
        assert_eq!(s.submit_graph(c.clone(), 26).activation_frame, Some(37));
        assert!(
            s.complete_graph_activation(
                GraphActivationAck {
                    source_total_frames: 48000,
                    request_id: c.request_id.clone(),
                    epochs,
                    graph_hash: p.preview.graph_hash.clone(),
                    activation_frame: 0,
                    observation: s.state.playback.as_ref().unwrap().clone()
                },
                27,
                |_| Ok(())
            )
            .is_err()
        );
        assert_eq!(s.state.graph_epoch, 1);
        let consumed = commit(&s, "consumed", &p);
        assert_eq!(s.submit_graph(consumed, 28).status, ReceiptStatus::Rejected);
    }
    #[test]
    fn graph_headroom_cannot_defeat_caps_or_grant_volume_authority() {
        let mut s = fixture();
        let p = ready(&mut s, "prepare", 100.0);
        let c = commit(&s, "commit", &p);
        s.submit_graph(c.clone(), 21);
        s.complete_graph_activation(
            GraphActivationAck {
                source_total_frames: 48000,
                request_id: c.request_id.clone(),
                epochs: s.state.epochs,
                graph_hash: p.preview.graph_hash.clone(),
                activation_frame: 0,
                observation: s.state.playback.as_ref().unwrap().clone(),
            },
            22,
            |_| Ok(()),
        )
        .unwrap();
        let loud = command(
            &s,
            "loud",
            Command::VolumeSet {
                millidecibels: -39000,
                ramp_frames: 0,
            },
        );
        assert_eq!(
            s.submit_graph(loud, 23).error.unwrap().code,
            ErrorCode::SafetyPolicyBlocked
        );
        let mut attenuated = (*p.preview).clone();
        attenuated.channel_peak_bounds = vec![0.001; 2];
        assert!(attenuated.validate_headroom(-10.0, -20.0).is_err());
        let mut malformed = attenuated;
        malformed.channel_peak_bounds[0] = f64::NAN;
        assert!(malformed.validate_headroom(-40.0, 0.0).is_err());
    }
    #[test]
    fn active_graph_mute_rewinds_only_at_a_new_stream_epoch() {
        for payload in [
            Command::Mute { muted: true },
            Command::VolumeCaps {
                software_millidecibels: -60000,
                hardware_millidecibels: None,
            },
        ] {
            let mut s = fixture();
            let p = ready(&mut s, "prepare-mute", 1.0);
            let c = commit(&s, "commit-mute", &p);
            s.submit_graph(c.clone(), 21);
            s.complete_graph_activation(
                GraphActivationAck {
                    source_total_frames: 48000,
                    request_id: c.request_id,
                    epochs: s.state.epochs,
                    graph_hash: p.preview.graph_hash.clone(),
                    activation_frame: 0,
                    observation: s.state.playback.clone().unwrap(),
                },
                22,
                |_| Ok(()),
            )
            .unwrap();
            let unmute = command(&s, "unmute-before-rewind", Command::Mute { muted: false });
            assert_eq!(s.submit_graph(unmute, 23).status, ReceiptStatus::Applied);
            let before = s.state.epochs;
            let mute = command(&s, "mute-for-rewind", payload.clone());
            assert_eq!(s.submit_graph(mute, 24).status, ReceiptStatus::Applied);
            assert!(s.state.muted);
            assert_eq!(s.state.epochs, before.next_stream().unwrap());
            let after = s.state.epochs;
            let repeat = command(&s, "repeat-mute-without-rewind", payload);
            assert_eq!(s.submit_graph(repeat, 25).status, ReceiptStatus::Applied);
            assert_eq!(s.state.epochs, after);
        }
    }
    #[test]
    fn activation_at_final_queue_join_updates_seek_authority_atomically() {
        let mut s = fixture();
        s.state.seekable = true;
        let first = s.state.active_entry_id.clone().unwrap();
        let last = s.queue.append("final.wav").unwrap();
        let format = s.state.playback.as_ref().unwrap().format;
        for (entry, total_frames) in [(&first, 48000), (&last, 12000)] {
            s.queue
                .update_status(
                    entry,
                    crate::queue::PreparationStatus::Ready {
                        format,
                        total_frames,
                    },
                )
                .unwrap();
        }
        s.state.reserved_next_entry_id = Some(last.clone());
        let p = ready(&mut s, "prepare-final-join", 1.0);
        let c = commit(&s, "commit-final-join", &p);
        s.submit_graph(c.clone(), 21);
        let mut observation = s.state.playback.clone().unwrap();
        observation.source_entry_id = last.clone();
        observation.source_position_frames = 37;
        s.complete_graph_activation(
            GraphActivationAck {
                source_total_frames: 12000,
                request_id: c.request_id,
                epochs: s.state.epochs,
                graph_hash: p.preview.graph_hash.clone(),
                activation_frame: 48037,
                observation,
            },
            22,
            |staged| {
                assert_eq!(staged.state.active_entry_id.as_ref(), Some(&last));
                assert_eq!(staged.state.reserved_next_entry_id, None);
                Ok(())
            },
        )
        .unwrap();
        let seek = command(
            &s,
            "seek-past-final-file",
            Command::Seek {
                position_frames: 24000,
            },
        );
        let receipt = s.submit_graph(seek, 23);
        assert_eq!(receipt.status, ReceiptStatus::Rejected);
        assert_eq!(receipt.error.unwrap().code, ErrorCode::AssetInvalid);
    }
    #[test]
    fn activation_persists_the_post_install_playback_observation_atomically() {
        let mut s = fixture();
        let p = ready(&mut s, "prepare", 1.0);
        let c = commit(&s, "commit", &p);
        s.submit_graph(c.clone(), 21);
        let mut observation = s.state.playback.as_ref().unwrap().clone();
        observation.source_position_frames = 37;
        observation.audible_source_position_frames = Some(31);
        observation.processing = Some(PlaybackProcessingObservation {
            streaming: None,
            source_boundary: None,
            graph_hash: p.preview.graph_hash.clone(),
            resident_bytes: 123,
            audio_payload_bytes: Some(64),
            failure_node: None,
            timing: PlaybackTiming::SameRate {
                latency_frames: 6,
                tail_frames: 6,
            },
            saturation_count: 0,
            graph_peak: None,
            prequantization_peak: None,
        });
        s.complete_graph_activation(
            GraphActivationAck {
                source_total_frames: 48000,
                request_id: c.request_id.clone(),
                epochs: s.state.epochs,
                graph_hash: p.preview.graph_hash.clone(),
                activation_frame: 37,
                observation: observation.clone(),
            },
            22,
            |staged| {
                assert_eq!(staged.state.playback.as_ref(), Some(&observation));
                assert_eq!(staged.state.decoded_position_frames, 37);
                assert_eq!(staged.state.audible_position_frames, Some(31));
                assert_eq!(
                    staged.graph.active.as_ref().unwrap().graph_hash,
                    observation.processing.as_ref().unwrap().graph_hash
                );
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(s.state.playback.as_ref(), Some(&observation));
    }
    #[test]
    fn persistence_failure_never_publishes_applied_graph_or_unmutes() {
        let mut s = fixture();
        let p = ready(&mut s, "prepare", 1.0);
        let c = commit(&s, "commit", &p);
        s.submit_graph(c.clone(), 21);
        assert!(
            s.complete_graph_activation(
                GraphActivationAck {
                    source_total_frames: 48000,
                    request_id: c.request_id.clone(),
                    epochs: s.state.epochs,
                    graph_hash: p.preview.graph_hash.clone(),
                    activation_frame: 0,
                    observation: s.state.playback.as_ref().unwrap().clone()
                },
                22,
                |_| Err(EngineError::new(
                    ErrorCode::InternalFault,
                    "test persistence failure"
                ))
            )
            .is_err()
        );
        assert!(s.state.muted);
        assert_eq!(s.state.graph_epoch, 0);
        assert!(s.graph_inspection().configured.is_none());
        assert_eq!(s.submit_graph(c, 23).status, ReceiptStatus::Failed);
    }
    #[test]
    fn exhausted_completion_counters_settle_the_accepted_request() {
        let mut s = fixture();
        let c = prepare(&s, "prepare");
        s.submit_graph(c.clone(), 10);
        let p = preview(&s, 1.0);
        s.graph.serial = u64::MAX;
        assert!(
            s.complete_graph_preparation(&c.request_id, &p.binding.clone(), 20, Ok(p))
                .is_err()
        );
        assert_eq!(s.submit_graph(c, 21).status, ReceiptStatus::Failed);
        assert!(s.graph_inspection().prepared.is_none());
    }
    #[test]
    fn stale_ready_slot_does_not_block_fresh_preparation_after_a_volume_change() {
        let mut s = fixture();
        ready(&mut s, "prepare", 1.0);
        let change = command(
            &s,
            "lower",
            Command::VolumeSet {
                millidecibels: -50000,
                ramp_frames: 0,
            },
        );
        assert_eq!(s.submit_graph(change, 21).status, ReceiptStatus::Applied);
        let replacement = prepare(&s, "replacement");
        assert_eq!(
            s.submit_graph(replacement, 22).status,
            ReceiptStatus::Accepted
        );
        assert!(s.graph_inspection().prepared.is_none());
    }
    #[test]
    fn failed_replacement_preserves_the_last_working_graph() {
        let mut s = fixture();
        let p = ready(&mut s, "prepare", 1.0);
        let c = commit(&s, "commit", &p);
        s.submit_graph(c.clone(), 21);
        s.complete_graph_activation(
            GraphActivationAck {
                source_total_frames: 48000,
                request_id: c.request_id.clone(),
                epochs: s.state.epochs,
                graph_hash: p.preview.graph_hash.clone(),
                activation_frame: 0,
                observation: s.state.playback.as_ref().unwrap().clone(),
            },
            22,
            |_| Ok(()),
        )
        .unwrap();
        let active = s.graph.active.clone().unwrap();
        let configured = s.graph.configured.clone().unwrap();
        let c = prepare(&s, "replacement");
        assert_eq!(
            s.submit_graph(c.clone(), 23).status,
            ReceiptStatus::Accepted
        );
        let binding = s.graph_job().unwrap().binding;
        assert!(
            s.complete_graph_preparation(
                &c.request_id,
                &binding,
                24,
                Err(
                    EngineError::new(ErrorCode::ResourceBudgetExceeded, "test allocation failure")
                        .into()
                )
            )
            .is_err()
        );
        assert!(Arc::ptr_eq(&active, s.graph.active.as_ref().unwrap()));
        assert!(Arc::ptr_eq(
            &configured,
            s.graph.configured.as_ref().unwrap()
        ));
        assert_eq!(s.submit_graph(c, 25).status, ReceiptStatus::Failed);
    }
    #[test]
    fn output_failure_cancels_preparation_and_clears_active_runtime_evidence() {
        let mut s = fixture();
        let c = prepare(&s, "prepare");
        s.submit_graph(c.clone(), 10);
        let p = preview(&s, 1.0);
        s.advance(EngineEvent {
            epochs: s.state.epochs,
            monotonic_ns: 15,
            kind: EngineEventKind::LocalFailed {
                code: ErrorCode::OutputFault,
            },
        })
        .unwrap();
        assert!(s.graph_job().is_none());
        assert!(
            s.complete_graph_preparation(&c.request_id, &p.binding.clone(), 20, Ok(p))
                .is_err()
        );
        assert_eq!(s.submit_graph(c, 21).status, ReceiptStatus::Failed);
        assert!(s.state.muted);
    }
    #[test]
    fn unsafe_preparation_fails_before_retaining_a_ready_pipeline() {
        let mut s = fixture();
        let c = prepare(&s, "prepare");
        s.submit_graph(c.clone(), 10);
        let p = preview(&s, 101.0);
        assert_eq!(
            s.complete_graph_preparation(&c.request_id, &p.binding.clone(), 20, Ok(p))
                .unwrap_err()
                .code,
            ErrorCode::SafetyPolicyBlocked
        );
        assert!(s.graph_inspection().prepared.is_none());
        assert_eq!(s.submit_graph(c, 21).status, ReceiptStatus::Failed);
    }
    #[test]
    fn restored_processing_intent_cannot_silently_play_bypass() {
        let mut s = fixture();
        let p = ready(&mut s, "prepare", 1.0);
        let c = commit(&s, "commit", &p);
        s.submit_graph(c.clone(), 21);
        s.complete_graph_activation(
            GraphActivationAck {
                source_total_frames: 48000,
                request_id: c.request_id.clone(),
                epochs: s.state.epochs,
                graph_hash: p.preview.graph_hash.clone(),
                activation_frame: 0,
                observation: s.state.playback.as_ref().unwrap().clone(),
            },
            22,
            |_| Ok(()),
        )
        .unwrap();
        let bytes = serde_json::to_vec(&s.checkpoint()).unwrap();
        let checkpoint = serde_json::from_slice(&bytes).unwrap();
        let mut restored = Session::restore_owner(
            ZoneId::new("main").unwrap(),
            OwnerId::new("restored").unwrap(),
            ControlLimits::default(),
            checkpoint,
        )
        .unwrap();
        restored.enable_local_playback();
        restored.state.output_consented = true;
        let c = command(&restored, "unmute", Command::Mute { muted: false });
        assert_eq!(
            restored.submit_graph(c, 23).error.unwrap().code,
            ErrorCode::SafetyPolicyBlocked
        );
        assert!(restored.graph_inspection().configured.is_some());
        assert!(restored.graph_inspection().active.is_none());
    }
    #[test]
    fn completed_validation_and_activation_replay_after_history_rotation() {
        let mut s = fixture();
        let mut validation = prepare(&s, "validate");
        if let Command::GraphPrepare { graph, processing } = validation.payload {
            validation.payload = Command::GraphValidate { graph, processing };
        }
        s.submit_graph(validation.clone(), 10);
        let p = preview(&s, 1.0);
        for i in 0..1025 {
            let c = command(
                &s,
                &format!("validation-query-{i}"),
                Command::GraphInspect {},
            );
            s.submit_graph(c, 11);
        }
        s.complete_graph_preparation(&validation.request_id, &p.binding.clone(), 20, Ok(p))
            .unwrap();
        assert_eq!(
            s.submit_graph(validation, 21).status,
            ReceiptStatus::Applied
        );
        let mut s = fixture();
        let p = ready(&mut s, "prepare", 1.0);
        let c = commit(&s, "commit", &p);
        s.submit_graph(c.clone(), 21);
        for i in 0..1025 {
            let q = command(
                &s,
                &format!("activation-query-{i}"),
                Command::GraphInspect {},
            );
            s.submit_graph(q, 22);
        }
        s.complete_graph_activation(
            GraphActivationAck {
                source_total_frames: 48000,
                request_id: c.request_id.clone(),
                epochs: s.state.epochs,
                graph_hash: p.preview.graph_hash.clone(),
                activation_frame: 0,
                observation: s.state.playback.as_ref().unwrap().clone(),
            },
            23,
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(s.submit_graph(c, 24).status, ReceiptStatus::Applied);
        assert_eq!(s.receipts.len(), 1024);
    }
    #[test]
    fn failed_graph_activation_also_settles_pending_transport() {
        let mut s = fixture();
        let resume = command(&s, "resume", Command::Resume);
        assert_eq!(
            s.submit_graph(resume.clone(), 1).status,
            ReceiptStatus::Accepted
        );
        let p = ready(&mut s, "prepare", 1.0);
        let c = commit(&s, "commit", &p);
        s.submit_graph(c.clone(), 21);
        s.fail_graph_activation(
            &c.request_id,
            EngineError::new(ErrorCode::OutputFault, "test activation failure"),
        );
        assert_eq!(s.submit_graph(resume, 22).status, ReceiptStatus::Failed);
        assert!(s.state.operation.is_none());
    }
    #[test]
    fn failed_activation_requires_explicit_graph_recovery_before_unmute() {
        let mut s = fixture();
        let p = ready(&mut s, "prepare", 100.0);
        let c = commit(&s, "commit", &p);
        s.submit_graph(c.clone(), 21);
        let previous_epochs = s.state.epochs;
        s.fail_graph_activation(
            &c.request_id,
            EngineError::new(ErrorCode::InternalFault, "test persistence failure"),
        );
        let consent = command(
            &s,
            "consent",
            Command::OutputConsent {
                device_id: DeviceId::new("dac").unwrap(),
                profile_id: ProfileId::new("profile").unwrap(),
            },
        );
        assert_eq!(s.submit_graph(consent, 22).status, ReceiptStatus::Applied);
        let loud = command(
            &s,
            "loud",
            Command::VolumeSet {
                millidecibels: 0,
                ramp_frames: 0,
            },
        );
        s.submit_graph(loud, 23);
        let unmute = command(&s, "unmute", Command::Mute { muted: false });
        assert_eq!(
            s.submit_graph(unmute, 24).error.unwrap().code,
            ErrorCode::SafetyPolicyBlocked
        );
        assert_ne!(s.state.epochs, previous_epochs);
        assert_eq!(
            serde_json::to_value(s.graph_inspection()).unwrap()["recovery_required"],
            true
        );
    }
    #[test]
    fn live_job_identity_survives_the_ordinary_receipt_window() {
        let mut s = fixture();
        let c = prepare(&s, "prepare");
        let receipt = s.submit_graph(c.clone(), 10);
        for i in 0..1025 {
            let query = command(&s, &format!("query-{i}"), Command::GraphInspect {});
            s.submit_graph(query, 11);
        }
        assert_eq!(s.receipts.len(), 1024);
        assert_eq!(s.submit_graph(c, 12), receipt);
    }
    #[test]
    fn pending_transport_identity_survives_receipt_rotation_and_settlement() {
        for payload in [
            Command::Pause,
            Command::Seek {
                position_frames: 50,
            },
            Command::Stop,
            Command::Start {
                source: SourceKind::LocalFile,
                device_id: DeviceId::new("dac").unwrap(),
                takeover: true,
                seekable: true,
            },
        ] {
            let mut s = fixture();
            s.state.seekable = true;
            let value = s.state.playback.clone().unwrap();
            s.queue
                .update_status(
                    &value.source_entry_id,
                    crate::queue::PreparationStatus::Ready {
                        format: value.format,
                        total_frames: 1000,
                    },
                )
                .unwrap();
            let request = command(&s, "pending-transport", payload.clone());
            let accepted = s.submit_graph(request.clone(), 1);
            assert_eq!(accepted.status, ReceiptStatus::Accepted);
            for i in 0..1025 {
                let query = command(&s, &format!("transport-poll-{i}"), Command::State);
                assert_eq!(s.submit_graph(query, 2).status, ReceiptStatus::Applied);
            }
            assert_eq!(s.submit_graph(request.clone(), 3), accepted);
            let reused = prepare(&s, "pending-transport");
            let rejected = s.submit_graph(reused.clone(), 3);
            assert_eq!(rejected.status, ReceiptStatus::Rejected);
            assert_eq!(rejected.error.unwrap().code, ErrorCode::AssetInvalid);
            let kind = match payload {
                Command::Stop => EngineEventKind::LocalStopped {
                    observation: Some(value),
                },
                Command::Start { .. } => EngineEventKind::LocalCompleted { observation: value },
                _ => EngineEventKind::LocalPaused { observation: value },
            };
            s.advance(EngineEvent {
                epochs: s.state.epochs,
                monotonic_ns: 4,
                kind,
            })
            .unwrap();
            assert_eq!(s.submit_graph(request, 5).status, ReceiptStatus::Applied);
            assert_eq!(
                s.submit_graph(reused, 5).error.unwrap().code,
                ErrorCode::AssetInvalid
            );
            assert_eq!(s.receipts.len(), 1024);
        }
    }
    #[test]
    fn activation_defers_reservation_until_the_matching_playback_acknowledgement() {
        let mut s = fixture();
        let old = s.snapshot();
        let last = s.queue.append("final.wav").unwrap();
        let p = ready(&mut s, "reserve-prepare", 1.0);
        let c = commit(&s, "reserve-commit", &p);
        assert_eq!(s.submit_graph(c, 21).status, ReceiptStatus::Accepted);
        s.advance(EngineEvent {
            epochs: s.state.epochs,
            monotonic_ns: 22,
            kind: EngineEventKind::LocalReserved {
                active: last,
                next: None,
            },
        })
        .unwrap();
        assert_eq!(s.state.active_entry_id, old.active_entry_id);
        assert_eq!(s.state.playback, old.playback);
    }
    #[test]
    fn activation_restores_readiness_when_the_open_final_tracks_old_ready_was_dropped() {
        let mut s = fixture();
        s.state.seekable = true;
        let last = s.queue.append("final.wav").unwrap();
        let old_epochs = s.state.epochs;
        let p = ready(&mut s, "ready-prepare", 1.0);
        let c = commit(&s, "ready-commit", &p);
        assert_eq!(
            s.submit_graph(c.clone(), 21).status,
            ReceiptStatus::Accepted
        );
        let mut value = s.state.playback.clone().unwrap();
        value.source_entry_id = last.clone();
        value.source_position_frames = 37;
        assert_eq!(
            s.set_queue_preparation(
                old_epochs,
                &last,
                crate::queue::PreparationStatus::Ready {
                    format: value.format,
                    total_frames: 1000,
                }
            )
            .unwrap_err()
            .code,
            ErrorCode::StaleSession
        );
        s.complete_graph_activation(
            GraphActivationAck {
                source_total_frames: 1000,
                request_id: c.request_id,
                epochs: s.state.epochs,
                graph_hash: p.preview.graph_hash.clone(),
                activation_frame: 37,
                observation: value.clone(),
            },
            22,
            |staged| {
                assert_eq!(
                    staged.queue_snapshot().entries.last().unwrap().status,
                    crate::queue::PreparationStatus::Ready {
                        format: value.format,
                        total_frames: 1000
                    }
                );
                Ok(())
            },
        )
        .unwrap();
        let seek = command(
            &s,
            "seek-ready-final",
            Command::Seek {
                position_frames: 999,
            },
        );
        assert_eq!(s.submit_graph(seek, 23).status, ReceiptStatus::Accepted);
    }
    #[test]
    fn activation_readiness_stays_private_until_persistence_succeeds() {
        for fail in [false, true] {
            let mut s = fixture();
            let current = s.state.active_entry_id.clone().unwrap();
            let next = s.queue.append("following.wav").unwrap();
            let p = ready(&mut s, "ready-atomic", 1.0);
            let c = commit(&s, "commit-atomic-ready", &p);
            assert_eq!(
                s.submit_graph(c.clone(), 21).status,
                ReceiptStatus::Accepted
            );
            let before = s.queue_snapshot();
            let snapshot = s.snapshot();
            let format = s.state.playback.as_ref().unwrap().format;
            for (id, total_frames) in [(&current, 48000), (&next, 12000)] {
                s.set_queue_preparation(
                    s.state.epochs,
                    id,
                    crate::queue::PreparationStatus::Ready {
                        format,
                        total_frames,
                    },
                )
                .unwrap();
            }
            assert_eq!(s.queue_snapshot(), before);
            assert_eq!(s.snapshot(), snapshot);
            let result = s.complete_graph_activation(
                GraphActivationAck {
                    source_total_frames: 48000,
                    request_id: c.request_id,
                    epochs: s.state.epochs,
                    graph_hash: p.preview.graph_hash.clone(),
                    activation_frame: 0,
                    observation: s.state.playback.clone().unwrap(),
                },
                22,
                |staged| {
                    assert!(staged.queue_snapshot().entries.iter().all(|e| matches!(
                        e.status,
                        crate::queue::PreparationStatus::Ready { .. }
                    )));
                    if fail {
                        Err(EngineError::new(
                            ErrorCode::InternalFault,
                            "test persistence fault",
                        ))
                    } else {
                        Ok(())
                    }
                },
            );
            if fail {
                assert!(result.is_err());
                assert_eq!(s.queue_snapshot(), before);
            } else {
                result.unwrap();
                assert!(matches!(
                    s.queue_snapshot().entries[1].status,
                    crate::queue::PreparationStatus::Ready {
                        total_frames: 12000,
                        ..
                    }
                ));
            }
            s.set_queue_preparation(
                s.state.epochs,
                &next,
                crate::queue::PreparationStatus::Ready {
                    format,
                    total_frames: 8000,
                },
            )
            .unwrap();
            assert!(matches!(
                s.queue_snapshot().entries[1].status,
                crate::queue::PreparationStatus::Ready {
                    total_frames: 8000,
                    ..
                }
            ));
        }
    }
    #[test]
    fn deferred_readiness_discards_removed_entries_and_stays_queue_bounded() {
        let mut s = fixture();
        let p = ready(&mut s, "bounded-ready", 1.0);
        let c = commit(&s, "bounded-ready-commit", &p);
        assert_eq!(
            s.submit_graph(c.clone(), 21).status,
            ReceiptStatus::Accepted
        );
        let format = s.state.playback.as_ref().unwrap().format;
        for index in 0..300 {
            let id = s.queue.append(format!("queued-{index}.wav")).unwrap();
            s.set_queue_preparation(
                s.state.epochs,
                &id,
                crate::queue::PreparationStatus::Ready {
                    format,
                    total_frames: 65,
                },
            )
            .unwrap();
            assert_eq!(s.graph.activation.as_ref().unwrap().readiness.len(), 1);
            s.queue.remove(&id).unwrap();
        }
        s.complete_graph_activation(
            GraphActivationAck {
                source_total_frames: 48000,
                request_id: c.request_id,
                epochs: s.state.epochs,
                graph_hash: p.preview.graph_hash.clone(),
                activation_frame: 0,
                observation: s.state.playback.clone().unwrap(),
            },
            22,
            |_| Ok(()),
        )
        .unwrap();
        assert_eq!(s.queue_snapshot().entries.len(), 1);
        assert!(matches!(
            s.queue_snapshot().entries[0].status,
            crate::queue::PreparationStatus::Ready {
                total_frames: 48000,
                ..
            }
        ));
    }
    #[test]
    fn newer_nonready_status_is_not_overwritten_by_deferred_ready_after_activation() {
        use crate::queue::PreparationStatus;
        for status in [
            PreparationStatus::Unprepared,
            PreparationStatus::Failed {
                code: ErrorCode::AssetInvalid,
            },
        ] {
            let mut s = fixture();
            let next = s.queue.append("following.wav").unwrap();
            let p = ready(&mut s, "prepare-newer-status", 1.0);
            let c = commit(&s, "commit-newer-status", &p);
            assert_eq!(
                s.submit_graph(c.clone(), 21).status,
                ReceiptStatus::Accepted
            );
            let format = s.state.playback.as_ref().unwrap().format;
            let before = s.queue_snapshot();
            s.set_queue_preparation(
                s.state.epochs,
                &next,
                PreparationStatus::Ready {
                    format,
                    total_frames: 12000,
                },
            )
            .unwrap();
            assert_eq!(s.queue_snapshot(), before);
            s.set_queue_preparation(s.state.epochs, &next, status)
                .unwrap();
            assert_eq!(s.queue_snapshot().entries[1].status, status);
            s.complete_graph_activation(
                GraphActivationAck {
                    source_total_frames: 48000,
                    request_id: c.request_id,
                    epochs: s.state.epochs,
                    graph_hash: p.preview.graph_hash.clone(),
                    activation_frame: 0,
                    observation: s.state.playback.clone().unwrap(),
                },
                22,
                |staged| {
                    assert_eq!(staged.queue_snapshot().entries[1].status, status);
                    Ok(())
                },
            )
            .unwrap();
            assert_eq!(s.queue_snapshot().entries[1].status, status);
        }
    }
    #[test]
    fn failed_nonready_updates_preserve_deferred_ready_and_the_reserved_terminal_slot() {
        use crate::queue::PreparationStatus;
        for failure in ["counter", "queue"] {
            let mut s = fixture();
            let next = s.queue.append("following.wav").unwrap();
            let p = ready(&mut s, "prepare-failed-status", 1.0);
            let c = commit(&s, "commit-failed-status", &p);
            assert_eq!(
                s.submit_graph(c.clone(), 21).status,
                ReceiptStatus::Accepted
            );
            let status = PreparationStatus::Ready {
                format: s.state.playback.as_ref().unwrap().format,
                total_frames: 12000,
            };
            s.set_queue_preparation(s.state.epochs, &next, status)
                .unwrap();
            if failure == "counter" {
                s.state.event_sequence = u64::MAX - 1;
            } else {
                s.queue.remove(&next).unwrap();
            }
            let before = s.queue_snapshot();
            let sequence = s.state.event_sequence;
            let error = s
                .set_queue_preparation(
                    s.state.epochs,
                    &next,
                    PreparationStatus::Failed {
                        code: ErrorCode::AssetInvalid,
                    },
                )
                .unwrap_err();
            assert_eq!(
                error.code,
                if failure == "counter" {
                    ErrorCode::ResourceBudgetExceeded
                } else {
                    ErrorCode::AssetInvalid
                }
            );
            assert_eq!(s.queue_snapshot(), before);
            assert_eq!(s.state.event_sequence, sequence);
            assert_eq!(
                s.graph.activation.as_ref().unwrap().readiness.get(&next),
                Some(&status)
            );
            s.complete_graph_activation(
                GraphActivationAck {
                    source_total_frames: 48000,
                    request_id: c.request_id,
                    epochs: s.state.epochs,
                    graph_hash: p.preview.graph_hash.clone(),
                    activation_frame: 0,
                    observation: s.state.playback.clone().unwrap(),
                },
                22,
                |_| Ok(()),
            )
            .unwrap();
            if failure == "counter" {
                assert_eq!(s.state.event_sequence, u64::MAX);
                assert_eq!(s.queue_snapshot().entries[1].status, status);
            } else {
                assert_eq!(s.queue_snapshot().entries.len(), 1);
            }
        }
    }
    #[test]
    fn failed_staged_stop_cannot_cancel_the_retained_transport_record() {
        let mut s = fixture();
        let pause = command(&s, "retained-pause", Command::Pause);
        let accepted = s.submit_graph(pause.clone(), 1);
        assert_eq!(accepted.status, ReceiptStatus::Accepted);
        let stop = command(&s, "failed-stop", Command::Stop);
        let failed = s.submit_with_graph_effects(
            stop,
            2,
            |_| Err(STATE),
            |_| {
                Err(EngineError::new(
                    ErrorCode::InternalFault,
                    "checkpoint refused staged stop",
                ))
            },
        );
        assert_eq!(failed.status, ReceiptStatus::Failed);
        assert_eq!(s.submit_graph(pause.clone(), 3), accepted);
        s.advance(EngineEvent {
            epochs: s.state.epochs,
            monotonic_ns: 4,
            kind: EngineEventKind::LocalPaused {
                observation: s.state.playback.clone().unwrap(),
            },
        })
        .unwrap();
        assert_eq!(s.submit_graph(pause, 5).status, ReceiptStatus::Applied);
    }
    #[test]
    fn retained_transport_records_keep_faults_and_shutdown_cancellation_after_rotation() {
        for shutdown in [false, true] {
            let mut s = fixture();
            let pause = command(&s, "terminal-pause", Command::Pause);
            assert_eq!(
                s.submit_graph(pause.clone(), 1).status,
                ReceiptStatus::Accepted
            );
            for i in 0..1025 {
                let query = command(&s, &format!("terminal-poll-{i}"), Command::State);
                s.submit_graph(query, 2);
            }
            let expected = if shutdown {
                let command = command(&s, "shutdown-pending-pause", Command::OwnerShutdown);
                assert_eq!(s.submit_graph(command, 3).status, ReceiptStatus::Applied);
                ReceiptStatus::Cancelled
            } else {
                s.advance(EngineEvent {
                    epochs: s.state.epochs,
                    monotonic_ns: 3,
                    kind: EngineEventKind::LocalFailed {
                        code: ErrorCode::OutputFault,
                    },
                })
                .unwrap();
                ReceiptStatus::Failed
            };
            let receipt = s.submit_graph(pause, 4);
            assert_eq!(receipt.status, expected);
            assert_eq!(
                receipt.error.unwrap().code,
                if shutdown {
                    ErrorCode::OperationCancelled
                } else {
                    ErrorCode::OutputFault
                }
            );
            assert!(s.state.operation.is_none());
        }
    }
    #[test]
    fn invalid_activation_source_facts_fail_closed_without_publishing_readiness() {
        for invalid in ["frames", "format", "entry"] {
            let mut s = fixture();
            let p = ready(&mut s, "invalid-source-prepare", 1.0);
            let c = commit(&s, "invalid-source-commit", &p);
            assert_eq!(
                s.submit_graph(c.clone(), 21).status,
                ReceiptStatus::Accepted
            );
            let queue = s.queue_snapshot();
            let mut observation = s.state.playback.clone().unwrap();
            match invalid {
                "frames" => observation.source_position_frames = 1001,
                "format" => {
                    observation.format =
                        PcmFormat::new(44100, ChannelLayout::Stereo, PcmRepresentation::Signed16)
                            .unwrap()
                }
                "entry" => {
                    observation.source_entry_id = QueueEntryId::new("missing-entry").unwrap()
                }
                _ => unreachable!(),
            }
            assert!(
                s.complete_graph_activation(
                    GraphActivationAck {
                        request_id: c.request_id.clone(),
                        epochs: s.state.epochs,
                        graph_hash: p.preview.graph_hash.clone(),
                        activation_frame: 0,
                        observation,
                        source_total_frames: 1000,
                    },
                    22,
                    |_| panic!("invalid source facts reached persistence")
                )
                .is_err()
            );
            assert_eq!(s.queue_snapshot(), queue);
            assert!(s.graph_activation().is_none());
            assert!(s.graph_inspection().recovery_required);
            assert_eq!(s.submit_graph(c, 23).status, ReceiptStatus::Failed);
        }
    }
}
