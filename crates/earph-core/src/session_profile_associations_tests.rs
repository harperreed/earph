// ABOUTME: Exercises trusted owner observations through real association commands and checkpoints.
// ABOUTME: Treats native identities as model facts without claiming physical-device qualification.
fn association_identity(n: u64) -> crate::profiles::ProfileObjectHash {
    format!("{n:064x}").try_into().unwrap()
}
fn association_fixture() -> Session {
    let mut s = profile_fixture();
    s.state.output.as_mut().unwrap().backend = OutputBackend::CoreAudio;
    let mut records = (*s.profile_records().unwrap()).clone();
    Arc::make_mut(&mut Arc::make_mut(&mut records[0]).profile)
        .output
        .backend = OutputBackend::CoreAudio;
    s.restore_profile_records(records).unwrap();
    let backend = s.state.playback.as_mut().unwrap().backend.as_mut().unwrap();
    backend.backend = OutputBackend::CoreAudio;
    backend.device_identity_sha256 = Some(association_identity(1));
    s
}
fn association_bind(s: &Session, id: &str) -> CommandEnvelope {
    command(
        s,
        id,
        Command::ProfilesBind {
            id: ProfileId::new("headphones").unwrap(),
            expected_profile_revision: 1,
            profile_sha256: "c".repeat(64).try_into().unwrap(),
            expected_device_epoch: s.state.device_epoch,
        },
    )
}
#[test]
fn association_binding_does_not_supersede_pending_profile_graph_preparation() {
    let mut s = association_fixture();
    let preparing = profile_prepare(&s, "pending-profile-prepare");
    assert_eq!(s.submit_graph(preparing, 9).status, ReceiptStatus::Accepted);
    let binding = s.graph_job().unwrap().binding;
    let c = association_bind(&s, "bind-while-preparing");
    assert_eq!(s.submit(c).error.unwrap().code, ErrorCode::SourceBusy);
    assert_eq!(s.graph_job().unwrap().binding, binding);
    assert!(s.checkpoint().profile_associations.is_empty());
}
fn association_resolve(s: &Session, id: &str) -> CommandEnvelope {
    command(
        s,
        id,
        Command::ProfilesResolve {
            manual_profile_id: None,
            output: s.profile_records().unwrap()[0].profile.output.clone(),
        },
    )
}
fn association_save(s: &mut Session, id: &str, device: &str) {
    use crate::profile_control::{ProfileDraft, ProfileRecord, ProfileVolumeDraft};
    let base = s.profile_records().unwrap()[0].clone();
    let mut output = base.profile.output.clone();
    output.device_id = DeviceId::new(device).unwrap();
    let draft = Arc::new(ProfileDraft {
        name: id.into(),
        source_policy: base.profile.source_policy,
        fidelity: base.profile.fidelity,
        output,
        volume: ProfileVolumeDraft {
            mode: VolumeMode::Software,
            software_cap_millidecibels: -10000,
            hardware_cap_millidecibels: None,
        },
        latency: base.profile.latency.clone(),
        corrections: vec![],
    });
    let previous = s.profile_references().iter().find(|r| r.id.as_str() == id);
    let payload = if let Some(previous) = previous {
        Command::ProfilesUpdate {
            id: previous.id.clone(),
            expected_profile_revision: previous.revision,
            draft,
            graph: base.graph.clone(),
        }
    } else {
        Command::ProfilesCreate {
            id: ProfileId::new(id).unwrap(),
            draft,
            graph: base.graph.clone(),
        }
    };
    let c = command(s, &format!("save-{id}-{}", s.state.revision), payload);
    let now = s.state.monotonic_ns + 1;
    assert_eq!(s.submit_graph(c, now).status, ReceiptStatus::Accepted);
    let job = s.profile_job().unwrap();
    let hash = association_identity(100 + job.revision);
    let record = Arc::new(ProfileRecord {
        profile: Arc::new(
            job.draft
                .document(job.id.clone(), job.revision, hash)
                .unwrap(),
        ),
        profile_sha256: hash,
        graph: job.graph.clone(),
        corrections: Arc::new(vec![]),
        correction_preview: None,
    });
    s.complete_profile_save(&job.request_id, &job.binding, now + 1, Ok(record), |_| {
        Ok(())
    })
    .unwrap();
}
#[test]
fn two_bound_setups_require_manual_choice_and_selector_updates_invalidate_only_their_bindings() {
    let mut s = association_fixture();
    let c = association_bind(&s, "bind-headphones");
    assert_eq!(s.submit(c).status, ReceiptStatus::Applied);
    association_save(&mut s, "speakers", "dac");
    let reference = s
        .profile_references()
        .iter()
        .find(|r| r.id.as_str() == "speakers")
        .unwrap()
        .clone();
    let c = command(
        &s,
        "bind-speakers",
        Command::ProfilesBind {
            id: reference.id,
            expected_profile_revision: reference.revision,
            profile_sha256: reference.profile_sha256,
            expected_device_epoch: s.state.device_epoch,
        },
    );
    assert_eq!(s.submit(c).status, ReceiptStatus::Applied);
    let q = association_resolve(&s, "resolve-ambiguous");
    let r = serde_json::to_value(s.submit(q)).unwrap();
    assert_eq!(r["result"]["value"]["reason"], "ambiguous");
    assert_eq!(
        r["result"]["value"]["candidates"].as_array().unwrap().len(),
        2
    );
    let mut q = association_resolve(&s, "resolve-manual");
    if let Command::ProfilesResolve {
        manual_profile_id, ..
    } = &mut q.payload
    {
        *manual_profile_id = Some(ProfileId::new("speakers").unwrap());
    }
    let r = serde_json::to_value(s.submit(q)).unwrap();
    assert_eq!(r["result"]["value"]["reason"], "manual");
    assert_eq!(r["result"]["value"]["profile"]["id"], "speakers");
    association_save(&mut s, "speakers", "another-logical-device");
    assert_eq!(s.checkpoint().profile_associations.len(), 1);
    assert_eq!(
        s.checkpoint().profile_associations[0].profile_id.as_str(),
        "headphones"
    );
    s.advance(EngineEvent {
        epochs: s.state.epochs,
        monotonic_ns: s.state.monotonic_ns + 1,
        kind: EngineEventKind::LocalStopped { observation: None },
    })
    .unwrap();
    assert!(s.state.playback.as_ref().unwrap().backend.is_some());
    let q = association_resolve(&s, "resolve-after-real-stop");
    let r = serde_json::to_value(s.submit(q)).unwrap();
    assert_eq!(r["result"]["value"]["reason"], "no_verified_match");
}
#[test]
fn observed_association_publishes_only_after_checkpoint_and_restores_without_native_authority() {
    let mut s = association_fixture();
    let before = serde_json::to_value(s.snapshot()).unwrap();
    let bind = association_bind(&s, "bind-observed");
    let receipt = s.submit_with_commit(bind, |staged| {
        assert_eq!(staged.checkpoint().profile_associations.len(), 1);
        assert!(staged.snapshot().output_consented);
        assert!(staged.snapshot().muted);
        Ok(())
    });
    assert_eq!(receipt.status, ReceiptStatus::Applied, "{receipt:?}");
    assert!(matches!(receipt.result, Some(QueryResult::ProfileBound(_))));
    let after = serde_json::to_value(s.snapshot()).unwrap();
    for key in [
        "output",
        "output_consented",
        "volume_policy",
        "local_volume_policy",
        "muted",
        "software_gain_db",
        "configured_profile",
    ] {
        assert_eq!(after[key], before[key], "{key}");
    }
    let q = association_resolve(&s, "resolve-observed");
    let resolved = serde_json::to_value(s.submit(q)).unwrap();
    assert_eq!(resolved["result"]["value"]["reason"], "exact_device");
    let q = association_bind(&s, "bind-duplicate");
    assert_eq!(s.submit(q).status, ReceiptStatus::Applied);
    assert_eq!(s.checkpoint().profile_associations.len(), 1);
    let checkpoint = serde_json::from_slice(&serde_json::to_vec(&s.checkpoint()).unwrap()).unwrap();
    let records = (*s.profile_records().unwrap()).clone();
    let mut restored = Session::restore_owner(
        s.state.zone_id.clone(),
        OwnerId::new("restored").unwrap(),
        ControlLimits::default(),
        checkpoint,
    )
    .unwrap();
    restored.restore_profile_records(records).unwrap();
    let q = association_resolve(&restored, "resolve-without-reopen");
    let resolution = serde_json::to_value(restored.submit(q)).unwrap();
    assert_eq!(resolution["result"]["value"]["reason"], "no_verified_match");
    assert!(!restored.snapshot().output_consented);
    assert!(restored.snapshot().muted);
}
#[test]
fn observed_association_refuses_stale_or_missing_backend_and_failed_storage() {
    for condition in [
        "epoch",
        "revision",
        "session",
        "identity",
        "backend",
        "stopped",
        "transition",
    ] {
        let mut s = association_fixture();
        let mut c = association_bind(&s, condition);
        match condition {
            "epoch" => {
                if let Command::ProfilesBind {
                    expected_device_epoch,
                    ..
                } = &mut c.payload
                {
                    *expected_device_epoch += 1;
                }
            }
            "revision" => {
                if let Command::ProfilesBind {
                    expected_profile_revision,
                    ..
                } = &mut c.payload
                {
                    *expected_profile_revision += 1;
                }
            }
            "session" => c.session_id = None,
            "identity" => {
                s.state
                    .playback
                    .as_mut()
                    .unwrap()
                    .backend
                    .as_mut()
                    .unwrap()
                    .device_identity_sha256 = None
            }
            "backend" => s.state.playback.as_mut().unwrap().backend = None,
            "stopped" => s.state.source = None,
            "transition" => s.local_boundary = Some(LocalBoundary::Stop),
            _ => unreachable!(),
        }
        let r = s.submit_with_commit(c, |_| panic!("invalid binding reached storage"));
        assert_eq!(r.status, ReceiptStatus::Rejected, "{condition}: {r:?}");
        assert!(s.checkpoint().profile_associations.is_empty());
    }
    let mut s = association_fixture();
    let c = association_bind(&s, "failed-storage");
    let r = s.submit_with_commit(c, |_| {
        Err(EngineError::new(
            ErrorCode::InternalFault,
            "controlled storage failure",
        ))
    });
    assert_eq!(r.status, ReceiptStatus::Failed);
    assert!(s.checkpoint().profile_associations.is_empty());
}
#[test]
fn observed_association_bounds_restored_catalog_and_retained_capacity() {
    let mut s = association_fixture();
    for n in 1..=64 {
        s.state
            .playback
            .as_mut()
            .unwrap()
            .backend
            .as_mut()
            .unwrap()
            .device_identity_sha256 = Some(association_identity(n));
        let c = association_bind(&s, &format!("bind-{n}"));
        assert_eq!(s.submit(c).status, ReceiptStatus::Applied);
    }
    s.state
        .playback
        .as_mut()
        .unwrap()
        .backend
        .as_mut()
        .unwrap()
        .device_identity_sha256 = Some(association_identity(65));
    let c = association_bind(&s, "overflow");
    assert_eq!(
        s.submit(c).error.unwrap().code,
        ErrorCode::ResourceBudgetExceeded
    );
    assert_eq!(s.checkpoint().profile_associations.len(), 64);
    for condition in [
        "duplicate",
        "missing-profile",
        "capacity",
        "id-capacity",
        "backend",
    ] {
        let mut checkpoint = s.checkpoint();
        match condition {
            "duplicate" => {
                checkpoint.profile_associations[1] = checkpoint.profile_associations[0].clone()
            }
            "missing-profile" => {
                checkpoint.profile_associations[0].profile_id = ProfileId::new("missing").unwrap()
            }
            "capacity" => checkpoint.profile_associations.reserve(128),
            "id-capacity" => {
                let mut id = String::with_capacity(1024);
                id.push_str("headphones");
                checkpoint.profile_associations[0].profile_id = ProfileId::new(id).unwrap();
            }
            "backend" => checkpoint.profile_associations[0].backend = OutputBackend::Alsa,
            _ => unreachable!(),
        }
        assert!(
            Session::restore_owner(
                s.state.zone_id.clone(),
                OwnerId::new("restore").unwrap(),
                ControlLimits::default(),
                checkpoint
            )
            .is_err(),
            "{condition}"
        );
    }
}
#[test]
fn profile_apply_rejects_changed_or_missing_identity_after_preparation() {
    for identity in [None, Some(association_identity(2))] {
        let mut s = association_fixture();
        let p = profile_ready(&mut s);
        s.state
            .playback
            .as_mut()
            .unwrap()
            .backend
            .as_mut()
            .unwrap()
            .device_identity_sha256 = identity;
        let c = profile_apply(&s, "changed-identity", &p);
        assert_eq!(
            s.submit_graph(c, 21).error.unwrap().code,
            ErrorCode::StaleRevision
        );
        assert!(s.snapshot().configured_profile.is_none());
    }
}
#[test]
fn profile_activation_rejects_changed_native_identity_before_persisting_ack() {
    let mut s = association_fixture();
    let p = profile_ready(&mut s);
    let c = profile_apply(&s, "apply-observed", &p);
    assert_eq!(
        s.submit_graph(c.clone(), 21).status,
        ReceiptStatus::Accepted
    );
    let mut ack = profile_ack(&s, &c, &p);
    ack.observation
        .backend
        .as_mut()
        .unwrap()
        .device_identity_sha256 = Some(association_identity(2));
    assert!(
        s.complete_graph_activation(ack, 22, |_| panic!(
            "wrong native identity reached persistence"
        ))
        .is_err()
    );
    assert!(s.snapshot().configured_profile.is_none());
    assert!(s.snapshot().muted);
    assert!(!s.snapshot().output_consented);
}
#[test]
fn profile_activation_with_matching_native_identity_stays_muted_and_needs_fresh_consent() {
    let mut s = association_fixture();
    let p = profile_ready(&mut s);
    assert_eq!(
        p.preview
            .profile_application
            .as_ref()
            .unwrap()
            .device_identity_sha256,
        Some(association_identity(1))
    );
    let c = profile_apply(&s, "apply-matching-identity", &p);
    assert_eq!(
        s.submit_graph(c.clone(), 21).status,
        ReceiptStatus::Accepted
    );
    let ack = profile_ack(&s, &c, &p);
    s.complete_graph_activation(ack, 22, |staged| {
        assert!(staged.snapshot().muted);
        assert!(!staged.snapshot().output_consented);
        assert_eq!(staged.snapshot().software_gain_db, -40.0);
        Ok(())
    })
    .unwrap();
    assert_eq!(
        s.snapshot()
            .configured_profile
            .as_ref()
            .unwrap()
            .id
            .as_str(),
        "headphones"
    );
    assert!(!s.snapshot().output_consented);
}
