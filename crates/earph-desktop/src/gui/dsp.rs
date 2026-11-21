// ABOUTME: Reviews exact graph and saved-profile candidates before explicit activation.
// ABOUTME: Keeps configured intent, compiled facts, and opened playback observations distinct.
use super::{GuiApp, UiIntent, profiles};
use crate::GraphPreviewView;
use earph_core::control::Command;
use eframe::egui;

pub(super) fn show(ui: &mut egui::Ui, app: &mut GuiApp) {
    ui.heading("DSP graph");
    if let Some(state) = app.controller.state()
        && let Some(playback) = state.playback_facts()
    {
        ui.label(format!(
            "Observed source format: {:?}; requested native period: {:?} frames",
            playback.format,
            state.output.as_ref().map(|o| o.period_frames)
        ));
    }
    if app.graph.show(ui) {
        app.controller.handle(UiIntent::DiscardPreparedGraph);
    }
    let enabled = app.controller.control_edit_available();
    let canonical = !app.graph.has_pending_raw_edits();
    if !canonical {
        ui.label("Accept or restore the raw graph JSON before saving, validating, preparing, or activating a candidate.");
    }
    let correction_reviewed = app
        .profiles
        .graph_review_required()
        .is_none_or(|reference| app.controller.profile_review_available(reference));
    let source = enabled
        && canonical
        && correction_reviewed
        && app
            .controller
            .state()
            .is_some_and(|s| s.graph_source_available());
    let mut command = None;
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(enabled, egui::Button::new("Inspect graph"))
            .clicked()
        {
            command = Some(Command::GraphInspect {});
        }
        if ui
            .add_enabled(source, egui::Button::new("Validate against current source"))
            .clicked()
        {
            let intent = app.graph.intent();
            command = Some(Command::GraphValidate {
                graph: intent.graph.clone(),
                processing: intent.processing,
            });
        }
        if ui
            .add_enabled(source, egui::Button::new("Prepare graph"))
            .clicked()
        {
            let intent = app.graph.intent();
            command = Some(Command::GraphPrepare {
                graph: intent.graph.clone(),
                processing: intent.processing,
            });
        }
    });
    ui.label("Live validation requires an actual opened source. Saving plain intent uses the owner's offline compiler.");
    let selected = profiles::selected_record(app)
        .map(|r| (r.profile.id.clone(), r.profile.revision, r.profile_sha256));
    let reviewed = profiles::selected_record(app).is_some_and(|record| {
        app.controller.profile_review_available(
            &earph_core::profile_control::ProfileCatalogReference {
                id: record.profile.id.clone(),
                revision: record.profile.revision,
                profile_sha256: record.profile_sha256,
            },
        )
    });
    if ui
        .add_enabled(
            source
                && app
                    .controller
                    .state()
                    .is_some_and(|s| s.source == Some(earph_core::control::SourceKind::LocalFile))
                && selected.is_some()
                && reviewed
                && app
                    .controller
                    .capabilities()
                    .is_some_and(|c| c.profiles.apply),
            egui::Button::new("Prepare selected profile"),
        )
        .clicked()
        && let Some((id, expected_profile_revision, profile_sha256)) = selected
    {
        command = Some(Command::ProfilesPrepare {
            id,
            expected_profile_revision,
            profile_sha256,
        });
    }
    if let Some(preview) = &app.controller.profiles.preview {
        ui.label(format!(
            "Validation requests {:?} · captured owner {:?} · revision {}",
            preview.request_ids, preview.basis.owner_id, preview.basis.revision
        ));
        preview_facts(ui, &preview.value);
    }
    let candidate = app.controller.prepared_graph().cloned();
    if let Some(prepared) = &candidate {
        ui.label(format!(
            "Prepared token: {}; owner-monotonic expiry {} ns; binding {:?}",
            prepared.token.as_str(),
            prepared.expires_monotonic_ns,
            prepared.preview.binding
        ));
        preview_facts(ui, &prepared.preview);
        ui.label("A prepared candidate is not active processing. Owner authorization and expiry remain authoritative.");
    } else {
        ui.label("Prepared graph: unavailable or discarded");
    }
    ui.label("Reconciled preparations remain receipt history. Use a fresh explicit Prepare before activation. The owner may retain its earlier candidate until expiry or a context change.");
    let applicable = canonical && app.controller.prepared_action_available();
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(
                applicable
                    && candidate
                        .as_ref()
                        .is_some_and(|p| p.preview.profile_application.is_none()),
                egui::Button::new("Commit prepared graph"),
            )
            .clicked()
            && let Some(prepared) = &candidate
        {
            command = Some(Command::GraphCommit {
                token: prepared.token.clone(),
                graph_hash: prepared.preview.graph_hash.as_str().into(),
            });
        }
        if ui
            .add_enabled(
                applicable
                    && app.controller.state().is_some_and(|s| {
                        s.source == Some(earph_core::control::SourceKind::LocalFile)
                    })
                    && candidate
                        .as_ref()
                        .is_some_and(|p| p.preview.profile_application.is_some()),
                egui::Button::new("Apply prepared profile"),
            )
            .clicked()
            && let Some(prepared) = &candidate
            && let Some(binding) = &prepared.preview.profile_application
        {
            command = Some(Command::ProfilesApply {
                id: binding.profile.id.clone(),
                expected_profile_revision: binding.profile.revision,
                profile_sha256: binding.profile.profile_sha256,
                token: prepared.token.clone(),
                graph_hash: prepared.preview.graph_hash.as_str().into(),
            });
        }
    });
    ui.label("Activation remains muted. Review the actual receipt and State, then use separate Output consent, arming, and Unmute actions.");
    if let Some((id, diagnostic)) = &app.controller.profiles.diagnostic {
        ui.label(format!(
            "Owner graph diagnostic for {}: {:?}: {}; node {:?}; edge {:?}",
            id.as_str(),
            diagnostic.code,
            diagnostic.message,
            diagnostic.node,
            diagnostic.edge
        ));
    }
    if let Some(observed) = &app.controller.profiles.graph {
        ui.label(format!(
            "Graph inspection requests {:?} · owner {:?} · revision {}",
            observed.request_ids, observed.basis.owner_id, observed.basis.revision
        ));
        inspection(ui, &observed.value);
    }
    if let Some(command) = command {
        profiles::action(app, command);
    }
}
pub(super) fn inspection(ui: &mut egui::Ui, graph: &crate::GraphInspectionView) {
    ui.label(format!(
        "Graph recovery required: {}; graph epoch {}; service {:?}",
        graph.recovery_required, graph.graph_epoch, graph.service
    ));
    ui.label(format!(
        "Pending graph request: {:?}; stage {:?}",
        graph.pending_request_id, graph.pending_stage
    ));
    ui.collapsing("Configured graph intent", |ui| {
        if let Some(configured) = &graph.configured {
            ui.label(
                serde_json::to_string_pretty(configured)
                    .expect("typed configured graph serialization"),
            );
        } else {
            ui.label("Configured graph: unavailable");
        }
    });
    if let Some(active) = &graph.active {
        ui.label("Actual active compiled graph");
        preview_facts(ui, active);
    } else {
        ui.label("Active compiled graph: unavailable");
    }
    if let Some(prepared) = &graph.prepared {
        ui.label(format!("Owner candidate observation: token {}; expires {} ns. Inspecting does not select this candidate for activation.", prepared.token.as_str(), prepared.expires_monotonic_ns));
    }
}
fn preview_facts(ui: &mut egui::Ui, preview: &GraphPreviewView) {
    ui.label(format!(
        "Graph hash: {}; kernel hash: {}; implementation {}",
        preview.graph_hash.as_str(),
        preview.kernel_hash.as_str(),
        preview.implementation_version
    ));
    ui.label(format!(
        "Resolved source format: {:?}; native output contract {:?}",
        preview.source_format, preview.native_output_format
    ));
    ui.label(format!(
        "Processing formats: {:?} → {:?}; quantum {} frames",
        preview.processing_input_format, preview.processing_output_format, preview.quantum_frames
    ));
    ui.label(format!(
        "Preservation decision: {:?}; transformations {:?}",
        preview.preservation, preview.transformations
    ));
    ui.label(format!("Owner resident/preparation observations: {:?}; channel peaks {:?}; required headroom {} dB", preview.resources, preview.channel_peak_bounds, preview.required_headroom_db));
    ui.label(format!(
        "Timing: {:?}; transition {:?}",
        preview.timing, preview.transition
    ));
    ui.label(format!(
        "Profile application binding: {:?}",
        preview.profile_application
    ));
    ui.label("These compiled facts do not establish audible playback or measured DAC fidelity.");
}
