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
        kind: EngineEventKind::LocalPrepared { observation },
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
            observation: observation.clone(),
        },
    })
    .unwrap();
    assert!(serde_json::to_value(s.snapshot()).unwrap()["audible_position_frames"].is_null());
    s.advance(EngineEvent {
        epochs: state.epochs,
        monotonic_ns: 2,
        kind: EngineEventKind::LocalPaused { observation },
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
