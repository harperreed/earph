// ABOUTME: Edits plain saved listening intent and presents exact catalog and resolution facts.
// ABOUTME: Keeps local drafts and selected saved references separate from owner application.
use super::{
    GuiApp, UiIntent, app::field, correction, dsp, graph_editor::GraphIntentEditor, output::number,
};
use crate::ProfileRecordView;
use earph_core::{
    control::{Command, OutputBackend, OutputRoute},
    ids::{DeviceId, OwnerId, ProfileId},
    profile_control::{ProfileCatalogReference, ProfileDraft, ProfileVolumeDraft},
    profiles::{
        ProfileLatencyIntent, ProfileLatencyMode, ProfileOutputSelector, ProfileSourcePolicy,
    },
    safety::{FidelityPolicy, VolumeMode},
};
use eframe::egui;
use std::sync::Arc;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Draft {
    id: String,
    name: String,
    backend: Option<OutputBackend>,
    route: OutputRoute,
    device: String,
    setup: String,
    mode: VolumeMode,
    fidelity: FidelityPolicy,
    software_cap: String,
    hardware_cap: String,
    latency: ProfileLatencyMode,
    resident: String,
    preparation: String,
    latency_frames: String,
    pub selected: Option<ProfileCatalogReference>,
    observed_owner: Option<OwnerId>,
    corrected: bool,
    error: Option<String>,
    loaded: Option<ProfileCatalogReference>,
}
impl Default for Draft {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            backend: None,
            route: OutputRoute::System,
            device: String::new(),
            setup: String::new(),
            mode: VolumeMode::Software,
            fidelity: FidelityPolicy::CompatibleConversion,
            software_cap: "-20000".into(),
            hardware_cap: String::new(),
            latency: ProfileLatencyMode::Balanced,
            resident: "268435456".into(),
            preparation: "268435456".into(),
            latency_frames: String::new(),
            selected: None,
            observed_owner: None,
            corrected: false,
            error: None,
            loaded: None,
        }
    }
}
impl Draft {
    pub(super) fn graph_review_required(&self) -> Option<&ProfileCatalogReference> {
        self.loaded.as_ref().filter(|_| self.corrected)
    }
    fn selector(&self) -> Result<ProfileOutputSelector, String> {
        Ok(ProfileOutputSelector {
            backend: self.backend.ok_or("Choose an explicit profile backend")?,
            route: self.route,
            device_id: DeviceId::new(&self.device).map_err(|e| e.to_string())?,
            manual_setup: (!self.setup.is_empty()).then(|| self.setup.clone()),
        })
    }
    fn document(&self) -> Result<ProfileDraft, String> {
        if self.corrected {
            return Err(
                "This saved profile contains corrections; use explicit correction import".into(),
            );
        }
        self.correction_document()
    }
    pub(super) fn correction_document(&self) -> Result<ProfileDraft, String> {
        Ok(ProfileDraft {
            name: self.name.clone(),
            source_policy: ProfileSourcePolicy::LocalFile,
            fidelity: self.fidelity,
            output: self.selector()?,
            volume: ProfileVolumeDraft {
                mode: self.mode,
                software_cap_millidecibels: number("Profile software cap", &self.software_cap)?,
                hardware_cap_millidecibels: if self.hardware_cap.is_empty() {
                    None
                } else {
                    Some(number("Profile hardware cap", &self.hardware_cap)?)
                },
            },
            latency: ProfileLatencyIntent {
                mode: self.latency,
                max_resident_bytes: number("Profile resident limit", &self.resident)?,
                max_preparation_bytes: number("Profile preparation limit", &self.preparation)?,
                max_latency_frames: if self.latency_frames.is_empty() {
                    None
                } else {
                    Some(number("Profile latency frames", &self.latency_frames)?)
                },
            },
            corrections: Vec::new(),
        })
    }
    pub(super) fn import_target(&self, update: bool) -> Result<(ProfileId, Option<u64>), String> {
        let id = ProfileId::new(&self.id).map_err(|e| e.to_string())?;
        if !update {
            return Ok((id, None));
        }
        let reference = self
            .selected
            .as_ref()
            .filter(|r| self.loaded.as_ref() == Some(r) && r.id == id)
            .ok_or(
                "Load the exact selected revision and retain its ID before importing an update",
            )?;
        Ok((id, Some(reference.revision)))
    }
    fn load(&mut self, record: &ProfileRecordView) -> Result<(), String> {
        let profile = &record.profile;
        if profile.volume.mode == VolumeMode::Hardware {
            return Err(
                "Hardware profile editing is unavailable; saved intent remains unchanged".into(),
            );
        }
        let software_cap = millidecibels(profile.volume.software_cap_db)?;
        let hardware_cap = profile
            .volume
            .hardware_cap_db
            .map(millidecibels)
            .transpose()?
            .map_or_else(String::new, |v| v.to_string());
        self.id = profile.id.as_str().into();
        self.name = profile.name.clone();
        self.backend = Some(profile.output.backend);
        self.route = profile.output.route;
        self.device = profile.output.device_id.as_str().into();
        self.setup = profile.output.manual_setup.clone().unwrap_or_default();
        self.mode = profile.volume.mode;
        self.fidelity = profile.fidelity;
        self.software_cap = software_cap.to_string();
        self.hardware_cap = hardware_cap;
        self.latency = profile.latency.mode;
        self.resident = profile.latency.max_resident_bytes.to_string();
        self.preparation = profile.latency.max_preparation_bytes.to_string();
        self.latency_frames = profile
            .latency
            .max_latency_frames
            .map_or_else(String::new, |v| v.to_string());
        self.corrected = !profile.corrections.is_empty();
        self.error = None;
        self.loaded = Some(ProfileCatalogReference {
            id: profile.id.clone(),
            revision: profile.revision,
            profile_sha256: record.profile_sha256,
        });
        Ok(())
    }
}
fn millidecibels(db: f64) -> Result<i32, String> {
    let milli = (db * 1000.0).round();
    if !db.is_finite()
        || milli < f64::from(i32::MIN)
        || milli > f64::from(i32::MAX)
        || f64::from(milli as i32) / 1000.0 != db
    {
        return Err(
            "Saved cap cannot be represented exactly by the millidecibel command form".into(),
        );
    }
    Ok(milli as i32)
}
pub(super) fn action(app: &mut GuiApp, command: Command) {
    if let Some(state) = app.controller.state() {
        app.controller.handle(UiIntent::ProfileAction {
            basis: Arc::new(state.clone()),
            command,
        });
    }
}
pub(super) fn selected_record(app: &GuiApp) -> Option<&ProfileRecordView> {
    let reference = app.profiles.selected.as_ref()?;
    app.controller.profile_record().filter(|r| {
        r.profile.id == reference.id
            && r.profile.revision == reference.revision
            && r.profile_sha256 == reference.profile_sha256
    })
}
pub(super) fn show(ui: &mut egui::Ui, app: &mut GuiApp) {
    let mut before = app.profiles.clone();
    before.error = None;
    ui.heading("Profiles / DSP");
    let owner = app.controller.state().and_then(|s| s.owner_id.clone());
    if !app.controller.attached() || app.profiles.observed_owner != owner {
        app.profiles.selected = None;
        app.profiles.observed_owner = if app.controller.attached() {
            owner
        } else {
            None
        };
    }
    let enabled = app.controller.control_edit_available();
    let save = enabled
        && !app.graph.has_pending_raw_edits()
        && app
            .controller
            .capabilities()
            .is_some_and(|c| c.profiles.save);
    let mut command = None;
    if ui
        .add_enabled(enabled, egui::Button::new("Refresh profiles"))
        .clicked()
    {
        command = Some(Command::ProfilesList {
            offset: 0,
            limit: 16,
        });
    }
    let mut discard_prepared = false;
    if let Some(catalog) = &app.controller.profiles.catalog {
        ui.label(format!(
            "Catalog owner: {}; observed revision {}; page requests {}",
            catalog
                .basis
                .owner_id
                .as_ref()
                .map(OwnerId::as_str)
                .unwrap_or("none"),
            catalog.basis.revision,
            catalog
                .request_ids
                .iter()
                .map(|id| id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        let operation = &catalog.value.operation;
        ui.label(format!(
            "Profile worker: {}; request {}; deadline {} ns",
            operation
                .stage
                .as_ref()
                .map(|stage| format!("{stage:?}"))
                .unwrap_or_else(|| "idle".into()),
            operation
                .request_id
                .as_ref()
                .map(|id| id.as_str())
                .unwrap_or("none"),
            operation
                .deadline_monotonic_ns
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unavailable".into())
        ));
        for entry in &catalog.value.entries {
            let reference = ProfileCatalogReference {
                id: entry.id.clone(),
                revision: entry.revision,
                profile_sha256: entry.profile_sha256,
            };
            let selected = app.profiles.selected.as_ref() == Some(&reference);
            if ui
                .selectable_label(
                    selected,
                    format!(
                        "{} · {} · revision {} · {}",
                        entry.name,
                        entry.id.as_str(),
                        entry.revision,
                        entry.profile_sha256.as_str()
                    ),
                )
                .clicked()
            {
                app.profiles.selected = Some(reference);
                discard_prepared = true;
            }
        }
    } else {
        ui.label("No fenced profile catalog observed.");
    }
    if discard_prepared {
        app.controller.handle(UiIntent::DiscardPreparedGraph);
    }
    let selected = app.profiles.selected.clone();
    ui.label(match &selected {
        Some(reference) => format!(
            "Selected saved reference (local choice): {} · revision {} · {}",
            reference.id.as_str(),
            reference.revision,
            reference.profile_sha256.as_str()
        ),
        None => "Selected saved reference (local choice): none".into(),
    });
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(
                enabled && selected.is_some(),
                egui::Button::new("Get selected profile"),
            )
            .clicked()
            && let Some(reference) = &selected
        {
            command = Some(Command::ProfilesGet {
                id: reference.id.clone(),
            });
        }
        if ui
            .add_enabled(
                enabled && selected_record(app).is_some(),
                egui::Button::new("Load selected profile into draft"),
            )
            .clicked()
            && let Some(record) = selected_record(app).cloned()
        {
            match app.profiles.load(&record) {
                Ok(()) => {
                    app.graph.load((*record.graph).clone());
                    app.controller.handle(UiIntent::DiscardPreparedGraph);
                }
                Err(error) => app.profiles.error = Some(error),
            }
        }
        if ui.button("New plain profile draft").clicked() {
            let observed_owner = app.profiles.observed_owner.clone();
            app.profiles = Draft {
                observed_owner,
                ..Draft::default()
            };
            app.graph = GraphIntentEditor::default();
            app.controller.handle(UiIntent::DiscardPreparedGraph);
        }
    });
    if let Some(observed) = &app.controller.profiles.record {
        ui.label(format!(
            "Observed saved profile: {} revision {} hash {}; captured owner {}; requests {}",
            observed.value.profile.id.as_str(),
            observed.value.profile.revision,
            observed.value.profile_sha256.as_str(),
            observed
                .basis
                .owner_id
                .as_ref()
                .map(OwnerId::as_str)
                .unwrap_or("none"),
            observed
                .request_ids
                .iter()
                .map(|id| id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
        saved_facts(ui, &observed.value);
    }
    ui.separator();
    ui.label("Local profile draft: saving does not select output, apply processing, consent, arm, change level, or unmute.");
    let draft = &mut app.profiles;
    field(ui, "Profile ID", "profile-id", &mut draft.id, 128);
    field(ui, "Profile name", "profile-name", &mut draft.name, 256);
    ui.label("Profile source policy: LocalFile");
    ui.horizontal_wrapped(|ui| {
        ui.selectable_value(
            &mut draft.backend,
            Some(OutputBackend::Alsa),
            "Profile ALSA",
        );
        ui.selectable_value(
            &mut draft.backend,
            Some(OutputBackend::CoreAudio),
            "Profile Core Audio",
        );
        ui.selectable_value(
            &mut draft.route,
            OutputRoute::System,
            "Profile System route",
        );
        ui.selectable_value(
            &mut draft.route,
            OutputRoute::DirectHardware,
            "Profile Direct hardware route",
        );
    });
    field(
        ui,
        "Profile configured device ID",
        "profile-device",
        &mut draft.device,
        128,
    );
    field(
        ui,
        "Manual listening setup",
        "profile-setup",
        &mut draft.setup,
        128,
    );
    ui.label("Saved output selectors contain no endpoint or trusted native identity.");
    ui.horizontal_wrapped(|ui| {
        ui.selectable_value(&mut draft.mode, VolumeMode::Software, "Profile Software");
        ui.selectable_value(&mut draft.mode, VolumeMode::Fixed, "Profile Fixed");
    });
    ui.label("Profile Hardware mode: unavailable");
    ui.horizontal_wrapped(|ui| {
        ui.selectable_value(
            &mut draft.fidelity,
            FidelityPolicy::StrictPreservation,
            "Profile StrictPreservation",
        );
        ui.selectable_value(
            &mut draft.fidelity,
            FidelityPolicy::CompatibleConversion,
            "Profile CompatibleConversion",
        );
        ui.selectable_value(
            &mut draft.fidelity,
            FidelityPolicy::ExpertOverride,
            "Profile ExpertOverride",
        );
    });
    field(
        ui,
        "Profile software cap (millidecibels)",
        "profile-software-cap",
        &mut draft.software_cap,
        12,
    );
    field(
        ui,
        "Profile hardware cap (millidecibels, optional)",
        "profile-hardware-cap",
        &mut draft.hardware_cap,
        12,
    );
    ui.horizontal_wrapped(|ui| {
        ui.selectable_value(
            &mut draft.latency,
            ProfileLatencyMode::QualityFocused,
            "QualityFocused",
        );
        ui.selectable_value(&mut draft.latency, ProfileLatencyMode::Balanced, "Balanced");
        ui.selectable_value(
            &mut draft.latency,
            ProfileLatencyMode::LowLatency,
            "LowLatency",
        );
    });
    field(
        ui,
        "Profile resident byte limit",
        "profile-resident",
        &mut draft.resident,
        20,
    );
    field(
        ui,
        "Profile preparation byte limit",
        "profile-preparation",
        &mut draft.preparation,
        20,
    );
    field(
        ui,
        "Profile latency frame limit (optional)",
        "profile-latency-frames",
        &mut draft.latency_frames,
        20,
    );
    if draft.corrected {
        ui.label("Correction intent is retained. Use correction import to replace the loaded corrected revision.");
    }
    let plain = !draft.corrected;
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(save && plain, egui::Button::new("Create profile"))
            .clicked()
        {
            match app.profiles.document().and_then(|draft| {
                Ok(Command::ProfilesCreate {
                    id: ProfileId::new(&app.profiles.id).map_err(|e| e.to_string())?,
                    draft: Arc::new(draft),
                    graph: Arc::new(app.graph.intent().clone()),
                })
            }) {
                Ok(value) => command = Some(value),
                Err(error) => app.profiles.error = Some(error),
            }
        }
        if ui
            .add_enabled(
                save && plain
                    && selected.as_ref().is_some_and(|r| {
                        app.profiles.loaded.as_ref() == Some(r) && app.profiles.id == r.id.as_str()
                    }),
                egui::Button::new("Update selected profile"),
            )
            .clicked()
            && let Some(reference) = &selected
        {
            match app.profiles.document() {
                Ok(draft) => {
                    command = Some(Command::ProfilesUpdate {
                        id: reference.id.clone(),
                        expected_profile_revision: reference.revision,
                        draft: Arc::new(draft),
                        graph: Arc::new(app.graph.intent().clone()),
                    })
                }
                Err(error) => app.profiles.error = Some(error),
            }
        }
        if ui
            .add_enabled(
                enabled
                    && app
                        .controller
                        .capabilities()
                        .is_some_and(|c| c.profiles.manual_resolution),
                egui::Button::new("Resolve profiles"),
            )
            .clicked()
        {
            match app.profiles.selector() {
                Ok(output) => {
                    command = Some(Command::ProfilesResolve {
                        manual_profile_id: selected.as_ref().map(|r| r.id.clone()),
                        output,
                    })
                }
                Err(error) => app.profiles.error = Some(error),
            }
        }
    });
    if let Some(error) = &app.profiles.error {
        ui.label(format!("Profile draft error: {error}"));
    }
    ui.label("Update requires loading that exact selected revision and retaining its ID. Create uses the draft ID.");
    if let Some(resolution) = &app.controller.profiles.resolution {
        ui.label(format!(
            "Resolution requests {:?}: {:?}; captured owner {:?}, revision {}; result {:?}",
            resolution.request_ids,
            resolution.command,
            resolution.basis.owner_id,
            resolution.basis.revision,
            resolution.value
        ));
    }
    let mut after = app.profiles.clone();
    after.error = None;
    if before != after {
        app.controller.handle(UiIntent::InvalidateCorrection);
    }
    correction::show(ui, app);
    ui.label("Packages and associations: unavailable in this desktop unit");
    if let Some(command) = command {
        action(app, command);
    }
    ui.separator();
    dsp::show(ui, app);
}

fn saved_facts(ui: &mut egui::Ui, record: &ProfileRecordView) {
    let profile = &record.profile;
    ui.label(format!("Saved authored name: {}", profile.name));
    ui.label(format!(
        "Saved source/fidelity: {:?}; {:?}",
        profile.source_policy, profile.fidelity
    ));
    ui.label(format!(
        "Saved output selector: {}; {:?}; configured device {}; manual setup {}",
        match profile.output.backend {
            OutputBackend::Alsa => "ALSA",
            OutputBackend::CoreAudio => "Core Audio",
        },
        profile.output.route,
        profile.output.device_id.as_str(),
        profile.output.manual_setup.as_deref().unwrap_or("none")
    ));
    ui.label(format!(
        "Saved volume intent: {:?}; software cap {:?} dB; hardware cap {}",
        profile.volume.mode,
        profile.volume.software_cap_db,
        profile
            .volume
            .hardware_cap_db
            .map(|db| format!("{db:?} dB"))
            .unwrap_or_else(|| "not set".into())
    ));
    ui.label(format!("Saved latency intent: {:?}; resident limit {} bytes; preparation limit {} bytes; frame limit {}",
        profile.latency.mode, profile.latency.max_resident_bytes, profile.latency.max_preparation_bytes,
        profile.latency.max_latency_frames.map(|value| value.to_string()).unwrap_or_else(|| "not specified".into())));
    ui.collapsing("Saved graph intent (read-only)", |ui| {
        ui.label(
            serde_json::to_string_pretty(&record.graph).expect("typed saved graph serialization"),
        );
    });
    ui.label(format!(
        "Saved corrections: {}; offline preview {}",
        record.corrections.len(),
        if record.correction_preview.is_some() {
            "available"
        } else {
            "unavailable"
        }
    ));
    ui.collapsing("Saved correction details", |ui| {
        ui.label(format!(
            "Corrections: {:?}; offline preview {:?}",
            record.corrections, record.correction_preview
        ));
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use earph_core::{
        control::{CommandEnvelope, ControlLimits},
        ids::{RequestId, ZoneId},
        session::Session,
    };

    #[test]
    fn profile_draft_reaches_the_actual_core_command_consumer_with_exact_authored_values() {
        let draft = Draft {
            id: "owned-profile".into(),
            name: "Owned listening intent".into(),
            backend: Some(OutputBackend::Alsa),
            device: "configured-only".into(),
            setup: "explicit passive setup".into(),
            software_cap: "-20123".into(),
            hardware_cap: "-12345".into(),
            resident: "12345678".into(),
            preparation: "23456789".into(),
            latency_frames: "4096".into(),
            ..Draft::default()
        };
        let mut session = Session::new_owner(
            ZoneId::new("main").unwrap(),
            OwnerId::new("pure-draft-owner").unwrap(),
            ControlLimits::default(),
        );
        let receipt = session.submit(CommandEnvelope {
            schema_version: 1,
            request_id: RequestId::new("pure-state").unwrap(),
            zone_id: ZoneId::new("main").unwrap(),
            owner_id: None,
            session_id: None,
            expected_revision: None,
            payload: Command::State,
        });
        let bytes = serde_json::to_vec(&serde_json::json!({"receipt":receipt})).unwrap();
        let crate::Reply::Receipt(receipt) = crate::decode_reply(bytes).unwrap() else {
            panic!("Core state")
        };
        let request = crate::Request::mutation(
            RequestId::new("pure-create").unwrap(),
            receipt.state().unwrap(),
            Command::ProfilesCreate {
                id: ProfileId::new(&draft.id).unwrap(),
                draft: Arc::new(draft.document().unwrap()),
                graph: Arc::new(GraphIntentEditor::default().intent().clone()),
            },
        )
        .unwrap();
        let decoded =
            earph_core::control::decode_command(request.bytes(), &ControlLimits::default())
                .unwrap();
        let Command::ProfilesCreate { id, draft, .. } = decoded.payload else {
            panic!("create command")
        };
        assert_eq!(id.as_str(), "owned-profile");
        assert_eq!(draft.name, "Owned listening intent");
        assert_eq!(draft.output.device_id.as_str(), "configured-only");
        assert_eq!(
            draft.output.manual_setup.as_deref(),
            Some("explicit passive setup")
        );
        assert_eq!(draft.volume.software_cap_millidecibels, -20123);
        assert_eq!(draft.volume.hardware_cap_millidecibels, Some(-12345));
        assert_eq!(draft.latency.max_resident_bytes, 12345678);
        assert_eq!(draft.latency.max_preparation_bytes, 23456789);
        assert_eq!(draft.latency.max_latency_frames, Some(4096));
        assert!(draft.corrections.is_empty());
    }

    #[test]
    fn loading_saved_caps_requires_an_exact_millidecibel_roundtrip() {
        assert_eq!(millidecibels(-20.123).unwrap(), -20123);
        assert_eq!(millidecibels(0.0).unwrap(), 0);
        for value in [
            -20.1234,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            3_000_000.0,
        ] {
            assert!(
                millidecibels(value).is_err(),
                "silently changed saved cap {value}"
            );
        }
    }

    #[test]
    fn plain_draft_requires_an_explicit_backend_and_numeric_caps() {
        let mut draft = Draft::default();
        assert!(draft.document().is_err());
        draft.backend = Some(OutputBackend::Alsa);
        draft.device = "configured-only".into();
        draft.name = "Owned".into();
        draft.software_cap = "-20.5".into();
        assert!(draft.document().is_err());
        draft.software_cap = "-20000".into();
        assert!(draft.document().is_ok());
    }

    #[test]
    fn core_saved_intent_is_reviewable_without_loading_or_changing_the_local_draft() {
        use earph_core::{
            control::ReceiptStatus, profile_control::ProfileRecord, profiles::ProfileObjectHash,
        };
        let draft = Draft {
            backend: Some(OutputBackend::Alsa),
            device: "configured-only".into(),
            name: "Owned read-only profile".into(),
            ..Draft::default()
        };
        let graph = Arc::new(GraphIntentEditor::default().intent().clone());
        let mut session = Session::new_owner(
            ZoneId::new("main").unwrap(),
            OwnerId::new("controlled-record-serializer").unwrap(),
            ControlLimits::default(),
        );
        let command = CommandEnvelope {
            schema_version: 1,
            request_id: RequestId::new("record-save").unwrap(),
            zone_id: ZoneId::new("main").unwrap(),
            owner_id: session.snapshot().owner_id,
            session_id: None,
            expected_revision: Some(0),
            payload: Command::ProfilesCreate {
                id: ProfileId::new("record-only").unwrap(),
                draft: Arc::new(draft.document().unwrap()),
                graph: graph.clone(),
            },
        };
        assert_eq!(
            session.submit_graph(command.clone(), 1).status,
            ReceiptStatus::Accepted
        );
        let job = session.profile_job().unwrap();
        // Controlled compiler completion qualifies Core serialization, not object hashing or offline compilation.
        let hash = ProfileObjectHash::try_from("0".repeat(64)).unwrap();
        let record = Arc::new(ProfileRecord {
            profile: Arc::new(
                job.draft
                    .document(job.id.clone(), job.revision, hash)
                    .unwrap(),
            ),
            profile_sha256: hash,
            graph: job.graph,
            corrections: Arc::new(Vec::new()),
            correction_preview: None,
        });
        session
            .complete_profile_save(&job.request_id, &job.binding, 2, Ok(record), |_| Ok(()))
            .unwrap();
        let receipt = session.submit_graph(command, 3);
        let crate::Reply::Receipt(receipt) = crate::decode_reply(
            serde_json::to_vec(&serde_json::json!({"receipt":receipt})).unwrap(),
        )
        .unwrap() else {
            panic!("Core receipt")
        };
        let Some(crate::ResultView::ProfileSaved(record)) = receipt.result else {
            panic!("Core saved record")
        };
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut output = context.run_ui(egui::RawInput::default(), |ui| saved_facts(ui, &record));
        output.textures_delta.clear();
        let tree = output.platform_output.accesskit_update.unwrap();
        let labels: Vec<_> = tree
            .nodes
            .iter()
            .filter_map(|(_, n)| n.label().or_else(|| n.value()))
            .collect();
        assert!(labels.contains(&"Saved authored name: Owned read-only profile"));
        assert!(labels.contains(&"Saved output selector: ALSA; System; configured device configured-only; manual setup none"));
        assert!(labels.contains(
            &"Saved volume intent: Software; software cap -20.0 dB; hardware cap not set"
        ));
        assert!(labels.contains(&"Saved latency intent: Balanced; resident limit 268435456 bytes; preparation limit 268435456 bytes; frame limit not specified"));
        assert!(
            labels
                .iter()
                .any(|s| s.starts_with("Saved source/fidelity:")
                    && s.contains("LocalFile")
                    && s.contains("CompatibleConversion"))
        );
        assert!(
            labels
                .iter()
                .any(|s| s.starts_with("Saved output selector:") && s.contains("configured-only"))
        );
        assert!(
            labels
                .iter()
                .any(|s| s.starts_with("Saved volume intent:") && s.contains("-20.0"))
        );
        assert!(
            labels
                .iter()
                .any(|s| s.starts_with("Saved latency intent:") && s.contains("Balanced"))
        );
        assert!(labels.contains(&"Saved graph intent (read-only)"));
        assert!(draft.loaded.is_none());
        assert_eq!(draft.name, "Owned read-only profile");
        assert_eq!(record.graph, graph);
    }
}
