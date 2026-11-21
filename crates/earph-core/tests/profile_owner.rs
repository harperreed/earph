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
        corrections: Arc::new(Vec::new()),
        correction_preview: None,
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
    let first = command(
        &s,
        "get-initial",
        Command::ProfilesGet {
            id: ProfileId::new("desk").unwrap(),
        },
    );
    let initial = s.submit(first);
    assert_eq!(initial.status, ReceiptStatus::Applied);
    let Some(QueryResult::Profile(saved)) = initial.result else {
        panic!("missing saved profile")
    };
    assert_eq!(saved.profile.id.as_str(), "desk");
    assert_eq!(saved.profile.revision, 1);
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
fn checkpoint_effective_caps_cannot_weaken_independent_local_caps() {
    let mut checkpoint = session().checkpoint();
    checkpoint.local_volume_policy =
        Some(earph_core::safety::VolumePolicy::new(-30.0, None).unwrap());
    checkpoint.volume_policy = earph_core::safety::VolumePolicy::new(0.0, None).unwrap();
    let error = Session::restore_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("fresh").unwrap(),
        ControlLimits::default(),
        checkpoint,
    )
    .err()
    .expect("restoration cannot weaken a local cap");
    assert_eq!(error.code, ErrorCode::AssetInvalid);
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

#[test]
fn empty_profile_history_cannot_enter_an_unreserved_graph_construction() {
    use earph_core::graph_control::GraphServiceObservation;
    let mut s = session();
    s.observe_graph_resources(
        GraphServiceObservation {
            resident_bytes: 0,
            pending_retirements: 0,
            preparing_or_candidate: true,
        },
        1,
    )
    .unwrap();
    let query = command(
        &s,
        "profiles-during-graph",
        Command::ProfilesList {
            offset: 0,
            limit: 16,
        },
    );
    let result = s.submit(query);
    assert_eq!(
        result.error.unwrap().code,
        ErrorCode::ResourceBudgetExceeded
    );
    assert_eq!(s.profile_storage_bytes(), 0);
    s.observe_graph_resources(
        GraphServiceObservation {
            resident_bytes: 0,
            pending_retirements: 0,
            preparing_or_candidate: false,
        },
        2,
    )
    .unwrap();
    let query = command(
        &s,
        "profiles-after-graph",
        Command::ProfilesList {
            offset: 0,
            limit: 16,
        },
    );
    assert_eq!(s.submit(query).status, ReceiptStatus::Applied);
}

#[test]
fn unloaded_catalog_blocks_profile_mutation_but_preserves_other_queries() {
    let mut checkpoint = session().checkpoint();
    checkpoint.profile_catalog.push(ProfileCatalogReference {
        id: ProfileId::new("saved").unwrap(),
        revision: 1,
        profile_sha256: ProfileObjectHash::try_from("0".repeat(64)).unwrap(),
    });
    let mut s = Session::restore_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("restored").unwrap(),
        ControlLimits::default(),
        checkpoint,
    )
    .unwrap();
    for (index, payload) in [
        Command::State,
        Command::Capabilities,
        Command::GraphInspect {},
        Command::QueueGet {
            offset: 0,
            limit: 16,
        },
    ]
    .into_iter()
    .enumerate()
    {
        let query = command(&s, &format!("query-{index}"), payload);
        assert_eq!(s.submit(query).status, ReceiptStatus::Applied);
    }
    let list = command(
        &s,
        "profiles",
        Command::ProfilesList {
            offset: 0,
            limit: 16,
        },
    );
    assert_eq!(s.submit(list).error.unwrap().code, ErrorCode::AssetInvalid);
    let save = create(&s, "create-unloaded", false);
    assert_eq!(s.submit(save).status, ReceiptStatus::Rejected);
    assert_eq!(s.checkpoint().profile_catalog[0].id.as_str(), "saved");
    assert!(s.profile_work().is_none());
}

#[test]
fn local_start_preparation_blocks_profile_work_until_actual_resources_arrive() {
    use earph_core::{
        audio::{ChannelLayout, PcmFormat, PcmRepresentation},
        session::{EngineEvent, EngineEventKind, PlaybackObservation},
    };
    let mut s = session();
    s.enable_local_playback();
    let output = serde_json::from_value(json!({"backend":"alsa","route":"system","endpoint":"null","device_id":"owned","profile_id":"legacy",
        "period_frames":64,"buffer_frames":256,"volume_mode":"software","fidelity":"compatible_conversion"})).unwrap();
    for (id, payload) in [
        ("output", Command::OutputSelect { output }),
        (
            "consent",
            Command::OutputConsent {
                device_id: DeviceId::new("owned").unwrap(),
                profile_id: ProfileId::new("legacy").unwrap(),
            },
        ),
        (
            "queue",
            Command::QueueAppend {
                path: "owned.wav".into(),
            },
        ),
    ] {
        let request = command(&s, id, payload);
        assert_eq!(s.submit(request).status, ReceiptStatus::Applied);
    }
    let mut start = command(
        &s,
        "start",
        Command::Start {
            source: SourceKind::LocalFile,
            device_id: DeviceId::new("owned").unwrap(),
            takeover: false,
            seekable: true,
        },
    );
    start.session_id = Some(SessionId::new("local").unwrap());
    assert_eq!(s.submit(start).status, ReceiptStatus::Accepted);
    let save = create(&s, "during-start", false);
    assert_eq!(s.submit(save).error.unwrap().code, ErrorCode::SourceBusy);
    let state = s.snapshot();
    s.advance(EngineEvent {
        epochs: state.epochs,
        monotonic_ns: 1,
        kind: EngineEventKind::LocalPrepared {
            observation: (PlaybackObservation {
                output_activity: None,
                processing: None,
                format: PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Signed16)
                    .unwrap(),
                submitted_frames: 0,
                consumed_frames: None,
                backend_delay_frames: None,
                consumption_basis: "prepared fixture",
                source_entry_id: state.active_entry_id.unwrap(),
                source_position_frames: 0,
                audible_entry_id: None,
                audible_source_position_frames: None,
                input_peak: None,
                output_peak: None,
                modified: false,
                backend: None,
            })
            .into(),
        },
    })
    .unwrap();
    let save = create(&s, "after-prepared", false);
    assert_eq!(s.submit_graph(save, 2).status, ReceiptStatus::Accepted);
}

#[test]
fn correction_requests_keep_strings_in_history_and_cancel_through_the_shared_worker_slot() {
    let mut s = session();
    let mut report_json = String::with_capacity(200_000);
    report_json.push_str("{}");
    let source_text = "Preamp: -6 dB\n".repeat(1000);
    let request_capacity = (report_json.capacity() + source_text.capacity()) as u64;
    let import = command(
        &s,
        "import",
        Command::ProfilesImport {
            id: ProfileId::new("imported").unwrap(),
            expected_profile_revision: None,
            draft: draft(),
            input: Arc::new(ProfileCorrectionInput::TextReport {
                report_json,
                source_text,
            }),
        },
    );
    assert_eq!(
        s.submit_graph(import.clone(), 1).status,
        ReceiptStatus::Accepted
    );
    let job = s.profile_work().unwrap();
    let stop = command(&s, "stop-import", Command::Stop);
    assert_eq!(s.submit(stop).status, ReceiptStatus::Applied);
    assert!(s.profile_worker_busy());
    assert!(s.profile_work().is_none());
    assert!(
        s.profile_storage_bytes() >= request_capacity,
        "retained report/source request strings disappeared after cancellation"
    );
    let query = command(
        &s,
        "retiring-import",
        Command::ProfilesList {
            offset: 0,
            limit: 16,
        },
    );
    let Some(QueryResult::Profiles(page)) = s.submit(query).result else {
        panic!("missing worker observation")
    };
    assert_eq!(page.operation.stage, Some(ProfileSaveStage::Retiring));
    assert_eq!(page.operation.request_id.as_ref(), Some(&job.request_id));
    assert_eq!(page.operation.deadline_monotonic_ns, None);
    let another = create(&s, "overlapping-save", false);
    assert_eq!(s.submit(another).error.unwrap().code, ErrorCode::SourceBusy);
    assert!(
        s.complete_profile_work(
            &job.request_id,
            &job.binding,
            2,
            Err(EngineError::new(
                ErrorCode::OperationCancelled,
                "actual cancelled work"
            )),
            |_| panic!("cancelled import reached publication")
        )
        .is_err()
    );
    assert!(!s.profile_worker_busy());
    assert_eq!(s.submit(import).status, ReceiptStatus::Cancelled);
    assert!(s.profile_storage_bytes() >= request_capacity);
    assert!(s.profile_references().is_empty());
}

#[test]
fn correction_request_capacity_refuses_small_content_with_oversized_retained_buffers() {
    for (report_capacity, source_capacity) in [
        (MAX_PROFILE_CORRECTION_BYTES + 1, 1),
        (1, MAX_PROFILE_TEXT_BYTES + 1),
    ] {
        let mut s = session();
        let mut report_json = String::with_capacity(report_capacity);
        report_json.push('x');
        let mut source_text = String::with_capacity(source_capacity);
        source_text.push('x');
        let import = command(
            &s,
            "oversized-import",
            Command::ProfilesImport {
                id: ProfileId::new("imported").unwrap(),
                expected_profile_revision: None,
                draft: draft(),
                input: Arc::new(ProfileCorrectionInput::TextReport {
                    report_json,
                    source_text,
                }),
            },
        );
        assert_eq!(
            s.submit(import).error.unwrap().code,
            ErrorCode::ResourceBudgetExceeded
        );
        assert!(s.profile_work().is_none());
        assert_eq!(s.profile_storage_bytes(), 0);
    }
}

#[test]
fn profile_fir_import_preserves_the_direct_paths_validation_error() {
    use earph_core::{audio::ChannelLayout, dsp_nodes::FirPurpose, graph::FiniteGraphScalar};
    for path in ["", "invalid\0name"] {
        let mut s = session();
        let direct = command(
            &s,
            "direct",
            Command::FirImport {
                path: path.into(),
                purpose: FirPurpose::Correction,
            },
        );
        let expected = s.submit(direct).error.unwrap();
        assert_eq!(expected.code, ErrorCode::AssetInvalid);
        let imported = command(
            &s,
            "profile",
            Command::ProfilesImport {
                id: ProfileId::new("invalid-path").unwrap(),
                expected_profile_revision: None,
                draft: draft(),
                input: Arc::new(ProfileCorrectionInput::Fir {
                    path: path.into(),
                    purpose: FirPurpose::Correction,
                    context: ProfileCorrectionContext {
                        sample_rate_hz: 48000,
                        channels: ChannelLayout::Stereo,
                        quantum_frames: 64,
                    },
                    metadata_json: "{}".into(),
                    preamp_db: FiniteGraphScalar::new(0.0).unwrap(),
                }),
            },
        );
        assert_eq!(s.submit(imported).error.unwrap(), expected);
    }
}

#[test]
fn correction_preview_cannot_understate_peak_headroom() {
    use earph_core::{audio::ChannelLayout, graph::FiniteGraphScalar};
    let scalar = |value| FiniteGraphScalar::new(value).unwrap();
    let mut preview = ProfileCorrectionPreview {
        context: ProfileCorrectionContext {
            sample_rate_hz: 48000,
            channels: ChannelLayout::Stereo,
            quantum_frames: 64,
        },
        preamp_db: scalar(0.0),
        channel_peak_bounds: vec![scalar(2.0), scalar(1.0)],
        required_headroom_db: scalar(0.0),
        normalization: CorrectionNormalization::Preserved,
    };
    for understated in [0.0, 6.0] {
        preview.required_headroom_db = scalar(understated);
        assert_eq!(
            preview.validate().unwrap_err().code,
            ErrorCode::AssetInvalid
        );
    }
    preview.required_headroom_db = scalar(6.1);
    preview.validate().unwrap();
    preview.channel_peak_bounds = vec![scalar(1.0), scalar(0.5)];
    preview.required_headroom_db = scalar(0.0);
    preview.validate().unwrap();
}

#[test]
fn package_import_preserves_positive_revision_and_waits_for_checkpoint() {
    let mut s = session();
    let save = create(&s, "seed", false);
    s.submit_graph(save, 1);
    let mut imported = (*record(&s.profile_job().unwrap())).clone();
    Arc::make_mut(&mut imported.profile).revision = 7;
    let reference = imported.reference();
    let mut s = session();
    let request = command(
        &s,
        "package",
        Command::ProfilesImportPackage {
            directory: "/owned/package".into(),
            expected_manifest_sha256: reference.profile_sha256,
            profile: reference.clone(),
        },
    );
    assert_eq!(
        s.submit_graph(request.clone(), 1).status,
        ReceiptStatus::Accepted
    );
    let work = s.profile_work().unwrap();
    let failure = EngineError::new(ErrorCode::InternalFault, "controlled checkpoint failure");
    assert_eq!(
        s.complete_profile_work(
            &work.request_id,
            &work.binding,
            2,
            Ok(ProfileWorkOutput::Profile(Arc::new(imported.clone()))),
            |staged| {
                assert_eq!(
                    staged.profile_references(),
                    std::slice::from_ref(&reference)
                );
                Err(failure)
            }
        ),
        Err(failure)
    );
    assert!(s.profile_references().is_empty());
    let mut s = session();
    let request = command(
        &s,
        "package",
        Command::ProfilesImportPackage {
            directory: "/owned/package".into(),
            expected_manifest_sha256: reference.profile_sha256,
            profile: reference.clone(),
        },
    );
    assert_eq!(
        s.submit_graph(request.clone(), 1).status,
        ReceiptStatus::Accepted
    );
    let work = s.profile_work().unwrap();
    s.complete_profile_work(
        &work.request_id,
        &work.binding,
        2,
        Ok(ProfileWorkOutput::Profile(Arc::new(imported))),
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(s.profile_references(), std::slice::from_ref(&reference));
    let collision = command(&s, "collision", request.payload);
    assert!(matches!(
        s.submit_graph(collision.clone(), 3).result,
        Some(QueryResult::ProfilePackageIssue(ProfilePackageIssue {
            reason: ProfilePackageReason::IdentityCollision,
            filesystem_state: PackageFilesystemState::Unchanged,
            ..
        }))
    ));
    assert_eq!(
        s.submit_graph(collision, 3).error.unwrap().code,
        ErrorCode::AssetInvalid
    );
    assert!(s.snapshot().muted);
    assert!(!s.snapshot().output_consented);
}

#[test]
fn cancelled_package_keeps_the_worker_slot_until_actual_completion() {
    let mut s = session();
    let request = command(
        &s,
        "inspect",
        Command::ProfilesPackageInspect {
            directory: "/owned/package".into(),
        },
    );
    assert_eq!(
        s.submit_graph(request.clone(), 1).status,
        ReceiptStatus::Accepted
    );
    let work = s.profile_work().unwrap();
    let mute = command(&s, "mute", Command::Mute { muted: true });
    assert_eq!(s.submit(mute).status, ReceiptStatus::Applied);
    assert_eq!(s.submit(request).status, ReceiptStatus::Cancelled);
    assert!(s.profile_worker_busy());
    let replacement = command(
        &s,
        "replacement",
        Command::ProfilesPackageInspect {
            directory: "/owned/package".into(),
        },
    );
    assert_eq!(
        s.submit_graph(replacement, 3).error.unwrap().code,
        ErrorCode::SourceBusy
    );
    assert!(
        s.complete_profile_work(
            &work.request_id,
            &work.binding,
            4,
            Err(EngineError::new(ErrorCode::OperationCancelled, "retired")),
            |_| panic!("cancelled package reached checkpoint")
        )
        .is_err()
    );
    assert!(!s.profile_worker_busy());
    assert!(s.profile_references().is_empty());
    let replacement = command(
        &s,
        "replacement-after-retirement",
        Command::ProfilesPackageInspect {
            directory: "/owned/package".into(),
        },
    );
    assert_eq!(
        s.submit_graph(replacement, 5).status,
        ReceiptStatus::Accepted
    );
}

#[test]
fn cancelled_package_completion_retains_cancelled_receipt_and_diagnostic() {
    let mut s = session();
    let request = command(
        &s,
        "inspect",
        Command::ProfilesPackageInspect {
            directory: "/owned/package".into(),
        },
    );
    s.submit_graph(request.clone(), 1);
    let work = s.profile_work().unwrap();
    let error = EngineError::new(ErrorCode::OperationCancelled, "package read cancelled");
    assert_eq!(
        s.complete_profile_work(
            &work.request_id,
            &work.binding,
            2,
            Ok(ProfileWorkOutput::PackageIssue {
                error,
                issue: ProfilePackageIssue {
                    reason: ProfilePackageReason::InvalidObject,
                    object: None,
                    filesystem_state: PackageFilesystemState::Unchanged
                }
            }),
            |_| panic!("cancelled work persisted")
        ),
        Err(error)
    );
    let receipt = s.submit(request);
    assert_eq!(receipt.status, ReceiptStatus::Cancelled);
    assert!(matches!(
        receipt.result,
        Some(QueryResult::ProfilePackageIssue(_))
    ));
}

#[test]
fn failed_dispatch_never_publishes_a_profile_worker_reservation() {
    for code in [
        ErrorCode::SourceBusy,
        ErrorCode::OperationCancelled,
        ErrorCode::InternalFault,
    ] {
        let mut s = session();
        let request = create(&s, "dispatch-failure", false);
        let failed = s.submit_with_graph_effects(
            request,
            1,
            |_| unreachable!(),
            |staged| {
                assert!(staged.profile_work().is_some());
                assert!(staged.profile_worker_busy());
                Err(EngineError::new(code, "controlled dispatch failure"))
            },
        );
        assert_eq!(failed.status, ReceiptStatus::Failed);
        assert_eq!(failed.error.unwrap().code, code);
        assert!(s.profile_work().is_none());
        assert!(!s.profile_worker_busy());
        assert!(s.profile_references().is_empty());
        let next = create(&s, "next-dispatch", false);
        assert_eq!(s.submit_graph(next, 2).status, ReceiptStatus::Accepted);
    }
}
