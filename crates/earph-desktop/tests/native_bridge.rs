// ABOUTME: Exercises the native bridge through real controller observations and owner requests.
// ABOUTME: Proves captured edit bases, local draft validation, and non-owning close behavior.
mod common;
#[path = "gui_receipt_fixture/mod.rs"]
mod receipt_fixture;
use common::*;
use earph_core::control::Command;
use earph_desktop::gui::NativeController;
use serde_json::{Value, json};
use std::{
    thread,
    time::{Duration, Instant},
};

fn value(controller: &NativeController) -> Value {
    serde_json::from_str(&controller.tick().unwrap()).unwrap()
}
fn send(controller: &NativeController, action: Value) -> Value {
    serde_json::from_str(&controller.send(action.to_string()).unwrap()).unwrap()
}
fn wait(controller: &NativeController, predicate: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        let snapshot = value(controller);
        if predicate(&snapshot) {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "native bridge deadline: {snapshot}"
        );
        thread::sleep(Duration::from_millis(4));
    }
}
fn attach(controller: &NativeController, owner: &Owner) -> Value {
    send(
        controller,
        json!({"version":1,"kind":"attach","runtime":owner.runtime.to_str().unwrap()}),
    );
    wait(controller, |s| s["flags"]["queue_edit_available"] == true)
}

#[test]
fn native_product_play_reports_missing_setup_without_granting_output_or_starting_audio() {
    let mut owner = Owner::start();
    let controller = NativeController::new();
    let before = attach(&controller, &owner);
    assert_eq!(before["flags"]["play_local_available"], false);
    assert_eq!(before["flags"]["play_local_pending"], false);
    assert_eq!(before["flags"]["volume_edit_available"], false);
    let volume = send(
        &controller,
        json!({"version":1,"kind":"set_volume", "basis_token":before["basis_token"],"millidecibels":-25000,"ramp_frames":64}),
    );
    assert!(
        volume["error"]
            .as_str()
            .is_some_and(|error| error.contains("Software volume is unavailable"))
    );
    let after = send(
        &controller,
        json!({"version":1,"kind":"play_local",
        "basis_token":before["basis_token"],"takeover":false}),
    );
    assert!(
        after["error"]
            .as_str()
            .is_some_and(|s| s.contains("approve an output"))
    );
    assert_eq!(after["flags"]["play_local_pending"], false);
    assert!(after["playback_action_status"].is_string());
    assert!(owner.state().session_id.is_none());
    assert!(owner.state().output.is_none());
    send(&controller, json!({"version":1,"kind":"close_controller"}));
    wait(&controller, |s| s["flags"]["ready_to_close"] == true);
    owner.stop();
}

#[test]
fn construction_and_local_drafts_never_attach_or_launch() {
    let controller = NativeController::new();
    let initial = value(&controller);
    assert!(initial["state"].is_null());
    assert_eq!(initial["flags"]["attached"], false);
    assert_eq!(initial["host"]["kind"], "idle");
    assert_eq!(
        initial["local_draft"]["graph"]["graph"]["nodes"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    let saved = initial["local_draft"]["graph"].clone();
    let rejected = send(
        &controller,
        json!({"version":1,"kind":"graph_accept_json","text":"{bad"}),
    );
    assert_eq!(rejected["local_draft"]["graph"], saved);
    assert!(!rejected["local_draft"]["graph_error"].is_null());
    assert_eq!(rejected["local_draft"]["graph_dirty"], true);
    let restored = send(&controller, json!({"version":1,"kind":"graph_restore"}));
    assert_eq!(restored["local_draft"]["graph_dirty"], false);
    assert!(restored["state"].is_null());
}

#[test]
fn malformed_actions_never_change_controller_state() {
    let controller = NativeController::new();
    for invalid in [
        json!({"version":2,"kind":"refresh"}),
        json!({"version":1,"kind":"unknown"}),
        json!({"version":1,"kind":"refresh","extra":true}),
        json!({"version":1,"kind":"control_edit","basis_token":"bad","command":{"method":"sessions.pause"}}),
    ] {
        assert!(controller.send(invalid.to_string()).is_err(), "{invalid}");
    }
    assert!(controller.send("x".repeat(1_048_577)).is_err());
    let observed = value(&controller);
    assert_eq!(observed["flags"]["attached"], false);
    assert!(observed["receipts"].as_array().unwrap().is_empty());
}

#[test]
fn queue_action_keeps_the_rendered_basis_and_rejects_expired_tokens() {
    let owner = Owner::start();
    let controller = NativeController::new();
    let rendered = attach(&controller, &owner);
    let token = rendered["queue_basis_token"].clone();
    let request = earph_desktop::Request::mutation(
        earph_core::ids::RequestId::new(unique()).unwrap(),
        &owner.state(),
        Command::QueueAppend {
            path: "/owned/external.wav".into(),
        },
    )
    .unwrap();
    owner.client().request(&request).unwrap();
    send(&controller, json!({"version":1,"kind":"refresh"}));
    let fresh = wait(&controller, |s| {
        s["queue"]["entries"]
            .as_array()
            .is_some_and(|e| e.len() == 1)
            && s["flags"]["queue_edit_available"] == true
    });
    assert_ne!(fresh["state"]["revision"], rendered["state"]["revision"]);
    send(
        &controller,
        json!({"version":1,"kind":"queue_edit","basis_token":token,
        "command":{"method":"queue.append","path":"/owned/stale.wav"}}),
    );
    wait(&controller, |s| s["error"].as_str().is_some());
    assert_eq!(
        owner.state().revision,
        fresh["state"]["revision"].as_u64().unwrap()
    );
    let expired = fresh["basis_token"].clone();
    for _ in 0..40 {
        let _ = value(&controller);
    }
    assert!(
        controller
            .send(
                json!({"version":1,"kind":"control_edit","basis_token":expired,
        "command":{"method":"sessions.pause"}})
                .to_string()
            )
            .is_err()
    );
    send(&controller, json!({"version":1,"kind":"close_controller"}));
    wait(&controller, |s| s["flags"]["ready_to_close"] == true);
    assert!(owner.state().muted);
    assert!(!owner.state().shutdown_requested);
}

#[test]
fn controller_preserves_exact_uncertain_request_for_reconciliation() {
    let owner = Owner::start();
    let proxy = common::proxy::Proxy::drop_first_append(owner.runtime.clone());
    let controller = NativeController::new();
    send(
        &controller,
        json!({"version":1,"kind":"attach","runtime":proxy.runtime.to_str().unwrap()}),
    );
    let observed = wait(&controller, |s| s["flags"]["queue_edit_available"] == true);
    send(
        &controller,
        json!({"version":1,"kind":"queue_edit","basis_token":observed["queue_basis_token"],
        "command":{"method":"queue.append","path":"/owned/uncertain.wav"}}),
    );
    let uncertain = wait(&controller, |s| {
        s["receipts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["presentation"] == "uncertain")
    });
    let receipt = uncertain["receipts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["presentation"] == "uncertain")
        .unwrap();
    let id = receipt["request_id"].clone();
    let bytes = receipt["request_json"].clone();
    send(
        &controller,
        json!({"version":1,"kind":"reconcile","request_id":id}),
    );
    wait(&controller, |s| {
        s["receipts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["request_id"] == id && r["presentation"] == "applied")
    });
    let frames = proxy.frames.lock().unwrap();
    let appends: Vec<_> = frames
        .iter()
        .filter(|b| {
            serde_json::from_slice::<Value>(b).unwrap()["payload"]["method"] == "queue.append"
        })
        .collect();
    assert!(appends.len() >= 2);
    assert!(
        appends
            .iter()
            .all(|b| b.as_slice() == bytes.as_str().unwrap().as_bytes())
    );
    drop(frames);
    send(&controller, json!({"version":1,"kind":"close_controller"}));
    wait(&controller, |s| s["flags"]["ready_to_close"] == true);
    assert!(!owner.state().shutdown_requested);
}

#[test]
fn mute_and_close_remain_nonblocking_while_real_observation_reply_is_held() {
    let owner = Owner::start();
    let proxy = common::proxy::Proxy::hold_first_capabilities(owner.runtime.clone());
    let controller = NativeController::new();
    send(
        &controller,
        json!({"version":1,"kind":"attach","runtime":proxy.runtime.to_str().unwrap()}),
    );
    wait(&controller, |s| !s["state"].is_null());
    proxy.entered.recv_timeout(Duration::from_secs(2)).unwrap();
    let started = Instant::now();
    send(&controller, json!({"version":1,"kind":"mute"}));
    assert!(started.elapsed() < Duration::from_millis(250));
    wait(&controller, |s| {
        s["receipts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["method"] == "mute.set" && r["presentation"] == "applied")
    });
    let started = Instant::now();
    let closing = send(&controller, json!({"version":1,"kind":"close_controller"}));
    assert!(started.elapsed() < Duration::from_millis(250));
    assert_eq!(closing["flags"]["ready_to_close"], false);
    proxy.release();
    wait(&controller, |s| s["flags"]["ready_to_close"] == true);
    assert!(!owner.state().shutdown_requested);
}

#[test]
fn native_chain_edits_use_shared_topology_and_preserve_endpoint_identity() {
    let controller = NativeController::new();
    let add = send(
        &controller,
        json!({"version":1,"kind":"graph_chain_edit","operation":"add","node_kind":{"kind":"gain","gain_linear":0.5}}),
    );
    let graph = &add["local_draft"]["graph"]["graph"];
    assert_eq!(graph["nodes"][1]["kind"]["gain_linear"], 0.5);
    assert_eq!(graph["edges"][0]["from"], "in");
    assert_eq!(graph["edges"][1]["to"], "out");
    let removed = send(
        &controller,
        json!({"version":1,"kind":"graph_chain_edit","operation":"remove","from":1}),
    );
    assert_eq!(
        removed["local_draft"]["graph"]["graph"]["nodes"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert!(
        controller
            .send(
                json!({"version":1,"kind":"graph_chain_edit","operation":"remove","from":0})
                    .to_string()
            )
            .is_err()
    );
}

#[test]
fn native_profile_and_diagnostic_actions_adopt_real_owner_views() {
    let owner = Owner::start();
    let controller = NativeController::new();
    let initial = attach(&controller, &owner);
    let create = serde_json::to_value(profile_payload()).unwrap();
    send(
        &controller,
        json!({"version":1,"kind":"profile_action","basis_token":initial["basis_token"],"command":create}),
    );
    let saved = wait(&controller, |s| {
        s["profiles"]["record"]["profile"]["id"] == "desktop-profile"
            && s["flags"]["control_edit_available"] == true
    });
    assert_eq!(saved["flags"]["graph_source_available"], false);
    assert_eq!(saved["flags"]["profile_review_available"], true);
    let loaded = send(
        &controller,
        json!({"version":1,"kind":"profile_draft_load","basis_token":saved["basis_token"]}),
    );
    assert_eq!(
        loaded["local_draft"]["profile_fields"]["id"],
        "desktop-profile"
    );
    assert_eq!(
        loaded["local_draft"]["profile_fields"]["software_cap"],
        "-20000"
    );
    assert_eq!(
        loaded["local_draft"]["graph"],
        saved["profiles"]["record"]["graph"]
    );
    for (method, key) in [
        ("signal_path.get", "signal_path"),
        ("metrics.get", "metrics"),
        ("events.get", "events"),
        ("diagnostics.preview", "preview"),
    ] {
        let ready = wait(&controller, |s| {
            s["flags"]["diagnostic_read_available"] == true
        });
        let command = if method == "events.get" {
            json!({"method":method,"cursor":null})
        } else {
            json!({"method":method})
        };
        send(
            &controller,
            json!({"version":1,"kind":"diagnostic_action","basis_token":ready["basis_token"],"command":command}),
        );
        wait(&controller, |s| !s["diagnostics"][key].is_null());
    }
    assert!(owner.state().muted);
    assert!(owner.state().source.is_none());
    send(&controller, json!({"version":1,"kind":"close_controller"}));
    wait(&controller, |s| s["flags"]["ready_to_close"] == true);
}

#[test]
fn overflowing_chain_positions_are_refused_without_poisoning_the_handle() {
    let controller = NativeController::new();
    let result = controller.send(
        json!({"version":1,"kind":"graph_chain_edit","operation":"remove","from":u64::MAX})
            .to_string(),
    );
    assert!(result.is_err());
    assert!(controller.snapshot().is_ok());
}

#[test]
fn native_chain_positions_follow_edges_instead_of_node_storage_order() {
    let controller = NativeController::new();
    let mut graph = value(&controller)["local_draft"]["graph"].clone();
    graph["graph"]["nodes"].as_array_mut().unwrap().swap(0, 1);
    let loaded = send(
        &controller,
        json!({"version":1,"kind":"graph_accept_json","text":graph.to_string()}),
    );
    assert_eq!(
        loaded["local_draft"]["graph_chain_nodes"],
        json!([{ "id":"in","position":0},{"id":"out","position":1}])
    );
}

#[test]
fn review_actions_require_the_context_that_the_person_saw() {
    let controller = NativeController::new();
    for kind in [
        "review_diagnostic_preview",
        "mark_package_object_reviewed",
        "export_reviewed_package",
        "shutdown_and_quit",
    ] {
        assert!(
            controller
                .send(json!({"version":1,"kind":kind}).to_string())
                .is_err(),
            "missing render token accepted for {kind}"
        );
    }
    assert!(controller.send(json!({"version":1,"kind":"package_acknowledgments","exact":true,"private":true,"measurements":false}).to_string()).is_err());
}

#[test]
fn diagnostic_review_uses_the_rendered_preview_and_destination() {
    let owner = Owner::start();
    let controller = NativeController::new();
    attach(&controller, &owner);
    let destination = format!("/tmp/{}/preview", unique());
    send(
        &controller,
        json!({"version":1,"kind":"diagnostic_destination","directory":destination}),
    );
    let first = diagnostic_preview(&controller);
    let old_token = first["basis_token"].clone();
    send(
        &controller,
        json!({"version":1,"kind":"review_diagnostic_preview","basis_token":old_token}),
    );
    assert_eq!(value(&controller)["diagnostics"]["reviewed"], true);
    let second = diagnostic_preview(&controller);
    assert!(
        controller
            .send(
                json!({"version":1,"kind":"review_diagnostic_preview","basis_token":old_token})
                    .to_string()
            )
            .is_err()
    );
    assert_eq!(value(&controller)["diagnostics"]["reviewed"], false);
    send(
        &controller,
        json!({"version":1,"kind":"diagnostic_destination","directory":format!("{destination}-changed")}),
    );
    assert!(controller.send(json!({"version":1,"kind":"review_diagnostic_preview","basis_token":second["basis_token"]}).to_string()).is_err());
    let current = value(&controller);
    send(
        &controller,
        json!({"version":1,"kind":"review_diagnostic_preview","basis_token":current["basis_token"]}),
    );
    assert_eq!(value(&controller)["diagnostics"]["reviewed"], true);
    send(&controller, json!({"version":1,"kind":"close_controller"}));
    wait(&controller, |s| s["flags"]["ready_to_close"] == true);
}
fn diagnostic_preview(controller: &NativeController) -> Value {
    let ready = wait(controller, |s| {
        s["flags"]["diagnostic_read_available"] == true
    });
    send(
        controller,
        json!({"version":1,"kind":"diagnostic_action","basis_token":ready["basis_token"],"command":{"method":"diagnostics.preview"}}),
    );
    wait(controller, |s| {
        !s["diagnostics"]["preview"].is_null() && s["flags"]["diagnostic_read_available"] == true
    })
}

#[test]
fn package_review_refuses_replaced_objects_destinations_and_candidates() {
    let owner = Owner::start();
    let controller = NativeController::new();
    let initial = attach(&controller, &owner);
    send(
        &controller,
        json!({"version":1,"kind":"profile_action","basis_token":initial["basis_token"],"command":serde_json::to_value(profile_payload()).unwrap()}),
    );
    let saved = wait(&controller, |s| {
        !s["profiles"]["record"].is_null()
            && s["flags"]["control_edit_available"] == true
            && s["state"]["revision"] == 1
    });
    let record = &saved["profiles"]["record"];
    let reference = json!({"id":record["profile"]["id"],"revision":record["profile"]["revision"],"profile_sha256":record["profile_sha256"]});
    send(
        &controller,
        json!({"version":1,"kind":"profile_action","basis_token":saved["basis_token"],"command":{"method":"profiles.package_preview","profile":reference}}),
    );
    let candidate = wait(&controller, |s| {
        !s["profiles"]["candidate"].is_null() && s["flags"]["busy"] == false
    });
    let inputs = send(
        &controller,
        json!({"version":1,"kind":"package_inputs","directory":"","destination":format!("/tmp/{}/package",unique())}),
    );
    let mut current = inputs;
    let count = candidate["profiles"]["candidate"]["review_sources"]
        .as_array()
        .unwrap()
        .len();
    assert!(count >= 2);
    let mut old_object_token = Value::Null;
    for index in 0..count {
        send(
            &controller,
            json!({"version":1,"kind":"read_package_object","index":index,"basis_token":current["basis_token"]}),
        );
        current = wait(&controller, |s| {
            !s["local_review"]["package_displayed_object"].is_null() && s["flags"]["busy"] == false
        });
        if index == 1 {
            assert!(controller.send(json!({"version":1,"kind":"mark_package_object_reviewed","basis_token":old_object_token}).to_string()).is_err());
        }
        old_object_token = current["basis_token"].clone();
        current = send(
            &controller,
            json!({"version":1,"kind":"mark_package_object_reviewed","basis_token":current["basis_token"]}),
        );
    }
    let acknowledged = send(
        &controller,
        json!({"version":1,"kind":"package_acknowledgments","basis_token":current["basis_token"],"exact":true,"private":true,"measurements":false}),
    );
    assert_eq!(acknowledged["flags"]["package_export_available"], true);
    send(
        &controller,
        json!({"version":1,"kind":"package_inputs","directory":"","destination":format!("/tmp/{}/changed",unique())}),
    );
    assert!(controller.send(json!({"version":1,"kind":"export_reviewed_package","basis_token":acknowledged["basis_token"]}).to_string()).is_err());
    assert!(controller.send(json!({"version":1,"kind":"package_acknowledgments","basis_token":current["basis_token"],"exact":true,"private":true,"measurements":false}).to_string()).is_err());
    let ready = value(&controller);
    send(
        &controller,
        json!({"version":1,"kind":"profile_action","basis_token":ready["basis_token"],"command":{"method":"profiles.package_preview","profile":reference}}),
    );
    wait(&controller, |s| {
        !s["profiles"]["candidate"].is_null() && s["flags"]["busy"] == false
    });
    assert!(controller.send(json!({"version":1,"kind":"mark_package_object_reviewed","basis_token":old_object_token}).to_string()).is_err());
    assert_eq!(
        value(&controller)["local_review"]["package_objects_read"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    let mut other = serde_json::to_value(profile_payload()).unwrap();
    other["id"] = json!("other-profile");
    let ready = wait(&controller, |s| s["flags"]["busy"] == false);
    send(
        &controller,
        json!({"version":1,"kind":"profile_action","basis_token":ready["basis_token"],"command":other}),
    );
    let other_saved = wait(&controller, |s| {
        s["profiles"]["record"]["profile"]["id"] == "other-profile" && s["flags"]["busy"] == false
    });
    let old_candidate_token = other_saved["basis_token"].clone();
    let record = &other_saved["profiles"]["record"];
    let other_reference = json!({"id":record["profile"]["id"],"revision":record["profile"]["revision"],"profile_sha256":record["profile_sha256"]});
    send(
        &controller,
        json!({"version":1,"kind":"profile_action","basis_token":other_saved["basis_token"],"command":{"method":"profiles.package_preview","profile":other_reference}}),
    );
    wait(&controller, |s| {
        s["profiles"]["candidate"]["manifest"]["profile"]["id"] == "other-profile"
            && s["flags"]["busy"] == false
    });
    assert!(controller.send(json!({"version":1,"kind":"read_package_object","basis_token":old_candidate_token,"index":0}).to_string()).is_err());
    assert!(owner.state().muted);
    assert!(owner.state().source.is_none());
    send(&controller, json!({"version":1,"kind":"close_controller"}));
    wait(&controller, |s| s["flags"]["ready_to_close"] == true);
}

#[test]
fn unchanged_rendered_package_review_exports_the_actual_original_objects() {
    use std::os::unix::fs::DirBuilderExt;
    let owner = Owner::start();
    let controller = NativeController::new();
    let initial = attach(&controller, &owner);
    send(
        &controller,
        json!({"version":1,"kind":"profile_action","basis_token":initial["basis_token"],"command":serde_json::to_value(profile_payload()).unwrap()}),
    );
    let saved = wait(&controller, |s| {
        !s["profiles"]["record"].is_null() && s["flags"]["busy"] == false
    });
    let record = &saved["profiles"]["record"];
    let reference = json!({"id":record["profile"]["id"],"revision":record["profile"]["revision"],"profile_sha256":record["profile_sha256"]});
    send(
        &controller,
        json!({"version":1,"kind":"profile_action","basis_token":saved["basis_token"],"command":{"method":"profiles.package_preview","profile":reference}}),
    );
    let candidate = wait(&controller, |s| {
        !s["profiles"]["candidate"].is_null() && s["flags"]["busy"] == false
    });
    let parent = std::env::temp_dir().join(unique());
    std::fs::DirBuilder::new()
        .mode(0o700)
        .create(&parent)
        .unwrap();
    let destination = parent.join("published");
    println!("Retained native package fixture: {}", parent.display());
    let mut current = send(
        &controller,
        json!({"version":1,"kind":"package_inputs","directory":"","destination":destination.to_str().unwrap()}),
    );
    for index in 0..candidate["profiles"]["candidate"]["review_sources"]
        .as_array()
        .unwrap()
        .len()
    {
        send(
            &controller,
            json!({"version":1,"kind":"read_package_object","basis_token":current["basis_token"],"index":index}),
        );
        current = wait(&controller, |s| {
            !s["local_review"]["package_displayed_object"].is_null() && s["flags"]["busy"] == false
        });
        current = send(
            &controller,
            json!({"version":1,"kind":"mark_package_object_reviewed","basis_token":current["basis_token"]}),
        );
    }
    let reviewed = send(
        &controller,
        json!({"version":1,"kind":"package_acknowledgments","basis_token":current["basis_token"],"exact":true,"private":true,"measurements":false}),
    );
    assert_eq!(reviewed["flags"]["package_export_available"], true);
    send(
        &controller,
        json!({"version":1,"kind":"export_reviewed_package","basis_token":reviewed["basis_token"]}),
    );
    let exported = wait(&controller, |s| {
        s["profiles"]["candidate"]["operation"] == "exported" && s["flags"]["busy"] == false
    });
    assert!(exported["error"].is_null());
    let manifest: Value =
        serde_json::from_slice(&std::fs::read(destination.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["profile"], reference);
    let result: earph_desktop::ProfilePackageResultView =
        serde_json::from_value(candidate["profiles"]["candidate"].clone()).unwrap();
    for source in result.review_sources {
        assert_eq!(
            std::fs::read(destination.join(source.object.file_name())).unwrap(),
            std::fs::read(source.local_path).unwrap()
        );
    }
    assert!(owner.state().muted);
    assert!(owner.state().source.is_none());
    send(&controller, json!({"version":1,"kind":"close_controller"}));
    wait(&controller, |s| s["flags"]["ready_to_close"] == true);
}

#[test]
fn correction_import_and_loaded_draft_keep_the_shared_review_gate() {
    let owner = Owner::start();
    let controller = NativeController::new();
    let initial = attach(&controller, &owner);
    let source = "Preamp: -6 dB\nFilter 1: ON PK Fc 100 Hz Gain 2 dB Q 1\n";
    send(
        &controller,
        json!({"version":1,"kind":"preview_correction","basis_token":initial["basis_token"],"request":{
        "source":{"kind":"text","format":"autoeq","text":source},
        "context":{"sample_rate_hz":48000,"channels":"stereo","quantum_frames":256},
        "metadata":{"headphone_model":"Owned public fixture","measurement_provider":null,"target":null,"parameter_set":null,"revision":null,"redistribution_license":null},"fir_preamp":null}}),
    );
    let preview = wait(&controller, |s| {
        s["flags"]["correction_preview_current"] == true
            && s["flags"]["control_edit_available"] == true
    });
    assert_eq!(
        preview["local_review"]["correction_preview"]["input"]["source_text"],
        source
    );
    let payload = serde_json::to_value(profile_payload()).unwrap();
    send(
        &controller,
        json!({"version":1,"kind":"profile_action","basis_token":preview["basis_token"],"command":{
        "method":"profiles.import_correction","id":"desktop-profile","expected_profile_revision":null,
        "draft":payload["draft"],"input":preview["local_review"]["correction_preview"]["input"]}}),
    );
    let saved = wait(&controller, |s| {
        !s["profiles"]["record"].is_null() && s["flags"]["control_edit_available"] == true
    });
    assert_eq!(
        saved["profiles"]["record"]["corrections"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let loaded = send(
        &controller,
        json!({"version":1,"kind":"profile_draft_load","basis_token":saved["basis_token"]}),
    );
    assert_eq!(loaded["flags"]["graph_review_available"], false);
    let graph = &loaded["local_draft"]["graph"];
    assert!(controller.send(json!({"version":1,"kind":"profile_action","basis_token":loaded["basis_token"],"command":{
        "method":"graph.validate","graph":graph["graph"],"processing":graph["processing"]}}).to_string()).is_err());
    let record = &loaded["profiles"]["record"];
    send(
        &controller,
        json!({"version":1,"kind":"review_correction","owner":loaded["state"]["owner_id"],"reference":{
        "id":record["profile"]["id"],"revision":record["profile"]["revision"],"profile_sha256":record["profile_sha256"]}}),
    );
    assert_eq!(value(&controller)["flags"]["graph_review_available"], true);
    let reset = send(
        &controller,
        json!({"version":1,"kind":"profile_draft_reset"}),
    );
    assert_eq!(reset["local_draft"]["profile_fields"]["id"], "");
    assert_eq!(reset["flags"]["graph_review_available"], true);
    assert!(owner.state().muted);
    assert!(owner.state().source.is_none());
    send(&controller, json!({"version":1,"kind":"close_controller"}));
    wait(&controller, |s| s["flags"]["ready_to_close"] == true);
}

#[test]
fn confirmed_shutdown_refuses_a_replacement_owner_and_closes_the_rendered_owner() {
    let first = Owner::start();
    let controller = NativeController::new();
    let old = attach(&controller, &first);
    send(&controller, json!({"version":1,"kind":"disconnect"}));
    wait(&controller, |s| s["flags"]["busy"] == false);
    let second = receipt_fixture::RealOwner::start();
    send(
        &controller,
        json!({"version":1,"kind":"attach","runtime":second.runtime.to_str().unwrap()}),
    );
    let current = wait(&controller, |s| {
        s["flags"]["queue_edit_available"] == true
            && s["state"]["owner_id"] != old["state"]["owner_id"]
    });
    assert!(
        controller
            .send(
                json!({"version":1,"kind":"shutdown_and_quit","basis_token":old["basis_token"]})
                    .to_string()
            )
            .is_err()
    );
    assert!(!second.state().shutdown_requested);
    send(
        &controller,
        json!({"version":1,"kind":"shutdown_and_quit","basis_token":current["basis_token"]}),
    );
    wait(&controller, |s| s["flags"]["ready_to_close"] == true);
    assert!(!first.state().shutdown_requested);
}

#[test]
fn native_quit_stops_the_attached_engine_before_the_controller_closes() {
    let runtime = std::path::PathBuf::from("/tmp").join(unique());
    let owner = earph_engine::EngineOwner::bind(&runtime).unwrap();
    let serve = std::thread::spawn(move || owner.serve());
    let controller = NativeController::new();
    send(
        &controller,
        json!({"version":1,"kind":"attach","runtime":runtime.to_str().unwrap()}),
    );
    wait(&controller, |s| !s["state"].is_null());
    send(&controller, json!({"version":1,"kind":"quit"}));
    wait(&controller, |s| s["flags"]["ready_to_close"] == true);
    let deadline = Instant::now() + Duration::from_secs(10);
    while !serve.is_finished() {
        assert!(Instant::now() < deadline, "Quit left the Engine serving");
        std::thread::sleep(Duration::from_millis(2));
    }
    serve.join().unwrap().unwrap();
}
