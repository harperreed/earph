// ABOUTME: Defines bounded default-redacted diagnostic reports and their shared strict validation.
// ABOUTME: Separates private local bindings from shareable facts and unavailable instrumentation.
use crate::{
    EngineError, ErrorCode,
    audio::PcmFormat,
    control::{OutputBackend, OutputRoute, SourceKind},
    graph::PreservationDecision,
    ids::{Epochs, OwnerId, SessionId, ZoneId},
    safety::{FidelityPolicy, VolumeMode, VolumePolicy},
    serde_helpers::nullable,
};
use serde::{Deserialize, Serialize};

#[path = "diagnostics_history.rs"]
mod history;
#[path = "diagnostics_preview.rs"]
mod preview;
pub use history::*;
pub use preview::*;
#[path = "diagnostics_artifacts.rs"]
mod artifacts;
pub use artifacts::*;

pub const MAX_DIAGNOSTIC_READ_BYTES: usize = 64 * 1024;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticCapabilities {
    pub schema_version: u32,
    pub max_read_bytes: usize,
    pub max_events_read_bytes: usize,
    pub max_preview_bytes: usize,
    pub max_preview_metadata_bytes: usize,
    pub max_history_events: usize,
    pub max_retained_bytes: usize,
    pub max_events_page: usize,
    pub candidate_lifetime_ns: u64,
    pub job_deadline_ns: u64,
    pub max_artifact_bytes: usize,
    pub methods: [DiagnosticMethod; 7],
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DiagnosticMethod {
    #[serde(rename = "signal_path.get")]
    SignalPath,
    #[serde(rename = "metrics.get")]
    Metrics,
    #[serde(rename = "events.get")]
    Events,
    #[serde(rename = "diagnostics.preview")]
    Preview,
    #[serde(rename = "diagnostics.export")]
    Export,
    #[serde(rename = "diagnostics.inspect")]
    Inspect,
    #[serde(rename = "diagnostics.cancel")]
    Cancel,
}
impl Default for DiagnosticCapabilities {
    fn default() -> Self {
        Self {
            schema_version: 1,
            max_read_bytes: MAX_DIAGNOSTIC_READ_BYTES,
            max_events_read_bytes: MAX_EVENTS_READ_BYTES,
            max_preview_bytes: MAX_PREVIEW_BYTES,
            max_preview_metadata_bytes: MAX_PREVIEW_METADATA_BYTES,
            max_history_events: MAX_DIAGNOSTIC_HISTORY,
            max_retained_bytes: MAX_DIAGNOSTIC_RETAINED_BYTES,
            max_events_page: MAX_EVENTS_PAGE,
            candidate_lifetime_ns: DIAGNOSTIC_CANDIDATE_LIFETIME_NS,
            job_deadline_ns: DIAGNOSTIC_JOB_DEADLINE_NS,
            max_artifact_bytes: MAX_DIAGNOSTIC_ARTIFACT_BYTES,
            methods: [
                DiagnosticMethod::SignalPath,
                DiagnosticMethod::Metrics,
                DiagnosticMethod::Events,
                DiagnosticMethod::Preview,
                DiagnosticMethod::Export,
                DiagnosticMethod::Inspect,
                DiagnosticMethod::Cancel,
            ],
        }
    }
}
const INVALID: EngineError = EngineError::new(ErrorCode::AssetInvalid, "invalid diagnostic report");
const BUDGET: EngineError = EngineError::new(
    ErrorCode::ResourceBudgetExceeded,
    "diagnostic read exceeds its bound",
);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticBinding {
    pub zone_id: ZoneId,
    #[serde(deserialize_with = "nullable")]
    pub owner_id: Option<OwnerId>,
    #[serde(deserialize_with = "nullable")]
    pub session_id: Option<SessionId>,
    pub revision: u64,
    pub epochs: Epochs,
    pub graph_epoch: u64,
    pub device_epoch: u64,
    pub event_sequence: u64,
    pub monotonic_ns: u64,
}

/// Contains no identities. Aliases in individual reports are local numeric indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticCut {
    pub revision: u64,
    pub epochs: Epochs,
    pub graph_epoch: u64,
    pub device_epoch: u64,
    pub event_sequence: u64,
    pub monotonic_ns: u64,
}
impl DiagnosticBinding {
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.zone_id.capacity_bytes() > 128
            || self
                .owner_id
                .as_ref()
                .is_some_and(|v| v.capacity_bytes() > 128)
            || self
                .session_id
                .as_ref()
                .is_some_and(|v| v.capacity_bytes() > 128)
        {
            return Err(BUDGET);
        }
        Ok(())
    }
    pub fn cut(&self) -> DiagnosticCut {
        DiagnosticCut {
            revision: self.revision,
            epochs: self.epochs,
            graph_epoch: self.graph_epoch,
            device_epoch: self.device_epoch,
            event_sequence: self.event_sequence,
            monotonic_ns: self.monotonic_ns,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticRead<T> {
    pub binding: DiagnosticBinding,
    pub report: T,
}
pub type SignalPathRead = DiagnosticRead<SignalPathReport>;
pub type MetricsRead = DiagnosticRead<MetricsReport>;

pub trait DiagnosticReport: Serialize {
    const MAX_READ_BYTES: usize = MAX_DIAGNOSTIC_READ_BYTES;
    fn cut(&self) -> DiagnosticCut;
    fn validate(&self) -> Result<(), EngineError>;
}
impl<T: DiagnosticReport> DiagnosticRead<T> {
    pub fn validate(&self) -> Result<(), EngineError> {
        if self.binding.cut() != self.report.cut() {
            return Err(INVALID);
        }
        self.report.validate()?;
        // Local identities also have bounded retained capacity, including typed callers.
        if self.binding.zone_id.capacity_bytes() > 128
            || self
                .binding
                .owner_id
                .as_ref()
                .is_some_and(|v| v.capacity_bytes() > 128)
            || self
                .binding
                .session_id
                .as_ref()
                .is_some_and(|v| v.capacity_bytes() > 128)
            || serde_json::to_vec(self).map_err(|_| INVALID)?.len() > T::MAX_READ_BYTES
        {
            return Err(BUDGET);
        }
        Ok(())
    }
}
impl<'de, T: DiagnosticReport + Deserialize<'de>> DiagnosticRead<T> {
    pub fn decode(bytes: &'de [u8]) -> Result<Self, EngineError> {
        if bytes.len() > T::MAX_READ_BYTES {
            return Err(BUDGET);
        }
        let value: Self = serde_json::from_slice(bytes).map_err(|_| INVALID)?;
        value.validate()?;
        Ok(value)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnavailableReason {
    Unsupported,
    NotObserved,
    NotApplicable,
    Stale,
}
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, bound(deserialize = "T: Deserialize<'de>"))]
pub struct Metric<T> {
    #[serde(deserialize_with = "nullable")]
    pub value: Option<T>,
    #[serde(deserialize_with = "nullable")]
    pub unavailable_reason: Option<UnavailableReason>,
    pub scope: MetricScope,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricScope {
    InstrumentationUnavailable,
    LastPlaybackObservation,
    GraphProcessingSinceReset,
}
impl<T> Metric<T> {
    pub(crate) fn unavailable(reason: UnavailableReason) -> Self {
        Self {
            value: None,
            unavailable_reason: Some(reason),
            scope: MetricScope::InstrumentationUnavailable,
        }
    }
    pub(crate) fn observed(value: Option<T>, scope: MetricScope) -> Self {
        let unavailable_reason = value
            .as_ref()
            .map_or(Some(UnavailableReason::NotObserved), |_| None);
        Self {
            value,
            unavailable_reason,
            scope,
        }
    }
    fn valid(&self) -> bool {
        self.value.is_some() == self.unavailable_reason.is_none()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnknownSegment {
    UpstreamProcessing,
    OutputRouteInternals,
    DacInternals,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestedPath {
    #[serde(deserialize_with = "nullable")]
    pub source: Option<SourceKind>,
    #[serde(deserialize_with = "nullable")]
    pub output: Option<RequestedOutput>,
    pub graph_configured: bool,
    pub software_gain_db: f64,
    pub gain_ramp_frames: u32,
    pub muted: bool,
    pub software_cap_db: f64,
    #[serde(deserialize_with = "nullable")]
    pub hardware_cap_db: Option<f64>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestedOutput {
    pub backend: OutputBackend,
    pub route: OutputRoute,
    pub volume_mode: VolumeMode,
    pub fidelity: FidelityPolicy,
    pub period_frames: u32,
    pub buffer_frames: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TransformationKind {
    Gain,
    Peq,
    Convolution,
    Src,
    BoundedIirTruncation,
    Delay,
    Matrix,
    Sum,
    Compensation,
    IntegerToFloat64,
    SoftwareVolume,
    Quantization,
    BackendAdapterConversion,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolvedPath {
    pub preparation_epochs: Epochs,
    pub preparation_revision: u64,
    pub source_format: PcmFormat,
    pub processing_input_format: PcmFormat,
    pub processing_output_format: PcmFormat,
    pub native_output_format: PcmFormat,
    pub preservation: PreservationDecision,
    pub transformations: Vec<TransformationKind>,
    pub required_headroom_db: f64,
    pub timing: DiagnosticTiming,
    #[serde(deserialize_with = "nullable")]
    pub compiled_nodes: Option<Vec<DiagnosticNode>>,
    #[serde(deserialize_with = "nullable")]
    pub nodes_unavailable_reason: Option<UnavailableReason>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticNodeKind {
    Input,
    Output,
    Bypass,
    Gain,
    Peq,
    Convolution,
    Src,
    Delay,
    Matrix,
    Sum,
    Meter,
}
/// Compiler facts describe a schedule, not a per-node execution trace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticNode {
    pub alias: u16,
    pub kind: DiagnosticNodeKind,
    pub format: PcmFormat,
    pub path_latency_frames: u64,
    #[serde(deserialize_with = "nullable")]
    pub gain_linear: Option<f64>,
    pub bypass: bool,
    #[serde(deserialize_with = "nullable")]
    pub implementation_version: Option<String>,
    pub implementation_unavailable_reason: UnavailableReason,
}
impl DiagnosticNode {
    pub fn validate(&self, index: usize) -> Result<(), EngineError> {
        if usize::from(self.alias) != index + 1
            || index >= crate::graph::MAX_GRAPH_NODES
            || self.implementation_version.is_some()
            || self.implementation_unavailable_reason != UnavailableReason::NotObserved
            || self.bypass != (self.kind == DiagnosticNodeKind::Bypass)
            || self.gain_linear.is_some() != (self.kind == DiagnosticNodeKind::Gain)
            || self.gain_linear.is_some_and(|v| !v.is_finite())
        {
            return Err(INVALID);
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DiagnosticTiming {
    SameRate {
        sample_rate_hz: u32,
        latency_frames: u64,
        tail_frames: u64,
    },
    Streaming {
        stages: Vec<DiagnosticStage>,
    },
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticStageKind {
    SameRateIsland,
    Src,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticStage {
    pub alias: u16,
    pub kind: DiagnosticStageKind,
    pub input_format: PcmFormat,
    pub output_format: PcmFormat,
    #[serde(deserialize_with = "nullable")]
    pub latency_frames: Option<u64>,
    #[serde(deserialize_with = "nullable")]
    pub tail_frames: Option<u64>,
    #[serde(deserialize_with = "nullable")]
    pub truncated_library_output_delay_frames: Option<u64>,
}
impl DiagnosticStage {
    pub fn from_compiled(index: usize, stage: &crate::graph_control::CompiledStreamStage) -> Self {
        use crate::graph_control::CompiledStreamStageKind;
        let (kind, latency_frames, tail_frames, truncated_library_output_delay_frames) =
            match &stage.kind {
                CompiledStreamStageKind::Island {
                    local_latency_frames,
                    emitted_tail_frames,
                    ..
                } => (
                    DiagnosticStageKind::SameRateIsland,
                    Some(*local_latency_frames),
                    Some(*emitted_tail_frames),
                    None,
                ),
                CompiledStreamStageKind::Src {
                    truncated_library_output_delay_frames,
                    ..
                } => (
                    DiagnosticStageKind::Src,
                    None,
                    None,
                    *truncated_library_output_delay_frames,
                ),
            };
        Self {
            alias: (index + 1) as u16,
            kind,
            input_format: stage.input_format,
            output_format: stage.output_format,
            latency_frames,
            tail_frames,
            truncated_library_output_delay_frames,
        }
    }
    fn valid(&self, index: usize) -> bool {
        usize::from(self.alias) == index + 1
            && match self.kind {
                DiagnosticStageKind::SameRateIsland => {
                    self.latency_frames.is_some()
                        && self.tail_frames.is_some()
                        && self.input_format == self.output_format
                        && self.truncated_library_output_delay_frames.is_none()
                }
                DiagnosticStageKind::Src => {
                    self.latency_frames.is_none() && self.tail_frames.is_none()
                }
            }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MeasurementBoundary {
    AlsaClientNegotiation,
    CoreAudioObservedStreamFormats,
    Unavailable,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SampleRepresentation {
    Signed16,
    Signed24,
    Signed32,
    Float32,
    Float64,
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticFormat {
    pub sample_rate_hz: f64,
    pub channels: u32,
    pub representation: SampleRepresentation,
    pub bytes_per_frame: u32,
    pub interleaved: bool,
}
impl DiagnosticFormat {
    fn valid(&self) -> bool {
        let scalar_bytes: u32 = match self.representation {
            SampleRepresentation::Signed16 => 2,
            SampleRepresentation::Signed24 => 3,
            SampleRepresentation::Signed32 | SampleRepresentation::Float32 => 4,
            SampleRepresentation::Float64 => 8,
            SampleRepresentation::Unknown => 1,
        };
        let frame_channels = if self.interleaved { self.channels } else { 1 };
        self.sample_rate_hz.is_finite()
            && self.sample_rate_hz > 0.0
            && self.channels > 0
            && self.bytes_per_frame > 0
            && scalar_bytes
                .checked_mul(frame_channels)
                .is_some_and(|bytes| self.bytes_per_frame >= bytes)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiagnosticNetworkReplacementBoundary {
    pub source_epochs: Epochs,
    pub source_position_frames: u64,
    pub previous_output_epochs: Epochs,
    pub output_epochs: Epochs,
    pub previous_output_format: PcmFormat,
    pub discarded_output_frames: u64,
}
impl DiagnosticNetworkReplacementBoundary {
    pub fn validate(&self) -> Result<(), EngineError> {
        crate::network_source::validate_replacement_facts(
            self.source_epochs,
            self.previous_output_epochs,
            self.output_epochs,
            self.previous_output_format,
            self.discarded_output_frames,
        )
    }
}
impl From<&crate::network_source::NetworkReplacementBoundary>
    for DiagnosticNetworkReplacementBoundary
{
    fn from(value: &crate::network_source::NetworkReplacementBoundary) -> Self {
        Self {
            source_epochs: value.source_epochs,
            source_position_frames: value.source_position_frames,
            previous_output_epochs: value.previous_output_epochs,
            output_epochs: value.output_epochs,
            previous_output_format: value.previous_output_format,
            discarded_output_frames: value.discarded_output_frames,
        }
    }
}
fn deserialize_network_replacement_boundary<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<DiagnosticNetworkReplacementBoundary>, D::Error> {
    DiagnosticNetworkReplacementBoundary::deserialize(deserializer).map(Some)
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedPath {
    #[serde(deserialize_with = "nullable")]
    pub sample_epochs: Option<Epochs>,
    #[serde(deserialize_with = "nullable")]
    pub sample_monotonic_ns: Option<u64>,
    pub playback_format: PcmFormat,
    pub modified: bool,
    #[serde(deserialize_with = "nullable")]
    pub backend: Option<ObservedBackend>,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "deserialize_network_replacement_boundary"
    )]
    pub network_replacement_boundary: Option<DiagnosticNetworkReplacementBoundary>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservedBackend {
    pub backend: OutputBackend,
    pub measurement_boundary: MeasurementBoundary,
    pub client_format: DiagnosticFormat,
    #[serde(deserialize_with = "nullable")]
    pub device_format: Option<DiagnosticFormat>,
    pub preservation: BackendPreservation,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendPreservation {
    Unverified,
    Modified,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignalPathReport {
    pub schema_version: u32,
    pub cut: DiagnosticCut,
    pub requested: RequestedPath,
    #[serde(deserialize_with = "nullable")]
    pub resolved: Option<ResolvedPath>,
    #[serde(deserialize_with = "nullable")]
    pub observed: Option<ObservedPath>,
    pub unknown_segments: Vec<UnknownSegment>,
}
impl DiagnosticReport for SignalPathReport {
    fn cut(&self) -> DiagnosticCut {
        self.cut
    }
    fn validate(&self) -> Result<(), EngineError> {
        let requested = &self.requested;
        if self.schema_version != 1
            || self.unknown_segments.capacity() > 8
            || self.unknown_segments
                != [
                    UnknownSegment::UpstreamProcessing,
                    UnknownSegment::OutputRouteInternals,
                    UnknownSegment::DacInternals,
                ]
            || !requested.software_gain_db.is_finite()
            || VolumePolicy::new(requested.software_cap_db, requested.hardware_cap_db).is_err()
            || !(-120.0..=requested.software_cap_db).contains(&requested.software_gain_db)
        {
            return Err(INVALID);
        }
        if let Some(resolved) = &self.resolved {
            if !crate::graph_control::graph_boundary_formats_match(
                resolved.source_format,
                resolved.processing_input_format,
                resolved.processing_output_format,
                resolved.native_output_format,
            ) {
                return Err(INVALID);
            }
            if let Some(nodes) = &resolved.compiled_nodes {
                if nodes.is_empty()
                    || nodes.capacity() > crate::graph::MAX_GRAPH_NODES
                    || resolved.nodes_unavailable_reason.is_some()
                {
                    return Err(INVALID);
                }
                for (index, node) in nodes.iter().enumerate() {
                    node.validate(index)?;
                }
            } else if resolved.nodes_unavailable_reason != Some(UnavailableReason::NotObserved) {
                return Err(INVALID);
            }
            if resolved.transformations.len() > crate::graph::MAX_GRAPH_NODES * 2 + 4
                || resolved.transformations.capacity() > crate::graph::MAX_GRAPH_NODES * 2 + 4
                || !resolved.required_headroom_db.is_finite()
            {
                return Err(INVALID);
            }
            match &resolved.timing {
                DiagnosticTiming::SameRate { sample_rate_hz, .. } => {
                    if *sample_rate_hz != resolved.processing_input_format.sample_rate_hz()
                        || *sample_rate_hz != resolved.processing_output_format.sample_rate_hz()
                    {
                        return Err(INVALID);
                    }
                }
                DiagnosticTiming::Streaming { stages } => {
                    if stages.is_empty()
                        || stages.capacity() > crate::graph::MAX_GRAPH_NODES
                        || stages.first().unwrap().input_format != resolved.processing_input_format
                        || stages.last().unwrap().output_format != resolved.processing_output_format
                        || stages
                            .iter()
                            .enumerate()
                            .any(|(index, stage)| !stage.valid(index))
                        || stages
                            .windows(2)
                            .any(|pair| pair[0].output_format != pair[1].input_format)
                    {
                        return Err(INVALID);
                    }
                }
            }
        }
        if let Some(observed) = &self.observed {
            if let Some(boundary) = &observed.network_replacement_boundary {
                boundary.validate()?;
                if (
                    boundary.output_epochs.session,
                    boundary.output_epochs.stream,
                ) > (self.cut.epochs.session, self.cut.epochs.stream)
                {
                    return Err(INVALID);
                }
                if self
                    .requested
                    .source
                    .is_some_and(|s| s != crate::control::SourceKind::HttpFlac)
                {
                    return Err(INVALID);
                }
            }
            // No current producer retains an observation's original epoch/time envelope.
            if observed.sample_epochs.is_some() || observed.sample_monotonic_ns.is_some() {
                return Err(INVALID);
            }
            if let Some(backend) = &observed.backend
                && (!backend.client_format.valid()
                    || backend.device_format.as_ref().is_some_and(|v| !v.valid()))
            {
                return Err(INVALID);
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetricsReport {
    pub schema_version: u32,
    pub cut: DiagnosticCut,
    pub underruns: Metric<u64>,
    pub deadline_misses: Metric<u64>,
    pub cpu_load: Metric<f64>,
    pub queue_depth_frames: Metric<u64>,
    pub input_peak: Metric<f64>,
    pub output_peak: Metric<f64>,
    pub saturation_count: Metric<u64>,
    pub backend_delay_frames: Metric<u64>,
}
impl DiagnosticReport for MetricsReport {
    fn cut(&self) -> DiagnosticCut {
        self.cut
    }
    fn validate(&self) -> Result<(), EngineError> {
        let unsupported = |value: bool, reason, scope| {
            !value
                && reason == Some(UnavailableReason::Unsupported)
                && scope == MetricScope::InstrumentationUnavailable
        };
        if self.schema_version != 1
            || !unsupported(
                self.underruns.value.is_some(),
                self.underruns.unavailable_reason,
                self.underruns.scope,
            )
            || !unsupported(
                self.deadline_misses.value.is_some(),
                self.deadline_misses.unavailable_reason,
                self.deadline_misses.scope,
            )
            || !unsupported(
                self.cpu_load.value.is_some(),
                self.cpu_load.unavailable_reason,
                self.cpu_load.scope,
            )
            || !unsupported(
                self.queue_depth_frames.value.is_some(),
                self.queue_depth_frames.unavailable_reason,
                self.queue_depth_frames.scope,
            )
            || !self.underruns.valid()
            || !self.deadline_misses.valid()
            || !self.cpu_load.valid()
            || !self.queue_depth_frames.valid()
            || !self.input_peak.valid()
            || !self.output_peak.valid()
            || !self.saturation_count.valid()
            || !self.backend_delay_frames.valid()
            || self.input_peak.scope != MetricScope::LastPlaybackObservation
            || self.output_peak.scope != MetricScope::LastPlaybackObservation
            || self.backend_delay_frames.scope != MetricScope::LastPlaybackObservation
            || self.saturation_count.scope != MetricScope::GraphProcessingSinceReset
            || self
                .cpu_load
                .value
                .is_some_and(|v| !v.is_finite() || v < 0.0)
            || self
                .input_peak
                .value
                .is_some_and(|v| !v.is_finite() || v < 0.0)
            || self
                .output_peak
                .value
                .is_some_and(|v| !v.is_finite() || v < 0.0)
        {
            return Err(INVALID);
        }
        Ok(())
    }
}

pub(crate) const DIAGNOSTIC_BUDGET: EngineError = BUDGET;
pub(crate) use preview::diagnostic_digest as preview_digest;
