// ABOUTME: Presents shared diagnostic observations and exact redacted preview review.
// ABOUTME: Keeps owner artifact effects and cancellation separate from filesystem or audio claims.
use super::{GuiController, UiIntent, app::field};
use crate::{Presentation, Reply, ResultView, StateView};
use earph_core::{control::Command, diagnostics::*, ids::RequestId};
use eframe::egui;
use std::{fmt::Display, sync::Arc};

#[derive(Default)]
pub(super) struct Views {
    pub signal_path: Option<(RequestId, Arc<SignalPathRead>)>,
    pub metrics: Option<(RequestId, Arc<MetricsRead>)>,
    pub events: Option<(RequestId, Arc<EventsRead>)>,
    pub preview: Option<(RequestId, Arc<PreviewRead>)>,
    pub directory: String,
    inspection_sha256: String,
    after_sequence: String,
    reviewed: Option<(RequestId, String)>,
}
impl Views {
    pub fn clear_owner(&mut self) {
        self.signal_path = None;
        self.metrics = None;
        self.events = None;
        self.begin_preview();
        self.after_sequence.clear();
    }
    pub fn begin_preview(&mut self) {
        self.preview = None;
        self.reviewed = None;
    }
    pub fn set_destination(&mut self, directory: String) -> bool {
        if self.directory == directory {
            return false;
        }
        self.directory = directory;
        self.reviewed = None;
        true
    }
    fn current_preview(&self, state: &StateView) -> Result<&(RequestId, Arc<PreviewRead>), String> {
        let preview = self
            .preview
            .as_ref()
            .ok_or("Capture a fresh explicit diagnostic preview first")?;
        if preview.1.binding.owner_id != state.owner_id
            || preview.1.binding.zone_id != state.zone_id
            || state.owner_id.is_none()
        {
            return Err("Diagnostic preview belongs to a different owner or zone".into());
        }
        validate_diagnostic_directory(&self.directory).map_err(|error| error.to_string())?;
        Ok(preview)
    }
    pub fn mark_review(&mut self, state: &StateView) -> Result<(), String> {
        let id = self.current_preview(state)?.0.clone();
        self.reviewed = Some((id, self.directory.clone()));
        Ok(())
    }
    pub fn export_command(&self, state: &StateView) -> Result<Command, String> {
        let (id, preview) = self.current_preview(state)?;
        if self
            .reviewed
            .as_ref()
            .is_none_or(|(reviewed, directory)| reviewed != id || directory != &self.directory)
        {
            return Err("Review the exact redacted preview and destination before export".into());
        }
        Ok(Command::DiagnosticsExport {
            directory: self.directory.clone(),
            expected_sha256: preview.report.sha256,
        })
    }
    pub fn adopt(
        &mut self,
        id: RequestId,
        command: &Command,
        result: ResultView,
        state: &StateView,
    ) -> Result<(), String> {
        let binding = match &result {
            ResultView::SignalPath(value) => &value.binding,
            ResultView::Metrics(value) => &value.binding,
            ResultView::Events(value) => &value.binding,
            ResultView::DiagnosticPreview(value) => &value.binding,
            _ => return Err("Diagnostic observation result unavailable".into()),
        };
        if binding.owner_id != state.owner_id
            || binding.zone_id != state.zone_id
            || state.owner_id.is_none()
        {
            return Err("Diagnostic observation owner or zone changed before adoption".into());
        }
        match (command, result) {
            (Command::SignalPathGet {}, ResultView::SignalPath(value)) => {
                self.signal_path = Some((id, value))
            }
            (Command::MetricsGet {}, ResultView::Metrics(value)) => {
                self.metrics = Some((id, value))
            }
            (Command::EventsGet { .. }, ResultView::Events(value)) => {
                self.events = Some((id, value))
            }
            (Command::DiagnosticsPreview {}, ResultView::DiagnosticPreview(value)) => {
                self.reviewed = None;
                self.preview = Some((id, value));
            }
            _ => return Err("Diagnostic result does not match the captured read".into()),
        }
        Ok(())
    }
}

pub(super) fn waiting(ui: &mut egui::Ui, controller: &GuiController) {
    ui.scope_builder(
        egui::UiBuilder::new().scope_id(egui::Id::unique("diagnostic-reconciliation-waiting")),
        |ui| {
            if let Some(id) = controller.waiting_reconciliation() {
                ui.label(format!(
                    "Exact reconciliation waiting (not resent): {}",
                    id.as_str()
                ));
            }
        },
    );
}

pub(super) fn show(ui: &mut egui::Ui, controller: &mut GuiController) {
    ui.separator();
    ui.heading("Owner diagnostic reports");
    ui.label("Only the exact redacted payload is shareable. Owner identities and raw receipts stay local.");
    let available = controller.diagnostic_read_available();
    let mut command = None;
    ui.horizontal_wrapped(|ui| {
        for (label, read) in [
            ("Read diagnostic signal path", Command::SignalPathGet {}),
            ("Read diagnostic metrics", Command::MetricsGet {}),
            ("Read event recovery", Command::EventsGet { cursor: None }),
            (
                "Capture redacted diagnostic preview",
                Command::DiagnosticsPreview {},
            ),
        ] {
            if ui
                .add_enabled(available, egui::Button::new(label))
                .clicked()
            {
                command = Some(read);
            }
        }
    });
    field(
        ui,
        "Event cursor (after sequence)",
        "diagnostic-event-cursor",
        &mut controller.diagnostics.after_sequence,
        20,
    );
    let cursor = controller
        .diagnostics
        .after_sequence
        .parse::<u64>()
        .ok()
        .zip(controller.state().and_then(|state| state.owner_id.clone()));
    if ui
        .add_enabled(
            available && cursor.is_some(),
            egui::Button::new("Read event page"),
        )
        .clicked()
        && let Some((after_sequence, owner_id)) = cursor
    {
        command = Some(Command::EventsGet {
            cursor: Some(EventCursor {
                owner_id,
                after_sequence,
            }),
        });
    }
    if let Some((id, read)) = &controller.diagnostics.signal_path {
        origin(ui, id, &read.binding);
        signal_path(ui, &read.report);
    }
    if let Some((id, read)) = &controller.diagnostics.metrics {
        origin(ui, id, &read.binding);
        metrics(ui, &read.report);
    }
    if let Some((id, read)) = &controller.diagnostics.events {
        origin(ui, id, &read.binding);
        events(ui, &read.report);
        if ui
            .add_enabled(available, egui::Button::new("Read next event page"))
            .clicked()
            && let Some(owner_id) = &read.binding.owner_id
        {
            command = Some(Command::EventsGet {
                cursor: Some(EventCursor {
                    owner_id: owner_id.clone(),
                    after_sequence: read.report.next_sequence,
                }),
            });
        }
    }
    ui.separator();
    ui.heading("Redacted diagnostic artifact");
    let mut directory = controller.diagnostics.directory.clone();
    field(
        ui,
        "Diagnostic destination directory",
        "diagnostic-destination",
        &mut directory,
        1024,
    );
    if directory != controller.diagnostics.directory {
        controller.handle(UiIntent::DiagnosticDestination(directory));
    }
    ui.label("Use an explicit absolute destination. Export never captures a replacement preview or repairs files.");
    if let Some((id, read)) = &controller.diagnostics.preview {
        origin(ui, id, &read.binding);
        preview(ui, read);
    } else {
        ui.label("No current diagnostic preview. Historical replay requires a fresh explicit capture before export.");
    }
    let reviewable = available
        && controller
            .state()
            .is_some_and(|state| controller.diagnostics.current_preview(state).is_ok());
    if ui
        .add_enabled(
            reviewable,
            egui::Button::new("Mark diagnostic preview reviewed"),
        )
        .clicked()
    {
        controller.handle(UiIntent::ReviewDiagnosticPreview);
    }
    let export = controller
        .state()
        .and_then(|state| controller.diagnostics.export_command(state).ok());
    ui.label(if export.is_some() {
        "Preview review: acknowledged for this exact payload and destination"
    } else {
        "Preview review: required"
    });
    if ui
        .add_enabled(
            controller.control_edit_available() && export.is_some(),
            egui::Button::new("Export reviewed diagnostic preview"),
        )
        .clicked()
    {
        command = export;
    }
    field(
        ui,
        "Diagnostic inspection SHA-256",
        "diagnostic-inspection-digest",
        &mut controller.diagnostics.inspection_sha256,
        64,
    );
    let inspection = earph_core::profiles::ProfileObjectHash::try_from(
        controller.diagnostics.inspection_sha256.clone(),
    )
    .ok();
    if ui
        .add_enabled(
            available
                && inspection.is_some()
                && validate_diagnostic_directory(&controller.diagnostics.directory).is_ok(),
            egui::Button::new("Inspect diagnostic artifact"),
        )
        .clicked()
        && let Some(expected_sha256) = inspection
    {
        command = Some(Command::DiagnosticsInspect {
            directory: controller.diagnostics.directory.clone(),
            expected_sha256,
        });
    }
    ui.label("Inspection never repairs files. Missing does not exclude a retained stage.");
    ui.label("Cancellation acknowledgment does not prove worker retirement or absence of filesystem effects.");
    for receipt in controller
        .receipts()
        .filter(|receipt| receipt.request.envelope().payload.is_diagnostic_artifact())
    {
        if let Some(action) = artifact_record(ui, &receipt, available, controller.state()) {
            command = Some(action);
        }
    }
    if let Some(command) = command
        && let Some(state) = controller.state()
    {
        controller.handle(UiIntent::DiagnosticAction {
            basis: Arc::new(state.clone()),
            command,
        });
    }
}

fn artifact_record(
    ui: &mut egui::Ui,
    receipt: &super::Receipt<'_>,
    available: bool,
    current: Option<&StateView>,
) -> Option<Command> {
    let mut command = None;
    let envelope = receipt.request.envelope();
    ui.scope_builder(
        egui::UiBuilder::new().scope_id(egui::Id::unique((
            envelope.request_id.as_str(),
            "diagnostic-operation",
        ))),
        |ui| {
            ui.separator();
            ui.label(format!(
                "Local diagnostic operation: {}; status {:?}; send in flight {}; owner changed {}",
                envelope.request_id.as_str(),
                receipt.presentation,
                receipt.in_flight,
                receipt.owner_changed
            ));
            match receipt.outcome {
                Some(Ok(Reply::Receipt(reply))) => {
                    match &reply.result {
                        Some(ResultView::DiagnosticArtifact(value)) => artifact_facts(ui, value),
                        Some(ResultView::DiagnosticCancel(value)) => cancel_facts(ui, value),
                        _ => {
                            ui.label("Filesystem effect: not reported");
                        }
                    }
                    if let Some(error) = &reply.error {
                        ui.label(format!("Owner error category: {:?}", error.code));
                    }
                }
                _ => {
                    ui.label("Filesystem effects remain unknown without an exact owner result.");
                }
            }
            let current_owner = !receipt.owner_changed
                && current.is_some_and(|state| {
                    state.owner_id == envelope.owner_id && state.zone_id == envelope.zone_id
                });
            if let Command::DiagnosticsExport {
                directory,
                expected_sha256,
            }
            | Command::DiagnosticsInspect {
                directory,
                expected_sha256,
            } = &envelope.payload
            {
                ui.label(format!(
                    "Captured destination: {directory}; expected SHA-256 {}",
                    expected_sha256.as_str()
                ));
                let inspect = ui
                    .scope_builder(
                        egui::UiBuilder::new().scope_id(egui::Id::unique((
                            envelope.request_id.as_str(),
                            "diagnostic-inspect",
                        ))),
                        |ui| {
                            ui.add_enabled(
                                available && current_owner,
                                egui::Button::new("Inspect this captured destination"),
                            )
                        },
                    )
                    .inner;
                if inspect.clicked() {
                    command = Some(Command::DiagnosticsInspect {
                        directory: directory.clone(),
                        expected_sha256: *expected_sha256,
                    });
                }
                let cancel = ui
                    .scope_builder(
                        egui::UiBuilder::new().scope_id(egui::Id::unique((
                            envelope.request_id.as_str(),
                            "diagnostic-cancel",
                        ))),
                        |ui| {
                            ui.add_enabled(
                                available
                                    && current_owner
                                    && matches!(
                                        receipt.presentation,
                                        Presentation::Accepted | Presentation::Uncertain
                                    ),
                                egui::Button::new("Request diagnostic cancellation"),
                            )
                        },
                    )
                    .inner;
                if cancel.clicked() {
                    command = Some(Command::DiagnosticsCancel {
                        request_id: envelope.request_id.clone(),
                    });
                }
            }
        },
    );
    command
}

fn cut(ui: &mut egui::Ui, label: &str, cut: DiagnosticCut) {
    ui.label(format!("{label}: revision {}; session epoch {}; stream epoch {}; graph epoch {}; device epoch {}; event {}; owner clock {} ns", cut.revision, cut.epochs.session, cut.epochs.stream, cut.graph_epoch, cut.device_epoch, cut.event_sequence, cut.monotonic_ns));
}
fn origin(ui: &mut egui::Ui, id: &RequestId, binding: &DiagnosticBinding) {
    ui.label(format!(
        "Private observation target: owner {}; zone {}; request {}",
        binding
            .owner_id
            .as_ref()
            .map_or("unknown", |owner| owner.as_str()),
        binding.zone_id.as_str(),
        id.as_str()
    ));
    cut(ui, "Observation cut", binding.cut());
}
fn signal_path(ui: &mut egui::Ui, report: &SignalPathReport) {
    ui.heading("Requested configuration");
    ui.label(format!(
        "Requested source: {:?}; configured graph {}; mute {}; exact zero {}; finite software gain {} dB; ramp {} frames",
        report.requested.source,
        report.requested.graph_configured,
        report.requested.muted,
        report.requested.software_gain_zero,
        report.requested.software_gain_db,
        report.requested.gain_ramp_frames
    ));
    ui.label(format!(
        "Requested caps: software {} dB; hardware {}",
        report.requested.software_cap_db,
        report
            .requested
            .hardware_cap_db
            .map_or_else(|| "unavailable".into(), |value| format!("{value} dB"))
    ));
    ui.collapsing("Exact requested diagnostic configuration", |ui| {
        ui.label(serde_json::to_string_pretty(&report.requested).expect("typed requested report"));
    });
    if let Some(resolved) = &report.resolved {
        ui.heading("Resolved compilation");
        ui.label(format!("Resolved preparation: revision {}; epochs {:?}; source {:?}; processing input {:?}; processing output {:?}; intended native output {:?}", resolved.preparation_revision, resolved.preparation_epochs, resolved.source_format, resolved.processing_input_format, resolved.processing_output_format, resolved.native_output_format));
        ui.label(format!(
            "Resolved transformations: {:?}; required headroom {} dB; preservation {:?}",
            resolved.transformations, resolved.required_headroom_db, resolved.preservation
        ));
        ui.label("Compiler schedule facts do not establish per-node execution.");
        ui.collapsing("Exact resolved compiler facts", |ui| {
            ui.label(serde_json::to_string_pretty(resolved).expect("typed resolved report"));
        });
    } else {
        ui.label("Resolved compilation: unavailable");
    }
    if let Some(observed) = &report.observed {
        ui.heading("Observed playback");
        ui.label(format!(
            "Observed playback format: {:?}; modified {}",
            observed.playback_format, observed.modified
        ));
        if let Some(boundary) = &observed.network_replacement_boundary {
            super::signal_path::replacement_boundary(ui, boundary);
        }
        ui.label("Playback sample origin and time: unavailable; this cut does not establish a fresh hardware sample.");
        if let Some(backend) = &observed.backend {
            ui.label(format!(
                "Observed backend boundary: {:?}; preservation {:?}; client format {:?}",
                backend.measurement_boundary, backend.preservation, backend.client_format
            ));
            ui.label(format!(
                "Observed native format: {}",
                backend
                    .device_format
                    .as_ref()
                    .map_or_else(|| "unavailable".into(), |format| format!("{format:?}"))
            ));
        } else {
            ui.label("Observed backend: unavailable");
        }
    } else {
        ui.label("Observed playback: unavailable");
    }
    for segment in &report.unknown_segments {
        ui.label(match segment {
            UnknownSegment::UpstreamProcessing => "Upstream processing: unknown",
            UnknownSegment::OutputRouteInternals => "Output route internals: unknown",
            UnknownSegment::DacInternals => "DAC internals: unknown",
        });
    }
}
fn metric<T: Display>(ui: &mut egui::Ui, label: &str, metric: &Metric<T>) {
    let scope = match metric.scope {
        MetricScope::InstrumentationUnavailable => "instrumentation unavailable",
        MetricScope::LastPlaybackObservation => "last retained playback observation",
        MetricScope::GraphProcessingSinceReset => "graph processing since reset",
    };
    match &metric.value {
        Some(value) => {
            ui.label(format!("{label}: {value} ({scope})"));
        }
        None => {
            let reason = match metric.unavailable_reason {
                Some(UnavailableReason::Unsupported) => "unsupported",
                Some(UnavailableReason::NotObserved) => "not observed",
                Some(UnavailableReason::NotApplicable) => "not applicable",
                Some(UnavailableReason::Stale) => "stale",
                None => "unavailable",
            };
            ui.label(format!("{label}: unavailable ({reason}; {scope})"));
        }
    }
}
fn metrics(ui: &mut egui::Ui, report: &MetricsReport) {
    ui.heading("Scoped diagnostic metrics");
    metric(ui, "Underruns", &report.underruns);
    metric(ui, "Deadline misses", &report.deadline_misses);
    metric(ui, "CPU load", &report.cpu_load);
    metric(ui, "Queue depth (frames)", &report.queue_depth_frames);
    metric(ui, "Input peak (linear)", &report.input_peak);
    metric(ui, "Output peak (linear)", &report.output_peak);
    metric(ui, "Saturation count", &report.saturation_count);
    metric(ui, "Backend delay (frames)", &report.backend_delay_frames);
    if report.saturation_count.value == Some(u64::MAX) {
        ui.label("Saturation count reached its saturating maximum; this is a lower bound.");
    }
    ui.label("Software statistics do not measure analog jitter, distortion, or audibility.");
}
fn events(ui: &mut egui::Ui, report: &EventsReport) {
    ui.heading("Scoped diagnostic events");
    ui.label("Event history is scoped retained State history, not a complete audit log.");
    cut(ui, "Event page cut", report.cut);
    if report.gap {
        ui.label("History gap: adopt the public recovery facts at this cut, then resume at the reported sequence.");
    }
    ui.label(format!(
        "Public recovery facts: transport {:?}; muted {}; exact zero {}; finite software gain {} dB; error category {:?}",
        report.snapshot.transport,
        report.snapshot.muted,
        report.snapshot.software_gain_zero,
        report.snapshot.software_gain_db,
        report.snapshot.error
    ));
    ui.label(format!(
        "Resume sequence: {}; more retained entries {}",
        report.next_sequence, report.has_more
    ));
    ui.label("Historical events do not update product State. Event clocks belong to Session, not receipt arrival or hardware samples.");
    for event in &report.events {
        ui.label(format!("Event {}: revision {}; epochs {:?}; owner clock {} ns; transport {:?}; muted {}; error category {:?}", event.sequence, event.revision, event.epochs, event.monotonic_ns, event.transport, event.muted, event.error));
    }
}
fn preview(ui: &mut egui::Ui, read: &PreviewRead) {
    ui.label(format!("Preview SHA-256: {}", read.report.sha256.as_str()));
    ui.label(format!("Preview UTF-8 bytes: {}", read.report.byte_len));
    cut(ui, "Preview capture cut", read.report.cut);
    ui.label(format!(
        "Preview owner clock: created {} ns; expires {} ns",
        read.report.created_monotonic_ns, read.report.expires_monotonic_ns
    ));
    ui.label("Owner decides preview expiry and replacement. Exact historical replay does not renew candidate authority.");
    if let Ok(bundle) = DiagnosticBundle::decode(read.report.bytes.as_bytes()) {
        let coverage = match bundle.reproduction {
            ReproductionCoverage::Incomplete => "incomplete",
        };
        let inventory = match bundle.test_inventory {
            TestInventory::NotAttached => "not attached",
        };
        ui.label(format!(
            "Evidence coverage: {coverage}; test inventory: {inventory}"
        ));
        ui.label(format!(
            "Missing evidence: {:?}; omitted categories: {:?}",
            bundle.missing_evidence, bundle.omitted_categories
        ));
        ui.collapsing("Compiled package provenance", |ui| {
            ui.label(
                serde_json::to_string_pretty(&bundle.running_build)
                    .expect("typed build provenance"),
            );
        });
    }
    ui.collapsing("Exact redacted diagnostic payload", |ui| {
        ui.label(read.report.bytes.as_ref());
    });
}
fn artifact_facts(ui: &mut egui::Ui, value: &DiagnosticArtifactReceipt) {
    let effect = match value.effect {
        DiagnosticFilesystemEffect::Unchanged => "unchanged",
        DiagnosticFilesystemEffect::StageRetained => "stage retained",
        DiagnosticFilesystemEffect::InstalledVerified => "installed and verified",
        DiagnosticFilesystemEffect::InstalledSyncUncertain => "installed; directory sync uncertain",
        DiagnosticFilesystemEffect::InspectRequired => "inspection required",
    };
    ui.label(format!("Filesystem effect: {effect}"));
    ui.label(format!(
        "Reported stage locator: {}",
        value.stage_directory.as_deref().unwrap_or("not reported")
    ));
    ui.label(format!(
        "Inspection result: {:?}; reason {:?}",
        value.inspection, value.reason
    ));
    ui.label(format!(
        "Owner completion time: {}",
        value
            .completed_monotonic_ns
            .map_or_else(|| "not reported".into(), |clock| format!("{clock} ns"))
    ));
    ui.label("Artifact effects do not establish audio playback. A reported stage locator does not prove the stage still exists.");
    ui.collapsing("Exact local diagnostic artifact receipt", |ui| {
        ui.label(serde_json::to_string_pretty(value).expect("typed artifact receipt"));
    });
}
fn cancel_facts(ui: &mut egui::Ui, value: &DiagnosticCancelReceipt) {
    let state = match value.state {
        DiagnosticCancelState::Requested => "requested",
        DiagnosticCancelState::AlreadyTerminal => "already terminal",
        DiagnosticCancelState::UnknownOperation => "unknown operation",
    };
    ui.label(format!(
        "Cancellation target: {}",
        value.request_id.as_str()
    ));
    ui.label(format!("Cancellation state: {state}"));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Reply, Request, ResultView, StateView};
    use earph_core::{control::*, ids::*, session::Session};

    #[test]
    fn network_replacement_diagnostic_widgets_show_only_allowlisted_frontier_and_discard() {
        let mut session = crate::network_fixture::replacement();
        let request = crate::network_fixture::command(
            &session,
            "replacement-render",
            serde_json::json!({"method":"signal_path.get"}),
        );
        let Some(QueryResult::SignalPath(value)) = session.submit_graph(request, 70).result else {
            panic!("Core diagnostic report")
        };
        value.validate().unwrap();
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut output = context.run_ui(egui::RawInput::default(), |ui| {
            signal_path(ui, &value.report)
        });
        output.textures_delta.clear();
        let tree = output.platform_output.accesskit_update.unwrap();
        let labels: Vec<_> = tree
            .nodes
            .iter()
            .filter_map(|(_, n)| n.label().or_else(|| n.value()))
            .collect();
        assert!(labels.contains(&"Network graph replacement: accepted source frontier 23 frames; discarded output 19 frames"));
        assert!(
            labels
                .iter()
                .any(|label| label.starts_with("Discarded output format:")
                    && label.contains("48000")
                    && label.contains("Signed16"))
        );
        assert!(!labels.iter().any(|label| label.contains("http-session")));
    }

    fn owner() -> Session {
        Session::new_owner(
            ZoneId::new("main").unwrap(),
            OwnerId::new("diagnostic-render-owner").unwrap(),
            ControlLimits::default(),
        )
    }
    fn state(session: &Session) -> StateView {
        serde_json::from_value(serde_json::to_value(session.snapshot()).unwrap()).unwrap()
    }
    fn observe(
        session: &mut Session,
        id: &str,
        command: Command,
    ) -> (RequestId, StateView, ResultView) {
        let (request, basis, reply) = observe_reply(session, id, command);
        let Reply::Receipt(receipt) = reply else {
            panic!("Core receipt")
        };
        (
            request.envelope().request_id.clone(),
            basis,
            receipt.result.unwrap(),
        )
    }
    fn observe_reply(
        session: &mut Session,
        id: &str,
        command: Command,
    ) -> (Request, StateView, Reply) {
        let basis = state(session);
        let id = RequestId::new(id).unwrap();
        let request = if command.is_diagnostic() {
            Request::diagnostic_query(id.clone(), &basis, command)
        } else {
            Request::diagnostic_artifact(id.clone(), &basis, command)
        }
        .unwrap();
        let receipt = session.submit_with_graph_effects(
            decode_command(request.bytes(), &ControlLimits::default()).unwrap(),
            0,
            |_| unreachable!("diagnostic reads never request audio"),
            |_| Ok(()),
        );
        assert!(
            matches!(
                receipt.status,
                ReceiptStatus::Applied | ReceiptStatus::Accepted
            ),
            "{:?}",
            receipt.error
        );
        let reply = crate::decode_reply(
            serde_json::to_vec(&serde_json::json!({"receipt": receipt})).unwrap(),
        )
        .unwrap();
        (request, basis, reply)
    }
    fn draw(
        context: &egui::Context,
        gui: &mut GuiController,
        events: Vec<egui::Event>,
    ) -> egui::accesskit::TreeUpdate {
        let mut output = context.run_ui(
            egui::RawInput {
                events,
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(1800.0, 14000.0),
                )),
                ..Default::default()
            },
            |ui| show(ui, gui),
        );
        output.textures_delta.clear();
        output.platform_output.accesskit_update.unwrap()
    }
    fn labels(tree: &egui::accesskit::TreeUpdate) -> Vec<&str> {
        tree.nodes
            .iter()
            .filter_map(|(_, node)| node.label().or_else(|| node.value()))
            .collect()
    }

    #[test]
    fn actual_core_reads_render_scopes_unavailable_metrics_and_exact_redacted_payload() {
        let mut session = owner();
        let mut gui = GuiController::new();
        for (id, command) in [
            ("path", Command::SignalPathGet {}),
            ("metrics", Command::MetricsGet {}),
            ("events", Command::EventsGet { cursor: None }),
            ("preview", Command::DiagnosticsPreview {}),
        ] {
            let (id, basis, result) = observe(&mut session, id, command.clone());
            gui.diagnostics.adopt(id, &command, result, &basis).unwrap();
        }
        let context = egui::Context::default();
        context.enable_accesskit();
        context.all_styles_mut(|style| style.animation_time = 0.0);
        let tree = draw(&context, &mut gui, vec![]);
        for expected in [
            "Requested configuration",
            "Resolved compilation: unavailable",
            "Observed playback: unavailable",
            "DAC internals: unknown",
            "Underruns: unavailable (unsupported; instrumentation unavailable)",
            "CPU load: unavailable (unsupported; instrumentation unavailable)",
            "Event history is scoped retained State history, not a complete audit log.",
            "Evidence coverage: incomplete; test inventory: not attached",
            "Owner decides preview expiry and replacement. Exact historical replay does not renew candidate authority.",
        ] {
            assert!(
                labels(&tree).contains(&expected),
                "missing diagnostic fact {expected}"
            );
        }
        let target = tree
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Exact redacted diagnostic payload"))
            .unwrap()
            .0;
        let tree = draw(
            &context,
            &mut gui,
            vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action: egui::accesskit::Action::Click,
                    target_tree: tree.tree_id,
                    target_node: target,
                    data: None,
                },
            )],
        );
        let (_, preview) = gui.diagnostics.preview.as_ref().unwrap();
        assert!(
            labels(&tree).contains(&preview.report.bytes.as_ref()),
            "redacted payload was reformatted or omitted"
        );
        assert!(
            labels(&tree)
                .contains(&format!("Preview SHA-256: {}", preview.report.sha256.as_str()).as_str())
        );
        assert!(
            gui.state().is_none(),
            "diagnostic reports became product State"
        );
        assert!(session.snapshot().muted && session.snapshot().output.is_none());
    }

    #[test]
    fn review_binds_the_exact_preview_destination_and_owner_without_automatic_recapture() {
        let mut session = owner();
        let mut views = Views::default();
        let command = Command::DiagnosticsPreview {};
        let (id, basis, result) = observe(&mut session, "first-preview", command.clone());
        views.adopt(id, &command, result, &basis).unwrap();
        views.set_destination("/owned/diagnostics-first".into());
        assert!(views.export_command(&basis).is_err());
        views.mark_review(&basis).unwrap();
        let digest = views.preview.as_ref().unwrap().1.report.sha256;
        let Command::DiagnosticsExport {
            directory,
            expected_sha256,
        } = views.export_command(&basis).unwrap()
        else {
            panic!("export")
        };
        assert_eq!(directory, "/owned/diagnostics-first");
        assert_eq!(expected_sha256, digest);
        views.set_destination("/owned/diagnostics-second".into());
        assert!(views.export_command(&basis).is_err());
        views.mark_review(&basis).unwrap();
        let (id, basis, result) = observe(&mut session, "metrics-read", Command::MetricsGet {});
        views
            .adopt(id, &Command::MetricsGet {}, result, &basis)
            .unwrap();
        assert!(
            views.export_command(&basis).is_ok(),
            "ordinary observation invalidated exact review"
        );
        views.begin_preview();
        assert!(views.preview.is_none() && views.export_command(&basis).is_err());
        let (id, basis, result) = observe(&mut session, "replacement-preview", command.clone());
        views.adopt(id, &command, result, &basis).unwrap();
        assert_eq!(views.preview.as_ref().unwrap().1.report.sha256, digest);
        assert!(
            views.export_command(&basis).is_err(),
            "identical bytes restored an old acknowledgment"
        );
        views.mark_review(&basis).unwrap();
        let replacement = Session::new_owner(
            ZoneId::new("main").unwrap(),
            OwnerId::new("replacement-owner").unwrap(),
            ControlLimits::default(),
        );
        assert!(views.export_command(&state(&replacement)).is_err());
        views.clear_owner();
        assert!(views.preview.is_none() && views.export_command(&basis).is_err());
        assert_eq!(views.directory, "/owned/diagnostics-second");
    }

    #[test]
    fn event_gap_displays_actual_recovery_without_adopting_historical_state() {
        let mut session = owner();
        let command = Command::EventsGet {
            cursor: Some(EventCursor {
                owner_id: session.snapshot().owner_id.unwrap(),
                after_sequence: u64::MAX,
            }),
        };
        let (id, basis, result) = observe(&mut session, "future-history", command.clone());
        let mut gui = GuiController::new();
        gui.diagnostics.adopt(id, &command, result, &basis).unwrap();
        let context = egui::Context::default();
        context.enable_accesskit();
        let tree = draw(&context, &mut gui, vec![]);
        assert!(labels(&tree).contains(&"History gap: adopt the public recovery facts at this cut, then resume at the reported sequence."));
        assert!(gui.diagnostics.events.as_ref().unwrap().1.report.gap);
        assert!(gui.state().is_none());
    }

    #[test]
    fn typed_metric_zero_is_visible_and_unavailable_is_never_replaced_by_zero() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut output = context.run_ui(egui::RawInput::default(), |ui| {
            metric(
                ui,
                "Saturation count",
                &Metric {
                    value: Some(0_u64),
                    unavailable_reason: None,
                    scope: MetricScope::GraphProcessingSinceReset,
                },
            );
            metric(
                ui,
                "Underruns",
                &Metric::<u64> {
                    value: None,
                    unavailable_reason: Some(UnavailableReason::Unsupported),
                    scope: MetricScope::InstrumentationUnavailable,
                },
            );
        });
        output.textures_delta.clear();
        let tree = output.platform_output.accesskit_update.unwrap();
        assert!(labels(&tree).contains(&"Saturation count: 0 (graph processing since reset)"));
        assert!(
            labels(&tree)
                .contains(&"Underruns: unavailable (unsupported; instrumentation unavailable)")
        );
    }

    #[test]
    fn actual_core_acceptance_and_cancel_ack_do_not_claim_installation_or_retirement() {
        let mut session = owner();
        let (_, _, preview) = observe(
            &mut session,
            "artifact-preview",
            Command::DiagnosticsPreview {},
        );
        let ResultView::DiagnosticPreview(preview) = preview else {
            panic!("preview")
        };
        let (id, _, result) = observe(
            &mut session,
            "artifact-export",
            Command::DiagnosticsExport {
                directory: "/owned/diagnostic-render-artifact".into(),
                expected_sha256: preview.report.sha256,
            },
        );
        let ResultView::DiagnosticArtifact(artifact) = result else {
            panic!("artifact")
        };
        let (_, _, result) = observe(
            &mut session,
            "artifact-cancel",
            Command::DiagnosticsCancel { request_id: id },
        );
        let ResultView::DiagnosticCancel(cancel) = result else {
            panic!("cancel")
        };
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut output = context.run_ui(egui::RawInput::default(), |ui| {
            artifact_facts(ui, &artifact);
            cancel_facts(ui, &cancel);
        });
        output.textures_delta.clear();
        let tree = output.platform_output.accesskit_update.unwrap();
        for expected in [
            "Filesystem effect: inspection required",
            "Reported stage locator: not reported",
            "Owner completion time: not reported",
            "Cancellation state: requested",
        ] {
            assert!(labels(&tree).contains(&expected), "missing {expected}");
        }
        assert!(
            session.diagnostic_work().is_some(),
            "cancel acknowledgment unexpectedly retired the work"
        );
        assert!(session.snapshot().muted && session.snapshot().output.is_none());
    }

    #[test]
    fn artifact_controls_keep_request_identity_when_preceding_observations_change() {
        let mut session = owner();
        let (_, _, preview) = observe(
            &mut session,
            "stable-preview",
            Command::DiagnosticsPreview {},
        );
        let ResultView::DiagnosticPreview(preview) = preview else {
            panic!("preview")
        };
        let (request, basis, reply) = observe_reply(
            &mut session,
            "stable-export",
            Command::DiagnosticsExport {
                directory: "/owned/stable-diagnostic-artifact".into(),
                expected_sha256: preview.report.sha256,
            },
        );
        let outcome = Some(Ok(reply));
        let record = super::super::Receipt {
            request: &request,
            presentation: Presentation::Accepted,
            in_flight: false,
            owner_changed: false,
            outcome: &outcome,
        };
        let context = egui::Context::default();
        context.enable_accesskit();
        let draw = |record: &super::super::Receipt<'_>, prefix| {
            let mut output = context.run_ui(egui::RawInput::default(), |ui| {
                for _ in 0..prefix {
                    ui.label("New diagnostic observation");
                }
                artifact_record(ui, record, true, Some(&basis));
            });
            output.textures_delta.clear();
            output.platform_output.accesskit_update.unwrap()
        };
        let before = draw(&record, 0);
        let after = draw(&record, 3);
        let job = session.diagnostic_work().unwrap();
        let mut failed = job.pending_receipt();
        failed.effect = DiagnosticFilesystemEffect::Unchanged;
        failed.reason = Some(DiagnosticArtifactReason::StorageUnavailable);
        // Controlled worker completion qualifies the UI transition, not filesystem execution.
        session
            .complete_diagnostic_work(
                &job.request_id,
                &job.binding,
                1,
                DiagnosticWorkCompletion {
                    receipt: failed,
                    status: ReceiptStatus::Failed,
                    error: Some(earph_core::EngineError::new(
                        earph_core::ErrorCode::InternalFault,
                        "controlled worker failure",
                    )),
                },
            )
            .unwrap();
        let terminal = session.submit_with_graph_effects(
            decode_command(request.bytes(), &ControlLimits::default()).unwrap(),
            2,
            |_| unreachable!(),
            |_| Ok(()),
        );
        assert_eq!(terminal.status, ReceiptStatus::Failed);
        let terminal = Some(Ok(crate::decode_reply(
            serde_json::to_vec(&serde_json::json!({"receipt": terminal})).unwrap(),
        )
        .unwrap()));
        let failed_record = super::super::Receipt {
            presentation: Presentation::Failed,
            outcome: &terminal,
            ..record
        };
        let after_failure = draw(&failed_record, 3);
        for label in [
            "Request diagnostic cancellation",
            "Inspect this captured destination",
            "Exact local diagnostic artifact receipt",
        ] {
            let named = |tree: &egui::accesskit::TreeUpdate| {
                tree.nodes
                    .iter()
                    .find(|(_, node)| node.label() == Some(label))
                    .unwrap()
                    .0
            };
            assert_eq!(
                named(&before),
                named(&after),
                "new observations retargeted {label}"
            );
            assert_eq!(
                named(&before),
                named(&after_failure),
                "failure details retargeted {label}"
            );
        }
    }
}
