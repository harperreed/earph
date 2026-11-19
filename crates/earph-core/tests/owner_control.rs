// ABOUTME: Tests owner-scoped queue commands and atomic persistence contracts.
// ABOUTME: Proves retries, failed commits, and recovery retain one authoritative state.
use earph_core::{
    EngineError, ErrorCode,
    control::*,
    ids::*,
    session::{Session, TransportState},
};
use std::cell::Cell;

fn owner(id: &str) -> Session {
    Session::new_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new(id).unwrap(),
        ControlLimits::default(),
    )
}

#[test]
fn ipc_rejects_invalid_paths_before_retaining_command_history() {
    let session = owner("owner-a");
    for path in [String::new(), "x".repeat(1025), "bad\0path".to_owned()] {
        for payload in [
            Command::QueueAppend { path: path.clone() },
            Command::QueueAppendDirectory { path: path.clone() },
        ] {
            let bytes = serde_json::to_vec(&command(&session, "invalid-path", payload)).unwrap();
            assert!(decode_command(&bytes, &ControlLimits::default()).is_err());
        }
    }
}
fn command(session: &Session, request: &str, payload: Command) -> CommandEnvelope {
    let state = session.snapshot();
    CommandEnvelope {
        schema_version: 1,
        request_id: RequestId::new(request).unwrap(),
        zone_id: state.zone_id,
        owner_id: state.owner_id,
        session_id: state.session_id,
        expected_revision: Some(state.revision),
        payload,
    }
}

#[test]
fn output_activity_capabilities_distinguish_support_from_observation() {
    let mut session = owner("activity-owner");
    let request = command(&session, "activity-capabilities", Command::Capabilities);
    let receipt = serde_json::to_value(session.submit(request)).unwrap();
    assert_eq!(
        receipt["result"]["value"].get("output_activity"),
        Some(&serde_json::json!({
            "backends": [],
            "scope": "output_instance_across_native_reopens",
            "measurement_boundary": "core_audio_sdk_entry_calls"
        }))
    );
    assert!(session.snapshot().playback.is_none());
}

#[test]
fn scoped_owner_requires_current_identity_only_for_mutations() {
    let mut session = owner("owner-a");
    let mut append = command(
        &session,
        "append",
        Command::QueueAppend {
            path: "雪.flac".into(),
        },
    );
    append.owner_id = None;
    assert_eq!(
        session.submit(append).error.unwrap().code,
        ErrorCode::StaleSession
    );
    let mut query = command(&session, "discover", Command::State);
    query.owner_id = None;
    let receipt = session.submit(query);
    assert!(
        matches!(receipt.result, Some(QueryResult::State(state)) if state.owner_id == Some(OwnerId::new("owner-a").unwrap()))
    );
    let mut stale = command(&session, "stale", Command::OwnerShutdown);
    stale.owner_id = Some(OwnerId::new("previous-owner").unwrap());
    assert_eq!(
        session.submit(stale).error.unwrap().code,
        ErrorCode::StaleSession
    );
    assert!(!session.shutdown_requested());
}

#[test]
fn queue_commands_share_revisions_and_receipt_deduplication() {
    let mut session = owner("owner-a");
    let append = command(
        &session,
        "append",
        Command::QueueAppend {
            path: "first.wav".into(),
        },
    );
    let first = session.submit(append.clone());
    assert_eq!(first.status, ReceiptStatus::Applied);
    assert_eq!(first.revision, 1);
    assert_eq!(session.submit(append), first);
    assert_eq!(session.queue_snapshot().entries.len(), 1);
    let id = session.queue_snapshot().entries[0].id.clone();
    let select = command(
        &session,
        "select",
        Command::QueueSelect {
            id: Some(id.clone()),
        },
    );
    assert_eq!(session.submit(select).revision, 2);
    let query = command(
        &session,
        "queue",
        Command::QueueGet {
            offset: 0,
            limit: 64,
        },
    );
    assert!(
        matches!(session.submit(query).result, Some(QueryResult::Queue(queue)) if queue.selected == Some(id.clone()))
    );
    let remove = command(&session, "remove", Command::QueueRemove { id });
    assert_eq!(session.submit(remove).revision, 3);
    assert!(session.queue_snapshot().entries.is_empty());
    assert_eq!(session.queue_snapshot().selected, None);
}

#[test]
fn persistence_failure_never_publishes_state_or_claims_application() {
    let mut session = owner("owner-a");
    let before = session.snapshot();
    let append = command(
        &session,
        "append",
        Command::QueueAppend {
            path: "first.wav".into(),
        },
    );
    let calls = Cell::new(0);
    let receipt = session.submit_with_commit(append.clone(), |staged| {
        calls.set(calls.get() + 1);
        assert_eq!(staged.queue_snapshot().entries.len(), 1);
        Err(EngineError::new(
            ErrorCode::InternalFault,
            "persistent store failed",
        ))
    });
    assert_eq!(receipt.status, ReceiptStatus::Failed);
    assert_eq!(receipt.revision, 0);
    assert_eq!(session.snapshot(), before);
    assert!(session.queue_snapshot().entries.is_empty());
    assert_eq!(
        session.submit_with_commit(append, |_| {
            calls.set(calls.get() + 1);
            Ok(())
        }),
        receipt
    );
    assert_eq!(calls.get(), 1);
}

#[test]
fn restored_queue_is_paused_muted_and_rejects_old_owner_commands() {
    let mut session = owner("owner-a");
    let append = command(
        &session,
        "append",
        Command::QueueAppend {
            path: "first.wav".into(),
        },
    );
    session.submit(append.clone());
    let id = session.queue_snapshot().entries[0].id.clone();
    let select = command(
        &session,
        "select",
        Command::QueueSelect {
            id: Some(id.clone()),
        },
    );
    session.submit(select);
    let bytes = serde_json::to_vec(&session.checkpoint()).unwrap();
    let checkpoint = serde_json::from_slice(&bytes).unwrap();
    let mut restored = Session::restore_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("owner-b").unwrap(),
        ControlLimits::default(),
        checkpoint,
    )
    .unwrap();
    let state = restored.snapshot();
    assert_eq!(state.transport, TransportState::Paused);
    assert!(state.muted);
    assert_eq!(state.source, None);
    assert_eq!(state.output_id, None);
    assert_eq!(state.operation, None);
    assert_eq!(state.revision, 2);
    assert_eq!(restored.queue_snapshot().selected, Some(id));
    assert_eq!(
        restored.submit(append).error.unwrap().code,
        ErrorCode::StaleSession
    );
    let append = command(
        &restored,
        "append-after-restart",
        Command::QueueAppend {
            path: "second.wav".into(),
        },
    );
    assert_eq!(restored.submit(append).status, ReceiptStatus::Applied);
    assert_ne!(
        restored.queue_snapshot().entries[0].id,
        restored.queue_snapshot().entries[1].id
    );
}

#[test]
fn control_only_owner_advertises_no_playable_sources() {
    let mut session = owner("owner-a");
    let query = command(&session, "caps", Command::Capabilities);
    assert!(
        matches!(session.submit(query).result, Some(QueryResult::Capabilities(caps)) if caps.sources.is_empty() && !caps.physical_output)
    );
    let mut start = command(
        &session,
        "start",
        Command::Start {
            source: SourceKind::Virtual,
            device_id: DeviceId::new("virtual").unwrap(),
            takeover: false,
            seekable: true,
        },
    );
    start.session_id = Some(SessionId::new("playback").unwrap());
    assert_eq!(
        session.submit(start).error.unwrap().code,
        ErrorCode::CapabilityUnsupported
    );
}

#[test]
fn malformed_recovery_documents_cannot_construct_an_owner() {
    let mut session = owner("owner-a");
    let append = command(
        &session,
        "append",
        Command::QueueAppend {
            path: "first.wav".into(),
        },
    );
    session.submit(append);
    let original = serde_json::to_value(session.checkpoint()).unwrap();
    let mut cases = Vec::new();
    for (pointer, value) in [
        ("/schema_version", serde_json::json!(2)),
        ("/queue/entries/0/path", serde_json::json!("")),
        ("/queue/entries/0/path", serde_json::json!("x".repeat(1025))),
        ("/queue/counter", serde_json::json!(0)),
        ("/queue/entries/0/id", serde_json::json!("foreign-id")),
        ("/queue/selected", serde_json::json!("missing")),
    ] {
        let mut doc = original.clone();
        *doc.pointer_mut(pointer).unwrap() = value;
        cases.push(doc);
    }
    let mut duplicates = original.clone();
    let entry = duplicates["queue"]["entries"][0].clone();
    duplicates["queue"]["entries"]
        .as_array_mut()
        .unwrap()
        .push(entry);
    cases.push(duplicates);
    let mut overflow = original.clone();
    overflow["queue"]["entries"] =
        serde_json::json!(vec![original["queue"]["entries"][0].clone(); 257]);
    cases.push(overflow);
    for doc in cases {
        let checkpoint = serde_json::from_value(doc).unwrap();
        assert!(
            Session::restore_owner(
                ZoneId::new("main").unwrap(),
                OwnerId::new("owner-b").unwrap(),
                ControlLimits::default(),
                checkpoint
            )
            .is_err()
        );
    }
}

#[test]
fn queue_query_bounds_worst_case_escaped_paths() {
    let mut queue = earph_core::queue::LocalQueue::default();
    for _ in 0..256 {
        queue.append("\u{1}".repeat(1024)).unwrap();
    }
    let checkpoint = earph_core::session::SessionCheckpoint {
        local_volume_policy: None,
        configured_profile: None,
        profile_catalog: vec![],
        profile_associations: vec![],
        graph_intent: None,
        volume_policy: Default::default(),
        output: None,
        schema_version: 1,
        revision: 0,
        queue: queue.snapshot(),
    };
    let mut session = Session::restore_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("owner-a").unwrap(),
        ControlLimits::default(),
        checkpoint,
    )
    .unwrap();
    let query = command(
        &session,
        "queue",
        Command::QueueGet {
            offset: 0,
            limit: 64,
        },
    );
    let receipt = session.submit(query);
    assert!(serde_json::to_vec(&receipt).unwrap().len() <= 1_048_576);
}

#[test]
fn directory_expansion_commits_once_and_rejects_partial_overflow() {
    let mut session = owner("owner-a");
    let append = command(
        &session,
        "folder",
        Command::QueueAppendDirectory {
            path: "/music".into(),
        },
    );
    let calls = Cell::new(0);
    let first = session.submit_with_queue_expansion(
        append.clone(),
        |_| {
            calls.set(calls.get() + 1);
            Ok(vec!["a.wav".into(), "b.wav".into()])
        },
        |_| Ok(()),
    );
    assert_eq!(first.status, ReceiptStatus::Applied);
    assert_eq!(first.revision, 1);
    assert_eq!(
        session.submit_with_queue_expansion(
            append,
            |_| {
                calls.set(calls.get() + 1);
                Ok(vec![])
            },
            |_| Ok(())
        ),
        first
    );
    assert_eq!(calls.get(), 1);
    let before = session.queue_snapshot();
    let overflow = command(
        &session,
        "overflow",
        Command::QueueAppendDirectory {
            path: "/too-many".into(),
        },
    );
    let receipt = session.submit_with_queue_expansion(
        overflow,
        |_| Ok(vec!["extra.wav".into(); 255]),
        |_| panic!("invalid expansion must not commit"),
    );
    assert_eq!(
        receipt.error.unwrap().code,
        ErrorCode::ResourceBudgetExceeded
    );
    assert_eq!(session.queue_snapshot(), before);
    assert_eq!(session.snapshot().revision, 1);
}

#[test]
fn queue_pages_reject_invalid_ranges_and_retain_total_and_selection() {
    let mut session = owner("owner-a");
    let append = command(
        &session,
        "append",
        Command::QueueAppend {
            path: "first.wav".into(),
        },
    );
    session.submit(append);
    let selected = session.queue_snapshot().entries[0].id.clone();
    let select = command(
        &session,
        "select",
        Command::QueueSelect {
            id: Some(selected.clone()),
        },
    );
    session.submit(select);
    for (index, (offset, limit)) in [(0, 0), (0, 65), (2, 1), (usize::MAX, 1)]
        .into_iter()
        .enumerate()
    {
        let query = command(
            &session,
            &format!("invalid-{index}"),
            Command::QueueGet { offset, limit },
        );
        assert_eq!(
            session.submit(query).error.unwrap().code,
            ErrorCode::AssetInvalid
        );
    }
    let query = command(
        &session,
        "empty-last-page",
        Command::QueueGet {
            offset: 1,
            limit: 64,
        },
    );
    let receipt = session.submit(query);
    assert_eq!(receipt.revision, 2);
    assert!(
        matches!(receipt.result, Some(QueryResult::Queue(page)) if page.entries.is_empty() && page.total == 1 && page.offset == 1 && page.selected == Some(selected))
    );
}
