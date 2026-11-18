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
const METADATA_ALLOWANCE: u64 = earph_core::graph_control::GRAPH_PREVIEW_RETENTION_BYTES as u64;
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
struct CompiledPreview {
    kernel_hash: String,
    implementation_version: String,
    timing: earph_core::graph_control::GraphTiming,
    preservation: PreservationDecision,
    transformations: Vec<earph_core::graph::Transformation>,
    bounds: Vec<f64>,
    resources: GraphResidentUsage,
}
enum Mode {
    Integer {
        _plan: Box<CompiledPlan>,
        epochs: Epochs,
    },
    Float64(Box<PreparedRender>),
    Streaming(Box<crate::stream_render::PreparedStreamRender>),
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
            Mode::Integer { .. } | Mode::Streaming(_) => None,
        }
    }
    pub fn render_mut(&mut self) -> Option<&mut PreparedRender> {
        match &mut self.payload.as_mut().expect("owned pipeline").mode {
            Mode::Float64(render) => Some(render),
            Mode::Integer { .. } | Mode::Streaming(_) => None,
        }
    }
    pub fn stream_render(&self) -> Option<&crate::stream_render::PreparedStreamRender> {
        match &self.payload.as_ref().expect("owned pipeline").mode {
            Mode::Streaming(render) => Some(render),
            Mode::Integer { .. } | Mode::Float64(_) => None,
        }
    }
    pub fn stream_render_mut(&mut self) -> Option<&mut crate::stream_render::PreparedStreamRender> {
        match &mut self.payload.as_mut().expect("owned pipeline").mode {
            Mode::Streaming(render) => Some(render),
            Mode::Integer { .. } | Mode::Float64(_) => None,
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
            Mode::Streaming(render) => render.reset(epochs).map_err(Into::into),
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
        // Streaming kernels share source cancellation; retire them by off-thread ownership release.
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
    let prospective;
    let snapshot = if let Some(application) = &request.profile_application {
        application.validate().map_err(GraphDiagnostic::from)?;
        if snapshot.source != Some(application.source) {
            return Err(STALE.into());
        }
        if snapshot
            .playback
            .as_ref()
            .and_then(|p| p.backend.as_ref())
            .is_none()
        {
            return Err(EngineError::new(
                ErrorCode::DeviceUnavailable,
                "profile preparation requires an observed open backend",
            )
            .into());
        }
        if snapshot
            .output
            .as_ref()
            .is_none_or(|o| o.volume_mode != earph_core::safety::VolumeMode::Software)
            || application.software_gain_db
                != snapshot
                    .software_gain_db
                    .min(application.effective_policy.software_cap_db())
        {
            return Err(STALE.into());
        }
        prospective = {
            let mut state = snapshot.clone();
            state.volume_policy = application.effective_policy.clone();
            state.software_gain_db = application.software_gain_db;
            state
        };
        &prospective
    } else {
        snapshot
    };
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
    let streaming = request
        .intent
        .graph
        .nodes
        .iter()
        .any(|n| matches!(n.kind, NodeKind::Src { .. }));
    if !float
        && request.intent.graph.nodes.iter().any(|n| {
            matches!(
                n.kind,
                NodeKind::Peq { .. } | NodeKind::Convolution { .. } | NodeKind::Src { .. }
            )
        })
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
    // Session-owned profile history can grow while compilation and receipt staging overlap.
    // Keep that shared allowance outside per-pipeline residency to avoid charging every pipeline.
    if snapshot.profile_storage_bytes > earph_core::profile_control::MAX_PROFILE_STORAGE_BYTES {
        return Err(RESOURCE.into());
    }
    let profile_retention = if snapshot.profile_storage_bytes == 0 {
        0
    } else {
        earph_core::profile_control::MAX_PROFILE_STORAGE_BYTES
            .checked_mul(2)
            .ok_or(RESOURCE)?
    };
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
    let native_period = if streaming {
        native_period.checked_mul(2).ok_or(RESOURCE)?
    } else {
        native_period
    };
    let stream_bridge = if streaming {
        Some(
            crate::stream_render::StreamRenderConfig {
                input_format: source,
                native_format: native,
                period_frames: quantum,
                queue_frames: output.buffer_frames as usize,
                epochs: request.binding.epochs,
                budget_bytes: LIMIT,
            }
            .bridge_resources(quantum)
            .map_err(GraphDiagnostic::from)?,
        )
    } else {
        None
    };
    let existing = retirement.accounting.resident.load(Ordering::Acquire);
    let backend_audio_bytes = playback
        .backend
        .as_ref()
        .map_or(0, |b| b.audio_storage_bytes);
    // Core Audio constructs its replacement while the closed output still owns its ring.
    let backend_audio_peak_bytes = match output.backend {
        earph_core::control::OutputBackend::CoreAudio => {
            backend_audio_bytes.checked_mul(2).ok_or(RESOURCE)?
        }
        earph_core::control::OutputBackend::Alsa => backend_audio_bytes,
    };
    let existing_route_bytes = source
        .storage_bytes(quantum as u64)
        .map_err(GraphDiagnostic::from)? as u64;
    let asset_admission =
        assets::GraphAssets::inspect(&request.intent.graph).map_err(GraphDiagnostic::from)?;
    let asset_scratch = asset_admission.scratch_bytes();
    let bridge_bytes = match stream_bridge {
        Some(bridge) => bridge.bridge_bytes,
        None => sum(&[host_input, host_output, native_period])?,
    };
    let fixed = sum(&[
        existing,
        backend_audio_peak_bytes,
        existing_route_bytes,
        metadata,
        bridge_bytes,
        profile_retention,
        if streaming { 0 } else { PREPARATION_SCRATCH },
        asset_scratch,
    ])?;
    let preparation_limit = request
        .profile_application
        .as_ref()
        .map_or(LIMIT, |a| LIMIT.min(a.latency.max_preparation_bytes));
    let resident_limit = request
        .profile_application
        .as_ref()
        .map_or(LIMIT, |a| LIMIT.min(a.latency.max_resident_bytes));
    let steady_fixed = sum(&[
        backend_audio_bytes,
        existing_route_bytes,
        metadata,
        bridge_bytes,
    ])?;
    let preparation_allowance = preparation_limit.checked_sub(fixed).ok_or(RESOURCE)?;
    let resident_allowance = resident_limit.checked_sub(steady_fixed).ok_or(RESOURCE)?;
    let kernel_allowance = preparation_allowance.min(resident_allowance);
    let resolved = asset_admission
        .resolve(asset_store, kernel_allowance, || {
            check().map_err(|e| e.error())
        })
        .map_err(GraphDiagnostic::from)?;
    check()?;
    let context = CompileContext {
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
            max_latency_frames: request
                .profile_application
                .as_ref()
                .and_then(|a| a.latency.max_latency_frames)
                .unwrap_or(u64::MAX),
            ..GraphBudget::default()
        },
    };
    if let Some(bridge) = stream_bridge {
        let plan = earph_dsp::compile_stream(
            &request.intent.graph,
            &context,
            &resolved,
            earph_dsp::stream::StreamBudget {
                max_resident_bytes: kernel_allowance,
                max_preparation_bytes: preparation_allowance,
                ..earph_dsp::stream::StreamBudget::default()
            },
        )
        .map_err(diagnostic)?;
        check()?;
        let resources = plan.resources();
        let aggregate_peak_bytes = sum(&[fixed, resources.preparation_peak_bytes])?;
        let pipeline_bytes = sum(&[metadata, bridge.bridge_bytes, resources.resident_bytes])?;
        if aggregate_peak_bytes > preparation_limit
            || sum(&[steady_fixed, resources.resident_bytes])? > resident_limit
        {
            return Err(RESOURCE.into());
        }
        let bridge_remainder = bridge
            .bridge_bytes
            .checked_sub(sum(&[host_input, host_output, native_period])?)
            .ok_or(RESOURCE)?;
        let mut preview = prepared_preview(
            CompiledPreview {
                kernel_hash: plan.content_hash().into(),
                implementation_version: plan.implementation_version().into(),
                timing: earph_core::graph_control::GraphTiming::Streaming {
                    stages: plan.stage_metadata(),
                },
                preservation: PreservationDecision::ProcessFloat64,
                transformations: plan.transformations().to_vec(),
                bounds: plan.output_peak_bounds().to_vec(),
                resources: GraphResidentUsage {
                    backend_audio_bytes,
                    backend_audio_peak_bytes,
                    stream_bridge_bytes: sum(&[
                        resources.fifo_bytes,
                        resources.staging_bytes,
                        bridge_remainder,
                    ])?,
                    kernel_audio_bytes: resources.kernel_bytes,
                    host_input_bytes: host_input,
                    host_output_bytes: host_output,
                    native_period_bytes: native_period,
                    metadata_bytes: sum(&[metadata, resources.metadata_bytes])?,
                    pipeline_bytes,
                    aggregate_peak_bytes,
                },
            },
            request,
            snapshot,
            context,
            source,
            native,
        )?;
        check()?;
        if request.validate_only {
            return Ok(GraphSuccess {
                preview,
                pipeline: None,
            });
        }
        let render = crate::stream_render::PreparedStreamRender::prepare(
            plan.into_stream_plan(),
            crate::stream_render::StreamRenderConfig {
                input_format: source,
                native_format: native,
                period_frames: quantum,
                queue_frames: output.buffer_frames as usize,
                epochs: request.binding.epochs,
                budget_bytes: kernel_allowance
                    .checked_add(bridge.bridge_bytes)
                    .ok_or(RESOURCE)?,
            },
            source_cancel.clone(),
        )
        .map_err(GraphDiagnostic::from)?;
        if sum(&[render.resources().resident_bytes, metadata])? != pipeline_bytes
            || render.resources().preparation_peak_bytes
                != sum(&[resources.preparation_peak_bytes, bridge.bridge_bytes])?
        {
            return Err(RESOURCE.into());
        }
        if let earph_core::graph_control::GraphTiming::Streaming { stages } =
            &mut Arc::make_mut(&mut preview).timing
        {
            for (index, stage) in Arc::make_mut(stages).iter_mut().enumerate() {
                if let earph_core::graph_control::CompiledStreamStageKind::Src {
                    truncated_library_output_delay_frames,
                    ..
                } = &mut stage.kind
                {
                    let actual = render.stream().src_metadata(index).ok_or(RESOURCE)?;
                    *truncated_library_output_delay_frames =
                        Some(u64::try_from(actual.truncated_output_delay).map_err(|_| RESOURCE)?);
                }
            }
        }
        check()?;
        return Ok(retained_pipeline(
            preview,
            request,
            Mode::Streaming(Box::new(render)),
            retirement,
        ));
    }
    let plan = earph_dsp::compile_with_assets(&request.intent.graph, &context, &resolved)
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
        backend_audio_peak_bytes,
        existing_route_bytes,
        pipeline_bytes,
        PREPARATION_SCRATCH,
        asset_scratch,
        profile_retention,
        plan.preparation_scratch_bytes(),
    ])?;
    if aggregate_peak_bytes > preparation_limit
        || sum(&[backend_audio_bytes, existing_route_bytes, pipeline_bytes])? > resident_limit
    {
        return Err(RESOURCE.into());
    }
    let preview = prepared_preview(
        CompiledPreview {
            kernel_hash: plan.content_hash().into(),
            implementation_version: plan.implementation_version().into(),
            timing: earph_core::graph_control::GraphTiming::SameRate {
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
            },
            preservation: plan.preservation(),
            transformations: plan.transformations().to_vec(),
            resources: GraphResidentUsage {
                backend_audio_bytes,
                backend_audio_peak_bytes,
                stream_bridge_bytes: 0,
                kernel_audio_bytes: kernel_bytes,
                host_input_bytes: host_input,
                host_output_bytes: host_output,
                native_period_bytes: native_period,
                metadata_bytes: metadata,
                pipeline_bytes,
                aggregate_peak_bytes,
            },
            bounds: plan.output_peak_bounds().to_vec(),
        },
        request,
        snapshot,
        context,
        source,
        native,
    )?;
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
    Ok(retained_pipeline(preview, request, mode, retirement))
}
fn retained_pipeline(
    preview: Arc<GraphPreview>,
    request: &GraphJobRequest,
    mode: Mode,
    retirement: &Arc<Retirement>,
) -> GraphSuccess {
    let pipeline_bytes = preview.resources.pipeline_bytes;
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
    GraphSuccess {
        preview,
        pipeline: Some(PreparedPipeline {
            payload: Some(payload),
            retirement: retirement.clone(),
            candidate: true,
        }),
    }
}
fn prepared_preview(
    compiled: CompiledPreview,
    request: &GraphJobRequest,
    snapshot: &SessionSnapshot,
    context: CompileContext,
    source: PcmFormat,
    native: PcmFormat,
) -> Result<Arc<GraphPreview>, GraphDiagnostic> {
    let playback = snapshot.playback.as_ref().ok_or(STALE)?;
    let maximum = compiled.bounds.iter().copied().fold(1.0_f64, f64::max);
    let mut transformations: Vec<_> = compiled
        .transformations
        .into_iter()
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
    let preview = Arc::new(GraphPreview {
        profile_application: request.profile_application.clone(),
        backend: playback.backend.clone(),
        graph_hash: activation_hash(
            &compiled.kernel_hash,
            context.quantum_frames,
            request,
            snapshot,
            source,
            native,
        )?,
        kernel_hash: compiled.kernel_hash,
        implementation_version: compiled.implementation_version,
        binding: request.binding.clone(),
        source_format: source,
        native_output_format: native,
        processing_input_format: context.input_format,
        processing_output_format: context.output_format,
        quantum_frames: context.quantum_frames,
        timing: compiled.timing,
        preservation: compiled.preservation,
        transformations,
        resources: compiled.resources,
        channel_peak_bounds: compiled.bounds,
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
    Ok(preview)
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
    kernel_hash: &str,
    quantum_frames: usize,
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
    #[derive(serde::Serialize)]
    struct BackendIdentity<'a> {
        backend: &'a earph_core::control::OutputBackend,
        client_format: &'a ObservedStreamFormat,
        device_format: &'a Option<ObservedStreamFormat>,
        measurement_boundary: &'static str,
        preservation: &'a earph_core::session::PreservationState,
    }
    // Storage affects admission, while these existing facts define route identity.
    let backend_identity = snapshot
        .playback
        .as_ref()
        .and_then(|p| p.backend.as_ref())
        .map(|b| BackendIdentity {
            backend: &b.backend,
            client_format: &b.client_format,
            device_format: &b.device_format,
            measurement_boundary: b.measurement_boundary,
            preservation: &b.preservation,
        });
    // This tuple intentionally omits binding IDs, epochs, revision, clocks, and TTL.
    // Output device/profile IDs remain because they describe the selected route.
    serde_json::to_writer(
        &mut writer,
        &(
            "earph-activation/1",
            kernel_hash,
            source,
            native,
            quantum_frames,
            snapshot.output.as_ref(),
            backend_identity,
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
    if let Some(application) = &request.profile_application {
        serde_json::to_writer(&mut writer, &("earph-profile-activation/1", application))
            .map_err(|_| GraphDiagnostic::from(RESOURCE))?;
    }
    Ok(writer
        .0
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}
