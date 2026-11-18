// ABOUTME: Exercises profile staging, cancellation, targeting, and bounded retained history.
// ABOUTME: Checks old checkpoint decoding and completion failures through real Session contracts.
use earph_core::{
    EngineError, ErrorCode,
    control::*,
    graph_control::GraphIntent,
    ids::*,
    profile_control::*,
    profiles::ProfileObjectHash,
    session::{Session, SessionCheckpoint},
};
use serde_json::json;
use std::sync::Arc;

fn draft() -> Arc<ProfileDraft> {
    Arc::new(serde_json::from_value(json!({"name":"Desk","source_policy":"local_file","fidelity":"compatible_conversion",
        "output":{"backend":"alsa","route":"system","device_id":"desk","manual_setup":null},
        "volume":{"mode":"software","software_cap_millidecibels":-10000,"hardware_cap_millidecibels":null},
        "latency":{"mode":"balanced","max_resident_bytes":268435456,"max_preparation_bytes":268435456,"max_latency_frames":null},"corrections":[]})).unwrap())
}
fn graph(large: bool) -> Arc<GraphIntent> {
    let count = if large { 128 } else { 2 };
    let ids: Vec<_> = (0..count)
        .map(|n| format!("node-{n:03}{}", "x".repeat(if large { 120 } else { 0 })))
        .collect();
    let port = json!({"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"signed16","max_quantum_frames":64});
    let nodes: Vec<_> = ids.iter().enumerate().map(|(n, id)| json!({"id":id,"kind":{"kind":if n == 0 {"input"} else if n == count-1 {"output"} else {"bypass"}},"format":port})).collect();
    let edges: Vec<_> = ids
        .windows(2)
        .map(|p| json!({"from":p[0],"to":p[1],"from_port":0,"to_port":0}))
        .collect();
    Arc::new(serde_json::from_value(json!({"graph":{"schema_version":1,"output":ids[count-1],"nodes":nodes,"edges":edges},"processing":{"kind":"integer_bypass"}})).unwrap())
}
fn session() -> Session {
    Session::new_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("owner").unwrap(),
        ControlLimits::default(),
    )
}
fn command(s: &Session, id: &str, payload: Command) -> CommandEnvelope {
    let state = s.snapshot();
    CommandEnvelope {
        schema_version: 1,
        request_id: RequestId::new(id).unwrap(),
        zone_id: state.zone_id,
        owner_id: state.owner_id,
        session_id: state.session_id,
        expected_revision: Some(state.revision),
        payload,
    }
}
fn create(s: &Session, id: &str, large: bool) -> CommandEnvelope {
    command(
        s,
        id,
        Command::ProfilesCreate {
            id: ProfileId::new("desk").unwrap(),
            draft: draft(),
            graph: graph(large),
        },
    )
}
fn record(job: &ProfileSaveJob) -> Arc<ProfileRecord> {
    let hash = ProfileObjectHash::try_from("0".repeat(64)).unwrap();
    Arc::new(ProfileRecord {
        profile: Arc::new(
            job.draft
                .document(job.id.clone(), job.revision, hash)
                .unwrap(),
        ),
        graph: job.graph.clone(),
        profile_sha256: hash,
    })
}

#[test]
fn save_is_pending_until_commit_and_replay_retains_the_terminal_result() {
    let mut s = session();
    let command = create(&s, "save", false);
    assert_eq!(
        s.submit_graph(command.clone(), 1).status,
        ReceiptStatus::Accepted
    );
    assert_eq!(s.snapshot().revision, 0);
    assert!(s.checkpoint().profile_catalog.is_empty());
    let job = s.profile_job().unwrap();
    for n in 0..1100 {
        s.submit(command_for_query(&s, &format!("query-{n}")));
    }
    assert_eq!(s.submit(command.clone()).status, ReceiptStatus::Accepted);
    s.complete_profile_save(
        &job.request_id,
        &job.binding,
        2,
        Ok(record(&job)),
        |staged| {
            assert_eq!(staged.checkpoint().profile_catalog.len(), 1);
            assert_eq!(staged.snapshot().revision, 1);
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(s.submit(command).status, ReceiptStatus::Applied);
    assert!(s.snapshot().muted);
}
fn command_for_query(s: &Session, id: &str) -> CommandEnvelope {
    command(s, id, Command::State)
}

#[test]
fn stop_shutdown_mute_revision_and_timeout_cannot_publish_delayed_saves() {
    for payload in [
        Command::Stop,
        Command::OwnerShutdown,
        Command::Mute { muted: true },
        Command::QueueAppend {
            path: "owned.wav".into(),
        },
    ] {
        let mut s = session();
        let save = create(&s, "save", false);
        s.submit_graph(save.clone(), 1);
        let job = s.profile_job().unwrap();
        let cancel = command(&s, "cancel", payload);
        let result = s.submit(cancel);
        assert!(
            matches!(
                result.status,
                ReceiptStatus::Applied | ReceiptStatus::Accepted
            ),
            "{result:?}"
        );
        assert!(
            s.complete_profile_save(
                &job.request_id,
                &job.binding,
                3,
                Ok(record(&job)),
                |_| panic!("cancelled completion reached persistence")
            )
            .is_err()
        );
        assert!(s.checkpoint().profile_catalog.is_empty());
        assert_eq!(s.submit(save).status, ReceiptStatus::Cancelled);
    }
    let mut s = session();
    let save = create(&s, "save", false);
    s.submit_graph(save.clone(), 1);
    let job = s.profile_job().unwrap();
    s.expire_profile_work(job.deadline_monotonic_ns);
    assert_eq!(s.submit(save).status, ReceiptStatus::Cancelled);
}

#[test]
fn failed_checkpoint_preserves_the_previous_catalog_and_replays_failure() {
    let mut s = session();
    let save = create(&s, "save", false);
    s.submit_graph(save.clone(), 1);
    let job = s.profile_job().unwrap();
    let failure = EngineError::new(ErrorCode::InternalFault, "controlled persistence failure");
    assert_eq!(
        s.complete_profile_save(&job.request_id, &job.binding, 2, Ok(record(&job)), |_| Err(
            failure
        )),
        Err(failure)
    );
    assert_eq!(s.snapshot().revision, 0);
    assert!(s.checkpoint().profile_catalog.is_empty());
    assert_eq!(s.submit(save).status, ReceiptStatus::Failed);
}

#[test]
fn profile_ids_and_nested_retained_capacity_reject_before_receipt_admission() {
    let mut s = session();
    let mut command = create(&s, "save", false);
    let mut id = String::with_capacity(100_000);
    id.push_str("save");
    command.request_id = RequestId::new(id).unwrap();
    assert_eq!(
        s.submit(command).error.unwrap().code,
        ErrorCode::ResourceBudgetExceeded
    );
    let mut command = create(&s, "save", false);
    if let Command::ProfilesCreate { draft, .. } = &mut command.payload {
        Arc::make_mut(draft).name.reserve(100_000);
    }
    assert_eq!(
        s.submit(command).error.unwrap().code,
        ErrorCode::ResourceBudgetExceeded
    );
    assert!(s.profile_job().is_none());
}

#[test]
fn retained_query_history_refuses_before_it_exceeds_the_profile_allowance() {
    let mut s = session();
    let save = create(&s, "save", true);
    assert_eq!(s.submit_graph(save, 1).status, ReceiptStatus::Accepted);
    let job = s.profile_job().unwrap();
    s.complete_profile_save(&job.request_id, &job.binding, 2, Ok(record(&job)), |_| {
        Ok(())
    })
    .unwrap();
    let mut refused = false;
    for n in 0..1024 {
        let get = command(
            &s,
            &format!("get-{n}"),
            Command::ProfilesGet {
                id: ProfileId::new("desk").unwrap(),
            },
        );
        let result = s.submit(get);
        assert!(s.profile_storage_bytes() <= MAX_PROFILE_STORAGE_BYTES);
        if result.status == ReceiptStatus::Rejected {
            assert_eq!(
                result.error.unwrap().code,
                ErrorCode::ResourceBudgetExceeded
            );
            refused = true;
            break;
        }
    }
    assert!(
        refused,
        "retained full-graph query receipts escaped their budget"
    );
    assert_eq!(s.checkpoint().profile_catalog.len(), 1);
}

#[test]
fn schema_one_checkpoint_without_catalog_remains_recoverable() {
    let s = session();
    let mut value = serde_json::to_value(s.checkpoint()).unwrap();
    value.as_object_mut().unwrap().remove("profile_catalog");
    let checkpoint: SessionCheckpoint = serde_json::from_value(value).unwrap();
    let restored = Session::restore_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("fresh").unwrap(),
        ControlLimits::default(),
        checkpoint,
    )
    .unwrap();
    assert!(restored.checkpoint().profile_catalog.is_empty());
    assert!(restored.snapshot().muted);
}

#[test]
fn unused_profiles_add_no_allowance_to_existing_graph_admission() {
    let mut s = session();
    s.submit(command_for_query(&s, "state"));
    assert_eq!(s.profile_storage_bytes(), 0);
    assert_eq!(s.snapshot().profile_storage_bytes, 0);
}

#[test]
fn profile_saves_share_admission_with_graph_residency_and_pending_work() {
    use earph_core::graph_control::GraphServiceObservation;
    for (resident, preparing, expected) in [
        (256 * 1024 * 1024, false, ErrorCode::ResourceBudgetExceeded),
        (0, true, ErrorCode::SourceBusy),
    ] {
        let mut s = session();
        s.observe_graph_resources(
            GraphServiceObservation {
                resident_bytes: resident,
                pending_retirements: 0,
                preparing_or_candidate: preparing,
            },
            1,
        )
        .unwrap();
        let create = create(&s, "save", false);
        let result = s.submit_graph(create, 2);
        assert_eq!(result.error.unwrap().code, expected);
        assert!(s.profile_job().is_none());
        assert!(s.checkpoint().profile_catalog.is_empty());
    }
}

#[test]
fn cancelled_worker_keeps_its_allocation_slot_until_actual_retirement() {
    let mut s = session();
    let save = create(&s, "save", false);
    s.submit_graph(save, 1);
    let job = s.profile_job().unwrap();
    let mute = command(&s, "mute", Command::Mute { muted: true });
    assert_eq!(s.submit(mute).status, ReceiptStatus::Applied);
    assert!(s.profile_job().is_none());
    let next = create(&s, "next", false);
    assert_eq!(s.submit(next).error.unwrap().code, ErrorCode::SourceBusy);
    let query = command(
        &s,
        "inspect-retiring",
        Command::ProfilesList {
            offset: 0,
            limit: 16,
        },
    );
    let result = s.submit(query);
    assert!(matches!(result.result, Some(QueryResult::Profiles(page))
            if page.operation.stage == Some(ProfileSaveStage::Retiring)
                && page.operation.request_id.as_ref() == Some(&job.request_id)
                && page.operation.deadline_monotonic_ns.is_none()));
    assert!(
        s.complete_profile_save(
            &job.request_id,
            &job.binding,
            3,
            Ok(record(&job)),
            |_| panic!("cancelled save published")
        )
        .is_err()
    );
    assert!(!s.profile_worker_busy());
    let next = create(&s, "after-retirement", false);
    assert_eq!(s.submit(next).status, ReceiptStatus::Accepted);
}
