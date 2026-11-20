// ABOUTME: Projects authoritative Session facts through the shared diagnostic allowlist.
// ABOUTME: Excludes authored identifiers and keeps missing sample provenance and metrics explicit.
use super::*;
use crate::{diagnostics::*, graph::Transformation};
#[path = "session_diagnostic_artifacts.rs"]
mod artifacts;

impl Session {
    pub(super) fn diagnostic_query(
        &self,
        command: &Command,
    ) -> Result<Option<QueryResult>, EngineError> {
        if !matches!(command, Command::SignalPathGet {} | Command::MetricsGet {}) {
            return Ok(None);
        }
        let binding = self.diagnostic_binding();
        if matches!(command, Command::SignalPathGet {}) {
            let resolved = self.graph.active.as_ref().map(|preview| ResolvedPath {
                preparation_epochs: preview.binding.epochs,
                preparation_revision: preview.binding.expected_revision,
                compiled_nodes: preview
                    .diagnostic_nodes
                    .as_ref()
                    .map(|nodes| nodes.to_vec()),
                nodes_unavailable_reason: preview
                    .diagnostic_nodes
                    .is_none()
                    .then_some(UnavailableReason::NotObserved),
                source_format: preview.source_format,
                processing_input_format: preview.processing_input_format,
                processing_output_format: preview.processing_output_format,
                native_output_format: preview.native_output_format,
                preservation: preview.preservation,
                transformations: preview.transformations.iter().map(transformation).collect(),
                required_headroom_db: preview.required_headroom_db,
                timing: match &preview.timing {
                    GraphTiming::SameRate {
                        latency_frames,
                        tail_frames,
                        ..
                    } => DiagnosticTiming::SameRate {
                        sample_rate_hz: preview.processing_input_format.sample_rate_hz(),
                        latency_frames: *latency_frames,
                        tail_frames: *tail_frames,
                    },
                    GraphTiming::Streaming { stages } => DiagnosticTiming::Streaming {
                        stages: stages
                            .iter()
                            .enumerate()
                            .map(|(index, stage)| DiagnosticStage::from_compiled(index, stage))
                            .collect(),
                    },
                },
            });
            let report = SignalPathReport {
                schema_version: 1,
                cut: binding.cut(),
                requested: RequestedPath {
                    source: self.state.source,
                    output: self.state.output.as_ref().map(|output| RequestedOutput {
                        backend: output.backend,
                        route: output.route,
                        volume_mode: output.volume_mode,
                        fidelity: output.fidelity,
                        period_frames: output.period_frames,
                        buffer_frames: output.buffer_frames,
                    }),
                    graph_configured: self.graph.configured.is_some(),
                    software_gain_db: self.state.software_gain_db,
                    gain_ramp_frames: self.state.gain_ramp_frames,
                    muted: self.state.muted,
                    software_cap_db: self.state.volume_policy.software_cap_db(),
                    hardware_cap_db: self.state.volume_policy.hardware_cap_db(),
                },
                resolved,
                observed: self.measured_playback_facts().map(|playback| ObservedPath {
                    // Session retains the last observation without its own epoch/time envelope.
                    sample_epochs: None,
                    sample_monotonic_ns: None,
                    playback_format: playback.format,
                    modified: playback.modified,
                    network_replacement_boundary: self
                        .state
                        .network_playback
                        .as_ref()
                        .and_then(|p| p.replacement_boundary.as_deref())
                        .map(DiagnosticNetworkReplacementBoundary::from),
                    backend: playback.backend.map(|backend| ObservedBackend {
                        backend: backend.backend,
                        measurement_boundary: match backend.measurement_boundary {
                            "alsa_client_negotiation" => MeasurementBoundary::AlsaClientNegotiation,
                            "core_audio_observed_stream_formats" => {
                                MeasurementBoundary::CoreAudioObservedStreamFormats
                            }
                            _ => MeasurementBoundary::Unavailable,
                        },
                        client_format: format(&backend.client_format),
                        device_format: backend.device_format.as_ref().map(format),
                        preservation: match backend.preservation {
                            PreservationState::Unverified => BackendPreservation::Unverified,
                            PreservationState::Modified => BackendPreservation::Modified,
                        },
                    }),
                }),
                unknown_segments: vec![
                    UnknownSegment::UpstreamProcessing,
                    UnknownSegment::OutputRouteInternals,
                    UnknownSegment::DacInternals,
                ],
            };
            let read = DiagnosticRead { binding, report };
            read.validate()?;
            Ok(Some(QueryResult::SignalPath(Arc::new(read))))
        } else {
            let playback = self.measured_playback_facts();
            let processing = playback.as_ref().and_then(|p| p.processing);
            let report = MetricsReport {
                schema_version: 1,
                cut: binding.cut(),
                underruns: Metric::unavailable(UnavailableReason::Unsupported),
                deadline_misses: Metric::unavailable(UnavailableReason::Unsupported),
                cpu_load: Metric::unavailable(UnavailableReason::Unsupported),
                queue_depth_frames: Metric::unavailable(UnavailableReason::Unsupported),
                input_peak: Metric::observed(
                    playback.as_ref().and_then(|p| p.input_peak),
                    MetricScope::LastPlaybackObservation,
                ),
                output_peak: Metric::observed(
                    playback.as_ref().and_then(|p| p.output_peak),
                    MetricScope::LastPlaybackObservation,
                ),
                saturation_count: Metric::observed(
                    processing.map(|p| p.saturation_count),
                    MetricScope::GraphProcessingSinceReset,
                ),
                backend_delay_frames: Metric::observed(
                    playback.as_ref().and_then(|p| p.backend_delay_frames),
                    MetricScope::LastPlaybackObservation,
                ),
            };
            let read = DiagnosticRead { binding, report };
            read.validate()?;
            Ok(Some(QueryResult::Metrics(Arc::new(read))))
        }
    }
}
fn format(value: &ObservedStreamFormat) -> DiagnosticFormat {
    DiagnosticFormat {
        sample_rate_hz: value.sample_rate_hz,
        channels: value.channels,
        representation: match value.representation {
            ObservedSampleRepresentation::Signed16 => SampleRepresentation::Signed16,
            ObservedSampleRepresentation::Signed24 => SampleRepresentation::Signed24,
            ObservedSampleRepresentation::Signed32 => SampleRepresentation::Signed32,
            ObservedSampleRepresentation::Float32 => SampleRepresentation::Float32,
            ObservedSampleRepresentation::Float64 => SampleRepresentation::Float64,
            ObservedSampleRepresentation::Unknown => SampleRepresentation::Unknown,
        },
        bytes_per_frame: value.bytes_per_frame,
        interleaved: value.interleaved,
    }
}
fn transformation(value: &SignalTransformation) -> TransformationKind {
    match value {
        SignalTransformation::IntegerToFloat64 => TransformationKind::IntegerToFloat64,
        SignalTransformation::SoftwareVolume => TransformationKind::SoftwareVolume,
        SignalTransformation::Quantization { .. } => TransformationKind::Quantization,
        SignalTransformation::BackendAdapterConversion { .. } => {
            TransformationKind::BackendAdapterConversion
        }
        SignalTransformation::Kernel { transformation } => match transformation {
            Transformation::Gain { .. } => TransformationKind::Gain,
            Transformation::Peq { .. } => TransformationKind::Peq,
            Transformation::Convolution { .. } => TransformationKind::Convolution,
            Transformation::Src { .. } => TransformationKind::Src,
            Transformation::BoundedIirTruncation { .. } => TransformationKind::BoundedIirTruncation,
            Transformation::Delay { .. } => TransformationKind::Delay,
            Transformation::Matrix { .. } => TransformationKind::Matrix,
            Transformation::Sum { .. } => TransformationKind::Sum,
            Transformation::Compensation { .. } => TransformationKind::Compensation,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn retained_observation() -> Session {
        let mut session = Session::new_owner(
            ZoneId::new("private-zone").unwrap(),
            OwnerId::new("private-owner").unwrap(),
            ControlLimits::default(),
        );
        session.state.monotonic_ns = 120;
        session.state.epochs = Epochs {
            session: 7,
            stream: 4,
        };
        let format = PcmFormat::new(
            48000,
            crate::audio::ChannelLayout::Stereo,
            PcmRepresentation::Signed16,
        )
        .unwrap();
        // Domain input only. This does not qualify a native device or a physical observation.
        session.state.playback = Some(PlaybackObservation {
            processing: None,
            format,
            submitted_frames: 64,
            consumed_frames: Some(64),
            backend_delay_frames: Some(0),
            consumption_basis: "private diagnostic string",
            source_entry_id: QueueEntryId::new("/private/media-path").unwrap(),
            source_position_frames: 64,
            audible_entry_id: None,
            audible_source_position_frames: None,
            input_peak: Some(0.0),
            output_peak: Some(0.0),
            modified: false,
            output_activity: None,
            backend: Some(BackendObservation {
                backend: OutputBackend::Alsa,
                device_identity_sha256: None,
                audio_storage_bytes: 256,
                client_format: format.into(),
                device_format: None,
                measurement_boundary: "private unrecognized boundary",
                preservation: PreservationState::Unverified,
            }),
        });
        session
    }
    fn path(session: &Session) -> Arc<SignalPathRead> {
        let Some(QueryResult::SignalPath(read)) = session
            .diagnostic_query(&Command::SignalPathGet {})
            .unwrap()
        else {
            panic!("path")
        };
        read
    }
    #[test]
    fn retained_observation_keeps_actual_zero_and_unknown_sample_origin_separate() {
        let session = retained_observation();
        let read = path(&session);
        assert_eq!(
            read.binding.epochs,
            Epochs {
                session: 7,
                stream: 4
            }
        );
        let observed = read.report.observed.as_ref().unwrap();
        assert!(observed.sample_epochs.is_none() && observed.sample_monotonic_ns.is_none());
        assert!(observed.backend.as_ref().unwrap().device_format.is_none());
        assert_eq!(
            observed.backend.as_ref().unwrap().measurement_boundary,
            MeasurementBoundary::Unavailable
        );
        assert!(
            !serde_json::to_string(&read.report)
                .unwrap()
                .contains("private")
        );
        let Some(QueryResult::Metrics(metrics)) =
            session.diagnostic_query(&Command::MetricsGet {}).unwrap()
        else {
            panic!("metrics")
        };
        assert_eq!(metrics.report.input_peak.value, Some(0.0));
        assert!(metrics.report.input_peak.unavailable_reason.is_none());
        assert_eq!(metrics.report.backend_delay_frames.value, Some(0));
        assert!(metrics.report.saturation_count.value.is_none());
        assert!(metrics.report.underruns.value.is_none());
    }
    #[test]
    fn decoder_does_not_promote_current_cut_into_unrecorded_sample_provenance() {
        let mut read = (*path(&retained_observation())).clone();
        read.report.observed.as_mut().unwrap().sample_epochs = Some(read.binding.epochs);
        read.report.observed.as_mut().unwrap().sample_monotonic_ns =
            Some(read.binding.monotonic_ns);
        assert!(read.validate().is_err());
    }
    #[test]
    fn malformed_backend_observation_is_rejected_before_a_shareable_result() {
        let mut session = retained_observation();
        session
            .state
            .playback
            .as_mut()
            .unwrap()
            .backend
            .as_mut()
            .unwrap()
            .client_format
            .sample_rate_hz = f64::NAN;
        assert!(
            session
                .diagnostic_query(&Command::SignalPathGet {})
                .is_err()
        );
    }
    #[test]
    fn backend_frame_geometry_cannot_understate_its_declared_format() {
        let mut session = retained_observation();
        session
            .state
            .playback
            .as_mut()
            .unwrap()
            .backend
            .as_mut()
            .unwrap()
            .client_format
            .bytes_per_frame = 1;
        assert!(
            session
                .diagnostic_query(&Command::SignalPathGet {})
                .is_err()
        );
    }
}

#[derive(Clone)]
pub(super) struct DiagnosticCandidate {
    request_id: RequestId,
    read: Arc<PreviewRead>,
}
#[derive(Clone)]
pub(super) struct DiagnosticState {
    build: Arc<crate::provenance::RunningBuildProvenance>,
    initialized: bool,
    observed: bool,
    candidate: Option<DiagnosticCandidate>,
    work: Option<Arc<DiagnosticWorkRequest>>,
    cancel_requested: bool,
    cancel_recorded: bool,
    pending_record: Option<DiagnosticRecord>,
    terminal_record: Option<DiagnosticRecord>,
    cancel_reply: Option<(RequestId, Arc<DiagnosticCancelReceipt>)>,
}
impl Default for DiagnosticState {
    fn default() -> Self {
        Self {
            build: Arc::new(crate::provenance::RunningBuildProvenance::unavailable(
                crate::provenance::BuildUnavailableReason::NotProvided,
            )),
            initialized: false,
            observed: false,
            candidate: None,
            work: None,
            cancel_requested: false,
            cancel_recorded: false,
            pending_record: None,
            terminal_record: None,
            cancel_reply: None,
        }
    }
}
impl Session {
    /// Installs this binary's immutable context before diagnostic receipts can expose it.
    pub fn with_diagnostic_build(
        mut self,
        build: Arc<crate::provenance::RunningBuildProvenance>,
    ) -> Result<Self, EngineError> {
        build.validate_for_engine()?;
        if self.diagnostics.initialized
            || self.diagnostics.observed
            || self.diagnostics.candidate.is_some()
        {
            return Err(EngineError::new(
                ErrorCode::AssetInvalid,
                "diagnostic build context is already bound",
            ));
        }
        self.diagnostics.build = build;
        self.diagnostics.initialized = true;
        Ok(self)
    }
    pub(super) fn retain_diagnostic_observation(&mut self, command: &Command) {
        self.diagnostics.observed |= command.is_diagnostic() || command.is_diagnostic_artifact();
    }
    fn diagnostic_binding(&self) -> DiagnosticBinding {
        DiagnosticBinding {
            zone_id: self.state.zone_id.clone(),
            owner_id: self.state.owner_id.clone(),
            session_id: self.state.session_id.clone(),
            revision: self.state.revision,
            epochs: self.state.epochs,
            graph_epoch: self.state.graph_epoch,
            device_epoch: self.state.device_epoch,
            event_sequence: self.state.event_sequence,
            monotonic_ns: self.state.monotonic_ns,
        }
    }
    fn diagnostic_history(
        &self,
        cursor: Option<&EventCursor>,
        limit: usize,
    ) -> Result<EventsReport, EngineError> {
        if cursor.is_some_and(|c| Some(&c.owner_id) != self.state.owner_id.as_ref()) {
            return Err(STALE);
        }
        self.project_diagnostic_history(cursor.map(|c| c.after_sequence), limit)
    }
    fn project_diagnostic_history(
        &self,
        after: Option<u64>,
        limit: usize,
    ) -> Result<EventsReport, EngineError> {
        let cut = self.diagnostic_binding().cut();
        let future = after.is_some_and(|n| n > cut.event_sequence);
        let gap = after.is_some_and(|n| self.event_history_gap(n));
        let mut retained = self
            .events
            .iter()
            .filter(|e| !future && after.is_some_and(|n| e.sequence > n));
        let events = retained
            .by_ref()
            .take(limit)
            .map(|e| DiagnosticEvent {
                sequence: e.sequence,
                revision: e.revision,
                epochs: e.epochs,
                monotonic_ns: e.monotonic_ns,
                transport: e.transport,
                muted: e.muted,
                error: e.error,
            })
            .collect::<Vec<_>>();
        let next_sequence = events.last().map_or(cut.event_sequence, |e| e.sequence);
        let has_more = retained.next().is_some();
        let report = EventsReport {
            schema_version: 1,
            cut,
            scope: EventHistoryScope::RetainedStateEvents,
            complete_audit_log: false,
            gap,
            events,
            snapshot: DiagnosticSnapshot {
                cut,
                transport: self.state.transport,
                muted: self.state.muted,
                software_gain_db: self.state.software_gain_db,
                error: self.state.error,
            },
            next_sequence,
            has_more,
        };
        report.validate_history(limit)?;
        Ok(report)
    }
    pub(super) fn diagnostic_events(
        &self,
        cursor: Option<&EventCursor>,
    ) -> Result<EventsRead, EngineError> {
        let read = DiagnosticRead {
            binding: self.diagnostic_binding(),
            report: self.diagnostic_history(cursor, MAX_EVENTS_PAGE)?,
        };
        read.validate()?;
        Ok(read)
    }
    pub(super) fn prepare_diagnostic_preview(
        &mut self,
        id: &RequestId,
        now_ns: u64,
    ) -> Result<(), EngineError> {
        if self
            .diagnostics
            .work
            .as_ref()
            .is_some_and(|w| w.operation == DiagnosticArtifactOperation::Export)
        {
            return Err(EngineError::new(
                ErrorCode::SourceBusy,
                "diagnostic preview is pinned by an export",
            ));
        }
        let expires_monotonic_ns = now_ns
            .checked_add(DIAGNOSTIC_CANDIDATE_LIFETIME_NS)
            .ok_or(DIAGNOSTIC_BUDGET)?;
        let Some(QueryResult::SignalPath(path)) =
            self.diagnostic_query(&Command::SignalPathGet {})?
        else {
            return Err(STATE);
        };
        let Some(QueryResult::Metrics(metrics)) = self.diagnostic_query(&Command::MetricsGet {})?
        else {
            return Err(STATE);
        };
        let history = self.project_diagnostic_history(Some(0), MAX_DIAGNOSTIC_HISTORY)?;
        let binding = self.diagnostic_binding();
        let bundle = DiagnosticBundle {
            schema_version: 1,
            policy_version: 1,
            cut: binding.cut(),
            signal_path: path.report.clone(),
            metrics: metrics.report.clone(),
            history,
            running_build: (*self.diagnostics.build).clone(),
            test_inventory: TestInventory::NotAttached,
            reproduction: ReproductionCoverage::Incomplete,
            missing_evidence: MISSING_DIAGNOSTIC_EVIDENCE,
            omitted_categories: DIAGNOSTIC_OMISSIONS,
        };
        bundle.validate()?;
        let bytes = serde_json::to_string(&bundle).map_err(|_| STATE)?;
        if bytes.len() > MAX_PREVIEW_BYTES {
            return Err(DIAGNOSTIC_BUDGET);
        }
        let sha256 = crate::diagnostics::preview_digest(bytes.as_bytes())?;
        let read = Arc::new(DiagnosticRead {
            binding,
            report: PreviewReport {
                schema_version: 1,
                policy_version: 1,
                cut: bundle.cut,
                digest_algorithm: DiagnosticDigestAlgorithm::Sha256,
                sha256,
                byte_len: bytes.len(),
                bytes: Arc::from(bytes.as_str()),
                created_monotonic_ns: now_ns,
                expires_monotonic_ns,
            },
        });
        read.validate()?;
        self.diagnostics.candidate = Some(DiagnosticCandidate {
            request_id: id.clone(),
            read,
        });
        Ok(())
    }
    pub(super) fn diagnostic_preview_reply(&self, id: &RequestId) -> Option<QueryResult> {
        self.diagnostics
            .candidate
            .as_ref()
            .filter(|c| &c.request_id == id)
            .map(|c| QueryResult::DiagnosticPreview(c.read.clone()))
    }
    pub(super) fn expire_diagnostic_candidate(&mut self, now_ns: u64) {
        if self
            .diagnostics
            .work
            .as_ref()
            .is_some_and(|w| w.operation == DiagnosticArtifactOperation::Export)
        {
            return;
        }
        if self
            .diagnostics
            .candidate
            .as_ref()
            .is_some_and(|c| now_ns >= c.read.report.expires_monotonic_ns)
        {
            self.diagnostics.candidate = None;
        }
    }
    fn diagnostic_storage_bytes(&self) -> Result<usize, EngineError> {
        let mut bytes = self.diagnostics.build.retained_capacity_bytes();
        if let Some(work) = &self.diagnostics.work {
            // Reserve the first scoped cancel's cache and receipt before reads can saturate history.
            let metadata = if self.diagnostics.cancel_recorded {
                3
            } else {
                8
            } * MAX_PREVIEW_METADATA_BYTES;
            bytes = bytes
                .checked_add(work.preview.as_ref().map_or(0, |p| p.report.bytes.len()) + metadata)
                .ok_or(DIAGNOSTIC_BUDGET)?;
        }
        // The active job reservation already covers its pending record and completion.
        for record in [&self.diagnostics.terminal_record].into_iter().flatten() {
            bytes = bytes
                .checked_add(diagnostic_receipt_bytes(&record.1)?)
                .ok_or(DIAGNOSTIC_BUDGET)?;
        }
        if self.diagnostics.cancel_reply.is_some() {
            bytes = bytes
                .checked_add(MAX_PREVIEW_METADATA_BYTES)
                .ok_or(DIAGNOSTIC_BUDGET)?;
        }
        if let Some(candidate) = &self.diagnostics.candidate {
            bytes = bytes
                .checked_add(candidate.read.report.bytes.len() + MAX_PREVIEW_METADATA_BYTES)
                .ok_or(DIAGNOSTIC_BUDGET)?;
        }
        for record in &self.receipts {
            if record.0.payload.is_diagnostic() || record.0.payload.is_diagnostic_artifact() {
                bytes = bytes
                    .checked_add(diagnostic_receipt_bytes(&record.1)?)
                    .ok_or(DIAGNOSTIC_BUDGET)?;
            }
        }
        Ok(bytes)
    }
    pub(super) fn validate_diagnostic_admission(
        &self,
        staged: &Session,
        command: &CommandEnvelope,
        receipt: &Receipt,
    ) -> Result<(), EngineError> {
        if !command.payload.is_diagnostic() && !command.payload.is_diagnostic_artifact() {
            return Ok(());
        }
        if serde_json::to_vec(receipt).map_err(|_| STATE)?.len() > self.limits.message_bytes {
            return Err(DIAGNOSTIC_BUDGET);
        }
        let original_bytes = self.diagnostic_storage_bytes()?;
        let staged_bytes = staged.diagnostic_storage_bytes()?;
        // Cover both pre-install copies and the installed receipt admission before publication.
        let copies = original_bytes
            .checked_add(staged_bytes)
            .ok_or(DIAGNOSTIC_BUDGET)?
            .max(staged_bytes.checked_mul(2).ok_or(DIAGNOSTIC_BUDGET)?);
        let bytes = copies
            .checked_add(
                diagnostic_receipt_bytes(receipt)?
                    .checked_mul(2)
                    .ok_or(DIAGNOSTIC_BUDGET)?,
            )
            .ok_or(DIAGNOSTIC_BUDGET)?;
        if bytes > MAX_DIAGNOSTIC_RETAINED_BYTES {
            return Err(DIAGNOSTIC_BUDGET);
        }
        Ok(())
    }
}
fn diagnostic_receipt_bytes(receipt: &Receipt) -> Result<usize, EngineError> {
    // Charge whole validated result bounds, plus fixed metadata and admitted identity capacities.
    // This deliberately overcharges shared Arcs and staged copies before admission.
    let payload = match &receipt.result {
        Some(QueryResult::SignalPath(_) | QueryResult::Metrics(_)) => MAX_DIAGNOSTIC_READ_BYTES,
        Some(QueryResult::Events(_)) => MAX_EVENTS_READ_BYTES,
        Some(QueryResult::DiagnosticPreview(read)) => {
            read.report.bytes.len() + MAX_PREVIEW_METADATA_BYTES
        }
        Some(QueryResult::DiagnosticArtifact(_) | QueryResult::DiagnosticCancel(_)) => {
            MAX_PREVIEW_METADATA_BYTES
        }
        _ => 0,
    };
    payload
        .checked_add(MAX_PREVIEW_METADATA_BYTES)
        .ok_or(DIAGNOSTIC_BUDGET)
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    fn owner() -> Session {
        Session::new_owner(
            ZoneId::new("test-zone").unwrap(),
            OwnerId::new("test-owner").unwrap(),
            ControlLimits::default(),
        )
    }
    fn query(id: usize, payload: Command) -> CommandEnvelope {
        CommandEnvelope {
            schema_version: 1,
            request_id: RequestId::new(format!("query-{id}")).unwrap(),
            zone_id: ZoneId::new("test-zone").unwrap(),
            owner_id: Some(OwnerId::new("test-owner").unwrap()),
            session_id: None,
            expected_revision: None,
            payload,
        }
    }
    #[test]
    fn expired_candidate_is_removed_before_historical_receipt_replay() {
        let mut s = owner();
        let command = query(0, Command::DiagnosticsPreview {});
        let receipt =
            s.submit_with_graph_effects(command.clone(), 9, |_| unreachable!(), |_| Ok(()));
        assert_eq!(receipt.status, ReceiptStatus::Applied);
        assert!(s.diagnostics.candidate.is_some());
        assert_eq!(
            s.submit_with_graph_effects(
                command,
                9 + DIAGNOSTIC_CANDIDATE_LIFETIME_NS,
                |_| unreachable!(),
                |_| Ok(())
            ),
            receipt
        );
        assert!(s.diagnostics.candidate.is_none());
    }
    #[test]
    fn aggregate_budget_refusal_keeps_the_prior_candidate_and_receipt() {
        let mut s = owner();
        let first = query(0, Command::DiagnosticsPreview {});
        let receipt = s.submit(first.clone());
        let prior = s.diagnostics.candidate.as_ref().unwrap().read.clone();
        let mut refused = false;
        for id in 1..1024 {
            let result = s.submit(query(id, Command::MetricsGet {}));
            if result.status == ReceiptStatus::Rejected {
                assert_eq!(
                    result.error.unwrap().code,
                    ErrorCode::ResourceBudgetExceeded
                );
                refused = true;
                break;
            }
        }
        assert!(refused);
        let rejected = s.submit(query(2000, Command::DiagnosticsPreview {}));
        assert_eq!(rejected.status, ReceiptStatus::Rejected);
        assert!(Arc::ptr_eq(
            &prior,
            &s.diagnostics.candidate.as_ref().unwrap().read
        ));
        assert_eq!(s.submit(first), receipt);
        assert!(s.diagnostic_storage_bytes().unwrap() <= MAX_DIAGNOSTIC_RETAINED_BYTES);
    }
    #[test]
    fn build_attachment_stays_closed_after_receipt_eviction() {
        let mut s = owner();
        assert_eq!(
            s.submit(query(0, Command::MetricsGet {})).status,
            ReceiptStatus::Applied
        );
        for id in 1..1030 {
            s.submit(query(id, Command::State));
        }
        assert!(s.receipts.iter().all(|r| !r.0.payload.is_diagnostic()));
        assert!(
            s.with_diagnostic_build(Arc::new(
                crate::provenance::RunningBuildProvenance::unavailable(
                    crate::provenance::BuildUnavailableReason::NotProvided
                )
            ))
            .is_err()
        );
    }
}

#[cfg(test)]
mod admission_tests {
    use super::*;
    #[test]
    fn failed_larger_candidate_admission_never_replaces_the_reviewed_candidate() {
        let mut s = Session::new_owner(
            ZoneId::new("z").unwrap(),
            OwnerId::new("o").unwrap(),
            ControlLimits::default(),
        );
        fn request(s: &Session, id: usize, payload: Command) -> CommandEnvelope {
            let state = s.snapshot();
            CommandEnvelope {
                schema_version: 1,
                request_id: RequestId::new(format!("admit-{id}")).unwrap(),
                zone_id: state.zone_id,
                owner_id: state.owner_id,
                session_id: None,
                expected_revision: Some(state.revision),
                payload,
            }
        }
        let c = request(&s, 0, Command::DiagnosticsPreview {});
        s.submit(c);
        for id in 1..=1024 {
            let c = request(&s, id, Command::Mute { muted: true });
            s.submit(c);
        }
        let reviewed = s.diagnostics.candidate.as_ref().unwrap().read.clone();
        let mut failures = 0;
        for id in 1025..1100 {
            for extra in 0..=1 {
                let mut attempt = s.clone();
                if extra == 1 {
                    let mut bad = request(&attempt, id + 3000, Command::MetricsGet {});
                    bad.owner_id = Some(OwnerId::new("wrong-owner").unwrap());
                    assert_eq!(attempt.submit(bad).status, ReceiptStatus::Rejected);
                }
                let c = request(&attempt, id + 1000, Command::DiagnosticsPreview {});
                let r = attempt.submit(c);
                if r.status == ReceiptStatus::Rejected {
                    failures += 1;
                    assert!(
                        Arc::ptr_eq(
                            &reviewed,
                            &attempt.diagnostics.candidate.as_ref().unwrap().read
                        ),
                        "failed preview changed the candidate at iteration {id}"
                    );
                }
            }
            let c = request(&s, id, Command::MetricsGet {});
            if s.submit(c).status == ReceiptStatus::Rejected {
                break;
            }
        }
        assert!(failures > 0);
    }
}
