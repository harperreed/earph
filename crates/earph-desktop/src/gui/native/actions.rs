// ABOUTME: Translates native presentation actions into the existing typed controller intents.
// ABOUTME: Resolves captured bases without rebasing stale edits or rebuilding owner envelopes.
use super::super::{CorrectionPreviewRequest, UiIntent, graph_editor::GraphIntentEditor};
use super::{NativeError, NativeState, invalid};
use earph_core::{
    control::{Command, OutputBackend},
    dsp_nodes::FirAssetReference,
    graph::NodeKind,
    ids::{OwnerId, RequestId},
    profile_control::ProfileCatalogReference,
};
use serde::Deserialize;
use serde_json::Value;
use std::path::PathBuf;

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum NativeAction {
    Attach {
        runtime: String,
    },
    Launch {
        runtime: String,
        executable: PathBuf,
    },
    Disconnect {},
    Refresh {},
    QueueEdit {
        basis_token: String,
        command: Command,
    },
    ControlEdit {
        basis_token: String,
        command: Command,
    },
    StartLocal {
        basis_token: String,
        takeover: bool,
    },
    PlayLocal {
        basis_token: String,
        takeover: bool,
        entry_id: Option<earph_core::ids::QueueEntryId>,
    },
    SetVolume {
        basis_token: String,
        millidecibels: i32,
        ramp_frames: u32,
    },
    Discover {
        backend: OutputBackend,
        endpoint: Option<String>,
    },
    ProfileAction {
        basis_token: String,
        command: Command,
    },
    DiagnosticAction {
        basis_token: String,
        command: Command,
    },
    DiagnosticDestination {
        directory: String,
    },
    ReviewDiagnosticPreview {
        basis_token: String,
    },
    PreviewCorrection {
        basis_token: String,
        request: CorrectionPreviewRequest,
    },
    InvalidateCorrection {},
    ReviewCorrection {
        owner: OwnerId,
        reference: ProfileCatalogReference,
    },
    ReviewFirAsset {
        owner: OwnerId,
        asset: FirAssetReference,
    },
    PackageInputs {
        directory: String,
        destination: String,
    },
    ReadPackageObject {
        index: usize,
        basis_token: String,
    },
    MarkPackageObjectReviewed {
        basis_token: String,
    },
    PackageAcknowledgments {
        basis_token: String,
        exact: bool,
        private: bool,
        measurements: bool,
    },
    ExportReviewedPackage {
        basis_token: String,
    },
    CancelLocalPackageReview {},
    DiscardPreparedGraph {},
    Mute {},
    Stop {},
    Reconcile {
        request_id: RequestId,
    },
    ShutdownAndQuit {
        basis_token: String,
    },
    CloseController {},
    Quit {},
    GraphReset {},
    GraphLoadProfile {
        basis_token: String,
    },
    GraphAcceptJson {
        text: String,
    },
    GraphRestore {},
    GraphAddFir {
        basis_token: String,
    },
    GraphChainEdit {
        operation: String,
        node_kind: Option<NodeKind>,
        from: Option<usize>,
        to: Option<usize>,
    },
    ProfileDraftLoad {
        basis_token: String,
    },
    ProfileDraftReset {},
    CorrectionReconstruct {
        report_json: String,
    },
}
pub(super) fn parse(text: &str) -> Result<NativeAction, NativeError> {
    if text.is_empty() || text.len() > earph_engine::control_frame_limit() {
        return Err(invalid("Native action exceeds its byte limit"));
    }
    let mut value: Value =
        serde_json::from_str(text).map_err(|error| invalid(error.to_string()))?;
    let object = value
        .as_object_mut()
        .ok_or_else(|| invalid("Native action must be an object"))?;
    if object.remove("version") != Some(Value::from(1)) {
        return Err(invalid("Native action requires version one"));
    }
    serde_json::from_value(value).map_err(|error| invalid(error.to_string()))
}
impl NativeState {
    pub(super) fn dispatch(&mut self, action: NativeAction) -> Result<(), NativeError> {
        let intent = match action {
            NativeAction::Attach { runtime } => UiIntent::Attach { runtime },
            NativeAction::Launch {
                runtime,
                executable,
            } => UiIntent::Launch {
                runtime,
                executable,
            },
            NativeAction::Disconnect {} => UiIntent::Disconnect,
            NativeAction::Refresh {} => UiIntent::Refresh,
            NativeAction::QueueEdit {
                basis_token,
                command,
            } => UiIntent::QueueEdit {
                basis: self.basis(&basis_token, true)?.state,
                command,
            },
            NativeAction::ControlEdit {
                basis_token,
                command,
            } => UiIntent::ControlEdit {
                basis: self.basis(&basis_token, false)?.state,
                command,
            },
            NativeAction::StartLocal {
                basis_token,
                takeover,
            } => UiIntent::StartLocal {
                basis: self.basis(&basis_token, false)?.state,
                takeover,
            },
            NativeAction::PlayLocal {
                basis_token,
                takeover,
                entry_id,
            } => UiIntent::PlayLocal {
                basis: self.basis(&basis_token, false)?.state,
                takeover,
                entry_id,
            },
            NativeAction::SetVolume {
                basis_token,
                millidecibels,
                ramp_frames,
            } => UiIntent::SetVolume {
                basis: self.basis(&basis_token, false)?.state,
                millidecibels,
                ramp_frames,
            },
            NativeAction::Discover { backend, endpoint } => {
                UiIntent::Discover { backend, endpoint }
            }
            NativeAction::ProfileAction {
                basis_token,
                command,
            } => {
                if matches!(
                    command,
                    Command::GraphValidate { .. }
                        | Command::GraphPrepare { .. }
                        | Command::ProfilesPrepare { .. }
                ) && !self.graph_review_available()
                {
                    return Err(invalid(
                        "Review the loaded profile's actual correction before preparing its graph",
                    ));
                }
                if self.graph.has_pending_raw_edits()
                    && matches!(
                        command,
                        Command::ProfilesCreate { .. }
                            | Command::ProfilesUpdate { .. }
                            | Command::GraphValidate { .. }
                            | Command::GraphPrepare { .. }
                            | Command::GraphCommit { .. }
                            | Command::ProfilesPrepare { .. }
                            | Command::ProfilesApply { .. }
                    )
                {
                    return Err(invalid(
                        "Accept or restore pending graph edits before saving or preparing processing",
                    ));
                }
                UiIntent::ProfileAction {
                    basis: self.basis(&basis_token, false)?.state,
                    command,
                }
            }
            NativeAction::DiagnosticAction {
                basis_token,
                command,
            } => UiIntent::DiagnosticAction {
                basis: self.basis(&basis_token, false)?.state,
                command,
            },
            NativeAction::DiagnosticDestination { directory } => {
                UiIntent::DiagnosticDestination(directory)
            }
            NativeAction::ReviewDiagnosticPreview { basis_token } => {
                self.diagnostic_review_context(&basis_token)?;
                UiIntent::ReviewDiagnosticPreview
            }
            NativeAction::PreviewCorrection {
                basis_token,
                request,
            } => UiIntent::PreviewCorrection {
                basis: self.basis(&basis_token, false)?.state,
                request,
            },
            NativeAction::InvalidateCorrection {} => UiIntent::InvalidateCorrection,
            NativeAction::ReviewCorrection { owner, reference } => {
                UiIntent::ReviewCorrection { owner, reference }
            }
            NativeAction::ReviewFirAsset { owner, asset } => {
                UiIntent::ReviewFirAsset { owner, asset }
            }
            NativeAction::PackageInputs {
                directory,
                destination,
            } => {
                self.package_directory = directory.clone();
                UiIntent::PackageInputs {
                    directory,
                    destination,
                }
            }
            NativeAction::ReadPackageObject { index, basis_token } => {
                self.package_review_context(&basis_token, false, false)?;
                UiIntent::ReadPackageObject { index }
            }
            NativeAction::MarkPackageObjectReviewed { basis_token } => {
                self.package_review_context(&basis_token, true, false)?;
                UiIntent::MarkPackageObjectReviewed
            }
            NativeAction::PackageAcknowledgments {
                basis_token,
                exact,
                private,
                measurements,
            } => {
                self.package_review_context(&basis_token, true, true)?;
                UiIntent::PackageAcknowledgments {
                    exact,
                    private,
                    measurements,
                }
            }
            NativeAction::ExportReviewedPackage { basis_token } => {
                self.package_review_context(&basis_token, true, true)?;
                UiIntent::ExportReviewedPackage
            }
            NativeAction::CancelLocalPackageReview {} => UiIntent::CancelLocalPackageReview,
            NativeAction::DiscardPreparedGraph {} => UiIntent::DiscardPreparedGraph,
            NativeAction::Mute {} => UiIntent::Mute,
            NativeAction::Stop {} => UiIntent::Stop,
            NativeAction::Reconcile { request_id } => UiIntent::Reconcile(request_id),
            NativeAction::ShutdownAndQuit { basis_token } => {
                let basis = self.basis(&basis_token, false)?;
                if !self.owner_scope_matches(&basis) {
                    return Err(invalid(
                        "Rendered engine owner changed; review the shutdown target again",
                    ));
                }
                UiIntent::ShutdownAndQuit
            }
            NativeAction::CloseController {} => UiIntent::CloseController,
            NativeAction::Quit {} => UiIntent::Quit,
            NativeAction::GraphReset {} => {
                self.graph = GraphIntentEditor::default();
                UiIntent::DiscardPreparedGraph
            }
            NativeAction::ProfileDraftReset {} => {
                self.profile = Default::default();
                self.graph = GraphIntentEditor::default();
                UiIntent::DiscardPreparedGraph
            }
            NativeAction::GraphLoadProfile { basis_token }
            | NativeAction::ProfileDraftLoad { basis_token } => {
                let captured = self.basis(&basis_token, false)?;
                let record = captured.profile.ok_or_else(|| {
                    invalid("Observe the exact saved profile before loading its draft")
                })?;
                self.profile.load(&record).map_err(invalid)?;
                self.profile.selected = Some(ProfileCatalogReference {
                    id: record.profile.id.clone(),
                    revision: record.profile.revision,
                    profile_sha256: record.profile_sha256,
                });
                self.graph.load((*record.graph).clone());
                UiIntent::DiscardPreparedGraph
            }
            NativeAction::GraphAcceptJson { text } => {
                self.graph.native_accept(text);
                UiIntent::DiscardPreparedGraph
            }
            NativeAction::GraphRestore {} => {
                self.graph.load(self.graph.intent().clone());
                UiIntent::DiscardPreparedGraph
            }
            NativeAction::GraphAddFir { basis_token } => {
                let captured = self.basis(&basis_token, false)?;
                let asset = captured
                    .fir_asset
                    .ok_or_else(|| invalid("Observe the verified FIR asset before adding it"))?;
                if self
                    .controller
                    .state()
                    .and_then(|state| state.owner_id.as_ref())
                    != captured.state.owner_id.as_ref()
                    || self.controller.fir_asset() != Some(&asset)
                    || !self.controller.fir_asset_reviewed()
                {
                    return Err(invalid(
                        "Review the actual verified FIR asset before adding it",
                    ));
                }
                self.graph.add_fir(asset).map_err(invalid)?;
                UiIntent::DiscardPreparedGraph
            }
            NativeAction::GraphChainEdit {
                operation,
                node_kind,
                from,
                to,
            } => {
                self.graph
                    .native_chain_edit(&operation, node_kind, from, to)
                    .map_err(invalid)?;
                UiIntent::DiscardPreparedGraph
            }
            NativeAction::CorrectionReconstruct { report_json } => {
                self.correction_request = Some(
                    CorrectionPreviewRequest::from_text_report(report_json.as_bytes())
                        .map_err(invalid)?,
                );
                UiIntent::InvalidateCorrection
            }
        };
        self.controller.handle(intent);
        Ok(())
    }
}
