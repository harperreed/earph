// ABOUTME: Tests local output consent, volume policy, and transport admission.
// ABOUTME: Keeps output safety and live queue edits inside the shared reducer.
use earph_core::{ErrorCode, control::*, ids::*, safety::*, session::*};

fn session() -> Session {
    let mut s = Session::new_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("owner").unwrap(),
        ControlLimits::default(),
    );
    s.enable_local_playback();
    s
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
fn output() -> OutputConfiguration {
    OutputConfiguration {
        backend: OutputBackend::Alsa,
        route: OutputRoute::System,
        endpoint: "capture".into(),
        device_id: DeviceId::new("dac").unwrap(),
        profile_id: ProfileId::new("profile").unwrap(),
        period_frames: 64,
        buffer_frames: 256,
        volume_mode: VolumeMode::Software,
        fidelity: FidelityPolicy::CompatibleConversion,
    }
}
fn configure(s: &mut Session) {
    let c = command(s, "output", Command::OutputSelect { output: output() });
    s.submit(c);
    let c = command(
        s,
        "consent",
        Command::OutputConsent {
            device_id: DeviceId::new("dac").unwrap(),
            profile_id: ProfileId::new("profile").unwrap(),
        },
    );
    s.submit(c);
    let c = command(
        s,
        "append",
        Command::QueueAppend {
            path: "first.wav".into(),
        },
    );
    s.submit(c);
    let id = s.queue_snapshot().entries[0].id.clone();
    let c = command(s, "select", Command::QueueSelect { id: Some(id) });
    s.submit(c);
}

#[test]
fn software_silence_is_independent_of_mute_and_finite_gain() {
    let mut s = session();
    configure(&mut s);
    assert_eq!(
        s.submit(command(
            &s,
            "unmute-before-zero",
            Command::Mute { muted: false }
        ))
        .status,
        ReceiptStatus::Applied
    );
    let before = s.snapshot();
    let payload = serde_json::from_value(serde_json::json!({"method":"volume.silence"})).unwrap();
    let receipt = s.submit(command(&s, "silence", payload));
    assert_eq!(receipt.status, ReceiptStatus::Applied, "{receipt:?}");
    let after = serde_json::to_value(s.snapshot()).unwrap();
    assert_eq!(after["software_gain_zero"], true);
    assert_eq!(s.snapshot().software_gain_db, before.software_gain_db);
    assert_eq!(s.snapshot().muted, before.muted);
    assert_eq!(s.snapshot().epochs, before.epochs);
    assert_eq!(s.snapshot().transport, before.transport);
    let receipt = s.submit(command(
        &s,
        "finite",
        Command::VolumeSet {
            millidecibels: -40000,
            ramp_frames: 23,
        },
    ));
    assert_eq!(receipt.status, ReceiptStatus::Applied);
    let after = serde_json::to_value(s.snapshot()).unwrap();
    assert_eq!(after["software_gain_zero"], false);
    assert!(!s.snapshot().muted);
    s.submit(command(
        &s,
        "mute-after-zero",
        Command::Mute { muted: true },
    ));
    let payload = serde_json::from_value(serde_json::json!({"method":"volume.silence"})).unwrap();
    assert_eq!(
        s.submit(command(&s, "muted-zero", payload)).status,
        ReceiptStatus::Applied
    );
    assert_eq!(
        s.submit(command(
            &s,
            "muted-finite",
            Command::VolumeSet {
                millidecibels: -40000,
                ramp_frames: 0
            }
        ))
        .status,
        ReceiptStatus::Applied
    );
    assert!(s.snapshot().muted);
}

#[test]
fn silence_rejects_unsupported_routes_without_leaving_latent_intent() {
    for output in [
        None,
        Some((VolumeMode::Fixed, FidelityPolicy::CompatibleConversion)),
        Some((VolumeMode::Hardware, FidelityPolicy::CompatibleConversion)),
        Some((VolumeMode::Software, FidelityPolicy::StrictPreservation)),
    ] {
        let mut s = session();
        if let Some((mode, fidelity)) = output {
            let mut selected = self::output();
            selected.volume_mode = mode;
            selected.fidelity = fidelity;
            s.submit(command(
                &s,
                "select",
                Command::OutputSelect { output: selected },
            ));
        }
        let before = s.snapshot();
        let payload =
            serde_json::from_value(serde_json::json!({"method":"volume.silence"})).unwrap();
        assert_eq!(
            s.submit(command(&s, "silence", payload)).status,
            ReceiptStatus::Rejected
        );
        assert_eq!(s.snapshot(), before);
    }
}

#[test]
fn factory_defaults_mutes_and_applies_the_current_cap_atomically() {
    let mut s = session();
    configure(&mut s);
    s.submit(command(
        &s,
        "caps",
        Command::VolumeCaps {
            software_millidecibels: -60000,
            hardware_millidecibels: None,
        },
    ));
    s.submit(command(
        &s,
        "gain",
        Command::VolumeSet {
            millidecibels: -80000,
            ramp_frames: 100,
        },
    ));
    let before = s.snapshot();
    let payload =
        serde_json::from_value(serde_json::json!({"method":"volume.factory_defaults"})).unwrap();
    assert_eq!(
        s.submit(command(&s, "preset", payload)).status,
        ReceiptStatus::Applied
    );
    let after = s.snapshot();
    assert_eq!(after.revision, before.revision + 1);
    assert!(after.muted);
    assert_eq!(after.software_gain_db, -60.0);
    assert_eq!(after.gain_ramp_frames, 0);
    assert_eq!(after.epochs, before.epochs);
    assert_eq!(after.output, before.output);
    assert_eq!(after.output_consented, before.output_consented);
    assert_eq!(after.volume_policy, before.volume_policy);
    assert_eq!(s.renderer_presets(), ["FactoryDefaults"]);
}

#[test]
fn factory_defaults_disarms_fixed_unity_and_rejects_incomplete_presets() {
    let mut s = session();
    assert!(s.renderer_presets().is_empty());
    let mut fixed = output();
    fixed.volume_mode = VolumeMode::Fixed;
    fixed.fidelity = FidelityPolicy::StrictPreservation;
    s.submit(command(
        &s,
        "fixed-preset-output",
        Command::OutputSelect { output: fixed },
    ));
    assert!(s.renderer_presets().is_empty());
    let before = s.snapshot();
    assert_eq!(
        s.submit(command(
            &s,
            "incomplete-fixed",
            Command::VolumeFactoryDefaults
        ))
        .status,
        ReceiptStatus::Rejected
    );
    assert_eq!(s.snapshot(), before);
    s.submit(command(
        &s,
        "fixed-preset-unity",
        Command::VolumeSet {
            millidecibels: 0,
            ramp_frames: 0,
        },
    ));
    s.submit(command(
        &s,
        "fixed-preset-consent",
        Command::OutputConsent {
            device_id: output().device_id,
            profile_id: output().profile_id,
        },
    ));
    s.submit(command(
        &s,
        "fixed-preset-arm",
        Command::FixedArm {
            device_id: output().device_id,
            profile_id: output().profile_id,
        },
    ));
    assert_eq!(s.renderer_presets(), ["FactoryDefaults"]);
    assert_eq!(
        s.submit(command(&s, "fixed-preset", Command::VolumeFactoryDefaults))
            .status,
        ReceiptStatus::Applied
    );
    assert_eq!(s.snapshot().software_gain_db, 0.0);
    assert!(s.snapshot().muted);
    assert!(!s.snapshot().software_gain_zero);
    assert_eq!(
        s.submit(command(
            &s,
            "fixed-preset-unmute",
            Command::Mute { muted: false }
        ))
        .error
        .unwrap()
        .code,
        ErrorCode::SafetyPolicyBlocked
    );
}

#[test]
fn failed_zero_delivery_does_not_publish_a_setting_or_successful_execution() {
    let mut s = session();
    configure(&mut s);
    let before = s.snapshot();
    let receipt = s.submit_with_queue_expansion(
        command(&s, "failed-zero", Command::VolumeSilence),
        |_| unreachable!(),
        |_| {
            Err(earph_core::EngineError::new(
                ErrorCode::OutputFault,
                "playback worker is unavailable",
            ))
        },
    );
    assert_eq!(receipt.status, ReceiptStatus::Failed);
    assert_eq!(s.snapshot(), before);
}

#[test]
fn selecting_an_incompatible_output_cannot_authorize_a_saved_zero_setting() {
    let mut s = session();
    configure(&mut s);
    let payload = serde_json::from_value(serde_json::json!({"method":"volume.silence"})).unwrap();
    assert_eq!(
        s.submit(command(&s, "zero", payload)).status,
        ReceiptStatus::Applied
    );
    let before = s.snapshot();
    let mut fixed = output();
    fixed.volume_mode = VolumeMode::Fixed;
    assert_eq!(
        s.submit(command(
            &s,
            "fixed",
            Command::OutputSelect { output: fixed }
        ))
        .status,
        ReceiptStatus::Rejected
    );
    assert_eq!(s.snapshot(), before);
    let checkpoint = serde_json::from_slice(&serde_json::to_vec(&s.checkpoint()).unwrap()).unwrap();
    let restored = Session::restore_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("restored").unwrap(),
        ControlLimits::default(),
        checkpoint,
    )
    .unwrap();
    assert_eq!(
        serde_json::to_value(restored.snapshot()).unwrap()["software_gain_zero"],
        false
    );
}
fn start(s: &Session) -> CommandEnvelope {
    let mut c = command(
        s,
        "start",
        Command::Start {
            source: SourceKind::LocalFile,
            device_id: DeviceId::new("dac").unwrap(),
            takeover: false,
            seekable: true,
        },
    );
    c.session_id = Some(SessionId::new("local-session").unwrap());
    c
}
#[test]
fn selecting_output_cannot_grant_signal_permission_or_unmute() {
    let mut s = session();
    let c = command(&s, "output", Command::OutputSelect { output: output() });
    assert_eq!(s.submit(c).status, ReceiptStatus::Applied);
    assert!(s.snapshot().muted);
    assert_eq!(s.snapshot().software_gain_db, -40.0);
    assert!(!s.snapshot().output_consented);
    let c = start(&s);
    assert_eq!(
        s.submit(c).error.unwrap().code,
        ErrorCode::SafetyPolicyBlocked
    );
    let c = command(
        &s,
        "wrong-consent",
        Command::OutputConsent {
            device_id: DeviceId::new("wrong").unwrap(),
            profile_id: ProfileId::new("profile").unwrap(),
        },
    );
    assert_eq!(s.submit(c).error.unwrap().code, ErrorCode::StaleSession);
}
#[test]
fn output_change_loses_consent_and_persisted_restore_loses_arming() {
    let mut s = session();
    configure(&mut s);
    let c = command(
        &s,
        "caps",
        Command::VolumeCaps {
            software_millidecibels: -50000,
            hardware_millidecibels: None,
        },
    );
    assert_eq!(s.submit(c).status, ReceiptStatus::Applied);
    assert!(s.snapshot().software_gain_db <= -50.0);
    let checkpoint = serde_json::from_slice(&serde_json::to_vec(&s.checkpoint()).unwrap()).unwrap();
    let restored = Session::restore_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("restored").unwrap(),
        ControlLimits::default(),
        checkpoint,
    )
    .unwrap();
    assert_eq!(restored.snapshot().volume_policy.software_cap_db(), -50.0);
    assert!(!restored.snapshot().output_consented);
    assert!(restored.snapshot().muted);
    let c = command(&s, "reselect", Command::OutputSelect { output: output() });
    s.submit(c);
    assert!(!s.snapshot().output_consented);
}
#[test]
fn start_has_playing_intent_but_requires_explicit_unmute() {
    let mut s = session();
    configure(&mut s);
    let c = start(&s);
    assert_eq!(s.submit(c).status, ReceiptStatus::Accepted);
    assert!(s.wants_playback());
    assert!(s.snapshot().muted);
    let c = command(&s, "pause-during-prepare", Command::Pause);
    assert_ne!(s.submit(c).status, ReceiptStatus::Rejected);
    assert!(!s.wants_playback());
}

#[test]
fn local_unmute_waits_for_source_domain_admission() {
    let mut s = session();
    configure(&mut s);
    s.submit(start(&s));
    let before = s.snapshot();
    assert_eq!(
        s.submit(command(
            &s,
            "unknown-domain",
            Command::Mute { muted: false }
        ))
        .status,
        ReceiptStatus::Rejected
    );
    assert_eq!(s.snapshot(), before);
    s.set_queue_preparation(
        before.epochs,
        &before.active_entry_id.unwrap(),
        earph_core::queue::PreparationStatus::Ready {
            format: earph_core::audio::PcmFormat::new(
                48000,
                earph_core::audio::ChannelLayout::Stereo,
                earph_core::audio::PcmRepresentation::Signed24,
            )
            .unwrap(),
            total_frames: 64,
        },
    )
    .unwrap();
    assert_eq!(
        s.submit(command(&s, "admitted-pcm", Command::Mute { muted: false }))
            .status,
        ReceiptStatus::Applied
    );
}

#[test]
fn previous_pcm_readiness_cannot_unmute_an_unknown_replacement_session() {
    let mut s = session();
    configure(&mut s);
    s.submit(start(&s));
    let first = s.snapshot();
    let id = first.active_entry_id.unwrap();
    let status = earph_core::queue::PreparationStatus::Ready {
        format: earph_core::audio::PcmFormat::new(
            48000,
            earph_core::audio::ChannelLayout::Stereo,
            earph_core::audio::PcmRepresentation::Signed24,
        )
        .unwrap(),
        total_frames: 64,
    };
    s.set_queue_preparation(first.epochs, &id, status).unwrap();
    let mut replacement = start(&s);
    replacement.request_id = RequestId::new("replacement-start").unwrap();
    replacement.session_id = Some(SessionId::new("replacement-session").unwrap());
    replacement.payload = Command::Start {
        source: SourceKind::LocalFile,
        device_id: DeviceId::new("dac").unwrap(),
        takeover: true,
        seekable: true,
    };
    assert_eq!(s.submit(replacement).status, ReceiptStatus::Accepted);
    let before = s.snapshot();
    assert_eq!(s.queue_snapshot().entries[0].status, status);
    assert_eq!(
        s.submit(command(
            &s,
            "unmute-old-admission",
            Command::Mute { muted: false }
        ))
        .status,
        ReceiptStatus::Rejected
    );
    assert_eq!(s.snapshot(), before);
    assert_eq!(
        s.submit(command(
            &s,
            "pause-replacement-before-admission",
            Command::Pause
        ))
        .status,
        ReceiptStatus::Applied
    );
    let paused = s.snapshot();
    assert_eq!(
        s.submit(command(
            &s,
            "seek-old-admission",
            Command::Seek { position_frames: 0 }
        ))
        .status,
        ReceiptStatus::Rejected
    );
    assert_eq!(s.snapshot(), paused);
    assert!(s.set_queue_preparation(first.epochs, &id, status).is_err());
    s.set_queue_preparation(before.epochs, &id, status).unwrap();
    assert_eq!(
        s.submit(command(
            &s,
            "unmute-current-admission",
            Command::Mute { muted: false }
        ))
        .status,
        ReceiptStatus::Applied
    );
}
#[test]
fn strict_unmute_requires_unity_without_a_ramp_and_fixed_requires_arming() {
    let mut s = session();
    let mut fixed = output();
    fixed.volume_mode = VolumeMode::Fixed;
    fixed.fidelity = FidelityPolicy::StrictPreservation;
    let c = command(&s, "fixed", Command::OutputSelect { output: fixed });
    s.submit(c);
    let c = command(
        &s,
        "consent",
        Command::OutputConsent {
            device_id: DeviceId::new("dac").unwrap(),
            profile_id: ProfileId::new("profile").unwrap(),
        },
    );
    s.submit(c);
    let c = command(
        &s,
        "unity",
        Command::VolumeSet {
            millidecibels: 0,
            ramp_frames: 0,
        },
    );
    assert_eq!(s.submit(c).status, ReceiptStatus::Applied);
    let c = command(&s, "unmute-blocked", Command::Mute { muted: false });
    assert_eq!(
        s.submit(c).error.unwrap().code,
        ErrorCode::SafetyPolicyBlocked
    );
    let c = command(
        &s,
        "arm",
        Command::FixedArm {
            device_id: DeviceId::new("dac").unwrap(),
            profile_id: ProfileId::new("profile").unwrap(),
        },
    );
    assert_eq!(s.submit(c).status, ReceiptStatus::Applied);
    let c = command(&s, "unmute", Command::Mute { muted: false });
    assert_eq!(s.submit(c).status, ReceiptStatus::Applied);
}

#[test]
fn prepared_local_seek_rejects_out_of_range_without_invalidating_the_stream() {
    let mut s = session();
    configure(&mut s);
    let c = start(&s);
    s.submit(c);
    let c = command(&s, "pause", Command::Pause);
    s.submit(c);
    let state = s.snapshot();
    let id = state.active_entry_id.clone().unwrap();
    let format = earph_core::audio::PcmFormat::new(
        48000,
        earph_core::audio::ChannelLayout::Stereo,
        earph_core::audio::PcmRepresentation::Signed24,
    )
    .unwrap();
    s.set_queue_preparation(
        state.epochs,
        &id,
        earph_core::queue::PreparationStatus::Ready {
            format,
            total_frames: 10,
        },
    )
    .unwrap();
    let before = s.snapshot();
    let c = command(
        &s,
        "too-far",
        Command::Seek {
            position_frames: 11,
        },
    );
    assert_eq!(s.submit(c).error.unwrap().code, ErrorCode::AssetInvalid);
    assert_eq!(s.snapshot(), before);
}

#[test]
fn active_and_reserved_entries_are_protected_while_farther_entries_remain_editable() {
    let mut s = session();
    configure(&mut s);
    for (request, path) in [("next", "next.wav"), ("farther", "farther.wav")] {
        let c = command(&s, request, Command::QueueAppend { path: path.into() });
        s.submit(c);
    }
    let c = start(&s);
    s.submit(c);
    let queue = s.queue_snapshot();
    assert_eq!(
        s.snapshot().reserved_next_entry_id,
        Some(queue.entries[1].id.clone())
    );
    for (index, entry) in queue.entries[..2].iter().enumerate() {
        let c = command(
            &s,
            &format!("remove-protected-{index}"),
            Command::QueueRemove {
                id: entry.id.clone(),
            },
        );
        assert_eq!(s.submit(c).error.unwrap().code, ErrorCode::SourceBusy);
    }
    let c = command(
        &s,
        "remove-farther",
        Command::QueueRemove {
            id: queue.entries[2].id.clone(),
        },
    );
    assert_eq!(s.submit(c).status, ReceiptStatus::Applied);
}

#[test]
fn a_local_stream_can_finish_without_claiming_a_sample_activation() {
    let mut s = session();
    configure(&mut s);
    let start = start(&s);
    s.submit(start.clone());
    let state = s.snapshot();
    let observation = PlaybackObservation {
        output_activity: None,
        processing: None,
        format: earph_core::audio::PcmFormat::new(
            48000,
            earph_core::audio::ChannelLayout::Stereo,
            earph_core::audio::PcmRepresentation::Signed24,
        )
        .unwrap(),
        submitted_frames: 0,
        consumed_frames: Some(0),
        backend_delay_frames: Some(0),
        consumption_basis: "alsa_client_estimate",
        source_entry_id: state.active_entry_id.clone().unwrap(),
        source_position_frames: 0,
        audible_entry_id: None,
        audible_source_position_frames: None,
        input_peak: None,
        output_peak: None,
        modified: false,
        backend: None,
    };
    s.advance(EngineEvent {
        epochs: state.epochs,
        monotonic_ns: 1,
        kind: EngineEventKind::LocalPrepared {
            observation: observation.into(),
        },
    })
    .unwrap();
    s.advance(EngineEvent {
        epochs: state.epochs,
        monotonic_ns: 2,
        kind: EngineEventKind::EndOfInput,
    })
    .unwrap();
    s.advance(EngineEvent {
        epochs: state.epochs,
        monotonic_ns: 3,
        kind: EngineEventKind::Drained,
    })
    .unwrap();
    assert_eq!(s.snapshot().transport, TransportState::Idle);
    let receipt = s.submit(start);
    assert_eq!(receipt.status, ReceiptStatus::Applied);
    assert_eq!(receipt.activation_frame, None);
}

#[test]
fn unknown_audible_timing_is_not_replaced_with_a_software_restart_cursor() {
    let mut s = session();
    configure(&mut s);
    let request = start(&s);
    s.submit(request);
    let state = s.snapshot();
    let observation = PlaybackObservation {
        output_activity: None,
        processing: None,
        format: earph_core::audio::PcmFormat::new(
            48000,
            earph_core::audio::ChannelLayout::Stereo,
            earph_core::audio::PcmRepresentation::Signed24,
        )
        .unwrap(),
        submitted_frames: 128,
        consumed_frames: None,
        backend_delay_frames: None,
        consumption_basis: "callback_delivery_not_dac",
        source_entry_id: state.active_entry_id.clone().unwrap(),
        source_position_frames: 256,
        audible_entry_id: None,
        audible_source_position_frames: None,
        input_peak: None,
        output_peak: None,
        modified: false,
        backend: None,
    };
    s.advance(EngineEvent {
        epochs: state.epochs,
        monotonic_ns: 1,
        kind: EngineEventKind::LocalPrepared {
            observation: (observation.clone()).into(),
        },
    })
    .unwrap();
    assert!(serde_json::to_value(s.snapshot()).unwrap()["audible_position_frames"].is_null());
    s.advance(EngineEvent {
        epochs: state.epochs,
        monotonic_ns: 2,
        kind: EngineEventKind::LocalPaused {
            observation: observation.into(),
        },
    })
    .unwrap();
    assert!(serde_json::to_value(s.snapshot()).unwrap()["audible_position_frames"].is_null());
    assert_eq!(s.snapshot().decoded_position_frames, 256);
}

#[test]
fn local_host_capabilities_report_the_available_native_adapter_without_implying_consent() {
    let mut s = session();
    let query = command(&s, "local-capabilities", Command::Capabilities);
    let result = s.submit(query);
    assert!(
        matches!(result.result, Some(QueryResult::Capabilities(caps)) if caps.sources == vec![SourceKind::LocalFile] && caps.physical_output)
    );
    assert!(!s.snapshot().output_consented);
}

#[test]
fn failed_safety_admission_invalidates_playing_without_claiming_native_closure() {
    let mut s = session();
    configure(&mut s);
    let request = start(&s);
    s.submit(request);
    let state = s.snapshot();
    s.set_queue_preparation(
        state.epochs,
        &state.active_entry_id.unwrap(),
        earph_core::queue::PreparationStatus::Ready {
            format: earph_core::audio::PcmFormat::new(
                48000,
                earph_core::audio::ChannelLayout::Stereo,
                earph_core::audio::PcmRepresentation::Signed24,
            )
            .unwrap(),
            total_frames: 64,
        },
    )
    .unwrap();
    for (monotonic_ns, kind) in [
        (1, EngineEventKind::Prepared),
        (
            2,
            EngineEventKind::Started {
                activation_frame: 0,
            },
        ),
    ] {
        s.advance(EngineEvent {
            epochs: s.snapshot().epochs,
            monotonic_ns,
            kind,
        })
        .unwrap();
    }
    let unmute = command(&s, "unmute", Command::Mute { muted: false });
    assert_eq!(s.submit(unmute).status, ReceiptStatus::Applied);
    assert_eq!(s.snapshot().transport, TransportState::Playing);
    assert!(!s.snapshot().muted);
    let before = s.snapshot();
    let stop = command(&s, "saturated-stop", Command::Stop);
    let receipt = s.submit_with_queue_expansion(
        stop.clone(),
        |_| unreachable!(),
        |_| {
            Err(earph_core::EngineError::new(
                ErrorCode::SourceBusy,
                "playback control queue is full",
            ))
        },
    );
    assert_eq!(receipt.status, ReceiptStatus::Failed);
    assert_eq!(s.snapshot(), before);
    s.advance(EngineEvent {
        epochs: before.epochs,
        monotonic_ns: 3,
        kind: EngineEventKind::LocalFailed {
            code: ErrorCode::SourceBusy,
        },
    })
    .unwrap();
    let failed = s.snapshot();
    assert_eq!(failed.transport, TransportState::Failed);
    assert!(failed.muted);
    assert!(!failed.output_consented);
    assert_ne!(failed.epochs, before.epochs);
    assert_eq!(failed.audible_position_frames, None);
    assert_eq!(s.submit(stop).status, ReceiptStatus::Failed);
    assert_eq!(failed.source, Some(SourceKind::LocalFile));
}
