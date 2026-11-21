// ABOUTME: Adapts admitted HTTP FLAC to the shared playback and streaming renderer.
// ABOUTME: Preserves immutable source identity and retains timed-out transport ownership.
use super::*;
use crate::{
    network_decode::{NetworkFlacWorker, Outcome, Ready, WorkerError},
    network_source::{LiveBinding, binding},
};
use earph_core::{
    audio::{BlockHeader, TimeDomain},
    network_source::*,
};
use std::time::Instant;

const DENIED: EngineError = EngineError::new(
    ErrorCode::AuthDenied,
    "network execution authority expired or changed",
);
fn worker_error(error: WorkerError) -> EngineError {
    EngineError::new(
        match error {
            WorkerError::Busy | WorkerError::CleanupTimeout => ErrorCode::SourceBusy,
            WorkerError::StaleBinding => ErrorCode::StaleSession,
            _ => ErrorCode::AssetInvalid,
        },
        "network FLAC worker failed",
    )
}
fn outcome_error(outcome: Option<Outcome>) -> Option<EngineError> {
    match outcome {
        Some(Outcome::DecodeFailed(error)) => Some(error),
        Some(Outcome::TransportFailed(_)) => Some(EngineError::new(
            ErrorCode::AssetInvalid,
            "network transport failed",
        )),
        Some(Outcome::Internal) => Some(EngineError::new(
            ErrorCode::InternalFault,
            "network decoder failed",
        )),
        Some(Outcome::Cancelled) => Some(EngineError::new(
            ErrorCode::OperationCancelled,
            "network source cancelled",
        )),
        _ => None,
    }
}
pub(super) struct HttpSource {
    worker: NetworkFlacWorker,
    pub context: Arc<NetworkSourceContext>,
    live: LiveBinding,
    clock: Instant,
    cancel: Arc<AtomicBool>,
    ready: Option<Ready>,
}
impl HttpSource {
    pub fn start(
        context: Arc<NetworkSourceContext>,
        permit: earph_network::FetchPermit,
        live: LiveBinding,
        clock: Instant,
        cancel: Arc<AtomicBool>,
    ) -> Result<Self, (EngineError, Self)> {
        let mut source = Self {
            worker: NetworkFlacWorker::new(),
            context,
            live,
            clock,
            cancel,
            ready: None,
        };
        let current = match source.current() {
            Ok(current) => current,
            Err(error) => return Err((error, source)),
        };
        if let Err(error) = source.worker.start(permit, &current) {
            return Err((worker_error(error), source));
        }
        Ok(source)
    }
    fn current(&self) -> Result<earph_network::SourceBinding, EngineError> {
        if self.cancel.load(Ordering::Acquire) {
            return Err(EngineError::new(
                ErrorCode::OperationCancelled,
                "network source cancelled",
            ));
        }
        if self.clock.elapsed().as_nanos() >= u128::from(self.context.expires_monotonic_ns) {
            return Err(DENIED);
        }
        let live = self.live.lock().map_err(|_| FAILED)?;
        let current = live
            .as_ref()
            .filter(|current| *current == &self.context.binding)
            .ok_or(DENIED)?;
        binding(current)
    }
    pub fn inspect(&mut self) -> Result<crate::network_decode::Observation, EngineError> {
        // Inspect the original job first so cancellation cannot overwrite a genuine failure.
        let bound = binding(&self.context.binding)?;
        let observation = self
            .worker
            .inspect(&bound)
            .map_err(worker_error)?
            .ok_or(FAILED)?;
        if let Some(error) = outcome_error(observation.outcome) {
            return Err(error);
        }
        self.current()?;
        if let Some(ready) = observation.ready {
            self.ready = Some(ready);
        }
        Ok(observation)
    }
    pub fn check(&mut self) -> Result<(), EngineError> {
        self.inspect().map(|_| ())
    }
    pub fn retire(&mut self) -> Result<(), EngineError> {
        self.worker.retire().map_err(worker_error)
    }
    pub fn cancellation(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }
    pub fn owns_job(&self) -> bool {
        self.worker.ownership() != crate::network_decode::Ownership::Idle
    }
    pub(super) fn poll(&mut self) -> Result<SourcePoll, EngineError> {
        let observed = self.inspect()?;
        let current = self.current()?;
        if let Some(accepted) = self.worker.take_frame(&current).map_err(worker_error)? {
            self.current()?;
            let frame = accepted.frame;
            let header = BlockHeader {
                epochs: self.context.source_epochs,
                sequence: frame.sequence(),
                frame_position: frame.frame_position(),
                frame_count: frame.frame_count(),
                format: frame.format(),
                time_domain: TimeDomain::HttpMediaFrames,
            };
            return Ok(SourcePoll::Block(PcmBlock::new(
                header,
                frame.into_samples(),
            )?));
        }
        Ok(if observed.outcome == Some(Outcome::Complete) {
            SourcePoll::End
        } else {
            SourcePoll::Pending
        })
    }
}
pub(super) enum SourcePoll {
    Pending,
    Block(PcmBlock),
    End,
}
pub(super) enum Reader {
    Local(LocalFile),
    Http(Box<HttpSource>),
}
impl Reader {
    pub fn format(&self) -> PcmFormat {
        match self {
            Self::Local(file) => file.format(),
            Self::Http(source) => source.ready.expect("measured network readiness").format,
        }
    }
    pub fn total_frames(&self) -> Option<u64> {
        match self {
            Self::Local(file) => Some(file.total_frames()),
            Self::Http(source) => source.ready.and_then(|r| r.total_frames),
        }
    }
    pub fn next_block(&mut self) -> Result<Option<PcmBlock>, EngineError> {
        match self {
            Self::Local(file) => file.next_block(),
            Self::Http(_) => Err(FAILED),
        }
    }
    pub fn seek(&mut self, position: u64, epochs: Epochs) -> Result<(), EngineError> {
        match self {
            Self::Local(file) => file.seek(position, epochs),
            Self::Http(_) => Err(EngineError::new(
                ErrorCode::CapabilityUnsupported,
                "HTTP source is not seekable",
            )),
        }
    }
    pub fn check(&mut self) -> Result<(), EngineError> {
        match self {
            Self::Local(_) => Ok(()),
            Self::Http(source) => source.check(),
        }
    }
}
pub(super) struct NetworkPreparation {
    pub settings: Settings,
    pub source: HttpSource,
}
pub(super) fn preparation_failed(
    settings: &Settings,
    context: &NetworkSourceContext,
    error: EngineError,
    events: &SyncSender<Event>,
) -> Result<(), EngineError> {
    emit(
        events,
        Event::Network(NetworkEvent {
            binding: context.binding.clone(),
            source_epochs: context.source_epochs,
            output_epochs: settings.snapshot.epochs,
            graph_epoch: settings.snapshot.graph_epoch,
            device_epoch: settings.snapshot.device_epoch,
            monotonic_ns: 0,
            kind: NetworkEventKind::Failed { code: error.code },
        }),
    )
}
pub(super) fn retire_owned(
    playback: &mut Option<Playback>,
    preparation: &mut Option<NetworkPreparation>,
    closing: &NetworkClosingSource,
) -> Result<NetworkRetirementAck, EngineError> {
    let ack = if let Some(current) = playback.as_mut().filter(|p| p.is_network()) {
        current.retire_network(closing)?
    } else {
        if let Some(preparing) = preparation {
            if preparing.source.context.binding != closing.context.binding {
                return Err(FAILED);
            }
            preparing.source.retire()?;
        }
        NetworkRetirementAck {
            binding: closing.context.binding.clone(),
            source_epochs: closing.context.source_epochs,
            output_epochs: closing.output_epochs,
            graph_epoch: closing.graph_epoch,
            device_epoch: closing.device_epoch,
            observation: None,
        }
    };
    if playback.as_ref().is_some_and(|p| p.is_network()) {
        *playback = None;
    }
    *preparation = None;
    Ok(ack)
}
impl Playback {
    pub(super) fn is_network(&self) -> bool {
        matches!(self.current.file, Reader::Http(_))
    }
    pub(super) fn activate_network_graph(
        &mut self,
        settings: Settings,
        activation: GraphActivationRequest,
        mut pipeline: PreparedPipeline,
        graph_epoch: u64,
        events: &SyncSender<Event>,
    ) -> Result<(), EngineError> {
        self.current.file.check()?;
        if pipeline.stream_render().is_none() {
            return Err(EngineError::new(
                ErrorCode::FidelityPolicyConflict,
                "network graph requires streaming float processing",
            ));
        }
        let previous_output_epochs = self.native_epochs;
        let previous_output_format = self.native_period().header().format;
        self.reposition(settings, None, events, false)?;
        let replacement_boundary = self
            .pipeline
            .as_ref()
            .and_then(|pipeline| pipeline.stream_render())
            .map(|stream| {
                let output = stream.output_positions();
                let discarded = output
                    .generated
                    .checked_sub(output.backend_origin)
                    .ok_or(FAILED)?;
                Ok::<_, EngineError>((discarded != 0).then(|| {
                    Arc::new(NetworkReplacementBoundary {
                        binding: self.network_context().binding.clone(),
                        source_epochs: self.network_context().source_epochs,
                        source_position_frames: self.source_position,
                        previous_output_epochs,
                        output_epochs: self.native_epochs,
                        previous_output_format,
                        discarded_output_frames: discarded,
                    })
                }))
            })
            .transpose()?
            .flatten();
        pipeline.reset(activation.epochs).map_err(render_error)?;
        let source_epochs = self.network_context().source_epochs;
        pipeline
            .stream_render_mut()
            .ok_or(FAILED)?
            .bind_source_epochs(source_epochs)?;
        self.current.file.check()?;
        self.pipeline = Some(pipeline.activate());
        self.installed_graph_epoch = graph_epoch;
        if let Some(boundary) = replacement_boundary {
            self.network_replacement_boundary = Some(boundary);
        }
        self.stream_position = 0;
        self.pending = false;
        self.started = false;
        self.saturation_count = 0;
        self.graph_peak = None;
        self.prequantization_peak = None;
        self.input_peak = None;
        self.output_peak = None;
        self.set_gain()?;
        let observation = self.observe_network(NetworkLifecycle::Ready)?;
        emit(
            events,
            Event::NetworkGraphActivated(NetworkGraphActivationAck {
                request_id: activation.request_id,
                epochs: activation.epochs,
                graph_hash: activation.prepared.preview.graph_hash.clone(),
                activation_frame: self.stream_position,
                observation,
            }),
        )
    }
    fn network_context(&self) -> &NetworkSourceContext {
        match &self.current.file {
            Reader::Http(source) => &source.context,
            Reader::Local(_) => unreachable!("network source context"),
        }
    }
    pub(super) fn emit_network(
        &self,
        events: &SyncSender<Event>,
        kind: NetworkEventKind,
    ) -> Result<(), EngineError> {
        let context = self.network_context();
        emit(
            events,
            Event::Network(NetworkEvent {
                binding: context.binding.clone(),
                source_epochs: context.source_epochs,
                output_epochs: if matches!(kind, NetworkEventKind::Failed { .. }) {
                    self.native_epochs
                } else {
                    self.epochs()
                },
                graph_epoch: self.installed_graph_epoch,
                device_epoch: self.settings.snapshot.device_epoch,
                monotonic_ns: 0,
                kind,
            }),
        )
    }
    pub(super) fn observe_network(
        &mut self,
        lifecycle: NetworkLifecycle,
    ) -> Result<NetworkPlaybackObservation, EngineError> {
        let progress = self.output.progress()?;
        if let Some(stream) = self.pipeline.as_mut().and_then(|p| p.stream_render_mut()) {
            stream.observe(progress.submitted, restart_cursor(progress)?)?;
        }
        self.last_counters = progress;
        self.last_counter_epochs = self.native_epochs;
        Ok(self.network_observation(progress, lifecycle))
    }
    pub(super) fn network_observation(
        &self,
        counters: Counters,
        lifecycle: NetworkLifecycle,
    ) -> NetworkPlaybackObservation {
        let context = self.network_context();
        NetworkPlaybackObservation {
            binding: context.binding.clone(),
            source_epochs: context.source_epochs,
            output_epochs: self.native_epochs,
            graph_epoch: self.installed_graph_epoch,
            device_epoch: self.settings.snapshot.device_epoch,
            lifecycle,
            format: self.format(),
            output_format: self.native_period().header().format,
            total_frames: self.current.file.total_frames(),
            source_position_frames: self.source_position,
            audible_source_position_frames: None,
            submitted_frames: counters.submitted,
            consumed_frames: counters.consumed,
            backend_delay_frames: counters.delay,
            consumption_basis: counters.basis,
            input_peak: self.input_peak,
            output_peak: self.output_peak,
            modified: self.modified || self.backend.preservation == PreservationState::Modified,
            backend: Some(self.backend.clone()),
            output_activity: self.output.activity(),
            processing: self.processing_observation(),
            replacement_boundary: self.network_replacement_boundary.clone(),
        }
    }
    pub(super) fn network_paused(&mut self, events: &SyncSender<Event>) -> Result<(), EngineError> {
        let observation = self.observe_network(NetworkLifecycle::Paused)?;
        self.emit_network(events, NetworkEventKind::Paused { observation })
    }
    pub(super) fn network_progress(
        &mut self,
        events: &SyncSender<Event>,
    ) -> Result<(), EngineError> {
        let lifecycle = if self.started {
            NetworkLifecycle::Playing
        } else {
            NetworkLifecycle::Ready
        };
        let observation = self.observe_network(lifecycle)?;
        self.emit_network(events, NetworkEventKind::Progress { observation })
    }
    pub(super) fn retire_network(
        &mut self,
        closing: &NetworkClosingSource,
    ) -> Result<NetworkRetirementAck, EngineError> {
        if self.network_context().binding != closing.context.binding {
            return Err(FAILED);
        }
        let actual = self.execution_fence();
        let coherent = self.last_counter_epochs == self.native_epochs
            && self.epochs() == self.native_epochs
            && self
                .pipeline
                .as_ref()
                .and_then(|p| p.stream_render())
                .is_none_or(|stream| stream.control_epochs() == self.native_epochs);
        let mut observation = coherent.then(|| {
            self.network_observation(self.network_terminal_counters(), NetworkLifecycle::Retiring)
        });
        // Stop output before waiting for decoder or transport retirement.
        self.output.stop()?;
        if let Reader::Http(source) = &mut self.current.file {
            source.retire()?;
        }
        if let Some(observation) = &mut observation {
            observation.output_activity = self.output.activity();
        }
        Ok(NetworkRetirementAck {
            binding: actual.binding,
            source_epochs: actual.source_epochs,
            output_epochs: actual.output_epochs,
            graph_epoch: actual.graph_epoch,
            device_epoch: actual.device_epoch,
            observation,
        })
    }
    pub(super) fn execution_fence(&self) -> NetworkExecutionFence {
        let context = self.network_context();
        NetworkExecutionFence {
            binding: context.binding.clone(),
            source_epochs: context.source_epochs,
            output_epochs: self.native_epochs,
            graph_epoch: self.installed_graph_epoch,
            device_epoch: self.settings.snapshot.device_epoch,
        }
    }
    pub(super) fn network_terminal_counters(&self) -> Counters {
        let mut counters = self.last_counters;
        if counters.submitted != self.stream_position {
            // Native acceptance survives cancellation even when the next progress read cannot run.
            counters.submitted = self.stream_position;
            counters.consumed = None;
            counters.restart_cursor = None;
            counters.delay = None;
        }
        counters
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use earph_core::{
        control::{Command, CommandEnvelope, ControlLimits, QueryResult, ReceiptStatus},
        ids::{OwnerId, RequestId, SessionId, ZoneId},
        session::Session,
    };

    #[test]
    fn cancelled_unstarted_source_releases_context_without_a_retirement_request() {
        let mut session = Session::new_owner(
            ZoneId::new("main").unwrap(),
            OwnerId::new("idle-source-owner").unwrap(),
            ControlLimits::default(),
        );
        let command = |session: &Session, id: &str, payload: Command| CommandEnvelope {
            schema_version: 1,
            request_id: RequestId::new(id).unwrap(),
            zone_id: ZoneId::new("main").unwrap(),
            owner_id: session.snapshot().owner_id,
            session_id: None,
            expected_revision: Some(session.snapshot().revision),
            payload,
        };
        let staged = command(
            &session,
            "stage-idle-source",
            Command::SourceStageUri {
                uri: "http://unused.test/never-opened.flac".into(),
                metadata: String::new(),
            },
        );
        assert_eq!(session.submit(staged).status, ReceiptStatus::Applied);
        let query = command(&session, "get-idle-source", Command::SourcesGet {});
        let Some(QueryResult::Sources(staging)) = session.submit(query).result else {
            panic!("missing real staged descriptor")
        };
        let descriptor = session
            .staged_uri_descriptor(&staging.staged.unwrap().media_id, staging.generation)
            .unwrap();
        let source_epochs = Epochs::default().next_session().unwrap();
        let binding_record = NetworkSourceBinding {
            owner_id: session.snapshot().owner_id.clone().unwrap(),
            session_id: SessionId::new("idle-source-session").unwrap(),
            media_id: descriptor.media_id().clone(),
            source_generation: 1,
            grant_generation: 1,
        };
        let grants = Arc::new(NetworkGrantDocument {
            origins: vec![NetworkOriginGrant {
                origin: "http://unused.test".into(),
                pins: vec!["192.168.1.2".parse().unwrap()],
                roots_der: vec![],
            }],
        });
        let context = Arc::new(NetworkSourceContext {
            binding: binding_record,
            source_epochs,
            descriptor,
            grants: grants.clone(),
            expires_monotonic_ns: 60_000_000_000,
        });
        let bound = binding(&context.binding).unwrap();
        let origin = earph_network::OriginGrant::from_document(
            grants,
            0,
            context.binding.owner_id.clone(),
            1,
            Instant::now() + Duration::from_secs(60),
        )
        .unwrap();
        let permit = earph_network::FetchPermit::new(
            earph_network::CheckedUrl::new(context.descriptor.uri()).unwrap(),
            bound.clone(),
            &bound,
            vec![origin],
            Instant::now(),
        )
        .unwrap();
        let weak = Arc::downgrade(&context);
        let mut snapshot = session.snapshot();
        snapshot.session_id = Some(context.binding.session_id.clone());
        snapshot.source = Some(SourceKind::HttpFlac);
        snapshot.epochs = source_epochs;
        let mut worker = Worker::spawn();
        worker
            .send(Action::StartNetwork {
                settings: Settings {
                    snapshot,
                    playing: true,
                },
                context,
                permit,
                live: Arc::new(Mutex::new(None)),
                clock: Instant::now(),
                cancel: Arc::new(AtomicBool::new(true)),
            })
            .unwrap();
        let event = worker.events.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(matches!(
            event,
            Event::Network(NetworkEvent {
                kind: NetworkEventKind::Failed {
                    code: ErrorCode::OperationCancelled
                },
                ..
            })
        ));
        let deadline = Instant::now() + Duration::from_secs(1);
        while weak.upgrade().is_some() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        assert!(
            weak.upgrade().is_none(),
            "an Idle failed start must not retain unreachable authority while waiting for a nonexistent retirement"
        );
    }
}
