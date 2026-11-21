// ABOUTME: Exercises fixed profile preparation through the existing Session transaction.
// ABOUTME: Checks fresh authorization, muted acknowledgment, safe restore, and refused escalation.
fn fixed_profile_fixture() -> Session {
    let mut s = profile_fixture();
    s.state.output.as_mut().unwrap().volume_mode = VolumeMode::Fixed;
    s.state.software_gain_db = 0.0;
    s.state.gain_ramp_frames = 0;
    let mut records = (*s.profile_records().unwrap()).clone();
    let profile = Arc::make_mut(&mut Arc::make_mut(&mut records[0]).profile);
    profile.volume.mode = VolumeMode::Fixed;
    profile.volume.software_cap_db = 0.0;
    s.restore_profile_records(records).unwrap();
    s.state.volume_policy.arm_fixed(OutputContext {
        device_id: DeviceId::new("dac").unwrap(),
        profile_id: ProfileId::new("profile").unwrap(),
    });
    s
}
#[test]
fn fixed_profile_prepares_without_target_arming_and_publishes_only_after_persisted_ack() {
    let mut s = fixed_profile_fixture();
    let p = profile_ready(&mut s);
    assert_eq!(
        p.preview
            .profile_application
            .as_ref()
            .unwrap()
            .software_gain_db,
        0.0
    );
    assert!(
        p.preview
            .profile_application
            .as_ref()
            .unwrap()
            .effective_policy
            .validate_unmute(
                &OutputContext {
                    device_id: DeviceId::new("dac").unwrap(),
                    profile_id: ProfileId::new("headphones").unwrap()
                },
                VolumeMode::Fixed,
                SampleDomain::Pcm,
                FidelityPolicy::CompatibleConversion,
                0.0,
                None
            )
            .is_err()
    );
    let c = profile_apply(&s, "apply-fixed", &p);
    assert_eq!(
        s.submit_graph(c.clone(), 21).status,
        ReceiptStatus::Accepted
    );
    assert!(s.state.muted && !s.state.output_consented);
    assert!(s.state.configured_profile.is_none());
    let ack = profile_ack(&s, &c, &p);
    s.complete_graph_activation(ack, 22, |staged| {
        assert!(staged.snapshot().muted && !staged.snapshot().output_consented);
        assert_eq!(
            staged
                .snapshot()
                .configured_profile
                .as_ref()
                .unwrap()
                .id
                .as_str(),
            "headphones"
        );
        Ok(())
    })
    .unwrap();
    let unmute = command(&s, "unmute-without-consent", Command::Mute { muted: false });
    assert_eq!(
        s.submit_graph(unmute, 23).error.unwrap().code,
        ErrorCode::SafetyPolicyBlocked
    );
    let consent = command(
        &s,
        "target-consent",
        Command::OutputConsent {
            device_id: DeviceId::new("dac").unwrap(),
            profile_id: ProfileId::new("headphones").unwrap(),
        },
    );
    assert_eq!(s.submit_graph(consent, 24).status, ReceiptStatus::Applied);
    let unmute = command(&s, "unmute-without-arming", Command::Mute { muted: false });
    assert_eq!(
        s.submit_graph(unmute, 25).error.unwrap().code,
        ErrorCode::SafetyPolicyBlocked
    );
    let arm = command(
        &s,
        "target-arm",
        Command::FixedArm {
            device_id: DeviceId::new("dac").unwrap(),
            profile_id: ProfileId::new("headphones").unwrap(),
        },
    );
    assert_eq!(s.submit_graph(arm, 26).status, ReceiptStatus::Applied);
    let unmute = command(&s, "authorized-unmute", Command::Mute { muted: false });
    assert_eq!(s.submit_graph(unmute, 27).status, ReceiptStatus::Applied);
}
#[test]
fn fixed_profile_preparation_refuses_missing_authority_and_never_raises_gain_or_caps() {
    for condition in [
        "consent",
        "arming",
        "wrong-profile",
        "gain",
        "ramp",
        "local-cap",
        "hardware-cap",
        "mode",
        "fidelity",
    ] {
        let mut s = fixed_profile_fixture();
        match condition {
            "consent" => s.state.output_consented = false,
            "arming" => s.state.volume_policy.disarm(),
            "wrong-profile" => s.state.volume_policy.arm_fixed(OutputContext {
                device_id: DeviceId::new("dac").unwrap(),
                profile_id: ProfileId::new("another").unwrap(),
            }),
            "gain" => s.state.software_gain_db = -40.0,
            "ramp" => s.state.gain_ramp_frames = 64,
            "local-cap" => s.state.local_volume_policy = VolumePolicy::new(-20.0, None).unwrap(),
            "hardware-cap" => {
                s.state.local_volume_policy = VolumePolicy::new(0.0, Some(-20.0)).unwrap()
            }
            "mode" => s.state.output.as_mut().unwrap().volume_mode = VolumeMode::Software,
            "fidelity" => {
                s.state.output.as_mut().unwrap().fidelity = FidelityPolicy::StrictPreservation
            }
            _ => unreachable!(),
        }
        let gain = s.state.software_gain_db;
        let caps = s.state.local_volume_policy.clone();
        let c = profile_prepare(&s, condition);
        let receipt = s.submit_graph(c, 10);
        assert_eq!(
            receipt.status,
            ReceiptStatus::Rejected,
            "{condition}: {receipt:?}"
        );
        assert!(s.graph_job().is_none());
        assert_eq!(s.state.software_gain_db, gain);
        assert_eq!(s.state.local_volume_policy, caps);
    }
}
#[test]
fn fixed_profile_checkpoint_restores_intent_at_lower_gain_without_runtime_authority() {
    let mut s = fixed_profile_fixture();
    let p = profile_ready(&mut s);
    let c = profile_apply(&s, "apply-before-fixed-restart", &p);
    assert_eq!(
        s.submit_graph(c.clone(), 21).status,
        ReceiptStatus::Accepted
    );
    let ack = profile_ack(&s, &c, &p);
    s.complete_graph_activation(ack, 22, |_| Ok(())).unwrap();
    let checkpoint = serde_json::from_slice(&serde_json::to_vec(&s.checkpoint()).unwrap()).unwrap();
    let mut restored = Session::restore_owner(
        s.state.zone_id.clone(),
        OwnerId::new("restored-fixed").unwrap(),
        ControlLimits::default(),
        checkpoint,
    )
    .unwrap();
    restored
        .restore_profile_records((*s.profile_records().unwrap()).clone())
        .unwrap();
    assert_eq!(restored.snapshot().software_gain_db, -40.0);
    assert!(restored.snapshot().muted && !restored.snapshot().output_consented);
    assert!(restored.graph_inspection().prepared.is_none());
    assert!(restored.snapshot().observation.pcm().cloned().is_none());
    assert!(
        restored
            .snapshot()
            .volume_policy
            .validate_unmute(
                &OutputContext {
                    device_id: DeviceId::new("dac").unwrap(),
                    profile_id: ProfileId::new("headphones").unwrap()
                },
                VolumeMode::Fixed,
                SampleDomain::Pcm,
                FidelityPolicy::CompatibleConversion,
                0.0,
                None
            )
            .is_err()
    );
}
#[test]
fn fixed_profile_failure_and_stop_keep_the_target_unpublished_and_unarmed() {
    for condition in ["storage", "stop"] {
        let mut s = fixed_profile_fixture();
        let p = profile_ready(&mut s);
        let c = profile_apply(&s, "apply-failing-fixed", &p);
        assert_eq!(
            s.submit_graph(c.clone(), 21).status,
            ReceiptStatus::Accepted
        );
        let ack = profile_ack(&s, &c, &p);
        if condition == "stop" {
            let stop = command(&s, "stop-fixed-activation", Command::Stop);
            assert_eq!(s.submit_graph(stop, 22).status, ReceiptStatus::Accepted);
        }
        let error = s
            .complete_graph_activation(ack, 23, |_| {
                assert_eq!(
                    condition, "storage",
                    "stopped activation reached persistence"
                );
                Err(EngineError::new(
                    ErrorCode::InternalFault,
                    "controlled fixed checkpoint failure",
                ))
            })
            .unwrap_err();
        assert_eq!(
            error.code,
            if condition == "storage" {
                ErrorCode::InternalFault
            } else {
                ErrorCode::StaleSession
            }
        );
        assert!(s.state.configured_profile.is_none());
        assert!(s.state.muted && !s.state.output_consented);
    }
}
#[test]
fn fixed_profile_apply_rechecks_current_consent_and_arming_before_consuming_the_token() {
    for condition in ["consent", "arming"] {
        let mut s = fixed_profile_fixture();
        let p = profile_ready(&mut s);
        if condition == "consent" {
            s.state.output_consented = false;
        } else {
            s.state.volume_policy.disarm();
        }
        let c = profile_apply(&s, "apply-after-authorization-change", &p);
        let receipt = s.submit_graph(c, 21);
        assert_eq!(receipt.status, ReceiptStatus::Rejected);
        assert_eq!(receipt.error.unwrap().code, ErrorCode::SafetyPolicyBlocked);
        assert!(s.graph_activation().is_none());
        assert!(s.graph_inspection().prepared.is_some());
        assert!(s.state.configured_profile.is_none());
    }
}
