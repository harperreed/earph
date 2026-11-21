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
    let dsd = state.as_ref().and_then(|s| s.dsd_playback.as_ref());
    let unit = if dsd.is_some() {
        "Source bits per channel"
    } else {
        "Source frame"
    };
    field(
        ui,
        unit,
        if dsd.is_some() {
            "transport-source-bits"
        } else {
            "transport-source-frame"
        },
        &mut app.transport.frame,
        20,
    );
    let bounds = dsd
        .map(|d| (&d.source_entry_id, d.total_source_bits))
        .or_else(|| app.controller.seek_bounds());
    if let Some((entry, total)) = bounds {
        ui.label(format!(
            "Seekable active entry: {}; {unit} bounds 0 through {total}",
            entry.as_str(),
        ));
    } else {
        ui.label("Source seek bounds: unknown or unavailable");
    }
    if ui
        .add_enabled(
            enabled && bounds.is_some(),
            egui::Button::new(if dsd.is_some() {
                "Seek to source bits"
            } else {
                "Seek to source frame"
            }),
        )
        .clicked()
    {
        match number::<u64>(unit, &app.transport.frame) {
            Ok(position_frames)
                if bounds.is_some_and(|(_, total)| position_frames <= total)
                    && (dsd.is_none() || position_frames.is_multiple_of(16)) =>
            {
                app.transport.error = None;
                command = Some(if dsd.is_some() {
                    Command::SeekDsd {
                        position_bits: position_frames,
                    }
                } else {
                    Command::Seek { position_frames }
                });
            }
            Ok(_) => {
                app.transport.error = Some(
                    if dsd.is_some() {
                        "DSD seek requires sixteen-bit alignment within the observed entry"
                    } else {
                        "Source frame exceeds the observed active entry"
                    }
                    .into(),
                )
            }
            Err(error) => app.transport.error = Some(error),
        }
    }
    if let Some(error) = &app.transport.error {
        ui.label(format!("Transport draft error: {error}"));
    }
    if let Some(state) = &state {
        if let Some(dsd) = &state.dsd_playback {
            ui.label(format!("DSD source: {} bits per second per channel; accepted source position: {} of {} bits; decoded: {} bits",dsd.source_format.bit_rate_hz(),dsd.source_position_bits,dsd.total_source_bits,dsd.decoded_position_bits));
            ui.label(format!("DoP carrier: {} frames per second; submitted carrier frames: {}; endpoint qualification: unverified",dsd.carrier_format.carrier_rate_hz(),dsd.output.as_ref().map_or_else(||"output closed".into(),|o|o.submitted_carrier_frames.to_string())));
            ui.label("DSD pause and seek use the accepted source frontier. Audible position is unknown. Public DSD output requires endpoint qualification.");
        } else {
            ui.label(format!(
                "Decoded position: {} frames; audible position: {}",
                state.decoded_position_frames,
                state
                    .audible_position_frames
                    .map_or_else(|| "unknown".into(), |n| format!("{n} frames"))
            ));
        }
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
