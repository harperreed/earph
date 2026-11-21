// ABOUTME: Captures immutable native presentation using the existing typed observations.
// ABOUTME: Preserves exact request bytes, review facts, unknown values, and receipt outcomes.
use super::{NativeError, NativeState};
use crate::{DeliveryFailure, Presentation, Reply};
use earph_core::profile_control::ProfileCatalogReference;
use serde_json::{Value, json};

impl NativeState {
    pub(super) fn render(&mut self) -> Result<String, NativeError> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| NativeError::Internal {
                detail: "Native render generation exhausted".into(),
            })?;
        let basis_token = self.capture(self.controller.state_basis(), false);
        let queue_basis_token = self.capture(self.controller.queue_state_basis(), true);
        let controller = &self.controller;
        let profiles = &controller.profiles;
        let review = &controller.package_review;
        let correction_reviewed = controller.profile_record().is_some_and(|record| {
            controller.correction_reviewed(&ProfileCatalogReference {
                id: record.profile.id.clone(),
                revision: record.profile.revision,
                profile_sha256: record.profile_sha256,
            })
        });
        let profile_review_available = controller.profile_record().is_some_and(|record| {
            controller.profile_review_available(&ProfileCatalogReference {
                id: record.profile.id.clone(),
                revision: record.profile.revision,
                profile_sha256: record.profile_sha256,
            })
        });
        let correction_preview = controller.correction_preview().map(|preview| {
            let mut value = serde_json::to_value(preview).expect("typed correction serialization");
            value["input"] = serde_json::to_value(preview.input())
                .expect("typed correction input serialization");
            value["source_sha256"] = json!(preview.source_sha256());
            value
        });
        let displayed = review.displayed.as_ref().map(|object|json!({
            "object": object.object, "path": object.path, "wav": object.wav,
            "text": if object.wav.is_none() { std::str::from_utf8(&object.bytes).ok() } else { None }
        }));
        let seek_bounds = controller.seek_bounds().map(
            |(entry_id, total_frames)| json!({"entry_id":entry_id,"total_frames":total_frames}),
        );
        let mut local_draft = self.graph.native_snapshot();
        local_draft
            .as_object_mut()
            .expect("graph snapshot object")
            .extend(
                self.profile
                    .native_snapshot()
                    .as_object()
                    .expect("profile snapshot object")
                    .clone(),
            );
        local_draft["correction_request"] = json!(self.correction_request);
        let receipts: Vec<Value> = controller.receipts().map(|receipt| {
            let command = serde_json::to_value(&receipt.request.envelope().payload).expect("typed command serialization");
            let (reply_json, outcome, failure) = match receipt.outcome {
                Some(Ok(reply)) => (
                    std::str::from_utf8(reply.bytes()).ok(),
                    Some(match reply {Reply::Receipt(value)=>json!(value),Reply::OwnerError(value)=>json!({"error":value.error})}),
                    None
                ),
                Some(Err(DeliveryFailure::Local {code,detail})) => (None,None,Some(json!({"kind":"local","code":code,"detail":detail}))),
                Some(Err(DeliveryFailure::Protocol(error))) => (None,None,Some(json!({"kind":"protocol","detail":error.detail,
                    "reply_hex":error.bytes.iter().map(|byte|format!("{byte:02x}")).collect::<String>()}))),
                None => (None,None,None),
            };
            json!({
                "request_id":receipt.request.envelope().request_id,
                "method":command["method"],"command":command,
                "presentation":presentation(receipt.presentation),
                "in_flight":receipt.in_flight,"owner_changed":receipt.owner_changed,
                "request_json":std::str::from_utf8(receipt.request.bytes()).expect("typed request JSON"),
                "reply_json":reply_json,"outcome":outcome,"failure":failure
            })
        }).collect();
        let view = json!({
            "version":1,"generation":self.generation,
            "basis_token":basis_token,"queue_basis_token":queue_basis_token,
            "host":controller.host_status(),"runtime":controller.runtime(),
            "flags":{
                "connected":controller.connected(),"attached":controller.attached(),"busy":controller.busy(),
                "ready_to_close":controller.ready_to_close(),"queue_stale":controller.queue_stale(),
                "queue_edit_pending":controller.queue_edit_pending(),"queue_edit_available":controller.queue_edit_available(),
                "control_edit_available":controller.control_edit_available(),"diagnostic_read_available":controller.diagnostic_read_available(),
                "prepared_action_available":controller.prepared_action_available(),"package_files_pending":controller.package_files_pending(),
                "package_export_available":controller.package_export_available(),"package_import_available":controller.package_import_available(&self.package_directory),
                "association_available":controller.association_available(),"correction_preview_current":controller.correction_preview_current(),
                "local_correction_pending":controller.local_correction_pending(),"fir_asset_reviewed":controller.fir_asset_reviewed(),
                "diagnostic_preview_reviewed":controller.diagnostic_preview_reviewed(),"seek_bounds":seek_bounds
                ,"graph_source_available":controller.state().is_some_and(crate::StateView::graph_source_available),
                "profile_review_available":profile_review_available
                ,"graph_review_available":self.graph_review_available()
            },
            "error":controller.error(),
            "observed_age_ms":controller.observed_at().map(|at|u64::try_from(at.elapsed().as_millis()).unwrap_or(u64::MAX)),
            "state":controller.state(),"capabilities":controller.capabilities(),"queue":controller.queue(),
            "discovery":controller.discovery(),"inspection":controller.inspection(),
            "profiles":{
                "catalog":controller.profile_catalog(),"record":controller.profile_record(),"resolution":controller.profile_resolution(),
                "graph_inspection":controller.graph_inspection(),"graph_preview":profiles.preview.as_ref().map(|view|&view.value),
                "graph_diagnostic":profiles.diagnostic.as_ref().map(|(request_id,diagnostic)|json!({"request_id":request_id,"diagnostic":diagnostic})),
                "prepared_graph":controller.prepared_graph(),"candidate":controller.package_candidate(),
                "package_issue":profiles.package_issue.as_ref().map(|(request_id,issue)|json!({"request_id":request_id,"issue":issue})),"bound":profiles.bound.as_ref().map(|view|&view.value)
            },
            "diagnostics":{
                "signal_path":controller.diagnostic_signal_path(),"metrics":controller.diagnostic_metrics(),
                "events":controller.diagnostic_events(),"preview":controller.diagnostic_preview(),
                "directory":controller.diagnostics.directory,"reviewed":controller.diagnostic_preview_reviewed()
            },
            "local_review":{
                "correction_preview":correction_preview,"local_correction_status":controller.local_correction_status(),
                "fir_asset":controller.fir_asset(),"correction_reviewed":correction_reviewed,
                "package_directory":self.package_directory,"package_destination":review.destination,
                "package_exact":review.exact,"package_private":review.private,"package_measurements":review.measurements,
                "package_objects_read":review.read,"package_displayed_object":displayed,
                "unresolved_package_write":controller.unresolved_package_write().map(|(request_id,directory,issue)|json!({"request_id":request_id,"directory":directory,"issue":issue}))
            },
            "receipts":receipts,"local_draft":local_draft
        });
        serde_json::to_string(&view).map_err(|error| NativeError::Internal {
            detail: error.to_string(),
        })
    }
}
fn presentation(value: Presentation) -> &'static str {
    match value {
        Presentation::Queued => "queued",
        Presentation::Accepted => "accepted",
        Presentation::Applied => "applied",
        Presentation::Rejected => "rejected",
        Presentation::Cancelled => "cancelled",
        Presentation::Failed => "failed",
        Presentation::OwnerError => "owner_error",
        Presentation::Uncertain => "uncertain",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gui::NativeController;
    use earph_core::{EngineError, ErrorCode, graph_control::GraphDiagnostic, ids::RequestId};
    use std::sync::Arc;

    #[test]
    fn cached_core_graph_diagnostic_remains_visible_without_a_retained_receipt() {
        let controller = NativeController::new();
        let diagnostic = GraphDiagnostic::from(EngineError::new(
            ErrorCode::GraphInvalid,
            "controlled diagnostic",
        ));
        let view = serde_json::from_value(serde_json::to_value(diagnostic).unwrap()).unwrap();
        controller.lock().unwrap().controller.profiles.diagnostic =
            Some((RequestId::new("native-diagnostic").unwrap(), Arc::new(view)));
        let snapshot: Value = serde_json::from_str(&controller.snapshot().unwrap()).unwrap();
        assert!(snapshot["receipts"].as_array().unwrap().is_empty());
        assert_eq!(
            snapshot["profiles"]["graph_diagnostic"]["request_id"],
            "native-diagnostic"
        );
        assert_eq!(
            snapshot["profiles"]["graph_diagnostic"]["diagnostic"]["code"],
            "GRAPH_INVALID"
        );
        assert!(snapshot["profiles"]["graph_diagnostic"]["diagnostic"]["node"].is_null());
    }
}
