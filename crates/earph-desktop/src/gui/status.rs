// ABOUTME: Displays actual owner safety and transport state with explicit freshness.
// ABOUTME: Keeps Mute and Stop independent of ordinary view work without adding policy.
use super::{GuiController, UiIntent};
use eframe::egui;

pub(super) fn show(ui: &mut egui::Ui, controller: &mut GuiController) {
    ui.label(if controller.connected() {
        "Connected: last observed owner state"
    } else {
        "Disconnected or outcome unknown: values below are last observed"
    });
    if let Some(time) = controller.observed_at() {
        ui.label(format!(
            "Last state received {:.1} s ago (local time)",
            time.elapsed().as_secs_f32()
        ));
    }
    if let Some(state) = controller.state() {
        ui.label(format!(
            "Owner: {} · Session: {} · Zone: {} · Revision: {}",
            state.owner_id.as_ref().map_or("unknown", |id| id.as_str()),
            state.session_id.as_ref().map_or("none", |id| id.as_str()),
            state.zone_id.as_str(),
            state.revision
        ));
        ui.label(format!(
            "Transport: {:?} · Source: {:?} · Muted: {} · Software gain: {:.3} dB",
            state.transport, state.source, state.muted, state.software_gain_db
        ));
        ui.label(format!(
            "Caps: software {:.3} dB · hardware {} · consent: {}",
            state.volume_policy.software_cap_db,
            state
                .volume_policy
                .hardware_cap_db
                .map_or_else(|| "unknown".into(), |n| format!("{n:.3} dB")),
            state.output_consented
        ));
        ui.label(format!(
            "Output: {} · Pending operation: {} · Error: {:?}",
            state.output_id.as_ref().map_or("none", |id| id.as_str()),
            state.operation.as_ref().map_or("none", |id| id.as_str()),
            state.error
        ));
        ui.collapsing("State details", |ui| {
            ui.label(format!("Epochs: {:?}; graph {}; device {}; event sequence {}", state.epochs, state.graph_epoch, state.device_epoch, state.event_sequence));
            ui.label(format!("Decoded position: {} frames; audible position: {}", state.decoded_position_frames, state.audible_position_frames.map_or_else(|| "unknown".into(), |n| format!("{n} frames"))));
            ui.label(format!("Requested output: {:?}", state.output));
            ui.label("Fixed-output arming status is not exposed by State. Software gain does not establish acoustic safety.");
        });
    } else {
        ui.label("No owner snapshot has been observed.");
    }
    if let Some(command) = controller.waiting_queue_edit() {
        ui.label(format!(
            "Queue edit waiting for background observation (not sent): {command:?}"
        ));
    } else if controller.queue_edit_pending() {
        ui.label("Queue edit preflight or mutation pending. Inspect its receipts below.");
    }
    ui.horizontal(|ui| {
        let enabled = controller.attached() && controller.state().is_some();
        if ui.add_enabled(enabled, egui::Button::new("Mute")).clicked() {
            controller.handle(UiIntent::Mute);
        }
        if ui.add_enabled(enabled, egui::Button::new("Stop")).clicked() {
            controller.handle(UiIntent::Stop);
        }
    });
    if let Some(error) = controller.error() {
        ui.group(|ui| {
            ui.label(format!("Action / connection error: {error}"));
        });
    }
}
