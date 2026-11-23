// ABOUTME: Presents explicit correction source previews and owner import provenance reviews.
// ABOUTME: Keeps authored drafts, advisory FIR reads, and actual saved facts separate.
use super::{
    GuiApp, UiIntent,
    app::field,
    output::number,
    profile_files::{CorrectionPreview, CorrectionPreviewRequest, CorrectionSource},
    profiles,
};
use earph_core::{
    audio::ChannelLayout, control::Command, dsp_nodes::FirPurpose, graph::FiniteGraphScalar,
    profile_control::ProfileCatalogReference, profiles::ProfileCorrectionKind,
};
use earph_media::correction_text::{CorrectionFormat, ImportContext, ImportMetadata};
use eframe::egui;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Format {
    Text(CorrectionFormat),
    Fir,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Draft {
    format: Option<Format>,
    pasted: bool,
    path: String,
    text: String,
    rate: String,
    channels: ChannelLayout,
    quantum: String,
    preamp: String,
    purpose: bool,
    metadata: [String; 6],
    error: Option<String>,
}
impl Default for Draft {
    fn default() -> Self {
        Self {
            format: None,
            pasted: false,
            path: String::new(),
            text: String::new(),
            rate: "48000".into(),
            channels: ChannelLayout::Stereo,
            quantum: "256".into(),
            preamp: String::new(),
            purpose: false,
            metadata: Default::default(),
            error: None,
        }
    }
}
impl Draft {
    fn request(&self) -> Result<CorrectionPreviewRequest, String> {
        let context = ImportContext {
            sample_rate_hz: number("Correction sample rate", &self.rate)?,
            channels: self.channels,
            quantum_frames: number("Correction quantum", &self.quantum)?,
        };
        context.validate().map_err(|e| e.to_string())?;
        let optional =
            |index: usize| (!self.metadata[index].is_empty()).then(|| self.metadata[index].clone());
        let metadata = ImportMetadata {
            headphone_model: optional(0),
            measurement_provider: optional(1),
            target: optional(2),
            parameter_set: optional(3),
            revision: optional(4),
            redistribution_license: optional(5),
        };
        let (source, fir_preamp) =
            match self.format.ok_or("Choose an explicit correction format")? {
                Format::Text(format) => (
                    if self.pasted {
                        CorrectionSource::Text {
                            text: self.text.clone(),
                            format,
                        }
                    } else {
                        CorrectionSource::TextFile {
                            path: self.path.clone(),
                            format,
                        }
                    },
                    None,
                ),
                Format::Fir => {
                    if self.pasted {
                        return Err("FIR WAV requires Read selected source file".into());
                    }
                    if !self.purpose {
                        return Err("Choose the explicit Correction FIR purpose".into());
                    }
                    let preamp = FiniteGraphScalar::new(number("FIR preamp", &self.preamp)?)
                        .map_err(|e| e.to_string())?;
                    (
                        CorrectionSource::Fir {
                            path: self.path.clone(),
                            purpose: FirPurpose::Correction,
                        },
                        Some(preamp),
                    )
                }
            };
        Ok(CorrectionPreviewRequest {
            source,
            context,
            metadata,
            fir_preamp,
        })
    }
    fn reconstruct(&mut self, bytes: &[u8]) -> Result<(), String> {
        let request = CorrectionPreviewRequest::from_text_report(bytes)?;
        let CorrectionSource::Text { text, format } = request.source else {
            return Err("Text report required".into());
        };
        self.format = Some(Format::Text(format));
        self.pasted = true;
        self.text = text;
        self.path.clear();
        self.rate = request.context.sample_rate_hz.to_string();
        self.channels = request.context.channels;
        self.quantum = request.context.quantum_frames.to_string();
        self.preamp.clear();
        self.purpose = false;
        self.metadata = [
            request.metadata.headphone_model,
            request.metadata.measurement_provider,
            request.metadata.target,
            request.metadata.parameter_set,
            request.metadata.revision,
            request.metadata.redistribution_license,
        ]
        .map(Option::unwrap_or_default);
        Ok(())
    }
}
pub(super) fn show(ui: &mut egui::Ui, app: &mut GuiApp) {
    ui.separator();
    ui.heading("Correction source and provenance");
    let mut before = app.correction.clone();
    before.error = None;
    ui.label("Choose the source format explicitly. Source files remain unchanged.");
    let draft = &mut app.correction;
    ui.horizontal_wrapped(|ui| {
        for (format, label) in [
            (Format::Text(CorrectionFormat::Rew), "REW"),
            (Format::Text(CorrectionFormat::AutoEq), "AutoEq"),
            (Format::Text(CorrectionFormat::CamillaDsp), "CamillaDSP"),
            (Format::Fir, "FIR WAV"),
        ] {
            ui.selectable_value(&mut draft.format, Some(format), label);
        }
    });
    ui.horizontal(|ui| {
        ui.selectable_value(&mut draft.pasted, false, "Read selected source file");
        ui.selectable_value(&mut draft.pasted, true, "Use pasted text");
    });
    field(
        ui,
        "Correction source path",
        "correction-path",
        &mut draft.path,
        1024,
    );
    let label = ui.label("Correction source text");
    ui.add(
        egui::TextEdit::multiline(&mut draft.text)
            .id_salt("correction-source")
            .char_limit(65536)
            .desired_rows(5)
            .desired_width(f32::INFINITY),
    )
    .labelled_by(label.id);
    field(
        ui,
        "Correction sample rate (Hz)",
        "correction-rate",
        &mut draft.rate,
        10,
    );
    ui.horizontal(|ui| {
        ui.selectable_value(&mut draft.channels, ChannelLayout::Mono, "Correction mono");
        ui.selectable_value(
            &mut draft.channels,
            ChannelLayout::Stereo,
            "Correction stereo",
        );
    });
    field(
        ui,
        "Correction quantum (frames)",
        "correction-quantum",
        &mut draft.quantum,
        10,
    );
    field(
        ui,
        "FIR preamp (dB, required)",
        "correction-preamp",
        &mut draft.preamp,
        32,
    );
    ui.checkbox(&mut draft.purpose, "FIR purpose: Correction");
    ui.label(
        "Text preamp comes from the exact source. Edit source text and preview again to change it.",
    );
    for (index, label) in [
        "Headphone model",
        "Measurement provider",
        "Target",
        "Parameter set",
        "Metadata revision",
        "Redistribution license",
    ]
    .into_iter()
    .enumerate()
    {
        field(
            ui,
            label,
            &format!("correction-metadata-{index}"),
            &mut draft.metadata[index],
            256,
        );
    }
    ui.label("Empty metadata fields remain null. Authored metadata describes the source; it proves no device identity or license rights.");
    let mut after = app.correction.clone();
    after.error = None;
    if before != after {
        app.controller.handle(UiIntent::InvalidateCorrection);
    }
    let enabled = app.controller.control_edit_available();
    if ui
        .add_enabled(enabled, egui::Button::new("Preview correction source"))
        .clicked()
    {
        match app.correction.request() {
            Ok(request) => {
                if let Some(state) = app.controller.state() {
                    app.controller.handle(UiIntent::PreviewCorrection {
                        basis: Arc::new(state.clone()),
                        request,
                    });
                }
            }
            Err(error) => app.correction.error = Some(error),
        }
    }
    if ui
        .add_enabled(
            app.controller.local_correction_pending(),
            egui::Button::new("Cancel local correction preview"),
        )
        .clicked()
    {
        app.controller.handle(UiIntent::InvalidateCorrection);
    }
    ui.label(app.controller.local_correction_status());
    let preview = app.controller.correction_preview().cloned();
    if preview.is_some() && !app.controller.correction_preview_current() {
        ui.label("The captured owner context changed. Preview again before importing; the previous parser facts remain visible for comparison.");
    }
    if let Some(preview) = &preview {
        ui.label(format!(
            "Captured source byte SHA256: {}",
            preview.source_sha256()
        ));
        match preview {
            CorrectionPreview::Text {
                report,
                source_text,
            } => {
                ui.label("Local parser report: exact captured UTF-8 source; owner import independently rebuilds it.");
                ui.label(serde_json::to_string_pretty(report).expect("typed report"));
                ui.collapsing("Exact captured text source", |ui| {
                    ui.label(source_text);
                });
            }
            CorrectionPreview::Fir { manifest, .. } => {
                ui.label("Advisory FIR preview. The owner rereads this path; these bytes are not atomically bound to import.");
                ui.label(serde_json::to_string_pretty(manifest).expect("typed manifest"));
            }
        }
    }
    ui.label("Local compiled headroom, CPU admission, and native qualification: unknown. Actual owner results appear separately.");
    let import = enabled
        && app.controller.correction_preview_current()
        && !app.graph.has_pending_raw_edits()
        && app
            .controller
            .capabilities()
            .is_some_and(|c| c.profiles.correction_import);
    let mut command = None;
    ui.label("Profile import preserves the declared preamp. FIR asset verification publishes only the WAV asset; adding it creates one convolution node. Author graph gain separately for a plain asset.");
    ui.horizontal_wrapped(|ui| {
        for (update, label) in [
            (false, "Import correction as new profile"),
            (true, "Import correction into loaded revision"),
        ] {
            if ui
                .add_enabled(
                    import && app.profiles.import_target(update).is_ok(),
                    egui::Button::new(label),
                )
                .clicked()
            {
                match app.profiles.import_target(update).and_then(
                    |(id, expected_profile_revision)| {
                        Ok(Command::ProfilesImport {
                            id,
                            expected_profile_revision,
                            draft: Arc::new(app.profiles.correction_document()?),
                            input: Arc::new(
                                preview.as_ref().ok_or("Preview this source first")?.input(),
                            ),
                        })
                    },
                ) {
                    Ok(value) => command = Some(value),
                    Err(error) => app.correction.error = Some(error),
                }
            }
        }
        if ui
            .add_enabled(
                import && matches!(preview, Some(CorrectionPreview::Fir { .. })),
                egui::Button::new("Verify FIR asset"),
            )
            .clicked()
            && let Some(CorrectionPreview::Fir { path, manifest }) = &preview
        {
            command = Some(Command::FirImport {
                path: path.clone(),
                purpose: manifest.asset.purpose,
            });
        }
    });
    let actual = app
        .controller
        .profile_record()
        .filter(|r| !r.corrections.is_empty())
        .cloned();
    if let Some(record) = &actual {
        ui.label("Actual saved correction documents and offline preview are shown above. Review their actual source, context, metadata, preamp, and hashes.");
        if let Some(local) = &preview {
            compare_saved(ui, local, record);
        }
    }
    if ui
        .add_enabled(
            enabled && actual.is_some(),
            egui::Button::new("Review actual imported correction"),
        )
        .clicked()
        && let Some(record) = &actual
        && let Some(owner) = app.controller.state().and_then(|s| s.owner_id.clone())
    {
        app.controller.handle(UiIntent::ReviewCorrection {
            owner,
            reference: ProfileCatalogReference {
                id: record.profile.id.clone(),
                revision: record.profile.revision,
                profile_sha256: record.profile_sha256,
            },
        });
    }
    if let Some(record) = &actual {
        ui.label(format!(
            "Actual correction reviewed: {}",
            app.controller
                .correction_reviewed(&ProfileCatalogReference {
                    id: record.profile.id.clone(),
                    revision: record.profile.revision,
                    profile_sha256: record.profile_sha256
                })
        ));
        for (index, object) in record.corrections.iter().enumerate() {
            if object.kind == ProfileCorrectionKind::TextReport
                && ui
                    .add_enabled(
                        enabled,
                        egui::Button::new(format!(
                            "Reconstruct text report {} as new source",
                            index + 1
                        )),
                    )
                    .clicked()
            {
                match app.correction.reconstruct(object.document_json.as_bytes()) {
                    Ok(()) => {
                        app.correction.error = None;
                        app.controller.handle(UiIntent::InvalidateCorrection);
                    }
                    Err(error) => app.correction.error = Some(error),
                }
            }
        }
    }
    ui.label("Reconstructed text is a new source. Preview computes its byte hash; the historical original source is unavailable.");
    let asset = app.controller.fir_asset().cloned();
    if let Some(asset) = &asset {
        ui.label(format!(
            "Actual owner-verified FIR asset: SHA256 {}; {} Hz; {:?}; taps per channel: {}; purpose {:?}",
            asset.sha256.as_str(),
            asset.sample_rate_hz,
            asset.channels,
            asset.taps_per_channel,
            asset.purpose
        ));
        if let Some(CorrectionPreview::Fir { manifest, .. }) = &preview {
            ui.label(format!(
                "Owner asset versus advisory WAV-byte SHA256 match: {}",
                manifest.asset.sha256 == asset.sha256
            ));
        }
    }
    if ui
        .add_enabled(
            enabled && asset.is_some(),
            egui::Button::new("Review actual verified FIR asset"),
        )
        .clicked()
        && let Some(asset) = &asset
        && let Some(owner) = app.controller.state().and_then(|s| s.owner_id.clone())
    {
        app.controller.handle(UiIntent::ReviewFirAsset {
            owner,
            asset: asset.clone(),
        });
    }
    if ui
        .add_enabled(
            enabled && app.controller.fir_asset_reviewed() && !app.graph.has_pending_raw_edits(),
            egui::Button::new("Add reviewed FIR asset to graph"),
        )
        .clicked()
        && let Some(asset) = asset
    {
        match app.graph.add_fir(asset) {
            Ok(()) => app.controller.handle(UiIntent::DiscardPreparedGraph),
            Err(error) => app.correction.error = Some(error),
        }
    }
    if let Some(error) = &app.correction.error {
        ui.label(format!("Correction draft error: {error}"));
    }
    if let Some(command) = command {
        profiles::action(app, command);
    }
}
fn compare_saved(ui: &mut egui::Ui, local: &CorrectionPreview, record: &crate::ProfileRecordView) {
    if let Some(object) = record.corrections.first() {
        let actual = match local {
            CorrectionPreview::Text { .. } => earph_media::correction_text::read_correction_report(
                object.document_json.as_bytes(),
            )
            .map(|r| r.source.sha256)
            .ok(),
            CorrectionPreview::Fir { .. } => {
                earph_media::read_fir_manifest(object.document_json.as_bytes())
                    .map(|m| m.asset.sha256.as_str().to_owned())
                    .ok()
            }
        };
        if let Some(actual) = actual {
            ui.label(format!(
                "Actual saved source versus advisory/captured byte SHA256 match: {}",
                actual == local.source_sha256()
            ));
        } else {
            ui.label("Source byte hash comparison: unavailable for these different correction document kinds.");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn form_requires_explicit_format_purpose_and_preamp_and_preserves_authored_metadata() {
        let mut draft = Draft::default();
        assert!(draft.request().is_err());
        draft.format = Some(Format::Text(CorrectionFormat::AutoEq));
        draft.pasted = true;
        draft.text = "Preamp: -6 dB\nFilter 1: ON PK Fc 100 Hz Gain 2 dB Q 1\n".into();
        let empty = draft.request().unwrap();
        assert_eq!(empty.metadata, ImportMetadata::default());
        draft.metadata = [
            "Headphone",
            "Provider",
            "Target",
            "Parameters",
            "Revision",
            "License",
        ]
        .map(str::to_owned);
        let request = draft.request().unwrap();
        assert_eq!(
            request.metadata.headphone_model.as_deref(),
            Some("Headphone")
        );
        assert_eq!(
            request.metadata.measurement_provider.as_deref(),
            Some("Provider")
        );
        assert_eq!(request.metadata.target.as_deref(), Some("Target"));
        assert_eq!(
            request.metadata.parameter_set.as_deref(),
            Some("Parameters")
        );
        assert_eq!(request.metadata.revision.as_deref(), Some("Revision"));
        assert_eq!(
            request.metadata.redistribution_license.as_deref(),
            Some("License")
        );
        draft.format = Some(Format::Fir);
        draft.pasted = false;
        assert!(draft.request().is_err());
        draft.purpose = true;
        assert!(draft.request().is_err());
        draft.preamp = "NaN".into();
        assert!(draft.request().is_err());
        draft.preamp = "-8.5".into();
        assert_eq!(draft.request().unwrap().fir_preamp.unwrap().get(), -8.5);
        draft.pasted = true;
        assert!(draft.request().is_err());
    }
}
