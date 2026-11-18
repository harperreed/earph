// ABOUTME: Prepares one graph candidate from captured session facts outside playback.
// ABOUTME: Retires every owned graph on its bounded preparation thread.
use crate::assets;
use crate::render::{PreparedRender, RenderError, RenderResources};
use earph_core::{
    EngineError, ErrorCode,
    audio::{MemoryBudget, PcmFormat, PcmRepresentation},
    graph::{GraphProcessingRequest, NodeKind, PreservationDecision},
    graph_control::{
        GraphBinding, GraphDiagnostic, GraphIntent, GraphJobRequest, GraphPreview,
        GraphResidentUsage, GraphTransition, SignalTransformation,
    },
    ids::{Epochs, RequestId},
    safety::{FidelityPolicy, OutputContext, SampleDomain},
    session::{ObservedStreamFormat, SessionSnapshot},
};
use earph_dsp::{CompileContext, CompiledPlan, GraphBudget, GraphError, ProcessingPolicy};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError},
    },
    thread::{self, JoinHandle, ThreadId},
    time::Instant,
};

const LIMIT: u64 = 256 * 1024 * 1024;
// Covers bounded compiler maps, canonical serialization, hashes, and preview construction.
const PREPARATION_SCRATCH: u64 = 4 * 1024 * 1024;
// Covers 128 compiled/runtime nodes, 256 inputs, IDs, and preview transformations.
const METADATA_ALLOWANCE: u64 = 2 * 1024 * 1024;
const BUSY: EngineError = EngineError::new(
    ErrorCode::SourceBusy,
    "graph preparation or retirement owns the candidate slot",
);
const RESOURCE: EngineError = EngineError::new(
    ErrorCode::ResourceBudgetExceeded,
    "aggregate graph storage exceeds admission",
);
const STALE: EngineError = EngineError::new(
    ErrorCode::StaleSession,
    "captured graph facts differ from the job binding",
);
const CANCELLED: EngineError = EngineError::new(
    ErrorCode::OperationCancelled,
    "graph preparation was cancelled",
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceResources {
    pub resident_bytes: u64,
    pub pending_retirements: usize,
    pub preparing_or_candidate: bool,
}
#[derive(Default)]
struct Accounting {
    resident: AtomicU64,
    retiring: AtomicUsize,
    owners: AtomicUsize,
    candidate: AtomicBool,
}

#[derive(Clone)]
pub struct GraphCancellation(Arc<AtomicBool>);
impl GraphCancellation {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
}

pub struct GraphCompletion {
    pub request_id: RequestId,
    pub binding: GraphBinding,
    pub result: Result<GraphSuccess, GraphDiagnostic>,
}
pub struct GraphSuccess {
    pub preview: Arc<GraphPreview>,
    pub pipeline: Option<PreparedPipeline>,
}

struct Payload {
    preview: Arc<GraphPreview>,
    _intent: Arc<GraphIntent>,
    mode: Mode,
}
enum Mode {
    Integer {
        _plan: Box<CompiledPlan>,
        epochs: Epochs,
    },
    Float64(Box<PreparedRender>),
}

/// This owner never frees graph storage on the playback worker.
pub struct PreparedPipeline {
    payload: Option<Box<Payload>>,
    retirement: Arc<Retirement>,
    candidate: bool,
}
impl PreparedPipeline {
    pub fn preview(&self) -> &Arc<GraphPreview> {
        &self.payload.as_ref().expect("owned pipeline").preview
    }
    pub fn binding(&self) -> &GraphBinding {
        &self.preview().binding
    }
    pub fn resources(&self) -> GraphResidentUsage {
        self.preview().resources
    }
    pub fn render(&self) -> Option<&PreparedRender> {
        match &self.payload.as_ref().expect("owned pipeline").mode {
            Mode::Float64(render) => Some(render),
            Mode::Integer { .. } => None,
        }
    }
    pub fn render_mut(&mut self) -> Option<&mut PreparedRender> {
        match &mut self.payload.as_mut().expect("owned pipeline").mode {
            Mode::Float64(render) => Some(render),
            Mode::Integer { .. } => None,
        }
    }
    /// Call only when the worker installs this candidate at its muted boundary.
    pub fn activate(mut self) -> Self {
        if self.candidate {
            self.candidate = false;
            self.retirement
                .accounting
                .candidate
                .store(false, Ordering::Release);
        }
        self
    }
    pub fn reset(&mut self, epochs: Epochs) -> Result<(), RenderError> {
        match &mut self.payload.as_mut().expect("owned pipeline").mode {
            Mode::Float64(render) => render.reset(epochs),
            Mode::Integer {
                epochs: current, ..
            } => {
                if epochs.session != current.session || epochs.stream <= current.stream {
                    return Err(STALE.into());
                }
                *current = epochs;
                Ok(())
            }
        }
    }
}
impl Drop for PreparedPipeline {
    fn drop(&mut self) {
        let Some(mut payload) = self.payload.take() else {
            return;
        };
        if let Mode::Float64(render) = &mut payload.mode {
            render.cancel();
        }
        self.retirement
            .accounting
            .retiring
            .fetch_add(1, Ordering::AcqRel);
        let retired = Retired {
            payload,
            candidate: self.candidate,
        };
        if thread::current().id() == self.retirement.thread_id {
            retire(retired, &self.retirement.accounting);
            return;
        }
        let mailbox = &self.retirement.mailbox;
        let mut state = mailbox
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // Two retirement slots cover both permitted owners. Waiting preserves
        // ownership if that invariant changes; it never frees a graph here.
        loop {
            if let Some(slot) = state.retired.iter_mut().find(|slot| slot.is_none()) {
                *slot = Some(retired);
                mailbox.wake.notify_one();
                return;
            }
            state = mailbox
                .wake
                .wait(state)
                .unwrap_or_else(|error| error.into_inner());
        }
    }
}

struct Retirement {
    mailbox: Arc<Mailbox>,
    join: Mutex<Option<JoinHandle<()>>>,
    thread_id: ThreadId,
    accounting: Arc<Accounting>,
}
impl Drop for Retirement {
    fn drop(&mut self) {
        {
            let mut state = self
                .mailbox
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state.shutdown = true;
            self.mailbox.wake.notify_one();
        }
        let join = self
            .join
            .get_mut()
            .unwrap_or_else(|error| error.into_inner())
            .take();
        if let Some(join) = join
            && join.thread().id() != thread::current().id()
        {
            let _ = join.join();
        }
    }
}
struct Job {
    request: GraphJobRequest,
    snapshot: Box<SessionSnapshot>,
    cancellation: GraphCancellation,
    source_cancel: Arc<AtomicBool>,
    retirement: Arc<Retirement>,
    started: Instant,
}
struct Retired {
    payload: Box<Payload>,
    candidate: bool,
}
#[derive(Default)]
struct MailboxState {
    pending: Option<Job>,
    retired: [Option<Retired>; 2],
    shutdown: bool,
}
#[derive(Default)]
struct Mailbox {
    state: Mutex<MailboxState>,
    wake: Condvar,
}

pub struct GraphService {
    retirement: Arc<Retirement>,
    completions: Receiver<GraphCompletion>,
    cancellation: Option<GraphCancellation>,
}
impl GraphService {
    pub fn spawn(asset_store: Option<PathBuf>) -> Result<Self, EngineError> {
        let (completions_tx, completions) = mpsc::sync_channel(1);
        let accounting = Arc::new(Accounting::default());
        let mailbox = Arc::new(Mailbox::default());
        let worker_accounting = accounting.clone();
        let worker_mailbox = mailbox.clone();
        let join = thread::Builder::new()
            .name("earph-graph-preparation".into())
            .spawn(move || {
                run(
                    worker_mailbox,
                    completions_tx,
                    worker_accounting,
                    asset_store,
                )
            })
            .map_err(|_| {
                EngineError::new(
                    ErrorCode::DeviceUnavailable,
                    "graph preparation thread is unavailable",
                )
            })?;
        let thread_id = join.thread().id();
        Ok(Self {
            retirement: Arc::new(Retirement {
                mailbox,
                join: Mutex::new(Some(join)),
                thread_id,
                accounting,
            }),
            completions,
            cancellation: None,
        })
    }
    pub fn resources(&self) -> ServiceResources {
        let accounting = &self.retirement.accounting;
        ServiceResources {
            resident_bytes: accounting.resident.load(Ordering::Acquire),
            pending_retirements: accounting.retiring.load(Ordering::Acquire),
            preparing_or_candidate: accounting.candidate.load(Ordering::Acquire),
        }
    }
    pub fn submit(
        &mut self,
        request: GraphJobRequest,
        snapshot: SessionSnapshot,
        source_cancel: Arc<AtomicBool>,
    ) -> Result<GraphCancellation, EngineError> {
        if source_cancel.load(Ordering::Acquire) {
            return Err(CANCELLED);
        }
        let accounting = &self.retirement.accounting;
        if accounting.retiring.load(Ordering::Acquire) != 0
            || accounting.owners.load(Ordering::Acquire) >= 2
            || accounting
                .candidate
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return Err(BUSY);
        }
        let cancellation = GraphCancellation(Arc::new(AtomicBool::new(false)));
        let mut state = self
            .retirement
            .mailbox
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        state.pending = Some(Job {
            request,
            snapshot: Box::new(snapshot),
            cancellation: cancellation.clone(),
            source_cancel,
            retirement: self.retirement.clone(),
            started: Instant::now(),
        });
        self.cancellation = Some(cancellation.clone());
        self.retirement.mailbox.wake.notify_one();
        Ok(cancellation)
    }
    pub fn try_completion(&mut self) -> Option<GraphCompletion> {
        match self.completions.try_recv() {
            Ok(completion) => {
                self.cancellation = None;
                if completion.result.as_ref().is_err()
                    || completion
                        .result
                        .as_ref()
                        .is_ok_and(|success| success.pipeline.is_none())
                {
                    self.retirement
                        .accounting
                        .candidate
                        .store(false, Ordering::Release);
                }
                Some(completion)
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
    }
}
impl Drop for GraphService {
    fn drop(&mut self) {
        if let Some(cancellation) = self.cancellation.take() {
            cancellation.cancel();
            // Wait for the bounded stage to finish before releasing shutdown ownership.
            if let Ok(completion) = self.completions.recv() {
                drop(completion);
            }
        }
        while let Ok(completion) = self.completions.try_recv() {
            drop(completion);
        }
        {
            let mut state = self
                .retirement
                .mailbox
                .state
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            state.shutdown = true;
            self.retirement.mailbox.wake.notify_one();
        }
        let accounting = &self.retirement.accounting;
        // External pipelines retain the handle and perform this join after retirement.
        // Separate retirement counters cannot prove that all external owners released their pipelines.
        if accounting.owners.load(Ordering::Acquire) == 0
            && let Some(join) = self
                .retirement
                .join
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .take()
        {
            let _ = join.join();
        }
    }
}
fn run(
    mailbox: Arc<Mailbox>,
    completions: SyncSender<GraphCompletion>,
    accounting: Arc<Accounting>,
    asset_store: Option<PathBuf>,
) {
    loop {
        let mut state = mailbox
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if let Some(retired) = state.retired.iter_mut().find_map(Option::take) {
            drop(state);
            retire(retired, &accounting);
            mailbox.wake.notify_all();
            continue;
        }
        if let Some(job) = state.pending.take() {
            drop(state);
            let Job {
                request,
                snapshot,
                cancellation,
                source_cancel,
                retirement,
                started,
            } = job;
            let request_id = request.request_id.clone();
            let binding = request.binding.clone();
            // Contain compiler panics so the one retirement executor remains alive.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                prepare(
                    &request,
                    &snapshot,
                    &cancellation,
                    &source_cancel,
                    &retirement,
                    started,
                    asset_store.as_deref(),
                )
            }))
            .unwrap_or_else(|_| {
                Err(EngineError::new(ErrorCode::OutputFault, "graph preparation panicked").into())
            });
            let completion = GraphCompletion {
                request_id,
                binding,
                result,
            };
            if let Err(error) = completions.try_send(completion) {
                match error {
                    TrySendError::Full(completion) | TrySendError::Disconnected(completion) => {
                        drop(completion)
                    }
                };
                accounting.candidate.store(false, Ordering::Release);
            }
            continue;
        }
        if state.shutdown && accounting.owners.load(Ordering::Acquire) == 0 {
            return;
        }
        drop(
            mailbox
                .wake
                .wait(state)
                .unwrap_or_else(|error| error.into_inner()),
        );
    }
}
fn retire(retired: Retired, accounting: &Accounting) {
    let bytes = retired.payload.preview.resources.pipeline_bytes;
    drop(retired.payload);
    accounting.resident.fetch_sub(bytes, Ordering::AcqRel);
    accounting.owners.fetch_sub(1, Ordering::AcqRel);
    if retired.candidate {
        accounting.candidate.store(false, Ordering::Release);
    }
    accounting.retiring.fetch_sub(1, Ordering::AcqRel);
}

fn prepare(
    request: &GraphJobRequest,
    snapshot: &SessionSnapshot,
    cancellation: &GraphCancellation,
    source_cancel: &Arc<AtomicBool>,
    retirement: &Arc<Retirement>,
    started: Instant,
    asset_store: Option<&Path>,
) -> Result<GraphSuccess, GraphDiagnostic> {
    let check = || -> Result<(), GraphDiagnostic> {
        let elapsed = u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        if cancellation.0.load(Ordering::Acquire) || source_cancel.load(Ordering::Acquire) {
            return Err(CANCELLED.into());
        }
        if snapshot
            .monotonic_ns
            .checked_add(elapsed)
            .is_none_or(|now| now >= request.deadline_monotonic_ns)
        {
            return Err(EngineError::new(
                ErrorCode::OperationCancelled,
                "graph preparation deadline expired",
            )
            .into());
        }
        Ok(())
    };
    check()?;
    if snapshot.owner_id.as_ref() != Some(&request.binding.owner_id)
        || snapshot.session_id.as_ref() != Some(&request.binding.session_id)
        || snapshot.epochs != request.binding.epochs
        || snapshot.device_epoch != request.binding.device_epoch
        || snapshot.revision != request.binding.expected_revision
    {
        return Err(STALE.into());
    }
    request
        .intent
        .graph
        .validate_structure()
        .map_err(GraphDiagnostic::from)?;
    let output = snapshot.output.as_ref().ok_or(STALE)?;
    output.validate().map_err(GraphDiagnostic::from)?;
    if output.fidelity == FidelityPolicy::StrictPreservation && snapshot.gain_ramp_frames != 0 {
        return Err(EngineError::new(
            ErrorCode::FidelityPolicyConflict,
            "strict output cannot apply a software gain ramp",
        )
        .into());
    }
    let playback = snapshot.playback.as_ref().ok_or(STALE)?;
    let source = playback.format;
    // Both native adapters consume this integer PcmBlock format. SDK client
    // formats can differ, and remain explicit evidence in the semantic hash.
    let native = source;
    if let Some(backend) = &playback.backend
        && (backend.backend != output.backend
            || std::iter::once(&backend.client_format)
                .chain(backend.device_format.iter())
                .any(|format| {
                    !format.sample_rate_hz.is_finite()
                        || format.sample_rate_hz <= 0.0
                        || format.channels == 0
                }))
    {
        return Err(EngineError::new(
            ErrorCode::FormatUnsupported,
            "captured native backend evidence is invalid",
        )
        .into());
    }
    source
        .representation()
        .integer_bounds()
        .map_err(GraphDiagnostic::from)?;
    if !snapshot.output_consented {
        return Err(EngineError::new(
            ErrorCode::SafetyPolicyBlocked,
            "output consent is required for graph preparation",
        )
        .into());
    }
    snapshot
        .volume_policy
        .validate_unmute(
            &OutputContext {
                device_id: output.device_id.clone(),
                profile_id: output.profile_id.clone(),
            },
            output.volume_mode,
            SampleDomain::Pcm,
            output.fidelity,
            snapshot.software_gain_db,
            None,
        )
        .map_err(GraphDiagnostic::from)?;
    let quantum = output.period_frames as usize;
    let float = matches!(
        request.intent.processing,
        GraphProcessingRequest::Float64 { .. }
    );
    if !float
        && request
            .intent
            .graph
            .nodes
            .iter()
            .any(|n| matches!(n.kind, NodeKind::Peq { .. } | NodeKind::Convolution { .. }))
    {
        return Err(EngineError::new(
            ErrorCode::FidelityPolicyConflict,
            "integer routes cannot admit filter processing",
        )
        .into());
    }
    if float && output.fidelity == FidelityPolicy::StrictPreservation {
        return Err(EngineError::new(
            ErrorCode::FidelityPolicyConflict,
            "float graph processing conflicts with strict preservation",
        )
        .into());
    }
    let processing_input = PcmFormat::new(
        source.sample_rate_hz(),
        source.channels(),
        if float {
            PcmRepresentation::Float64
        } else {
            source.representation()
        },
    )
    .map_err(GraphDiagnostic::from)?;
    let processing_output = PcmFormat::new(
        native.sample_rate_hz(),
        native.channels(),
        if float {
            PcmRepresentation::Float64
        } else {
            native.representation()
        },
    )
    .map_err(GraphDiagnostic::from)?;
    let intent_bytes = intent_capacity(&request.intent)?;
    let metadata = METADATA_ALLOWANCE
        .checked_add(intent_bytes)
        .ok_or(RESOURCE)?;
    let host_input = if float {
        processing_input
            .storage_bytes(quantum as u64)
            .map_err(GraphDiagnostic::from)? as u64
    } else {
        0
    };
    let host_output = if float {
        processing_output
            .storage_bytes(quantum as u64)
            .map_err(GraphDiagnostic::from)? as u64
    } else {
        0
    };
    let native_period = if float {
        native
            .storage_bytes(quantum as u64)
            .map_err(GraphDiagnostic::from)? as u64
    } else {
        0
    };
    let existing = retirement.accounting.resident.load(Ordering::Acquire);
    let existing_route_bytes = source
        .storage_bytes(quantum as u64)
        .map_err(GraphDiagnostic::from)? as u64;
    let mut references = BTreeMap::new();
    for node in &request.intent.graph.nodes {
        if let NodeKind::Convolution { asset } = &node.kind {
            asset.validate().map_err(GraphDiagnostic::from)?;
            if let Some(previous) = references.insert(asset.sha256.clone(), asset)
                && previous != asset
            {
                return Err(EngineError::new(
                    ErrorCode::AssetInvalid,
                    "correction identity has conflicting geometry",
                )
                .into());
            }
        }
    }
    let declared_asset_bytes = references
        .values()
        .try_fold(0_u64, |n, a| {
            n.checked_add(
                (a.taps_per_channel as u64)
                    .checked_mul(a.channels.count() as u64)?
                    .checked_mul(8)?
                    .checked_add(1024)?,
            )
        })
        .ok_or(RESOURCE)?;
    // One bounded source, packet, decoder buffer, and metadata exist during sequential resolution.
    let asset_scratch = if references.is_empty() {
        0
    } else {
        8 * 1024 * 1024
    };
    let fixed = sum(&[
        existing,
        existing_route_bytes,
        metadata,
        host_input,
        host_output,
        native_period,
        PREPARATION_SCRATCH,
        asset_scratch,
    ])?;
    let kernel_allowance = LIMIT.checked_sub(fixed).ok_or(RESOURCE)?;
    if declared_asset_bytes > kernel_allowance {
        return Err(RESOURCE.into());
    }
    let mut resolved = Vec::with_capacity(references.len());
    for reference in references.values() {
        check()?;
        let correction = assets::resolve(
            asset_store.ok_or_else(|| {
                GraphDiagnostic::from(EngineError::new(
                    ErrorCode::AssetInvalid,
                    "correction store is unavailable",
                ))
            })?,
            reference,
        )
        .map_err(GraphDiagnostic::from)?;
        resolved.push(
            earph_dsp::FirCoefficients::new(correction.reference, correction.taps)
                .map_err(diagnostic)?,
        );
    }
    check()?;
    let plan = earph_dsp::compile_with_assets(
        &request.intent.graph,
        &CompileContext {
            input_format: processing_input,
            output_format: processing_output,
            quantum_frames: quantum,
            policy: if float {
                ProcessingPolicy::AllowProcessing
            } else {
                ProcessingPolicy::StrictPreservation
            },
            budget: GraphBudget {
                max_buffer_bytes: kernel_allowance,
                ..GraphBudget::default()
            },
        },
        &resolved,
    )
    .map_err(diagnostic)?;
    check()?;
    if !float
        && (plan.preservation() != PreservationDecision::RetainInteger
            || !plan.transformations().is_empty())
    {
        return Err(EngineError::new(
            ErrorCode::FidelityPolicyConflict,
            "integer bypass requires strict integer preservation",
        )
        .into());
    }
    if !float
        && plan
            .schedule()
            .iter()
            .any(|node| matches!(node.kind(), NodeKind::Meter))
    {
        return Err(EngineError::new(
            ErrorCode::CapabilityUnsupported,
            "integer graph metering is not implemented",
        )
        .into());
    }
    let bounds = plan.output_peak_bounds().to_vec();
    let maximum = bounds.iter().copied().fold(1.0_f64, f64::max);
    let mut transformations: Vec<_> = plan
        .transformations()
        .iter()
        .cloned()
        .map(|transformation| SignalTransformation::Kernel { transformation })
        .collect();
    if let GraphProcessingRequest::Float64 { quantization } = request.intent.processing {
        transformations.insert(0, SignalTransformation::IntegerToFloat64);
        transformations.push(SignalTransformation::SoftwareVolume);
        transformations.push(SignalTransformation::Quantization {
            policy: quantization,
        });
    } else if snapshot.software_gain_db != 0.0 || snapshot.gain_ramp_frames != 0 {
        transformations.push(SignalTransformation::SoftwareVolume);
    }
    if let Some(backend) = &playback.backend
        && backend.client_format.representation != ObservedStreamFormat::from(native).representation
    {
        transformations.push(SignalTransformation::BackendAdapterConversion {
            client_representation: backend.client_format.representation,
        });
    }
    let kernel_bytes = if float {
        plan.resources().buffer_bytes
    } else {
        0
    };
    let pipeline_bytes = sum(&[
        kernel_bytes,
        host_input,
        host_output,
        native_period,
        metadata,
    ])?;
    let aggregate_peak_bytes = sum(&[
        existing,
        existing_route_bytes,
        pipeline_bytes,
        PREPARATION_SCRATCH,
        asset_scratch,
        plan.preparation_scratch_bytes(),
    ])?;
    if aggregate_peak_bytes > LIMIT {
        return Err(RESOURCE.into());
    }
    let preview = Arc::new(GraphPreview {
        backend: playback.backend.clone(),
        graph_hash: activation_hash(&plan, request, snapshot, source, native)?,
        kernel_hash: plan.content_hash().into(),
        implementation_version: plan.implementation_version().into(),
        binding: request.binding.clone(),
        source_format: source,
        native_output_format: native,
        processing_input_format: processing_input,
        processing_output_format: processing_output,
        quantum_frames: quantum,
        latency_frames: plan.latency_frames(),
        tail_frames: plan.tail_frames(),
        tail_policy: if plan.bounded_iir_truncation() {
            earph_core::graph_control::GraphTailPolicy::BoundedIirTruncation
        } else {
            earph_core::graph_control::GraphTailPolicy::AuthoredFiniteSupport
        },
        terminal_output_bounds: plan.terminal_output_bounds().to_vec(),
        node_tails: plan
            .schedule()
            .iter()
            .filter(|n| {
                matches!(
                    n.kind(),
                    NodeKind::Peq { .. } | NodeKind::Convolution { .. }
                )
            })
            .map(|n| earph_core::graph_control::FilterTailObservation {
                node: n.id().clone(),
                channels: n.format().channels,
                horizon_frames: n.tail_frames(),
                terminal_output_bounds: n.terminal_output_bounds().to_vec(),
            })
            .collect(),
        preservation: plan.preservation(),
        transformations,
        resources: GraphResidentUsage {
            kernel_audio_bytes: kernel_bytes,
            host_input_bytes: host_input,
            host_output_bytes: host_output,
            native_period_bytes: native_period,
            metadata_bytes: metadata,
            pipeline_bytes,
            aggregate_peak_bytes,
        },
        channel_peak_bounds: bounds,
        required_headroom_db: 20.0 * maximum.log10(),
        transition: GraphTransition::MutedRebuffer,
    });
    preview.validate().map_err(GraphDiagnostic::from)?;
    if !request.validate_only {
        preview
            .validate_headroom(
                snapshot.software_gain_db,
                snapshot.volume_policy.software_cap_db(),
            )
            .map_err(GraphDiagnostic::from)?;
    }
    check()?;
    if request.validate_only {
        return Ok(GraphSuccess {
            preview,
            pipeline: None,
        });
    }
    let mode = if float {
        let resources = RenderResources::required(&plan, native).map_err(render_diagnostic)?;
        if resources.audio_payload_bytes as u64
            != sum(&[kernel_bytes, host_input, host_output, native_period])?
        {
            return Err(RESOURCE.into());
        }
        Mode::Float64(Box::new(
            PreparedRender::prepare(
                plan,
                source,
                native,
                request.binding.epochs,
                MemoryBudget::new(resources.audio_payload_bytes).map_err(GraphDiagnostic::from)?,
            )
            .map_err(render_diagnostic)?,
        ))
    } else {
        Mode::Integer {
            _plan: Box::new(plan),
            epochs: request.binding.epochs,
        }
    };
    check()?;
    let payload = Box::new(Payload {
        preview: preview.clone(),
        _intent: request.intent.clone(),
        mode,
    });
    retirement
        .accounting
        .resident
        .fetch_add(pipeline_bytes, Ordering::AcqRel);
    retirement.accounting.owners.fetch_add(1, Ordering::AcqRel);
    Ok(GraphSuccess {
        preview,
        pipeline: Some(PreparedPipeline {
            payload: Some(payload),
            retirement: retirement.clone(),
            candidate: true,
        }),
    })
}
fn sum(terms: &[u64]) -> Result<u64, GraphDiagnostic> {
    terms.iter().try_fold(0_u64, |total, value| {
        total.checked_add(*value).ok_or_else(|| RESOURCE.into())
    })
}
fn diagnostic(error: GraphError) -> GraphDiagnostic {
    GraphDiagnostic {
        code: error.code,
        message: error.message,
        node: error.node,
        edge: error.edge,
    }
}
fn render_diagnostic(error: RenderError) -> GraphDiagnostic {
    GraphDiagnostic {
        code: error.code,
        message: error.message,
        node: None,
        edge: None,
    }
}

fn intent_capacity(intent: &GraphIntent) -> Result<u64, GraphDiagnostic> {
    let graph = &intent.graph;
    // Reject excessive retained caller capacities before allocating a compiled plan.
    if graph.nodes.capacity() > 128 || graph.edges.capacity() > 256 {
        return Err(RESOURCE.into());
    }
    for node in &graph.nodes {
        if let NodeKind::Matrix { coefficients, .. } = &node.kind
            && coefficients.capacity() > 4
        {
            return Err(RESOURCE.into());
        }
    }
    sum(&[
        size_of::<GraphIntent>() as u64,
        u64::try_from(
            graph
                .retained_capacity_bytes()
                .map_err(GraphDiagnostic::from)?,
        )
        .map_err(|_| GraphDiagnostic::from(RESOURCE))?,
    ])
}
fn activation_hash(
    plan: &CompiledPlan,
    request: &GraphJobRequest,
    snapshot: &SessionSnapshot,
    source: PcmFormat,
    native: PcmFormat,
) -> Result<String, GraphDiagnostic> {
    struct HashWriter(Sha256);
    impl Write for HashWriter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut writer = HashWriter(Sha256::new());
    // This tuple intentionally omits binding IDs, epochs, revision, clocks, and TTL.
    // Output device/profile IDs remain because they describe the selected route.
    serde_json::to_writer(
        &mut writer,
        &(
            "earph-activation/1",
            plan.content_hash(),
            source,
            native,
            plan.context().quantum_frames,
            snapshot.output.as_ref(),
            snapshot.playback.as_ref().and_then(|p| p.backend.as_ref()),
            request.intent.processing,
            &snapshot.volume_policy,
            snapshot.software_gain_db,
            snapshot.gain_ramp_frames,
            snapshot.output_consented,
            true,
        ),
    )
    .map_err(|_| {
        GraphDiagnostic::from(EngineError::new(
            ErrorCode::GraphInvalid,
            "graph activation hash serialization failed",
        ))
    })?;
    Ok(writer
        .0
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}
