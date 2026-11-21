// ABOUTME: Presents immutable package candidates, exact byte reviews, and explicit publication choices.
// ABOUTME: Keeps local review acknowledgments separate from owner validation and filesystem outcomes.
use super::{GuiApp, UiIntent, app::field, package_files::ObjectBytes, profiles};
use crate::{ProfilePackageResultView, ProfilePackageStageView};
use earph_core::{
    control::Command,
    profile_control::{PackageContentReview, PackageExportReview, PackageRedistribution},
    profile_packages::PackageObject,
};
use eframe::egui;

#[derive(Default)]
pub(super) struct Draft {
    pub directory: String,
    pub destination: String,
    selected: Option<usize>,
}
#[derive(Default)]
pub(super) struct Review {
    pub destination: String,
    pub exact: bool,
    pub private: bool,
    pub measurements: bool,
    pub read: Vec<PackageObject>,
    pub displayed: Option<ObjectBytes>,
}
impl Review {
    pub fn reset(&mut self) {
        self.exact = false;
        self.private = false;
        self.measurements = false;
        self.read.clear();
        self.displayed = None;
    }
    pub fn all_read(&self, candidate: &ProfilePackageResultView) -> bool {
        !candidate.manifest.objects.is_empty()
            && candidate
                .manifest
                .objects
                .iter()
                .all(|object| self.read.contains(object))
    }
    pub fn export(&self, candidate: &ProfilePackageResultView) -> Result<Command, String> {
        if !self.exact
            || !self.private
            || !self.all_read(candidate)
            || candidate.operation != ProfilePackageStageView::Previewed
        {
            return Err("Review every exact object and acknowledge this candidate and its privacy before export".into());
        }
        earph_core::profile_control::validate_package_path(&self.destination)
            .map_err(|e| e.to_string())?;
        Ok(Command::ProfilesExport {
            directory: self.destination.clone(),
            profile: candidate.manifest.profile.clone(),
            expected_manifest_sha256: candidate.manifest_sha256,
            export_review: PackageExportReview::ReviewedExactCandidate,
            content_review: PackageContentReview::ReviewedNoPrivateData,
            redistribution: if self.measurements {
                PackageRedistribution::ReviewedRequiredMeasurements
            } else {
                PackageRedistribution::RefuseMeasurementBytes
            },
        })
    }
}
pub(super) fn show(ui: &mut egui::Ui, app: &mut GuiApp) {
    ui.separator();
    ui.heading("Packages and associations");
    let before = (
        app.packages.directory.clone(),
        app.packages.destination.clone(),
    );
    field(
        ui,
        "Package directory",
        "package-directory",
        &mut app.packages.directory,
        1024,
    );
    field(
        ui,
        "Package destination",
        "package-destination",
        &mut app.packages.destination,
        1024,
    );
    if before
        != (
            app.packages.directory.clone(),
            app.packages.destination.clone(),
        )
    {
        app.controller.handle(UiIntent::PackageInputs {
            directory: app.packages.directory.clone(),
            destination: app.packages.destination.clone(),
        });
    }
    let enabled = app.controller.control_edit_available()
        && app
            .controller
            .capabilities()
            .is_some_and(|c| c.profiles.packages);
    if ui
        .add_enabled(
            app.controller.package_files_pending(),
            egui::Button::new("Cancel local package review"),
        )
        .clicked()
    {
        app.controller.handle(UiIntent::CancelLocalPackageReview);
    }
    ui.label(format!(
        "Local profile-file worker: {}",
        app.controller.local_correction_status()
    ));
    let selected = profiles::selected_record(app).map(|r| {
        earph_core::profile_control::ProfileCatalogReference {
            id: r.profile.id.clone(),
            revision: r.profile.revision,
            profile_sha256: r.profile_sha256,
        }
    });
    let mut command = None;
    let captured_export = app
        .controller
        .receipts()
        .filter_map(|receipt| {
            let envelope = receipt.request.envelope();
            if receipt.owner_changed
                || app
                    .controller
                    .state()
                    .is_none_or(|state| state.owner_id != envelope.owner_id)
            {
                return None;
            }
            if let Command::ProfilesExport { directory, .. } = &envelope.payload {
                Some((
                    envelope.request_id.clone(),
                    directory.clone(),
                    receipt.presentation,
                ))
            } else {
                None
            }
        })
        .last();
    ui.label("Reconcile an uncertain export's exact request first. Owner inspection changes control state and must wait for uncertainty to settle. The destination stays unchanged; retries remain explicit.");
    ui.label("A historical Accepted receipt can remain after its producer worker retires. Check the current worker status and the actual safety reply separately.");
    if let Some((id, directory, presentation)) = captured_export {
        ui.label(format!(
            "Captured export destination: {directory}; request {}; status {presentation:?}",
            id.as_str()
        ));
        if matches!(
            presentation,
            crate::Presentation::Uncertain
                | crate::Presentation::Cancelled
                | crate::Presentation::Failed
        ) {
            ui.label("Preserve existing files and inspect explicitly after settlement. This receipt alone does not establish filesystem rollback.");
        }
        if ui
            .add_enabled(
                enabled
                    && !matches!(
                        presentation,
                        crate::Presentation::Uncertain
                            | crate::Presentation::Accepted
                            | crate::Presentation::Queued
                    ),
                egui::Button::new("Inspect captured export destination"),
            )
            .clicked()
        {
            app.packages.directory = directory.clone();
            app.controller.handle(UiIntent::PackageInputs {
                directory: directory.clone(),
                destination: app.packages.destination.clone(),
            });
            command = Some(Command::ProfilesPackageInspect { directory });
        }
    }
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(enabled, egui::Button::new("Inspect package directory"))
            .clicked()
        {
            command = Some(Command::ProfilesPackageInspect {
                directory: app.packages.directory.clone(),
            });
        }
        if ui
            .add_enabled(
                enabled && selected.is_some(),
                egui::Button::new("Preview selected profile package"),
            )
            .clicked()
            && let Some(profile) = &selected
        {
            command = Some(Command::ProfilesPackagePreview {
                profile: profile.clone(),
            });
        }
    });
    let candidate = app.controller.package_candidate().cloned();
    if let Some(candidate) = &candidate {
        ui.label(format!("Actual owner package stage: {:?}; manifest {}; manifest bytes {}; total bytes {}; profile {} revision {} hash {}", candidate.operation, candidate.manifest_sha256.as_str(), candidate.manifest_bytes, candidate.total_bytes, candidate.manifest.profile.id.as_str(), candidate.manifest.profile.revision, candidate.manifest.profile.profile_sha256.as_str()));
        ui.label(format!(
            "Measurement bytes require review: {}; content review required: {}",
            candidate.measurement_review_required, candidate.content_review_required
        ));
        for (index, source) in candidate.review_sources.iter().enumerate() {
            if ui
                .selectable_label(
                    app.packages.selected == Some(index),
                    format!(
                        "{:?} · {} · {} bytes · {}",
                        source.object.kind,
                        source.object.sha256.as_str(),
                        source.object.bytes,
                        source.local_path
                    ),
                )
                .clicked()
            {
                app.packages.selected = Some(index);
            }
        }
    } else {
        ui.label("No exact package inspection or export preview observed.");
    }
    if ui
        .add_enabled(
            enabled
                && app.packages.selected.is_some_and(|index| {
                    candidate
                        .as_ref()
                        .is_some_and(|c| index < c.review_sources.len())
                }),
            egui::Button::new("Read exact object bytes"),
        )
        .clicked()
        && let Some(index) = app.packages.selected
    {
        app.controller.handle(UiIntent::ReadPackageObject { index });
    }
    if let Some(displayed) = &app.controller.package_review.displayed {
        ui.label(format!(
            "Exact original bytes read: {:?} {} · {} bytes · {}",
            displayed.object.kind,
            displayed.object.sha256.as_str(),
            displayed.bytes.len(),
            displayed.path
        ));
        if let Some(wav) = &displayed.wav {
            ui.label(format!("Actual WAV geometry: {wav:?}. Byte/hash review does not prove privacy or redistribution rights."));
        } else {
            ui.label(std::str::from_utf8(&displayed.bytes).expect("worker validated UTF-8"));
        }
    }
    if ui
        .add_enabled(
            enabled && app.controller.package_review.displayed.is_some(),
            egui::Button::new("Mark displayed object reviewed"),
        )
        .clicked()
    {
        app.controller.handle(UiIntent::MarkPackageObjectReviewed);
    }
    let all_read = candidate
        .as_ref()
        .is_some_and(|c| app.controller.package_review.all_read(c));
    ui.label(format!(
        "All candidate objects explicitly reviewed: {all_read}"
    ));
    let mut exact = app.controller.package_review.exact;
    let mut private = app.controller.package_review.private;
    let mut measurements = app.controller.package_review.measurements;
    ui.add_enabled_ui(enabled && all_read, |ui| {
        let changed = ui.checkbox(&mut exact, "Acknowledge exact candidate").changed() | ui.checkbox(&mut private, "Acknowledge no private data").changed();
        ui.label("Review authored metadata and original source bytes. The app cannot prove privacy automatically.");
        let redistribution_changed = ui.radio_value(&mut measurements, false, "Refuse measurement bytes").changed() | ui.radio_value(&mut measurements, true, "Reviewed required measurements").changed();
        if changed || redistribution_changed { app.controller.handle(UiIntent::PackageAcknowledgments { exact, private, measurements }); }
    });
    ui.label("Refusing required measurements causes an owner rejection. Dependencies are never silently omitted.");
    if ui
        .add_enabled(
            enabled
                && app
                    .controller
                    .package_import_available(&app.packages.directory),
            egui::Button::new("Import inspected package"),
        )
        .clicked()
        && let Some(candidate) = &candidate
    {
        command = Some(Command::ProfilesImportPackage {
            directory: app.packages.directory.clone(),
            expected_manifest_sha256: candidate.manifest_sha256,
            profile: candidate.manifest.profile.clone(),
        });
    }
    if ui
        .add_enabled(
            enabled && app.controller.package_export_available(),
            egui::Button::new("Export reviewed package"),
        )
        .clicked()
    {
        app.controller.handle(UiIntent::ExportReviewedPackage);
    }
    ui.label("Export requires an existing private parent directory owned by your account. The owner refuses occupied destinations. Import preserves its exact ID and revision. Neither action applies processing or changes output safety.");
    if let Some((request, issue)) = &app.controller.profiles.package_issue {
        ui.label(format!("Actual package issue for {}: {:?}; object {}; filesystem {:?}. Retained stages and uncertain effects require explicit inspection; no rollback or automatic retry is claimed.", request.as_str(), issue.reason, object_label(issue.object.as_ref()), issue.filesystem_state));
    }
    if let Some((request, directory, issue)) = app.controller.unresolved_package_write() {
        ui.label(format!("Unresolved write facts for {} at {directory}: {:?}; object {}; filesystem {:?}. A later read reporting Unchanged does not establish rollback of this earlier write.", request.as_str(), issue.reason, object_label(issue.object.as_ref()), issue.filesystem_state));
    }
    ui.separator();
    ui.label("Exact associations use the actual opened device identity. ALSA exact binding is unavailable. Passive headphones require an explicit manual listening setup.");
    let bind = selected.is_some() && app.controller.association_available();
    if ui
        .add_enabled(
            bind,
            egui::Button::new("Bind selected profile to opened device"),
        )
        .clicked()
        && let Some(reference) = selected
        && let Some(state) = app.controller.state()
    {
        command = Some(Command::ProfilesBind {
            id: reference.id,
            expected_profile_revision: reference.revision,
            profile_sha256: reference.profile_sha256,
            expected_device_epoch: state.device_epoch,
        });
    }
    if let Some(bound) = &app.controller.profiles.bound {
        ui.label(format!(
            "Actual owner association: profile {}; backend {:?}; route {:?}; device identity SHA256 {}; captured owner {}; requests {}",
            bound.value.profile_id.as_str(), bound.value.backend, bound.value.route, bound.value.device_identity_sha256.as_str(), bound.basis.owner_id.as_ref().map(|owner| owner.as_str()).unwrap_or("unavailable"), bound.request_ids.iter().map(|id| id.as_str()).collect::<Vec<_>>().join(", ")
        ));
    }
    ui.label("Manual resolution stays in the profile selector above. Binding and resolving never apply a profile or select output.");
    if let Some(command) = command {
        profiles::action(app, command);
    }
}
fn object_label(object: Option<&PackageObject>) -> String {
    object
        .map(|object| {
            format!(
                "{:?} · {} · {} bytes",
                object.kind,
                object.sha256.as_str(),
                object.bytes
            )
        })
        .unwrap_or_else(|| "unavailable".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_association_and_issue_show_canonical_hashes_through_the_actual_app_renderer() {
        use earph_core::{
            control::{ControlLimits, OutputBackend, OutputRoute},
            ids::{OwnerId, ProfileId, RequestId, ZoneId},
            profile_packages::PackageObjectKind,
            profiles::{ProfileAssociation, ProfileObjectHash},
        };
        use std::sync::Arc;
        // Declared Core transport facts qualify rendering, not native identity or hash admission.
        let state = earph_core::session::Session::new_owner(
            ZoneId::new("main").unwrap(),
            OwnerId::new("controlled-association-owner").unwrap(),
            ControlLimits::default(),
        )
        .snapshot();
        let basis = Arc::new(
            serde_json::from_value::<crate::StateView>(serde_json::to_value(state).unwrap())
                .unwrap(),
        );
        let hex = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let hash = ProfileObjectHash::try_from(hex.to_owned()).unwrap();
        let id = ProfileId::new("controlled-association-profile").unwrap();
        let request = RequestId::new("controlled-association-request").unwrap();
        let mut controller = super::super::GuiController::new();
        controller.profiles.bound = Some(super::super::workflow::ObservedProfile {
            basis,
            request_ids: vec![request.clone()],
            command: Command::ProfilesBind {
                id: id.clone(),
                expected_profile_revision: 1,
                profile_sha256: hash,
                expected_device_epoch: 0,
            },
            value: Arc::new(ProfileAssociation {
                backend: OutputBackend::CoreAudio,
                route: OutputRoute::System,
                device_identity_sha256: hash,
                profile_id: id,
            }),
        });
        controller.profiles.package_issue = Some((
            request,
            Arc::new(crate::ProfilePackageIssueView {
                reason: crate::ProfilePackageReasonView::MissingObject,
                object: Some(PackageObject {
                    kind: PackageObjectKind::Graph,
                    sha256: hash,
                    bytes: 100,
                }),
                filesystem_state: crate::PackageFilesystemStateView::Unchanged,
            }),
        ));
        let mut app = GuiApp::new(controller);
        let context = egui::Context::default();
        context.enable_accesskit();
        let mut first = context.run_ui(egui::RawInput::default(), |ui| app.draw(ui));
        first.textures_delta.clear();
        let tree = first.platform_output.accesskit_update.unwrap();
        let target = tree
            .nodes
            .iter()
            .find(|(_, node)| {
                node.role() != egui::accesskit::Role::TextRun
                    && node.label() == Some("Profiles / DSP")
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
            .filter_map(|(_, node)| node.label().or_else(|| node.value()))
            .collect();
        assert!(
            labels
                .iter()
                .any(|label| label.starts_with("Actual owner association:") && label.contains(hex)),
            "association digest must render in canonical full hex; matching values: {:?}",
            labels
                .iter()
                .filter(|label| label.contains("association")
                    || label.contains("issue")
                    || label.contains(hex))
                .collect::<Vec<_>>()
        );
        assert!(
            labels
                .iter()
                .any(|label| label.starts_with("Actual package issue for") && label.contains(hex)),
            "issue object digest must render in canonical full hex"
        );
    }
    fn candidate() -> ProfilePackageResultView {
        // This declared transport fixture exercises review admission, not owner publication.
        serde_json::from_value(serde_json::json!({"operation":"previewed", "manifest":{"schema_version":1,"profile":{"id":"declared-review","revision":1,"profile_sha256":"0000000000000000000000000000000000000000000000000000000000000000"},"objects":[{"kind":"profile","sha256":"0000000000000000000000000000000000000000000000000000000000000000","bytes":10},{"kind":"graph","sha256":"1111111111111111111111111111111111111111111111111111111111111111","bytes":20}]},"manifest_sha256":"2222222222222222222222222222222222222222222222222222222222222222","manifest_bytes":100,"total_bytes":130,"measurement_review_required":false,"content_review_required":true,"review_sources":[]})).unwrap()
    }
    #[test]
    fn package_export_requires_every_object_and_both_reviews_and_refuses_measurements_by_default() {
        let candidate = candidate();
        let mut review = Review {
            destination: "/owned/new-package".into(),
            ..Default::default()
        };
        assert!(review.export(&candidate).is_err());
        review.exact = true;
        review.private = true;
        review.read.push(candidate.manifest.objects[0].clone());
        assert!(review.export(&candidate).is_err());
        review.read.push(candidate.manifest.objects[1].clone());
        let Command::ProfilesExport {
            redistribution,
            profile,
            ..
        } = review.export(&candidate).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            redistribution,
            PackageRedistribution::RefuseMeasurementBytes
        );
        assert_eq!(profile, candidate.manifest.profile);
        review.measurements = true;
        assert!(matches!(
            review.export(&candidate).unwrap(),
            Command::ProfilesExport {
                redistribution: PackageRedistribution::ReviewedRequiredMeasurements,
                ..
            }
        ));
        let mut changed = candidate.clone();
        changed.manifest.objects[0].bytes += 1;
        assert!(review.export(&changed).is_err());
        review.reset();
        assert!(!review.exact && !review.private && !review.measurements && review.read.is_empty());
        assert!(review.export(&candidate).is_err());
    }
}
