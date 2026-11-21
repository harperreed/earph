// ABOUTME: Submits current-source transport actions against observed owner context.
// ABOUTME: Uses source-frame seek bounds and never joins playback with consent or Unmute.
use super::{
    GuiApp, UiIntent,
    app::field,
    output::{number, submit},
};
use earph_core::control::Command;
use eframe::egui;
use std::sync::Arc;

#[derive(Default)]
pub(super) struct Draft {
    takeover: bool,
    frame: String,
    error: Option<String>,
}
pub(super) fn show(ui: &mut egui::Ui, app: &mut GuiApp) {
    ui.heading("Transport");
    ui.checkbox(
        &mut app.transport.takeover,
        "Allow takeover of the retained session",
    );
    let selected = app.controller.queue().and_then(|q| q.selected.clone());
    ui.label(format!(
        "Retained local queue selection: {}",
        selected.as_ref().map_or("none", |id| id.as_str())
    ));
    let state = app.controller.state().cloned();
    let enabled = app.controller.control_edit_available();
    let mut command = None;
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(
                app.controller.queue_edit_available() && selected.is_some(),
                egui::Button::new("Start selected local queue"),
            )
            .clicked()
            && let Some(state) = &state
        {
            app.controller.handle(UiIntent::StartLocal {
                basis: Arc::new(state.clone()),
                takeover: app.transport.takeover,
            });
        }
        if ui
            .add_enabled(
                enabled && state.as_ref().is_some_and(|s| s.session_id.is_some()),
                egui::Button::new("Pause"),
            )
            .clicked()
        {
            command = Some(Command::Pause);
        }
        if ui
            .add_enabled(
                enabled
                    && state
                        .as_ref()
                        .is_some_and(|s| s.transport == crate::TransportView::Paused),
                egui::Button::new("Resume"),
            )
            .clicked()
        {
            command = Some(Command::Resume);
        }
    });
    field(
        ui,
        "Source frame",
        "transport-source-frame",
        &mut app.transport.frame,
        20,
    );
    let bounds = app.controller.seek_bounds();
    if let Some((entry, total)) = bounds {
        ui.label(format!(
            "Seekable active entry: {}; source frame bounds 0 through {total}",
            entry.as_str()
        ));
    } else {
        ui.label("Source seek bounds: unknown or unavailable");
    }
    if ui
        .add_enabled(
            enabled && bounds.is_some(),
            egui::Button::new("Seek to source frame"),
        )
        .clicked()
    {
        match number::<u64>("Source frame", &app.transport.frame) {
            Ok(position_frames) if bounds.is_some_and(|(_, total)| position_frames <= total) => {
                app.transport.error = None;
                command = Some(Command::Seek { position_frames });
            }
            Ok(_) => {
                app.transport.error = Some("Source frame exceeds the observed active entry".into())
            }
            Err(error) => app.transport.error = Some(error),
        }
    }
    if let Some(error) = &app.transport.error {
        ui.label(format!("Transport draft error: {error}"));
    }
    if let Some(state) = &state {
        ui.label(format!(
            "Decoded position: {} frames; audible position: {}",
            state.decoded_position_frames,
            state
                .audible_position_frames
                .map_or_else(|| "unknown".into(), |n| format!("{n} frames"))
        ));
        if let Some(playback) = state.playback_facts() {
            ui.label(format!("Source position: {} frames; submitted output: {} frames; consumed output: {}; audible source position: {}", playback.source_position_frames, playback.submitted_frames, playback.consumed_frames.map_or_else(|| "unknown".into(), |n| format!("{n} frames")), playback.audible_source_position_frames.map_or_else(|| "unknown".into(), |n| format!("{n} frames"))));
        }
    }
    if state
        .as_ref()
        .is_some_and(|s| s.source == Some(earph_core::control::SourceKind::HttpFlac))
    {
        ui.label("The local queue remains local intent. HTTP FLAC does not support seek, next, previous, profiles, or strict preservation.");
    }
    ui.label("Accepted playback may remain pending while muted. Backend readiness does not mean Applied or audible playback.");
    if let Some(command) = command {
        submit(app, command);
    }
}
