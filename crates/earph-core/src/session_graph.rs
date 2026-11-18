// ABOUTME: Applies graph jobs and one-use activation tokens through Session.
// ABOUTME: Keeps graph receipts separate from pending transport operations.
use super::*;
use crate::graph::GraphProcessingRequest;

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
    recovery_required: bool,
    service: GraphServiceObservation,
}
impl Session {
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
        if self.local_stop_pending
            && matches!(
                command,
                Command::Pause | Command::Resume | Command::Seek { .. }
            )
        {
            return Err(BUSY);
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
                    || self.local_stop_pending
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
            Command::GraphCommit { token, graph_hash } => {
                let ready = self.graph.ready.as_ref().ok_or(STALE)?;
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
                    self.state.software_gain_db,
                    self.state.volume_policy.software_cap_db(),
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
                });
                self.state.revision = revision;
                self.state.epochs = epochs;
                self.state.muted = true;
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
            if !job.request.validate_only {
                preview
                    .validate_headroom(
                        self.state.software_gain_db,
                        self.state.volume_policy.software_cap_db(),
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
        let mut activated = staged.graph.activation.take().ok_or(STALE)?;
        staged.graph.active = Some(activated.request.prepared.preview.clone());
        staged.graph.configured = Some(activated.intent);
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
        staged.state.decoded_position_frames = observation.source_position_frames;
        staged.state.audible_position_frames = observation.audible_source_position_frames;
        staged.state.playback = Some(observation);
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
            self.local_stop_pending = false;
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
            backend: None,
            graph_hash: "a".repeat(64),
            kernel_hash: "b".repeat(64),
            implementation_version: "test-contract".into(),
            binding: job.binding,
            source_format: native,
            native_output_format: native,
            processing_input_format: float,
            processing_output_format: float,
            quantum_frames: 64,
            latency_frames: 0,
            tail_frames: 0,
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
            assert_eq!(s.submit_graph(c, 3).status, ReceiptStatus::Rejected);
        }
        let c = prepare(&s, "prepare-after-closing-resume");
        assert_eq!(s.submit_graph(c, 4).status, ReceiptStatus::Rejected);
        assert!(s.graph_job().is_none());
    }
    #[test]
    fn activation_completion_keeps_its_final_event_slot() {
        for source in ["mute", "resource", "queue", "progress"] {
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
    fn activation_persists_the_post_install_playback_observation_atomically() {
        let mut s = fixture();
        let p = ready(&mut s, "prepare", 1.0);
        let c = commit(&s, "commit", &p);
        s.submit_graph(c.clone(), 21);
        let mut observation = s.state.playback.as_ref().unwrap().clone();
        observation.source_position_frames = 37;
        observation.audible_source_position_frames = Some(31);
        observation.processing = Some(PlaybackProcessingObservation {
            graph_hash: p.preview.graph_hash.clone(),
            resident_bytes: 123,
            audio_payload_bytes: 64,
            failure_node: None,
            latency_frames: 6,
            tail_frames: 6,
            saturation_count: 0,
            graph_peak: None,
            prequantization_peak: None,
        });
        s.complete_graph_activation(
            GraphActivationAck {
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
}
