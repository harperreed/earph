// ABOUTME: Edits local output intent and submits each safety action separately.
// ABOUTME: Separates advertised discovery, selected configuration, and opened backend facts.
use super::{GuiApp, UiIntent, app::field, transport};
use crate::{DeviceObservationView, DeviceQueryResultView};
use earph_core::{
    control::{Command, OutputBackend, OutputConfiguration, OutputRoute},
    ids::{DeviceId, ProfileId},
    safety::{FidelityPolicy, VolumeMode},
};
use eframe::egui;
use std::{str::FromStr, sync::Arc};

pub(super) struct Draft {
    backend: Option<OutputBackend>,
    route: OutputRoute,
    endpoint: String,
    device: String,
    profile: String,
    period: String,
    buffer: String,
    mode: VolumeMode,
    fidelity: FidelityPolicy,
    level: String,
    ramp: String,
    software_cap: String,
    hardware_cap: String,
    error: Option<String>,
}
impl Default for Draft {
    fn default() -> Self {
        Self {
            backend: None,
            route: OutputRoute::System,
            endpoint: String::new(),
            device: String::new(),
            profile: String::new(),
            period: "64".into(),
            buffer: "256".into(),
            mode: VolumeMode::Software,
            fidelity: FidelityPolicy::CompatibleConversion,
            level: "-30000".into(),
            ramp: "64".into(),
            software_cap: "-20000".into(),
            hardware_cap: String::new(),
            error: None,
        }
    }
}
impl Draft {
    fn configuration(&self) -> Result<OutputConfiguration, String> {
        let output = OutputConfiguration {
            backend: self.backend.ok_or("Choose an explicit backend")?,
            route: self.route,
            endpoint: self.endpoint.clone(),
            device_id: DeviceId::new(&self.device).map_err(|e| e.to_string())?,
            profile_id: ProfileId::new(&self.profile).map_err(|e| e.to_string())?,
            period_frames: number("Period frames", &self.period)?,
            buffer_frames: number("Buffer frames", &self.buffer)?,
            volume_mode: self.mode,
            fidelity: self.fidelity,
        };
        output.validate().map_err(|e| e.to_string())?;
        Ok(output)
    }
}
pub(super) fn number<T: FromStr>(label: &str, text: &str) -> Result<T, String> {
    text.parse()
        .map_err(|_| format!("{label} must be a valid whole number"))
}
pub(super) fn submit(app: &mut GuiApp, command: Command) {
    if let Some(state) = app.controller.state() {
        app.controller.handle(UiIntent::ControlEdit {
            basis: Arc::new(state.clone()),
            command,
        });
    }
}
fn fidelity_policy(ui: &mut egui::Ui, draft: &mut Draft, network: bool) {
    ui.label("Requested fidelity policy");
    ui.horizontal_wrapped(|ui| {
        ui.add_enabled_ui(!network, |ui| {
            ui.selectable_value(
                &mut draft.fidelity,
                FidelityPolicy::StrictPreservation,
                "StrictPreservation",
            );
        });
        ui.selectable_value(
            &mut draft.fidelity,
            FidelityPolicy::CompatibleConversion,
            "CompatibleConversion",
        );
        ui.selectable_value(
            &mut draft.fidelity,
            FidelityPolicy::ExpertOverride,
            "ExpertOverride",
        );
    });
}

pub(super) fn show(ui: &mut egui::Ui, app: &mut GuiApp) {
    ui.heading("Output / Safety");
    ui.label("Local draft: editing fields changes no owner state.");
    discovery(ui, app);
    ui.separator();
    let network = app
        .controller
        .state()
        .is_some_and(|s| s.source == Some(earph_core::control::SourceKind::HttpFlac));
    let draft = &mut app.output;
    ui.label("Requested backend");
    ui.horizontal_wrapped(|ui| {
        ui.selectable_value(&mut draft.backend, Some(OutputBackend::Alsa), "ALSA");
        ui.selectable_value(
            &mut draft.backend,
            Some(OutputBackend::CoreAudio),
            "Core Audio",
        );
    });
    ui.label("Requested route");
    ui.horizontal_wrapped(|ui| {
        ui.selectable_value(&mut draft.route, OutputRoute::System, "System route");
        ui.selectable_value(
            &mut draft.route,
            OutputRoute::DirectHardware,
            "Direct hardware route",
        );
    });
    field(ui, "Endpoint", "output-endpoint", &mut draft.endpoint, 4096);
    field(
        ui,
        "Configured device ID",
        "output-device",
        &mut draft.device,
        128,
    );
    field(
        ui,
        "Configured profile ID",
        "output-profile",
        &mut draft.profile,
        128,
    );
    ui.label("Configured IDs are local identifiers; they are not observed native identity.");
    field(ui, "Period frames", "output-period", &mut draft.period, 10);
    field(ui, "Buffer frames", "output-buffer", &mut draft.buffer, 10);
    ui.label("Requested volume mode");
    ui.horizontal_wrapped(|ui| {
        ui.selectable_value(&mut draft.mode, VolumeMode::Software, "Software");
        ui.selectable_value(&mut draft.mode, VolumeMode::Fixed, "Fixed");
    });
    fidelity_policy(ui, draft, network);
    if network {
        ui.label("HTTP FLAC requires flexible PCM fidelity; StrictPreservation is unavailable.");
    }
    let enabled = app.controller.control_edit_available();
    let mut command = None;
    if ui
        .add_enabled(
            enabled && !(network && draft.fidelity == FidelityPolicy::StrictPreservation),
            egui::Button::new("Select output"),
        )
        .clicked()
    {
        match draft.configuration() {
            Ok(output) => {
                draft.error = None;
                command = Some(Command::OutputSelect { output });
            }
            Err(error) => draft.error = Some(error),
        }
    }
    if let Some(error) = &draft.error {
        ui.label(format!("Draft error: {error}"));
    }
    ui.separator();
    let state = app.controller.state().cloned();
    if let Some(state) = &state {
        match &state.output {
            Some(output) => {
                ui.label(format!("Selected output intent: {output:?}"));
            }
            None => {
                ui.label("Selected output intent: none");
            }
        }
        ui.label(format!("Consent observed: {}", state.output_consented));
        ui.label(format!(
            "Local caps: software {:.3} dB; configured hardware {:?} dB",
            state.local_volume_policy.software_cap_db, state.local_volume_policy.hardware_cap_db
        ));
        ui.label(format!(
            "Effective caps: software {:.3} dB; configured hardware {:?} dB",
            state.volume_policy.software_cap_db, state.volume_policy.hardware_cap_db
        ));
        ui.label(format!(
            "Actual software gain: {:.3} dB; ramp {} frames",
            state.software_gain_db, state.gain_ramp_frames
        ));
        if let Some(backend) = state.playback_facts().and_then(|p| p.backend) {
            ui.label(format!(
                "Opened backend: {:?}; native identity {}",
                backend.backend,
                backend
                    .device_identity_sha256
                    .as_ref()
                    .map_or("unknown", |h| h.as_str())
            ));
            ui.label(format!(
                "Opened client format: {:?}; device format {}",
                backend.client_format,
                backend
                    .device_format
                    .as_ref()
                    .map_or_else(|| "unknown".into(), |format| format!("{format:?}"))
            ));
            ui.label(format!(
                "Backend measurement boundary: {}; preservation {:?}",
                backend.measurement_boundary, backend.preservation
            ));
        } else {
            ui.label("Opened backend: unknown");
        }
    } else {
        ui.label("Selected output intent: unknown; no owner observation");
    }
    ui.label("Hardware volume: unsupported; readback unknown");
    let selected = state.as_ref().and_then(|s| s.output.as_ref());
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(
                enabled && selected.is_some(),
                egui::Button::new("Approve current output"),
            )
            .clicked()
            && let Some(output) = selected
        {
            command = Some(Command::OutputConsent {
                device_id: output.device_id.clone(),
                profile_id: output.profile_id.clone(),
            });
        }
        if ui
            .add_enabled(
                enabled && selected.is_some_and(|o| o.volume_mode == VolumeMode::Fixed),
                egui::Button::new("Arm current fixed output"),
            )
            .clicked()
            && let Some(output) = selected
        {
            command = Some(Command::FixedArm {
                device_id: output.device_id.clone(),
                profile_id: output.profile_id.clone(),
            });
        }
        if ui
            .add_enabled(
                enabled && selected.is_some(),
                egui::Button::new("Unmute current output"),
            )
            .clicked()
        {
            command = Some(Command::Mute { muted: false });
        }
    });
    ui.label("Fixed arming has no State readback. Its receipt records a past action only.");
    field(
        ui,
        "Software level (millidecibels)",
        "output-level",
        &mut draft.level,
        12,
    );
    field(ui, "Ramp frames", "output-ramp", &mut draft.ramp, 10);
    if ui
        .add_enabled(enabled, egui::Button::new("Set software level"))
        .clicked()
    {
        match number("Software level", &draft.level).and_then(|millidecibels| {
            Ok(Command::VolumeSet {
                millidecibels,
                ramp_frames: number("Ramp frames", &draft.ramp)?,
            })
        }) {
            Ok(value) => {
                draft.error = None;
                command = Some(value);
            }
            Err(error) => draft.error = Some(error),
        }
    }
    field(
        ui,
        "Software cap (millidecibels)",
        "output-software-cap",
        &mut draft.software_cap,
        12,
    );
    field(
        ui,
        "Hardware cap (millidecibels, optional)",
        "output-hardware-cap",
        &mut draft.hardware_cap,
        12,
    );
    if ui
        .add_enabled(enabled, egui::Button::new("Set independent caps"))
        .clicked()
    {
        match number("Software cap", &draft.software_cap).and_then(|software_millidecibels| {
            Ok(Command::VolumeCaps {
                software_millidecibels,
                hardware_millidecibels: if draft.hardware_cap.is_empty() {
                    None
                } else {
                    Some(number("Hardware cap", &draft.hardware_cap)?)
                },
            })
        }) {
            Ok(value) => {
                draft.error = None;
                command = Some(value);
            }
            Err(error) => draft.error = Some(error),
        }
    }
    if let Some(command) = command {
        submit(app, command);
    }
    ui.separator();
    transport::show(ui, app);
}
fn discovery(ui: &mut egui::Ui, app: &mut GuiApp) {
    ui.label("Owner discovery: advertised facts; no output is opened.");
    if let Some(caps) = app.controller.capabilities() {
        ui.label(format!(
            "Discovery backends advertised: {:?}",
            caps.devices.backends
        ));
    }
    let backend = app.output.backend;
    let enabled = app.controller.control_edit_available()
        && backend.is_some_and(|b| {
            app.controller
                .capabilities()
                .is_some_and(|c| c.devices.backends.contains(&b))
        });
    let mut query = None;
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(enabled, egui::Button::new("Refresh devices"))
            .clicked()
        {
            query = backend.map(|b| (b, None));
        }
        if ui
            .add_enabled(
                enabled && !app.output.endpoint.is_empty(),
                egui::Button::new("Inspect endpoint"),
            )
            .clicked()
        {
            query = backend.map(|b| (b, Some(app.output.endpoint.clone())));
        }
    });
    if let Some(result) = app.controller.discovery() {
        observation_header(ui, result);
        match &result.observation {
            DeviceObservationView::List {
                devices,
                complete,
                failures,
            } => {
                ui.label(format!(
                    "Inventory complete: {complete}; failures: {failures:?}"
                ));
                for (index, device) in devices.iter().enumerate() {
                    ui.push_id(("device", index), |ui| {
                        ui.group(|ui| {
                            ui.label(format!(
                                "Advertised name: {}",
                                device.display_name.as_deref().unwrap_or("unknown")
                            ));
                            ui.label(format!("Exact endpoint: {}", device.endpoint));
                            ui.label(format!(
                                "Description: {}",
                                device.description.as_deref().unwrap_or("unknown")
                            ));
                            ui.label(format!("Selection error: {:?}", device.selection_error));
                            if let Some(selection) = &device.selection
                                && ui.button("Copy backend and endpoint to draft").clicked()
                            {
                                app.output.backend = Some(selection.backend);
                                app.output.endpoint.clone_from(&selection.endpoint);
                            }
                            let inspectable = app.controller.control_edit_available()
                                && app
                                    .controller
                                    .capabilities()
                                    .is_some_and(|c| c.devices.backends.contains(&result.backend));
                            if ui
                                .add_enabled(
                                    inspectable,
                                    egui::Button::new("Inspect this endpoint"),
                                )
                                .clicked()
                            {
                                query = Some((result.backend, Some(device.endpoint.clone())));
                            }
                        });
                    });
                }
            }
            other => {
                ui.label(format!("Discovery observation: {other:?}"));
            }
        }
    } else {
        ui.label("No device inventory observed. Refresh devices explicitly.");
    }
    if let Some(result) = app.controller.inspection() {
        observation_header(ui, result);
        ui.label(format!("Inspection observation: {:?}", result.observation));
        ui.label("Absent current properties remain unknown; advertised formats do not prove opened playback.");
    }
    if let Some((backend, endpoint)) = query {
        app.controller
            .handle(UiIntent::Discover { backend, endpoint });
    }
}
fn observation_header(ui: &mut egui::Ui, result: &DeviceQueryResultView) {
    ui.label(format!(
        "Discovery owner: {}; backend {:?}; read boundary {:?}; completed {} ns",
        result.owner_id.as_str(),
        result.backend,
        result.read_boundary,
        result.completed_monotonic_ns
    ));
}

#[cfg(test)]
mod network_tests {
    use super::*;
    use crate::network_fixture;
    #[test]
    fn network_fidelity_widgets_disable_strict_preservation_and_keep_flexible_choices() {
        let session = network_fixture::ready(None);
        let state = session.snapshot();
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut draft = Draft::default();
        let mut output = context.run_ui(egui::RawInput::default(), |ui| {
            fidelity_policy(
                ui,
                &mut draft,
                state.source == Some(earph_core::control::SourceKind::HttpFlac),
            )
        });
        output.textures_delta.clear();
        let tree = output.platform_output.accesskit_update.unwrap();
        let strict = tree
            .nodes
            .iter()
            .find(|(_, n)| {
                n.label() == Some("StrictPreservation")
                    && n.role() != egui::accesskit::Role::TextRun
            })
            .unwrap();
        assert!(strict.1.is_disabled());
        let flexible = tree
            .nodes
            .iter()
            .find(|(_, n)| {
                n.label() == Some("CompatibleConversion")
                    && n.role() != egui::accesskit::Role::TextRun
            })
            .unwrap();
        assert!(!flexible.1.is_disabled());
    }
}
