// ABOUTME: Projects authoritative Session facts through the shared diagnostic allowlist.
// ABOUTME: Excludes authored identifiers and keeps missing sample provenance and metrics explicit.
use super::*;
use crate::{diagnostics::*, graph::Transformation};

impl Session {
    pub(super) fn diagnostic_query(
        &self,
        command: &Command,
    ) -> Result<Option<QueryResult>, EngineError> {
        if !matches!(command, Command::SignalPathGet {} | Command::MetricsGet {}) {
            return Ok(None);
        }
        let binding = DiagnosticBinding {
            zone_id: self.state.zone_id.clone(),
            owner_id: self.state.owner_id.clone(),
            session_id: self.state.session_id.clone(),
            revision: self.state.revision,
            epochs: self.state.epochs,
            graph_epoch: self.state.graph_epoch,
            device_epoch: self.state.device_epoch,
            event_sequence: self.state.event_sequence,
            monotonic_ns: self.state.monotonic_ns,
        };
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
                observed: self.state.playback.as_ref().map(|playback| ObservedPath {
                    // Session retains the last observation without its own epoch/time envelope.
                    sample_epochs: None,
                    sample_monotonic_ns: None,
                    playback_format: playback.format,
                    modified: playback.modified,
                    backend: playback.backend.as_ref().map(|backend| ObservedBackend {
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
            let playback = self.state.playback.as_ref();
            let processing = playback.and_then(|p| p.processing.as_ref());
            let report = MetricsReport {
                schema_version: 1,
                cut: binding.cut(),
                underruns: Metric::unavailable(UnavailableReason::Unsupported),
                deadline_misses: Metric::unavailable(UnavailableReason::Unsupported),
                cpu_load: Metric::unavailable(UnavailableReason::Unsupported),
                queue_depth_frames: Metric::unavailable(UnavailableReason::Unsupported),
                input_peak: Metric::observed(
                    playback.and_then(|p| p.input_peak),
                    MetricScope::LastPlaybackObservation,
                ),
                output_peak: Metric::observed(
                    playback.and_then(|p| p.output_peak),
                    MetricScope::LastPlaybackObservation,
                ),
                saturation_count: Metric::observed(
                    processing.map(|p| p.saturation_count),
                    MetricScope::GraphProcessingSinceReset,
                ),
                backend_delay_frames: Metric::observed(
                    playback.and_then(|p| p.backend_delay_frames),
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
