// ABOUTME: Coordinates bounded native workflows using actual owner snapshots and receipts.
// ABOUTME: Fences queue pages and captured edits without repairing stale targets.
use super::profile_files::{CorrectionPreview, CorrectionPreviewRequest, CorrectionWorker};
use super::{HostStatus, host::OwnerHost, request_id};
use crate::{
    CapabilitiesView, DeliveryFailure, DesktopClient, DeviceQueryResultView, DispatchError,
    GraphDiagnosticView, GraphInspectionView, GraphPreparedView, GraphPreviewView, Lane,
    Presentation, PresentationModel, ProfilePageView, ProfileRecordView, ProfileResolutionView,
    QueueView, Reply, Request, RequestRecord, ResultView, StateView, SubmitError,
};
use earph_core::{
    control::{Command, OutputBackend},
    ids::{OwnerId, RequestId, SessionId, ZoneId},
};
use std::{
    path::{Component, Path, PathBuf},
    sync::Arc,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

pub enum UiIntent {
    Attach {
        runtime: String,
    },
    Launch {
        runtime: String,
        executable: PathBuf,
    },
    Disconnect,
    Refresh,
    QueueEdit {
        basis: Arc<StateView>,
        command: Command,
    },
    ControlEdit {
        basis: Arc<StateView>,
        command: Command,
    },
    StartLocal {
        basis: Arc<StateView>,
        takeover: bool,
    },
    Discover {
        backend: OutputBackend,
        endpoint: Option<String>,
    },
    ProfileAction {
        basis: Arc<StateView>,
        command: Command,
    },
    PreviewCorrection {
        basis: Arc<StateView>,
        request: CorrectionPreviewRequest,
    },
    InvalidateCorrection,
    ReviewCorrection {
        owner: OwnerId,
        reference: earph_core::profile_control::ProfileCatalogReference,
    },
    ReviewFirAsset {
        owner: OwnerId,
        asset: earph_core::dsp_nodes::FirAssetReference,
    },
    DiscardPreparedGraph,
    Mute,
    Stop,
    Reconcile(RequestId),
    ShutdownAndQuit,
    CloseController,
}
pub struct Receipt<'a> {
    pub request: &'a Request,
    pub presentation: Presentation,
    pub in_flight: bool,
    pub owner_changed: bool,
    pub outcome: &'a Option<Result<Reply, DeliveryFailure>>,
}
fn record_view(r: &RequestRecord) -> Receipt<'_> {
    Receipt {
        request: &r.request,
        presentation: r.presentation,
        in_flight: r.in_flight,
        owner_changed: r.owner_changed,
        outcome: &r.outcome,
    }
}
fn same_context(a: &StateView, b: &StateView) -> bool {
    a.owner_id == b.owner_id
        && a.zone_id == b.zone_id
        && a.session_id == b.session_id
        && a.revision == b.revision
        && a.epochs == b.epochs
        && a.output == b.output
        && a.device_epoch == b.device_epoch
        && a.graph_epoch == b.graph_epoch
        && a.active_entry_id == b.active_entry_id
}
fn same_review_context(a: &StateView, b: &StateView) -> bool {
    a.owner_id == b.owner_id
        && a.zone_id == b.zone_id
        && a.session_id == b.session_id
        && a.epochs == b.epochs
        && a.output == b.output
        && a.device_epoch == b.device_epoch
        && a.graph_epoch == b.graph_epoch
        && a.active_entry_id == b.active_entry_id
        && a.source == b.source
}
pub(super) struct ObservedProfile<T> {
    pub basis: Arc<StateView>,
    pub request_ids: Vec<RequestId>,
    pub command: Command,
    pub value: T,
}
#[derive(Default)]
pub(super) struct ProfileViews {
    pub catalog: Option<ObservedProfile<ProfilePageView>>,
    pub record: Option<ObservedProfile<Arc<ProfileRecordView>>>,
    pub resolution: Option<ObservedProfile<ProfileResolutionView>>,
    pub graph: Option<ObservedProfile<Arc<GraphInspectionView>>>,
    pub preview: Option<ObservedProfile<Arc<GraphPreviewView>>>,
    pub prepared: Option<ObservedProfile<GraphPreparedView>>,
    pub diagnostic: Option<(RequestId, Arc<GraphDiagnosticView>)>,
    pub fir_asset: Option<ObservedProfile<Arc<earph_core::dsp_nodes::FirAssetReference>>>,
}
struct LocalCorrection {
    basis: Arc<StateView>,
    draft_epoch: u64,
    worker: CorrectionWorker,
    canceled: bool,
}
struct CapturedCorrection {
    basis: Arc<StateView>,
    draft_epoch: u64,
    value: CorrectionPreview,
}
struct ProfileRead {
    basis: Arc<StateView>,
    command: Command,
    foreground: bool,
    pages: Option<ProfilePageView>,
    ids: Vec<RequestId>,
}
enum Task {
    LocalPreview {
        basis: Arc<StateView>,
        request: CorrectionPreviewRequest,
        draft_epoch: u64,
    },
    Before,
    Capabilities,
    Page(usize),
    After,
    Preflight {
        basis: Arc<StateView>,
        command: Command,
        session: Option<SessionId>,
        draft_epoch: u64,
    },
    Mutation {
        draft_epoch: u64,
    },
    Discovery {
        command: Command,
        owner: OwnerId,
    },
    ProfileBefore {
        basis: Option<Arc<StateView>>,
        command: Command,
        foreground: bool,
    },
    ProfileResult(ProfileRead),
    ProfileAfter {
        read: ProfileRead,
        result: ResultView,
    },
}
impl Task {
    fn edits_foreground(&self) -> bool {
        matches!(
            self,
            Self::Preflight { .. }
                | Self::Mutation { .. }
                | Self::Discovery { .. }
                | Self::LocalPreview { .. }
        ) || match self {
            Self::ProfileBefore { foreground, .. } => *foreground,
            Self::ProfileResult(read) | Self::ProfileAfter { read, .. } => read.foreground,
            _ => false,
        }
    }
}
pub struct GuiController {
    client: Option<DesktopClient>,
    disconnected: Option<PresentationModel>,
    capabilities: Option<CapabilitiesView>,
    queue: Option<QueueView>,
    queue_basis: Option<Arc<StateView>>,
    queue_stale: bool,
    candidate: Option<QueueView>,
    basis: Option<Arc<StateView>>,
    pending: Option<(RequestId, Task)>,
    waiting_edit: Option<Task>,
    safety: Vec<RequestId>,
    operation_cursor: usize,
    discovery: Option<Arc<DeviceQueryResultView>>,
    inspection: Option<Arc<DeviceQueryResultView>>,
    shutdown: Option<RequestId>,
    wait_for_child: bool,
    refresh: bool,
    next_poll: Instant,
    next_replay: Instant,
    replay_cursor: usize,
    connected: bool,
    error: Option<String>,
    observation_error: Option<String>,
    cleanup: Option<JoinHandle<Result<(), String>>>,
    closing: bool,
    host: OwnerHost,
    host_error_reported: bool,
    runtime: Option<String>,
    pub(super) profiles: ProfileViews,
    draft_epoch: u64,
    local_correction: Option<LocalCorrection>,
    local_preview: Option<CapturedCorrection>,
    local_status: String,
    correction_review: Option<(
        Arc<StateView>,
        earph_core::profile_control::ProfileCatalogReference,
    )>,
    fir_review: Option<(Arc<StateView>, earph_core::dsp_nodes::FirAssetReference)>,
}
impl Default for GuiController {
    fn default() -> Self {
        Self::new()
    }
}
impl GuiController {
    pub fn new() -> Self {
        Self {
            client: None,
            disconnected: Some(PresentationModel::default()),
            capabilities: None,
            queue: None,
            queue_basis: None,
            queue_stale: true,
            candidate: None,
            basis: None,
            pending: None,
            waiting_edit: None,
            safety: Vec::new(),
            operation_cursor: 0,
            discovery: None,
            inspection: None,
            shutdown: None,
            wait_for_child: false,
            refresh: false,
            next_poll: Instant::now(),
            next_replay: Instant::now(),
            replay_cursor: 0,
            connected: false,
            error: None,
            observation_error: None,
            cleanup: None,
            closing: false,
            host: OwnerHost::default(),
            host_error_reported: false,
            runtime: None,
            profiles: ProfileViews::default(),
            draft_epoch: 0,
            local_correction: None,
            local_preview: None,
            local_status: "No local correction worker".into(),
            correction_review: None,
            fir_review: None,
        }
    }
    pub fn state(&self) -> Option<&StateView> {
        self.model()
            .latest_state()
            .map(|observed| observed.state.as_ref())
    }
    pub fn correction_preview(&self) -> Option<&CorrectionPreview> {
        self.local_preview.as_ref().map(|capture| &capture.value)
    }
    pub fn correction_preview_current(&self) -> bool {
        self.local_preview.as_ref().is_some_and(|capture| {
            capture.draft_epoch == self.draft_epoch
                && self
                    .state()
                    .is_some_and(|state| same_context(&capture.basis, state))
        })
    }
    pub fn local_correction_pending(&self) -> bool {
        self.local_correction.is_some()
            || matches!(self.waiting_edit, Some(Task::LocalPreview { .. }))
    }
    pub fn local_correction_status(&self) -> &str {
        &self.local_status
    }
    pub fn fir_asset(&self) -> Option<&earph_core::dsp_nodes::FirAssetReference> {
        self.profiles.fir_asset.as_ref().map(|a| a.value.as_ref())
    }
    pub fn correction_reviewed(
        &self,
        reference: &earph_core::profile_control::ProfileCatalogReference,
    ) -> bool {
        self.correction_review
            .as_ref()
            .is_some_and(|(basis, saved)| {
                self.state()
                    .is_some_and(|state| same_review_context(basis, state))
                    && saved == reference
            })
    }
    pub fn fir_asset_reviewed(&self) -> bool {
        self.fir_review.as_ref().is_some_and(|(basis, asset)| {
            self.state()
                .is_some_and(|state| same_review_context(basis, state))
                && self.fir_asset() == Some(asset)
        })
    }
    pub fn profile_review_available(
        &self,
        reference: &earph_core::profile_control::ProfileCatalogReference,
    ) -> bool {
        self.profile_record().is_some_and(|record| {
            record.profile.id == reference.id
                && record.profile.revision == reference.revision
                && record.profile_sha256 == reference.profile_sha256
                && (record.corrections.is_empty() || self.correction_reviewed(reference))
        })
    }
    fn invalidate_correction(&mut self) {
        self.local_preview = None;
        self.correction_review = None;
        self.fir_review = None;
        if self.waiting_edit.as_ref().is_some_and(correction_unsent) {
            if matches!(self.waiting_edit, Some(Task::LocalPreview { .. })) {
                self.local_status = "Correction preview canceled before worker start".into();
            }
            self.waiting_edit = None;
        }
        if let Some((_, task)) = self.pending.as_mut()
            && correction_unsent(task)
        {
            *task = Task::Before;
        }
        if let Some(local) = self.local_correction.as_mut() {
            local.worker.cancel();
            local.canceled = true;
            self.local_status = "Correction preview cancellation requested; worker retiring".into();
        }
    }
    fn start_local(&mut self, task: Task) -> Result<(), String> {
        let Task::LocalPreview {
            basis,
            request,
            draft_epoch,
        } = task
        else {
            return Err("Missing local correction capture".into());
        };
        if self.local_correction.is_some()
            || draft_epoch != self.draft_epoch
            || self.state().is_none_or(|s| !same_context(&basis, s))
        {
            return Err(
                "Correction capture changed before preview; review and submit again".into(),
            );
        }
        self.local_correction = Some(LocalCorrection {
            basis,
            draft_epoch,
            worker: CorrectionWorker::start(request)?,
            canceled: false,
        });
        self.local_status = "Reading and parsing correction source; worker running".into();
        Ok(())
    }
    fn poll_local(&mut self) {
        let Some(result) = self
            .local_correction
            .as_mut()
            .and_then(|local| local.worker.poll())
        else {
            return;
        };
        let local = self.local_correction.take().unwrap();
        if local.canceled
            || local.draft_epoch != self.draft_epoch
            || !self.connected()
            || self.state().is_none_or(|s| !same_context(&local.basis, s))
        {
            self.local_status = "Correction preview discarded; worker retired".into();
            return;
        }
        self.local_status = "Correction preview worker retired".into();
        match result {
            Ok(preview) => {
                self.local_preview = Some(CapturedCorrection {
                    basis: local.basis,
                    draft_epoch: local.draft_epoch,
                    value: preview,
                });
            }
            Err(error) => self.error = Some(error),
        }
    }
    fn validate_correction_action(
        &self,
        basis: &StateView,
        command: &Command,
    ) -> Result<(), String> {
        if !matches!(
            command,
            Command::ProfilesImport { .. } | Command::FirImport { .. }
        ) {
            return Ok(());
        }
        if !self.correction_preview_current()
            || self.state().is_none_or(|state| !same_context(basis, state))
        {
            return Err(
                "Correction preview context or draft changed; preview again before import".into(),
            );
        }
        let preview = self
            .correction_preview()
            .ok_or("Preview the correction source before import")?;
        let matches = match (command, preview) {
            (Command::ProfilesImport { input, .. }, preview) => input.as_ref() == &preview.input(),
            (
                Command::FirImport { path, purpose },
                CorrectionPreview::Fir {
                    path: selected,
                    manifest,
                },
            ) => path == selected && *purpose == manifest.asset.purpose,
            _ => false,
        };
        if matches {
            Ok(())
        } else {
            Err("Correction command differs from the exact previewed input".into())
        }
    }
    pub fn queue(&self) -> Option<&QueueView> {
        self.queue.as_ref()
    }
    pub fn profile_catalog(&self) -> Option<&ProfilePageView> {
        self.profiles.catalog.as_ref().map(|o| &o.value)
    }
    pub fn profile_record(&self) -> Option<&ProfileRecordView> {
        self.profiles.record.as_ref().map(|o| o.value.as_ref())
    }
    pub fn profile_resolution(&self) -> Option<&ProfileResolutionView> {
        self.profiles.resolution.as_ref().map(|o| &o.value)
    }
    pub fn graph_inspection(&self) -> Option<&GraphInspectionView> {
        self.profiles.graph.as_ref().map(|o| o.value.as_ref())
    }
    pub fn prepared_graph(&self) -> Option<&GraphPreparedView> {
        self.profiles.prepared.as_ref().map(|o| &o.value)
    }
    pub fn prepared_action_available(&self) -> bool {
        self.control_edit_available()
            && self
                .prepared_graph()
                .zip(self.state())
                .is_some_and(|(prepared, state)| {
                    prepared_matches_state(prepared, state)
                        && prepared
                            .preview
                            .profile_application
                            .as_ref()
                            .is_none_or(|application| {
                                self.profile_review_available(&application.profile)
                            })
                })
    }
    fn validate_prepared_action(&self, state: &StateView, command: &Command) -> Result<(), String> {
        if let Command::GraphPrepare { graph, .. } = command
            && let Some(record) = self.profile_record().filter(|record| {
                !record.corrections.is_empty() && record.graph.graph.as_ref() == graph.as_ref()
            })
            && !self.correction_reviewed(&earph_core::profile_control::ProfileCatalogReference {
                id: record.profile.id.clone(),
                revision: record.profile.revision,
                profile_sha256: record.profile_sha256,
            })
        {
            return Err("Review the actual owner-returned correction before Prepare".into());
        }
        let profile = match command {
            Command::ProfilesPrepare {
                id,
                expected_profile_revision,
                profile_sha256,
            }
            | Command::ProfilesApply {
                id,
                expected_profile_revision,
                profile_sha256,
                ..
            } => Some(earph_core::profile_control::ProfileCatalogReference {
                id: id.clone(),
                revision: *expected_profile_revision,
                profile_sha256: *profile_sha256,
            }),
            _ => None,
        };
        if let Some(reference) = profile {
            let record = self
                .profile_record()
                .filter(|r| {
                    r.profile.id == reference.id
                        && r.profile.revision == reference.revision
                        && r.profile_sha256 == reference.profile_sha256
                })
                .ok_or("Observe the exact saved profile before Prepare or Apply")?;
            if !record.corrections.is_empty() && !self.correction_reviewed(&reference) {
                return Err(
                    "Review the actual owner-returned correction before Prepare or Apply".into(),
                );
            }
        }
        let (token, hash) = match command {
            Command::GraphCommit { token, graph_hash }
            | Command::ProfilesApply {
                token, graph_hash, ..
            } => (token, graph_hash),
            _ => return Ok(()),
        };
        let prepared = self
            .prepared_graph()
            .ok_or("Review a fresh prepared candidate first")?;
        if token != &prepared.token
            || hash != prepared.preview.graph_hash.as_str()
            || !prepared_matches_state(prepared, state)
        {
            return Err("Prepared graph token or context changed; prepare and review again".into());
        }
        match (command, prepared.preview.profile_application.as_ref()) {
            (Command::GraphCommit { .. }, None) => Ok(()),
            (
                Command::ProfilesApply {
                    id,
                    expected_profile_revision,
                    profile_sha256,
                    ..
                },
                Some(binding),
            ) if &binding.profile.id == id
                && binding.profile.revision == *expected_profile_revision
                && binding.profile.profile_sha256 == *profile_sha256 =>
            {
                Ok(())
            }
            _ => Err(
                "Prepared candidate kind or saved profile reference differs from this action"
                    .into(),
            ),
        }
    }
    fn adopt_profile_mutation(
        &mut self,
        id: &RequestId,
        command: &Command,
        receipt: &crate::ReceiptView,
        draft_epoch: u64,
    ) -> Result<(), String> {
        let captured = self
            .model()
            .record(id)
            .ok_or("Profile action request identity unavailable")?;
        let envelope = captured.request.envelope();
        if captured.owner_changed
            || self.state().is_none_or(|state| {
                state.owner_id != envelope.owner_id || state.zone_id != envelope.zone_id
            })
        {
            return Err(
                "Profile action owner or zone changed; its exact receipt remains in history".into(),
            );
        }
        if matches!(
            command,
            Command::GraphCommit { .. } | Command::ProfilesApply { .. }
        ) {
            if receipt.result.is_some() || receipt.activation_frame.is_none() {
                return Err(
                    "Activation acknowledgment lacks its actual frame or has an unexpected result"
                        .into(),
                );
            }
            self.profiles.prepared = None;
            return Ok(());
        }
        let result = receipt
            .result
            .clone()
            .ok_or("Profile action result unavailable")?;
        validate_profile_result(command, &result)?;
        let basis = Arc::new(
            self.state()
                .ok_or("Profile action basis unavailable")?
                .clone(),
        );
        if let ResultView::GraphPreview(preview) = &result
            && !preview_matches_state(preview, &basis)
        {
            return Err("Graph validation binding differs from the captured source context".into());
        }
        if let ResultView::GraphPrepared(prepared) = &result {
            if !prepared_matches_state(prepared, &basis) {
                return Err(
                    "Prepared candidate owner binding differs from the submitted source context"
                        .into(),
                );
            }
            if draft_epoch != self.draft_epoch {
                return Err("Draft or selected profile changed while preparation was pending; its receipt remains retained".into());
            }
            if let Some(application) = &prepared.preview.profile_application {
                let output = basis
                    .output
                    .as_ref()
                    .ok_or("Prepared profile lacks current output context")?;
                let identity = basis
                    .playback
                    .as_ref()
                    .and_then(|p| p.backend.as_ref())
                    .and_then(|b| b.device_identity_sha256);
                if application.volume_mode != output.volume_mode
                    || application.fidelity != output.fidelity
                    || application.device_identity_sha256 != identity
                    || Some(application.source) != basis.source
                {
                    return Err("Prepared profile application facts differ from the observed output/source context".into());
                }
                if let Some(record) = self.profile_record()
                    && record.profile.id == application.profile.id
                    && record.profile.revision == application.profile.revision
                    && record.profile_sha256 == application.profile.profile_sha256
                    && record.profile.graph_sha256 != application.graph_sha256
                {
                    return Err(
                        "Prepared profile graph object differs from the reviewed saved record"
                            .into(),
                    );
                }
            }
        }
        self.adopt_profile_result(basis, vec![id.clone()], command.clone(), result)
    }
    fn adopt_profile_result(
        &mut self,
        basis: Arc<StateView>,
        request_ids: Vec<RequestId>,
        command: Command,
        result: ResultView,
    ) -> Result<(), String> {
        validate_profile_result(&command, &result)?;
        macro_rules! observed {
            ($value:expr) => {
                ObservedProfile {
                    basis,
                    request_ids,
                    command,
                    value: $value,
                }
            };
        }
        match result {
            ResultView::Profiles(value) => {
                self.profiles.catalog = Some(observed!((*value).clone()))
            }
            ResultView::Profile(value) | ResultView::ProfileSaved(value) => {
                if self.profiles.record.as_ref().is_none_or(|old| {
                    old.basis.owner_id != basis.owner_id || old.value.as_ref() != value.as_ref()
                }) {
                    self.correction_review = None;
                }
                self.profiles.record = Some(observed!(value))
            }
            ResultView::FirAsset(value) => {
                self.fir_review = None;
                self.profiles.fir_asset = Some(observed!(value));
            }
            ResultView::ProfileResolution(value) => {
                self.profiles.resolution = Some(observed!(value))
            }
            ResultView::GraphInspection(value) => self.profiles.graph = Some(observed!(value)),
            ResultView::GraphPreview(value) => self.profiles.preview = Some(observed!(value)),
            ResultView::GraphPrepared(value) => {
                self.profiles.diagnostic = None;
                self.profiles.prepared = Some(observed!(value));
            }
            _ => return Err("Unexpected profile observation result".into()),
        }
        Ok(())
    }
    pub fn queue_basis(&self) -> Option<&StateView> {
        self.queue_basis.as_deref()
    }
    pub fn queue_stale(&self) -> bool {
        self.queue_stale
    }
    pub fn queue_edit_pending(&self) -> bool {
        self.local_correction.is_some()
            || self.waiting_edit.is_some()
            || self
                .pending
                .as_ref()
                .is_some_and(|(_, task)| task.edits_foreground())
    }
    pub fn waiting_queue_edit(&self) -> Option<&Command> {
        match self.waiting_edit.as_ref() {
            Some(
                Task::Preflight { command, .. }
                | Task::Discovery { command, .. }
                | Task::ProfileBefore { command, .. },
            ) => Some(command),
            _ => None,
        }
    }
    pub fn queue_edit_available(&self) -> bool {
        self.control_edit_available()
            && !self.queue_stale
            && self
                .queue_basis
                .as_ref()
                .zip(self.state())
                .is_some_and(|(basis, current)| same_context(basis, current))
    }
    pub fn control_edit_available(&self) -> bool {
        self.connected
            && self.client.is_some()
            && !self.queue_edit_pending()
            && !self.closing
            && self.shutdown.is_none()
            && self.cleanup.is_none()
            && !self.host.launching()
            && !self.model().records().iter().any(|record| {
                !record.owner_changed
                    && !record.request.envelope().payload.is_query()
                    && (record.presentation == Presentation::Uncertain
                        || (record.presentation == Presentation::Accepted
                            && !is_transport_operation(&record.request.envelope().payload)
                            && record.request.lane() == Lane::Ordinary))
            })
    }
    pub fn capabilities(&self) -> Option<&CapabilitiesView> {
        self.capabilities.as_ref()
    }
    pub fn discovery(&self) -> Option<&DeviceQueryResultView> {
        self.discovery.as_deref()
    }
    pub fn inspection(&self) -> Option<&DeviceQueryResultView> {
        self.inspection.as_deref()
    }
    pub fn connected(&self) -> bool {
        self.connected
    }
    pub fn attached(&self) -> bool {
        self.client.is_some()
    }
    pub fn busy(&self) -> bool {
        self.local_correction.is_some()
            || self.pending.is_some()
            || self.waiting_edit.is_some()
            || self.cleanup.is_some()
            || self.host.launching()
            || self.shutdown.is_some()
    }
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref().or(self.observation_error.as_deref())
    }
    pub fn observed_at(&self) -> Option<Instant> {
        self.model()
            .latest_state()
            .map(|observed| observed.received_at)
    }
    pub fn host_status(&self) -> &HostStatus {
        &self.host.status
    }
    pub fn runtime(&self) -> Option<&str> {
        self.runtime.as_deref()
    }
    pub fn ready_to_close(&self) -> bool {
        self.closing
            && self.client.is_none()
            && self.cleanup.is_none()
            && self.local_correction.is_none()
            && !self.host.launching()
            && (!self.wait_for_child || !self.host.has_child())
    }
    pub fn receipts(&self) -> impl Iterator<Item = Receipt<'_>> {
        self.model().records().iter().map(record_view)
    }
    fn model(&self) -> &PresentationModel {
        self.client.as_ref().map_or_else(
            || {
                self.disconnected
                    .as_ref()
                    .expect("disconnected controller owns its presentation model")
            },
            DesktopClient::model,
        )
    }
    pub fn handle(&mut self, intent: UiIntent) {
        self.error = None;
        if let Err(error) = self.intent(intent) {
            self.error = Some(error);
        }
    }
    fn intent(&mut self, intent: UiIntent) -> Result<(), String> {
        if self.closing {
            return Err("Controller cleanup is in progress".into());
        }
        match intent {
            UiIntent::Attach { runtime } => self.attach(runtime),
            UiIntent::Launch {
                runtime,
                executable,
            } => {
                validate_runtime(&runtime)?;
                if self.client.is_some() || self.cleanup.is_some() {
                    return Err("Disconnect the current client before launching".into());
                }
                self.host_error_reported = false;
                self.host.launch(executable, runtime)
            }
            UiIntent::Disconnect => {
                self.disconnect(false);
                Ok(())
            }
            UiIntent::CloseController => {
                self.shutdown = None;
                self.wait_for_child = false;
                self.disconnect(true);
                Ok(())
            }
            UiIntent::Refresh => {
                self.refresh = true;
                self.next_poll = Instant::now();
                Ok(())
            }
            UiIntent::DiscardPreparedGraph => {
                self.invalidate_correction();
                self.profiles.prepared = None;
                self.draft_epoch = self
                    .draft_epoch
                    .checked_add(1)
                    .ok_or("Graph draft generation exhausted")?;
                Ok(())
            }
            UiIntent::InvalidateCorrection => {
                self.invalidate_correction();
                self.profiles.prepared = None;
                self.draft_epoch = self
                    .draft_epoch
                    .checked_add(1)
                    .ok_or("Draft generation exhausted")?;
                Ok(())
            }
            UiIntent::PreviewCorrection { basis, request } => {
                if !self.control_edit_available() {
                    return Err("A foreground action is waiting, pending, or uncertain".into());
                }
                self.invalidate_correction();
                let task = Task::LocalPreview {
                    basis,
                    request,
                    draft_epoch: self.draft_epoch,
                };
                if self.pending.is_some() {
                    self.local_status =
                        "Correction preview captured; waiting for foreground slot".into();
                    self.waiting_edit = Some(task);
                    Ok(())
                } else {
                    self.start_local(task)
                }
            }
            UiIntent::ReviewCorrection { owner, reference } => {
                let observed = self
                    .profiles
                    .record
                    .as_ref()
                    .ok_or("Get the actual imported profile before review")?;
                if self.state().and_then(|s| s.owner_id.as_ref()) != Some(&owner)
                    || observed.basis.owner_id.as_ref() != Some(&owner)
                    || observed.value.profile.id != reference.id
                    || observed.value.profile.revision != reference.revision
                    || observed.value.profile_sha256 != reference.profile_sha256
                    || observed.value.corrections.is_empty()
                {
                    return Err(
                        "Actual correction owner or saved facts changed; review again".into(),
                    );
                }
                self.correction_review = Some((
                    Arc::new(self.state().ok_or("Owner state unavailable")?.clone()),
                    reference,
                ));
                Ok(())
            }
            UiIntent::ReviewFirAsset { owner, asset } => {
                if self.state().and_then(|s| s.owner_id.as_ref()) != Some(&owner)
                    || self.fir_asset() != Some(&asset)
                {
                    return Err("Actual verified FIR owner or asset changed; review again".into());
                }
                self.fir_review = Some((
                    Arc::new(self.state().ok_or("Owner state unavailable")?.clone()),
                    asset,
                ));
                Ok(())
            }
            UiIntent::ProfileAction { basis, command } => {
                validate_profile_command(&command)?;
                self.validate_correction_action(&basis, &command)?;
                if !self.control_edit_available() {
                    return Err("A foreground action is waiting, pending, or uncertain".into());
                }
                if command.is_query() {
                    let task = Task::ProfileBefore {
                        basis: Some(basis),
                        command,
                        foreground: true,
                    };
                    if self.pending.is_some() {
                        self.waiting_edit = Some(task);
                        Ok(())
                    } else {
                        self.start_query(Command::State, task)
                    }
                } else {
                    self.validate_prepared_action(&basis, &command)?;
                    self.capture(basis, command)
                }
            }
            UiIntent::QueueEdit { basis, command } => {
                validate_queue(&command)?;
                if self.queue_edit_pending() {
                    return Err(
                        "A queue edit is already waiting or pending; inspect its progress".into(),
                    );
                }
                if !self.queue_edit_available() {
                    return Err("Observe a fresh published queue context before editing".into());
                }
                self.capture(basis, command)
            }
            UiIntent::ControlEdit { basis, command } => {
                if !matches!(
                    command,
                    Command::OutputSelect { .. }
                        | Command::OutputConsent { .. }
                        | Command::VolumeCaps { .. }
                        | Command::VolumeSet { .. }
                        | Command::FixedArm { .. }
                        | Command::Pause
                        | Command::Resume
                        | Command::Seek { .. }
                        | Command::Mute { muted: false }
                ) {
                    return Err(
                        "Only output and transport commands belong to a control edit".into(),
                    );
                }
                if !self.control_edit_available() {
                    return Err("A foreground edit is waiting, pending, or uncertain; observe and reconcile first".into());
                }
                if let Command::Seek { position_frames } = command {
                    let bounds = self.seek_bounds();
                    if !basis.seekable || bounds.is_none_or(|(_, total)| position_frames > total) {
                        return Err(
                            "Observe a seekable active entry and its source-frame bounds first"
                                .into(),
                        );
                    }
                }
                self.capture(basis, command)
            }
            UiIntent::StartLocal { basis, takeover } => {
                if !self.queue_edit_available() {
                    return Err(
                        "Observe a fresh selected queue and reconcile pending playback first"
                            .into(),
                    );
                }
                if self
                    .queue
                    .as_ref()
                    .and_then(|q| q.selected.as_ref())
                    .is_none()
                {
                    return Err("Select a local queue entry first".into());
                }
                let device_id = basis
                    .output
                    .as_ref()
                    .ok_or("Select an output first")?
                    .device_id
                    .clone();
                self.capture(
                    basis,
                    Command::Start {
                        source: earph_core::control::SourceKind::LocalFile,
                        device_id,
                        takeover,
                        seekable: true,
                    },
                )
            }
            UiIntent::Discover { backend, endpoint } => {
                if !self.control_edit_available() {
                    return Err("A foreground action is waiting, pending, or uncertain".into());
                }
                if !self
                    .capabilities
                    .as_ref()
                    .is_some_and(|c| c.devices.backends.contains(&backend))
                {
                    return Err("This owner does not advertise discovery for that backend".into());
                }
                let command = match endpoint {
                    Some(endpoint) => Command::DevicesInspect { backend, endpoint },
                    None => Command::DevicesList { backend },
                };
                let task = Task::Discovery {
                    command: command.clone(),
                    owner: self
                        .state()
                        .and_then(|s| s.owner_id.clone())
                        .ok_or("Observe an owner before discovery")?,
                };
                if self.pending.is_some() {
                    self.waiting_edit = Some(task);
                    Ok(())
                } else {
                    self.start_query(command, task)
                }
            }
            UiIntent::Mute => self.safety_command(Command::Mute { muted: true }),
            UiIntent::Stop => self.safety_command(Command::Stop),
            UiIntent::Reconcile(id) => self.reconcile(&id),
            UiIntent::ShutdownAndQuit => {
                self.invalidate_correction();
                self.waiting_edit = None;
                if self.shutdown.is_some() {
                    return Err("Shutdown already has an outstanding receipt".into());
                }
                if self.model().records().iter().any(|record| {
                    matches!(record.request.envelope().payload, Command::OwnerShutdown)
                        && !record.owner_changed
                        && matches!(
                            record.presentation,
                            Presentation::Accepted | Presentation::Uncertain
                        )
                }) {
                    return Err("Reconcile the retained shutdown request before submitting another shutdown".into());
                }
                let state = self.state().ok_or("Observe an owner before shutdown")?;
                let wait_for_child = matches!(&self.host.status, HostStatus::OwnedRunning { owner_id, .. } if state.owner_id.as_ref() == Some(owner_id));
                let request = Request::mutation(request_id()?, state, Command::OwnerShutdown)
                    .map_err(|e| format!("{e:?}"))?;
                let id = self.submit(request)?;
                self.shutdown = Some(id);
                self.wait_for_child = wait_for_child;
                self.pending = None;
                self.refresh = false;
                Ok(())
            }
        }
    }
    fn capture(&mut self, basis: Arc<StateView>, command: Command) -> Result<(), String> {
        let session = if matches!(command, Command::Start { .. }) {
            Some(SessionId::new(request_id()?.as_str()).map_err(|e| e.to_string())?)
        } else {
            None
        };
        let task = Task::Preflight {
            basis,
            command,
            session,
            draft_epoch: self.draft_epoch,
        };
        if self.pending.is_some() {
            self.waiting_edit = Some(task);
            Ok(())
        } else {
            self.start_query(Command::State, task)
        }
    }
    pub fn seek_bounds(&self) -> Option<(&earph_core::ids::QueueEntryId, u64)> {
        let state = self.state()?;
        if !state.seekable || self.queue_stale {
            return None;
        }
        let active = state.active_entry_id.as_ref()?;
        let entry = self
            .queue
            .as_ref()?
            .entries
            .iter()
            .find(|entry| &entry.id == active)?;
        match entry.status {
            earph_core::queue::PreparationStatus::Ready { total_frames, .. } => {
                Some((active, total_frames))
            }
            _ => None,
        }
    }
    fn cancel_unsent(&mut self) {
        self.invalidate_correction();
        self.waiting_edit = None;
        if let Some((_, task)) = self.pending.as_mut()
            && matches!(task, Task::Preflight { .. } | Task::ProfileBefore { .. })
        {
            *task = Task::Before;
        }
    }
    fn attach(&mut self, runtime: String) -> Result<(), String> {
        validate_runtime(&runtime)?;
        if self.cleanup.is_some() {
            return Err("Client workers are still retiring".into());
        }
        if self.client.is_some() {
            if self.runtime.as_deref() == Some(&runtime) {
                self.refresh = true;
                return Ok(());
            }
            return Err("Disconnect before attaching another runtime".into());
        }
        let model = self
            .disconnected
            .take()
            .expect("disconnected controller owns its presentation model");
        match DesktopClient::resume(runtime.clone(), model) {
            Ok(client) => self.client = Some(client),
            Err((error, model)) => {
                self.disconnected = Some(*model);
                return Err(format!("{error:?}"));
            }
        }
        self.runtime = Some(runtime);
        self.connected = false;
        self.capabilities = None;
        self.queue_stale = true;
        self.refresh = true;
        self.next_poll = Instant::now();
        Ok(())
    }
    fn submit(&mut self, request: Request) -> Result<RequestId, String> {
        let id = request.envelope().request_id.clone();
        match self
            .client
            .as_mut()
            .ok_or("Attach to an owner first")?
            .submit(request)
            .map_err(|e| format!("Client admission: {e:?}"))?
        {
            crate::Submission::Admitted => Ok(id),
            crate::Submission::Coalesced(existing) => Ok(existing),
        }
    }
    fn start_query(&mut self, command: Command, task: Task) -> Result<(), String> {
        if let Task::Discovery { owner, .. } = &task
            && self.state().and_then(|s| s.owner_id.as_ref()) != Some(owner)
        {
            return Err("Discovery owner changed before submission; refresh explicitly".into());
        }
        let request = if matches!(task, Task::Discovery { .. }) {
            Request::device_query(
                request_id()?,
                self.state().ok_or("Observe an owner before discovery")?,
                command,
            )
        } else {
            Request::query(
                request_id()?,
                ZoneId::new("main").map_err(|e| e.to_string())?,
                command,
            )
        }
        .map_err(|e| format!("{e:?}"))?;
        let id = self.submit(request)?;
        self.pending = Some((id, task));
        Ok(())
    }
    fn safety_command(&mut self, command: Command) -> Result<(), String> {
        self.cancel_unsent();
        if self.safety.len() == 2 {
            return Err("Safety requests are pending".into());
        }
        let state = self
            .state()
            .ok_or("Observe an owner before sending safety commands")?;
        let request =
            Request::mutation(request_id()?, state, command).map_err(|e| format!("{e:?}"))?;
        let id = self.submit(request)?;
        self.safety.push(id);
        Ok(())
    }
    fn reconcile(&mut self, id: &RequestId) -> Result<(), String> {
        if let Some(record) = self.model().record(id)
            && matches!(
                record.request.envelope().payload,
                Command::DevicesList { .. } | Command::DevicesInspect { .. }
            )
            && record.request.envelope().owner_id.as_ref()
                != self.state().and_then(|s| s.owner_id.as_ref())
        {
            return Err("Discovery target owner changed or is unobserved; refresh explicitly after attachment".into());
        }
        let restore_safety = self
            .client
            .as_ref()
            .and_then(|client| client.model().record(id))
            .is_some_and(|record| {
                record.request.lane() == Lane::Safety
                    && !matches!(record.request.envelope().payload, Command::OwnerShutdown)
                    && !is_transport_operation(&record.request.envelope().payload)
            })
            && !self.safety.contains(id);
        if restore_safety && self.safety.len() == 2 {
            return Err(
                "Safety requests are pending; reconcile after a tracking slot retires".into(),
            );
        }
        self.client
            .as_mut()
            .ok_or("Attach before reconciliation")?
            .reconcile(id)
            .map_err(|e| format!("Cannot reconcile: {e:?}"))?;
        if restore_safety {
            self.safety.push(id.clone());
        }
        Ok(())
    }
    fn disconnect(&mut self, close: bool) {
        self.invalidate_correction();
        self.closing |= close;
        self.shutdown = None;
        if !close {
            self.wait_for_child = false;
        }
        self.pending = None;
        self.waiting_edit = None;
        self.refresh = false;
        self.connected = false;
        self.queue_stale = true;
        self.safety.clear();
        self.discovery = None;
        self.inspection = None;
        self.profiles = ProfileViews::default();
        if let Some(client) = self.client.take() {
            let (model, shutdown) = client.disconnect();
            self.disconnected = Some(model);
            self.cleanup = Some(thread::spawn(move || {
                shutdown
                    .join()
                    .map_err(|_| "Client worker panicked during cleanup".into())
            }));
        }
    }
    pub fn tick(&mut self, now: Instant) {
        self.poll_local();
        if let Some(runtime) = self.host.poll()
            && !self.closing
            && let Err(e) = self.attach(runtime)
        {
            self.error = Some(e);
        }
        if let HostStatus::Failed { detail } = &self.host.status
            && !self.host_error_reported
        {
            self.error = Some(detail.clone());
            self.host_error_reported = true;
        }
        if self.cleanup.as_ref().is_some_and(|j| j.is_finished()) {
            match self.cleanup.take().unwrap().join() {
                Ok(Ok(())) => {}
                Ok(Err(e)) => self.error = Some(e),
                Err(_) => self.error = Some("Cleanup worker panicked".into()),
            }
        }
        let previous_owner = self.state().and_then(|state| state.owner_id.clone());
        if let Some(client) = self.client.as_mut() {
            client.poll();
        }
        if let Some(observed) = self.model().latest_state().map(|o| o.state.clone()) {
            if previous_owner != observed.owner_id {
                self.invalidate_correction();
                self.capabilities = None;
                self.discovery = None;
                self.inspection = None;
                self.profiles = ProfileViews::default();
                if self
                    .pending
                    .as_ref()
                    .is_some_and(|(_, task)| matches!(task, Task::Discovery { .. }))
                {
                    self.pending = None;
                    self.error =
                        Some("Discovery owner changed; its sent receipt remains in history".into());
                }
                if self.waiting_edit.is_some()
                    || self.pending.as_ref().is_some_and(|(_, t)| {
                        matches!(t, Task::Preflight { .. } | Task::ProfileBefore { .. })
                    })
                {
                    self.cancel_unsent();
                    self.error = Some(
                        "Owner changed; waiting queue edit cancelled before submission".into(),
                    );
                }
            }
            if self
                .queue_basis
                .as_ref()
                .is_some_and(|basis| !same_context(basis, &observed))
            {
                self.queue_stale = true;
            }
        }
        self.observe_discovery();
        if let Some(id) = self.shutdown.clone() {
            match self.outcome(&id) {
                Some((Presentation::Applied, _)) => {
                    self.disconnect(true);
                }
                Some((p, _)) if !matches!(p, Presentation::Queued | Presentation::Accepted) => {
                    self.shutdown = None;
                    self.wait_for_child = false;
                    self.refresh = true;
                    self.queue_stale = true;
                    if p == Presentation::Uncertain {
                        self.connected = false;
                    }
                    self.error = Some(format!(
                        "Shutdown {p:?}; inspect its receipt before closing"
                    ));
                }
                _ => {}
            }
            if self.shutdown.is_some() || self.closing {
                self.poll_accepted(now);
                return;
            }
        }
        if self.closing {
            return;
        }
        // Muted transport may stay Accepted after readiness. Release the send workflow,
        // while the same bounded model record retains the exact operation identity.
        if let Some((id, Task::Mutation { .. })) = self.pending.as_ref()
            && self
                .outcome(id)
                .is_some_and(|(p, _)| p == Presentation::Accepted)
            && self
                .model()
                .record(id)
                .is_some_and(|r| is_transport_operation(&r.request.envelope().payload))
        {
            self.pending = None;
            self.refresh = true;
            self.queue_stale = true;
        }
        let completed = self
            .pending
            .as_ref()
            .and_then(|(id, _)| self.outcome(id))
            .map(|(p, r)| (p, r.cloned()));
        if let Some((presentation, reply)) = completed
            && !matches!(presentation, Presentation::Accepted | Presentation::Queued)
        {
            let (id, task) = self.pending.take().unwrap();
            let command = self
                .model()
                .record(&id)
                .unwrap()
                .request
                .envelope()
                .payload
                .clone();
            let background_observation = matches!(
                task,
                Task::Before | Task::Capabilities | Task::Page(_) | Task::After
            );
            let holds_read = matches!(task, Task::ProfileResult(_));
            if let Some(Ok(Reply::Receipt(receipt))) = &reply
                && let Some(ResultView::GraphDiagnostic(diagnostic)) = &receipt.result
            {
                self.profiles.diagnostic = Some((id.clone(), diagnostic.clone()));
            }
            if presentation == Presentation::Applied {
                if let Some(Ok(Reply::Receipt(receipt))) = reply {
                    match self.complete(task, receipt, now, &id, &command) {
                        Ok(()) => {
                            if !holds_read && let Some(client) = self.client.as_mut() {
                                client.retire_observation(&id);
                            }
                        }
                        Err(e) => {
                            self.waiting_edit = None;
                            if background_observation {
                                self.observation_error = Some(e);
                            } else {
                                self.error = Some(e);
                            }
                            self.queue_stale = true;
                            self.next_poll = now + Duration::from_millis(500);
                        }
                    }
                }
            } else {
                self.waiting_edit = None;
                self.error = Some(format!(
                    "Request {presentation:?}; inspect its retained receipt"
                ));
                if presentation == Presentation::Uncertain {
                    self.connected = false;
                }
                self.next_poll = now + Duration::from_millis(500);
            }
        }
        let completed_safety: Vec<_> = self
            .safety
            .iter()
            .filter(|id| {
                self.outcome(id)
                    .is_some_and(|(p, _)| p != Presentation::Queued && p != Presentation::Accepted)
            })
            .cloned()
            .collect();
        for id in completed_safety {
            self.safety.retain(|saved| saved != &id);
            self.refresh = true;
        }
        self.poll_accepted(now);
        if self.client.is_some() && self.pending.is_none() && self.local_correction.is_none() {
            if let Some(task) = self.waiting_edit.take() {
                if matches!(task, Task::LocalPreview { .. }) {
                    if let Err(error) = self.start_local(task) {
                        self.error = Some(error);
                    }
                    return;
                }
                let command = match &task {
                    Task::Discovery { command, .. } => command.clone(),
                    _ => Command::State,
                };
                if let Err(error) = self.start_query(command, task) {
                    self.error = Some(error);
                }
            } else if self.refresh || now >= self.next_poll {
                self.refresh = false;
                if let Err(e) = self.start_query(Command::State, Task::Before) {
                    self.error = Some(e);
                }
                self.next_poll = now + Duration::from_millis(500);
            }
        }
    }
    fn observe_discovery(&mut self) {
        if !self.connected || self.client.is_none() {
            return;
        }
        let results: Vec<_> = self
            .model()
            .records()
            .iter()
            .filter_map(|record| {
                if let Some(Ok(Reply::Receipt(receipt))) = &record.outcome
                    && let Some(ResultView::Devices(result)) = &receipt.result
                    && self.state().and_then(|s| s.owner_id.as_ref()) == Some(&result.owner_id)
                    && record.request.envelope().owner_id.as_ref() == Some(&result.owner_id)
                {
                    Some((
                        matches!(
                            record.request.envelope().payload,
                            Command::DevicesList { .. }
                        ),
                        result.clone(),
                    ))
                } else {
                    None
                }
            })
            .collect();
        // Retain the actual shared result, including failures. Archived exact replay cannot
        // replace a more recent observation or populate another owner's inventory.
        for (list, result) in results {
            let slot = if list {
                &mut self.discovery
            } else {
                &mut self.inspection
            };
            if slot
                .as_ref()
                .is_none_or(|old| old.completed_monotonic_ns < result.completed_monotonic_ns)
            {
                *slot = Some(result);
            }
        }
    }
    fn outcome(
        &self,
        id: &RequestId,
    ) -> Option<(Presentation, Option<&Result<Reply, DeliveryFailure>>)> {
        let record = self.client.as_ref()?.model().record(id)?;
        (!record.in_flight).then_some((record.presentation, record.outcome.as_ref()))
    }
    fn poll_accepted(&mut self, now: Instant) {
        if now < self.next_replay || self.client.is_none() {
            return;
        }
        // Fixed slots keep changing ordinary request identities from starving safety work.
        let operations: Vec<_> = self
            .model()
            .records()
            .iter()
            .filter(|r| {
                !r.owner_changed
                    && !r.in_flight
                    && r.presentation == Presentation::Accepted
                    && !self
                        .pending
                        .as_ref()
                        .is_some_and(|(id, _)| id == &r.request.envelope().request_id)
                    && !self.safety.contains(&r.request.envelope().request_id)
                    && (is_transport_operation(&r.request.envelope().payload)
                        || (matches!(
                            r.request.envelope().payload,
                            Command::DevicesList { .. } | Command::DevicesInspect { .. }
                        ) && self.connected
                            && r.request.envelope().owner_id.as_ref()
                                == self.state().and_then(|s| s.owner_id.as_ref())))
            })
            .map(|r| r.request.envelope().request_id.clone())
            .collect();
        let operation = if operations.is_empty() {
            None
        } else {
            Some(operations[self.operation_cursor % operations.len()].clone())
        };
        let slots = [
            self.pending.as_ref().map(|(id, _)| id.clone()),
            operation,
            self.safety.first().cloned(),
            self.safety.get(1).cloned(),
            self.shutdown.clone(),
        ];
        for offset in 0..slots.len() {
            let slot = (self.replay_cursor + offset) % slots.len();
            let Some(id) = &slots[slot] else {
                continue;
            };
            if !self
                .outcome(id)
                .is_some_and(|(presentation, _)| presentation == Presentation::Accepted)
            {
                continue;
            }
            self.replay_cursor = (slot + 1) % slots.len();
            if slot == 1 {
                self.operation_cursor = self.operation_cursor.wrapping_add(1);
            }
            self.next_replay = now + Duration::from_millis(100);
            if let Err(error) = self.client.as_mut().unwrap().reconcile(id) {
                if !matches!(error, SubmitError::Dispatch(DispatchError::Full(_))) {
                    self.safety.retain(|saved| saved != id);
                    if self.pending.as_ref().is_some_and(|(saved, _)| saved == id) {
                        self.pending = None;
                        self.waiting_edit = None;
                        self.queue_stale = true;
                    }
                    if self.shutdown.as_ref() == Some(id) {
                        self.shutdown = None;
                        self.wait_for_child = false;
                    }
                    self.refresh = true;
                    self.error = Some(format!("Receipt polling stopped: {error:?}"));
                } else {
                    self.error = Some(format!("Receipt polling delayed: {error:?}"));
                }
            }
            break;
        }
    }
    fn complete(
        &mut self,
        task: Task,
        receipt: crate::ReceiptView,
        now: Instant,
        request_id: &RequestId,
        command: &Command,
    ) -> Result<(), String> {
        match task {
            Task::LocalPreview { .. } => {
                Err("Local correction preview cannot consume an owner receipt".into())
            }
            Task::Before => {
                let Some(ResultView::State(state)) = receipt.result else {
                    return Err("State result unavailable".into());
                };
                self.connected = true;
                self.basis = Some(state);
                self.candidate = None;
                if self.capabilities.is_none() {
                    self.start_query(Command::Capabilities, Task::Capabilities)
                } else {
                    self.start_query(
                        Command::QueueGet {
                            offset: 0,
                            limit: 64,
                        },
                        Task::Page(0),
                    )
                }
            }
            Task::Capabilities => {
                let Some(ResultView::Capabilities(caps)) = receipt.result else {
                    return Err("Capabilities unavailable".into());
                };
                self.capabilities = Some(caps);
                self.start_query(
                    Command::QueueGet {
                        offset: 0,
                        limit: 64,
                    },
                    Task::Page(0),
                )
            }
            Task::Page(offset) => {
                let Some(ResultView::Queue(page)) = receipt.result else {
                    return Err("Queue result unavailable".into());
                };
                let basis = self.basis.as_ref().ok_or("Missing queue read context")?;
                if receipt.revision != basis.revision
                    || page.offset != offset
                    || page.total > 256
                    || page.entries.len() > 64
                    || offset + page.entries.len() > page.total
                    || (page.entries.is_empty() && offset < page.total)
                {
                    return Err("Queue changed or returned an invalid page; refresh again".into());
                }
                let candidate = self.candidate.get_or_insert_with(|| QueueView {
                    entries: Vec::new(),
                    selected: page.selected.clone(),
                    offset: 0,
                    total: page.total,
                });
                if candidate.total != page.total
                    || candidate.selected != page.selected
                    || candidate.entries.len() != offset
                {
                    return Err("Queue changed between pages".into());
                }
                for entry in page.entries {
                    if candidate.entries.iter().any(|old| old.id == entry.id) {
                        return Err("Queue repeated an entry identity".into());
                    }
                    candidate.entries.push(entry);
                }
                let next = candidate.entries.len();
                if next == candidate.total {
                    self.start_query(Command::State, Task::After)
                } else {
                    self.start_query(
                        Command::QueueGet {
                            offset: next,
                            limit: 64,
                        },
                        Task::Page(next),
                    )
                }
            }
            Task::After => {
                let Some(ResultView::State(state)) = receipt.result else {
                    return Err("Queue fence State unavailable".into());
                };
                if !self
                    .basis
                    .as_ref()
                    .is_some_and(|basis| same_context(basis, &state))
                {
                    return Err(
                        "Owner or queue changed during read; previous queue stays stale".into(),
                    );
                }
                self.queue = self.candidate.take();
                self.queue_basis = Some(state);
                self.basis = None;
                self.queue_stale = false;
                self.observation_error = None;
                self.connected = true;
                self.next_poll = now + Duration::from_millis(500);
                Ok(())
            }
            Task::Preflight {
                basis,
                command,
                session,
                draft_epoch,
            } => {
                let Some(ResultView::State(state)) = receipt.result else {
                    return Err("Edit preflight State unavailable".into());
                };
                if !same_context(&basis, &state) {
                    self.refresh = true;
                    return Err(
                        "Owner, session, or revision changed; review the draft and submit again"
                            .into(),
                    );
                }
                if is_profile_mutation(&command) && draft_epoch != self.draft_epoch {
                    return Err("Draft changed before submission; review and submit again".into());
                }
                if self.model().records().iter().any(|r| {
                    !r.owner_changed
                        && !r.request.envelope().payload.is_query()
                        && r.presentation == Presentation::Uncertain
                }) {
                    return Err("A sent action became uncertain; reconcile its exact request before another edit".into());
                }
                if is_profile_mutation(&command) {
                    self.validate_correction_action(&state, &command)?;
                    self.validate_prepared_action(&state, &command)?;
                }
                let request = match session {
                    Some(session) => Request::start(super::request_id()?, &state, session, command),
                    None => Request::mutation(super::request_id()?, &state, command),
                }
                .map_err(|e| format!("{e:?}"))?;
                let id = self.submit(request)?;
                self.pending = Some((id, Task::Mutation { draft_epoch }));
                Ok(())
            }
            Task::Mutation { draft_epoch } => {
                if is_profile_mutation(command) {
                    self.adopt_profile_mutation(request_id, command, &receipt, draft_epoch)?;
                    self.refresh = true;
                    self.queue_stale = true;
                    let observe = if matches!(
                        command,
                        Command::ProfilesCreate { .. }
                            | Command::ProfilesUpdate { .. }
                            | Command::ProfilesImport { .. }
                            | Command::FirImport { .. }
                    ) {
                        Command::ProfilesList {
                            offset: 0,
                            limit: 16,
                        }
                    } else {
                        Command::GraphInspect {}
                    };
                    return self.start_query(
                        Command::State,
                        Task::ProfileBefore {
                            basis: None,
                            command: observe,
                            foreground: false,
                        },
                    );
                }
                self.refresh = true;
                self.queue_stale = true;
                Ok(())
            }
            Task::Discovery { .. } => Ok(()),
            Task::ProfileBefore {
                basis,
                command,
                foreground,
            } => {
                let Some(ResultView::State(state)) = receipt.result else {
                    return Err("Profile preflight State unavailable".into());
                };
                if basis.as_ref().is_some_and(|basis| {
                    basis.owner_id != state.owner_id || basis.zone_id != state.zone_id
                }) {
                    return Err("Profile owner or zone changed; review and submit again".into());
                }
                let read = ProfileRead {
                    basis: state,
                    command: command.clone(),
                    foreground,
                    pages: None,
                    ids: Vec::new(),
                };
                self.start_query(command, Task::ProfileResult(read))
            }
            Task::ProfileResult(mut read) => {
                if receipt.revision != read.basis.revision {
                    return Err(
                        "Profile query revision differs from its captured observation".into(),
                    );
                }
                let result = receipt.result.ok_or("Profile query result unavailable")?;
                validate_profile_result(&read.command, &result)?;
                read.ids.push(request_id.clone());
                let result =
                    if let (Command::ProfilesList { offset, limit }, ResultView::Profiles(page)) =
                        (&read.command, &result)
                    {
                        if page.total
                            > self
                                .capabilities
                                .as_ref()
                                .map_or(32, |c| c.profiles.max_profiles)
                            || page.entries.len() > *limit
                            || offset + page.entries.len() > page.total
                            || (page.entries.is_empty() && *offset < page.total)
                            || page.next_offset.is_some_and(|next| {
                                next != offset + page.entries.len() || next >= page.total
                            })
                            || (page.next_offset.is_none()
                                && offset + page.entries.len() != page.total)
                        {
                            return Err("Profile catalog returned an invalid page".into());
                        }
                        if let Some(all) = read.pages.as_mut() {
                            if all.total != page.total
                                || all.operation != page.operation
                                || all.entries.len() != *offset
                            {
                                return Err("Profile catalog changed between pages".into());
                            }
                            for entry in &page.entries {
                                if all.entries.iter().any(|old| old.id == entry.id) {
                                    return Err("Profile catalog repeated an ID".into());
                                }
                                all.entries.push(entry.clone());
                            }
                            all.next_offset = page.next_offset;
                        } else {
                            read.pages = Some((**page).clone());
                        }
                        if let Some(offset) = page.next_offset {
                            let command = Command::ProfilesList {
                                offset,
                                limit: *limit,
                            };
                            read.command = command.clone();
                            return self.start_query(command, Task::ProfileResult(read));
                        }
                        read.command = Command::ProfilesList {
                            offset: 0,
                            limit: *limit,
                        };
                        ResultView::Profiles(Arc::new(read.pages.take().unwrap()))
                    } else {
                        result
                    };
                self.start_query(Command::State, Task::ProfileAfter { read, result })
            }
            Task::ProfileAfter { read, result } => {
                let Some(ResultView::State(state)) = receipt.result else {
                    return Err("Profile result fence State unavailable".into());
                };
                if !same_context(&read.basis, &state) {
                    return Err("Profile query owner or revision changed before adoption".into());
                }
                if read.ids.is_empty() {
                    return Err("Profile query identity unavailable".into());
                }
                self.adopt_profile_result(read.basis, read.ids.clone(), read.command, result)?;
                if let Some(client) = self.client.as_mut() {
                    for id in read.ids {
                        client.retire_observation(&id);
                    }
                }
                Ok(())
            }
        }
    }
}
impl Drop for GuiController {
    fn drop(&mut self) {
        self.disconnect(false);
        if let Some(join) = self.cleanup.take() {
            let _ = join.join();
        }
        self.local_correction.take();
    }
}
fn validate_runtime(runtime: &str) -> Result<(), String> {
    let path = Path::new(runtime);
    if !path.is_absolute() || path.components().any(|p| p == Component::ParentDir) {
        return Err("Runtime must be an absolute path without parent traversal".into());
    }
    if runtime.len() > 60 {
        return Err("Runtime exceeds the owner's 60-byte path limit".into());
    }
    Ok(())
}
fn validate_queue(command: &Command) -> Result<(), String> {
    match command {
        Command::QueueAppend { path } | Command::QueueAppendDirectory { path } => {
            if path.is_empty() || path.len() > 1024 || path.contains('\0') {
                return Err("Queue path must contain 1–1,024 UTF-8 bytes without NUL".into());
            }
            Ok(())
        }
        Command::QueueSelect { .. } | Command::QueueMove { .. } | Command::QueueRemove { .. } => {
            Ok(())
        }
        _ => Err("Only queue commands belong to a queue edit".into()),
    }
}
fn is_transport_operation(command: &Command) -> bool {
    matches!(
        command,
        Command::Start { .. } | Command::Resume | Command::Pause | Command::Seek { .. }
    )
}
fn correction_unsent(task: &Task) -> bool {
    matches!(
        task,
        Task::LocalPreview { .. }
            | Task::Preflight {
                command: Command::ProfilesImport { .. } | Command::FirImport { .. },
                ..
            }
    )
}
fn is_profile_mutation(command: &Command) -> bool {
    matches!(
        command,
        Command::ProfilesCreate { .. }
            | Command::ProfilesImport { .. }
            | Command::FirImport { .. }
            | Command::ProfilesUpdate { .. }
            | Command::ProfilesPrepare { .. }
            | Command::ProfilesApply { .. }
            | Command::GraphValidate { .. }
            | Command::GraphPrepare { .. }
            | Command::GraphCommit { .. }
    )
}
fn validate_profile_command(command: &Command) -> Result<(), String> {
    if matches!(command, Command::ProfilesList { offset, .. } if *offset != 0) {
        return Err("Refresh the profile catalog from its first page".into());
    }
    if matches!(
        command,
        Command::ProfilesList { .. }
            | Command::ProfilesGet { .. }
            | Command::ProfilesResolve { .. }
            | Command::GraphInspect { .. }
    ) || is_profile_mutation(command)
    {
        Ok(())
    } else {
        Err("Only released profile and DSP commands belong to this workflow".into())
    }
}
fn validate_profile_result(command: &Command, result: &ResultView) -> Result<(), String> {
    let valid = match (command, result) {
        (Command::ProfilesList { .. }, ResultView::Profiles(_)) => true,
        (Command::ProfilesGet { id }, ResultView::Profile(record)) => &record.profile.id == id,
        (Command::ProfilesCreate { id, draft, graph }, ResultView::ProfileSaved(record)) => {
            &record.profile.id == id
                && record.profile.revision == 1
                && saved_matches_draft(record, draft, graph)
        }
        (
            Command::ProfilesImport {
                id,
                expected_profile_revision,
                draft,
                input,
            },
            ResultView::ProfileSaved(record),
        ) => {
            &record.profile.id == id
                && expected_profile_revision.unwrap_or(0).checked_add(1)
                    == Some(record.profile.revision)
                && saved_profile_policy_matches(record, draft)
                && imported_matches_input(record, input)
        }
        (Command::FirImport { purpose, .. }, ResultView::FirAsset(asset)) => {
            asset.purpose == *purpose && asset.validate().is_ok()
        }
        (
            Command::ProfilesUpdate {
                id,
                expected_profile_revision,
                draft,
                graph,
            },
            ResultView::ProfileSaved(record),
        ) => {
            &record.profile.id == id
                && expected_profile_revision.checked_add(1) == Some(record.profile.revision)
                && saved_matches_draft(record, draft, graph)
        }
        (
            Command::ProfilesResolve {
                manual_profile_id, ..
            },
            ResultView::ProfileResolution(resolution),
        ) => match resolution {
            ProfileResolutionView::Selected { profile, reason } => match manual_profile_id {
                Some(id) => {
                    id == &profile.id && *reason == crate::ProfileSelectionReasonView::Manual
                }
                None => *reason == crate::ProfileSelectionReasonView::ExactDevice,
            },
            ProfileResolutionView::SelectionRequired { .. } => true,
        },
        (Command::GraphInspect { .. }, ResultView::GraphInspection(_)) => true,
        (Command::GraphValidate { .. }, ResultView::GraphPreview(preview)) => {
            preview.profile_application.is_none()
        }
        (Command::GraphPrepare { .. }, ResultView::GraphPrepared(prepared)) => {
            prepared.preview.profile_application.is_none()
        }
        (
            Command::ProfilesPrepare {
                id,
                expected_profile_revision,
                profile_sha256,
            },
            ResultView::GraphPrepared(prepared),
        ) => prepared
            .preview
            .profile_application
            .as_ref()
            .is_some_and(|binding| {
                &binding.profile.id == id
                    && binding.profile.revision == *expected_profile_revision
                    && binding.profile.profile_sha256 == *profile_sha256
            }),
        _ => false,
    };
    if valid {
        Ok(())
    } else {
        Err("Profile/DSP result kind or target does not match the captured command".into())
    }
}
fn prepared_matches_state(prepared: &GraphPreparedView, state: &StateView) -> bool {
    preview_matches_state(&prepared.preview, state)
        && state.monotonic_ns < prepared.expires_monotonic_ns
}
fn saved_matches_draft(
    record: &ProfileRecordView,
    draft: &earph_core::profile_control::ProfileDraft,
    graph: &earph_core::graph_control::GraphIntent,
) -> bool {
    record.graph.as_ref() == graph
        && record.profile.corrections == draft.corrections
        && saved_profile_policy_matches(record, draft)
}
fn saved_profile_policy_matches(
    record: &ProfileRecordView,
    draft: &earph_core::profile_control::ProfileDraft,
) -> bool {
    let profile = &record.profile;
    profile.name == draft.name
        && profile.source_policy == draft.source_policy
        && profile.fidelity == draft.fidelity
        && profile.output.backend == draft.output.backend
        && profile.output.route == draft.output.route
        && profile.output.device_id == draft.output.device_id
        && profile.output.manual_setup == draft.output.manual_setup
        && profile.volume.mode == draft.volume.mode
        && profile.volume.software_cap_db
            == f64::from(draft.volume.software_cap_millidecibels) / 1000.0
        && profile.volume.hardware_cap_db
            == draft
                .volume
                .hardware_cap_millidecibels
                .map(|v| f64::from(v) / 1000.0)
        && profile.latency.mode == draft.latency.mode
        && profile.latency.max_resident_bytes == draft.latency.max_resident_bytes
        && profile.latency.max_preparation_bytes == draft.latency.max_preparation_bytes
        && profile.latency.max_latency_frames == draft.latency.max_latency_frames
}
fn imported_matches_input(
    record: &ProfileRecordView,
    input: &earph_core::profile_control::ProfileCorrectionInput,
) -> bool {
    use earph_core::profile_control::ProfileCorrectionInput;
    if record.corrections.len() != 1
        || record.profile.corrections.len() != 1
        || record.correction_preview.is_none()
        || record.graph.processing
            != (earph_core::graph::GraphProcessingRequest::Float64 {
                quantization: earph_core::graph::QuantizationPolicy::RoundTiesAwaySaturateNoDither,
            })
    {
        return false;
    }
    let object = &record.corrections[0];
    let digest: String = {
        use sha2::{Digest, Sha256};
        Sha256::digest(object.document_json.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    if object.object_sha256.as_str() != digest
        || record.profile.corrections[0].object_sha256 != object.object_sha256
        || record.profile.corrections[0].kind != object.kind
    {
        return false;
    }
    match input {
        ProfileCorrectionInput::TextReport { report_json, .. } => {
            let expected =
                earph_media::correction_text::read_correction_report(report_json.as_bytes());
            let actual = earph_media::correction_text::read_correction_report(
                object.document_json.as_bytes(),
            );
            match (expected, actual) {
                (Ok(expected), Ok(actual)) => {
                    object.kind == earph_core::profiles::ProfileCorrectionKind::TextReport
                        && expected == actual
                        && record.graph.graph.as_ref() == &actual.graph
                        && correction_preview_matches(record, actual.context, actual.preamp_db)
                }
                _ => false,
            }
        }
        ProfileCorrectionInput::Fir {
            purpose,
            context,
            metadata_json,
            preamp_db,
            ..
        } => {
            let actual = earph_media::read_fir_manifest(object.document_json.as_bytes());
            actual.is_ok_and(|manifest| {
                object.kind == earph_core::profiles::ProfileCorrectionKind::FirManifest
                    && manifest.asset.purpose == *purpose
                    && manifest.context.sample_rate_hz == context.sample_rate_hz
                    && manifest.context.channels == context.channels
                    && manifest.context.quantum_frames == context.quantum_frames
                    && manifest.preamp_db == *preamp_db
                    && correction_preview_matches(
                        record,
                        manifest.context,
                        manifest.preamp_db.get(),
                    )
                    && serde_json::from_str::<earph_media::correction_text::ImportMetadata>(
                        metadata_json,
                    )
                    .is_ok_and(|metadata| metadata == manifest.metadata)
                    && manifest
                        .graph()
                        .is_ok_and(|graph| record.graph.graph.as_ref() == &graph)
            })
        }
    }
}
fn correction_preview_matches(
    record: &ProfileRecordView,
    context: earph_media::correction_text::ImportContext,
    preamp: f64,
) -> bool {
    record.correction_preview.as_ref().is_some_and(|preview| {
        preview.context.sample_rate_hz == context.sample_rate_hz
            && preview.context.channels == context.channels
            && preview.context.quantum_frames == context.quantum_frames
            && preview.preamp_db.get() == preamp
    })
}
fn preview_matches_state(preview: &GraphPreviewView, state: &StateView) -> bool {
    let binding = &preview.binding;
    state.owner_id.as_ref() == Some(&binding.owner_id)
        && state.session_id.as_ref() == Some(&binding.session_id)
        && state.revision == binding.expected_revision
        && state.device_epoch == binding.device_epoch
        && state.epochs == binding.epochs
        && state
            .playback
            .as_ref()
            .is_some_and(|p| p.format == preview.source_format)
        && state
            .output
            .as_ref()
            .is_some_and(|o| o.period_frames as usize == preview.quantum_frames)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::OwnerClient;

    fn correction_state() -> Arc<StateView> {
        let session = earph_core::session::Session::new_owner(
            ZoneId::new("main").unwrap(),
            OwnerId::new("owned-correction-context").unwrap(),
            earph_core::control::ControlLimits::default(),
        );
        Arc::new(serde_json::from_value(serde_json::to_value(session.snapshot()).unwrap()).unwrap())
    }
    #[test]
    fn correction_input_changes_cancel_only_their_unsent_work_and_preserve_a_waiting_queue_edit() {
        let basis = correction_state();
        let queue = Command::QueueAppend {
            path: "/owned-queued-source.wav".into(),
        };
        let mut gui = GuiController::new();
        gui.waiting_edit = Some(Task::Preflight {
            basis: basis.clone(),
            command: queue.clone(),
            session: None,
            draft_epoch: 0,
        });
        gui.handle(UiIntent::InvalidateCorrection);
        assert_eq!(gui.waiting_queue_edit(), Some(&queue));
        let request = CorrectionPreviewRequest {
            source: super::super::CorrectionSource::Text {
                text: "owned pending source".into(),
                format: earph_media::correction_text::CorrectionFormat::AutoEq,
            },
            context: earph_media::correction_text::ImportContext {
                sample_rate_hz: 48000,
                channels: earph_core::audio::ChannelLayout::Stereo,
                quantum_frames: 256,
            },
            metadata: Default::default(),
            fir_preamp: None,
        };
        gui.waiting_edit = Some(Task::LocalPreview {
            basis,
            request,
            draft_epoch: gui.draft_epoch,
        });
        assert!(gui.local_correction_pending());
        gui.handle(UiIntent::InvalidateCorrection);
        assert!(!gui.local_correction_pending());
        assert!(!gui.queue_edit_pending());
        assert!(
            gui.local_correction_status()
                .contains("before worker start")
        );
    }
    #[test]
    fn actual_correction_review_ignores_only_control_revision_and_refuses_changed_source_facts() {
        let state = correction_state();
        let mut changed = (*state).clone();
        changed.revision += 1;
        assert!(same_review_context(&state, &changed));
        changed.epochs.stream += 1;
        assert!(!same_review_context(&state, &changed));
    }

    #[test]
    fn actual_core_partial_inventory_keeps_unselectable_endpoint_and_failures_visible() {
        use earph_core::{
            control::{CommandEnvelope, ControlLimits},
            devices::{DeviceObservation, DeviceQueryError, DeviceSummary},
            session::Session,
        };
        use eframe::egui;
        let owner = OwnerId::new("controlled-discovery-owner").unwrap();
        let mut session = Session::new_owner(
            ZoneId::new("main").unwrap(),
            owner,
            ControlLimits::default(),
        );
        session.enable_device_discovery(OutputBackend::CoreAudio);
        let command = CommandEnvelope {
            schema_version: 1,
            request_id: request_id().unwrap(),
            zone_id: ZoneId::new("main").unwrap(),
            owner_id: None,
            session_id: None,
            expected_revision: None,
            payload: Command::DevicesList {
                backend: OutputBackend::CoreAudio,
            },
        };
        session.submit(command.clone());
        let endpoint = "owned-uid-".repeat(40);
        session
            .complete_device_query(
                &session.device_query_job().unwrap(),
                1,
                DeviceObservation::List {
                    devices: vec![DeviceSummary::advertised(
                        OutputBackend::CoreAudio,
                        endpoint.clone(),
                        Some("Controlled long endpoint".into()),
                        None,
                        None,
                    )],
                    complete: false,
                    failures: vec![DeviceQueryError {
                        code: earph_core::ErrorCode::DeviceUnavailable,
                        message: "Controlled native property failure".into(),
                        native_status: Some(-1),
                        native_object_id: Some(42),
                    }],
                },
            )
            .unwrap();
        let bytes =
            serde_json::to_vec(&serde_json::json!({"receipt": session.submit(command)})).unwrap();
        let Reply::Receipt(receipt) = crate::decode_reply(bytes).unwrap() else {
            panic!("Core receipt")
        };
        let Some(ResultView::Devices(result)) = receipt.result else {
            panic!("Core inventory")
        };
        let mut controller = GuiController::new();
        controller.discovery = Some(result);
        let mut app = super::super::GuiApp::new(controller);
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut output = context.run_ui(egui::RawInput::default(), |ui| app.draw(ui));
        output.textures_delta.clear();
        let tree = output.platform_output.accesskit_update.unwrap();
        let target = tree
            .nodes
            .iter()
            .find(|(_, n)| {
                n.role() != egui::accesskit::Role::TextRun && n.label() == Some("Output / Safety")
            })
            .unwrap()
            .0;
        let mut output = context.run_ui(
            egui::RawInput {
                events: vec![egui::Event::AccessKitActionRequest(
                    egui::accesskit::ActionRequest {
                        action: egui::accesskit::Action::Click,
                        target_tree: tree.tree_id,
                        target_node: target,
                        data: None,
                    },
                )],
                ..Default::default()
            },
            |ui| app.draw(ui),
        );
        output.textures_delta.clear();
        let tree = output.platform_output.accesskit_update.unwrap();
        let labels: Vec<_> = tree
            .nodes
            .iter()
            .filter_map(|(_, n)| n.label().or_else(|| n.value()))
            .collect();
        assert!(
            labels
                .iter()
                .any(|s| *s == format!("Exact endpoint: {endpoint}"))
        );
        assert!(
            labels
                .iter()
                .any(|s| s.contains("Inventory complete: false")
                    && s.contains("Controlled native property failure"))
        );
        assert!(!labels.contains(&"Copy backend and endpoint to draft"));
    }

    #[test]
    fn archived_snapshot_replay_cannot_replace_a_newer_cache_after_reconnect() {
        let runtime = PathBuf::from("/tmp").join(request_id().unwrap().as_str());
        let owner = earph_engine::EngineOwner::bind(&runtime).unwrap();
        let serve = thread::spawn(move || owner.serve());
        let observer = OwnerClient::attach(runtime.to_str().unwrap().into()).unwrap();
        let old = Request::query(
            request_id().unwrap(),
            ZoneId::new("main").unwrap(),
            Command::State,
        )
        .unwrap();
        let Reply::Receipt(old_reply) = observer.request(&old).unwrap() else {
            panic!("old State")
        };
        let mutation = Request::mutation(
            request_id().unwrap(),
            old_reply.state().unwrap(),
            Command::QueueAppend {
                path: "/newer-observation.wav".into(),
            },
        )
        .unwrap();
        observer.request(&mutation).unwrap();
        let fresh = Request::query(
            request_id().unwrap(),
            ZoneId::new("main").unwrap(),
            Command::State,
        )
        .unwrap();
        let Reply::Receipt(new_reply) = observer.request(&fresh).unwrap() else {
            panic!("new State")
        };
        let new_state = Arc::new(new_reply.state().unwrap().clone());
        let mut gui = GuiController::new();
        gui.handle(UiIntent::Attach {
            runtime: runtime.to_str().unwrap().into(),
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while !gui.connected() || gui.busy() {
            gui.tick(Instant::now());
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        let observed = gui.observed_at();
        gui.handle(UiIntent::Disconnect);
        while gui.busy() {
            gui.tick(Instant::now());
            thread::sleep(Duration::from_millis(1));
        }
        gui.handle(UiIntent::Attach {
            runtime: runtime.to_str().unwrap().into(),
        });
        gui.next_poll = Instant::now() + Duration::from_secs(60);
        let id = old.envelope().request_id.clone();
        gui.client.as_mut().unwrap().submit(old).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let client = gui.client.as_mut().unwrap();
            client.poll();
            if !client.model().record(&id).unwrap().in_flight {
                break;
            }
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(1));
        }
        gui.tick(Instant::now());
        let actual_revision = gui.state().unwrap().revision;
        let actual_time = gui.observed_at();
        observer
            .request(
                &Request::mutation(request_id().unwrap(), &new_state, Command::OwnerShutdown)
                    .unwrap(),
            )
            .unwrap();
        serve.join().unwrap().unwrap();
        assert_eq!(actual_revision, new_state.revision);
        assert_eq!(actual_time, observed);
    }
}
