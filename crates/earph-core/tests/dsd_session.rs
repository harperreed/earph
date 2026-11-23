// ABOUTME: Tests typed DSD admission and exclusive source observations through Session.
// ABOUTME: Keeps source bits, carrier counts, PCM frames, and endpoint authority separate.
use earph_core::{
    audio::ChannelLayout,
    control::*,
    dop::*,
    ids::*,
    queue::PreparationStatus,
    safety::{FidelityPolicy, VolumeMode},
    session::*,
};
use serde_json::json;

fn source() -> DsdSourceFormat {
    DsdSourceFormat::new(2_822_400, ChannelLayout::Stereo).unwrap()
}
fn observed(id: QueueEntryId, epochs: Epochs) -> DsdPlaybackObservation {
    DsdPlaybackObservation {
        source_format: source(),
        total_source_bits: 64,
        source_entry_id: id,
        source_position_bits: 16,
        decoded_position_bits: 48,
        source_epochs: epochs,
        carrier_format: DopCarrierFormat::from_source(source()),
        output: None,
        last_boundary: None,
        qualification: DsdQualification::Unverified,
        completion_basis: None,
    }
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
fn prepared() -> Session {
    let mut s = Session::new_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("owner").unwrap(),
        ControlLimits::default(),
    );
    s.enable_local_playback();
    for (id, payload) in [
        (
            "output",
            Command::OutputSelect {
                output: OutputConfiguration {
                    backend: OutputBackend::Alsa,
                    route: OutputRoute::System,
                    endpoint: "unqualified".into(),
                    device_id: DeviceId::new("dac").unwrap(),
                    profile_id: ProfileId::new("profile").unwrap(),
                    period_frames: 64,
                    buffer_frames: 256,
                    volume_mode: VolumeMode::Fixed,
                    fidelity: FidelityPolicy::StrictPreservation,
                },
            },
        ),
        (
            "unity",
            Command::VolumeSet {
                millidecibels: 0,
                ramp_frames: 0,
            },
        ),
        (
            "consent",
            Command::OutputConsent {
                device_id: DeviceId::new("dac").unwrap(),
                profile_id: ProfileId::new("profile").unwrap(),
            },
        ),
        (
            "arm",
            Command::FixedArm {
                device_id: DeviceId::new("dac").unwrap(),
                profile_id: ProfileId::new("profile").unwrap(),
            },
        ),
        (
            "append",
            Command::QueueAppend {
                path: "source.dsf".into(),
            },
        ),
    ] {
        assert_eq!(
            s.submit(command(&s, id, payload)).status,
            ReceiptStatus::Applied
        );
    }
    let mut start = command(
        &s,
        "start",
        Command::Start {
            source: SourceKind::LocalFile,
            device_id: DeviceId::new("dac").unwrap(),
            takeover: false,
            seekable: true,
        },
    );
    start.session_id = Some(SessionId::new("dsd").unwrap());
    assert_eq!(s.submit(start).status, ReceiptStatus::Accepted);
    let state = s.snapshot();
    let id = state.active_entry_id.unwrap();
    s.set_queue_preparation(
        state.epochs,
        &id,
        PreparationStatus::DsdReady {
            format: source(),
            total_source_bits: 64,
        },
    )
    .unwrap();
    s.advance(EngineEvent {
        epochs: state.epochs,
        monotonic_ns: 1,
        kind: EngineEventKind::LocalPrepared {
            observation: LocalPlaybackObservation::from(observed(id, state.epochs)),
        },
    })
    .unwrap();
    s
}
#[test]
fn source_and_carrier_formats_validate_their_own_domains() {
    for rate in [2_822_400, 5_644_800] {
        let format = DsdSourceFormat::new(rate, ChannelLayout::Stereo).unwrap();
        assert_eq!(
            DopCarrierFormat::from_source(format).carrier_rate_hz(),
            rate / 16
        );
        assert_eq!(
            serde_json::from_value::<DsdSourceFormat>(serde_json::to_value(format).unwrap())
                .unwrap(),
            format
        );
    }
    for document in [
        json!({"bit_rate_hz":48_000,"channels":"stereo"}),
        json!({"bit_rate_hz":2_822_400,"channels":"mono"}),
    ] {
        assert!(serde_json::from_value::<DsdSourceFormat>(document).is_err());
    }
    for document in [
        json!({"carrier_rate_hz":48_000,"channels":"stereo","transport":"packed24_le"}),
        json!({"carrier_rate_hz":176_400,"channels":"mono","transport":"packed24_le"}),
        json!({"carrier_rate_hz":176_400,"channels":"stereo","transport":"float64"}),
    ] {
        assert!(serde_json::from_value::<DopCarrierFormat>(document).is_err());
    }
}
#[test]
fn dsd_admission_publishes_only_typed_bits_and_no_qualification() {
    let s = prepared();
    let value = serde_json::to_value(s.snapshot()).unwrap();
    assert!(value["playback"].is_null());
    assert!(value.get("network_playback").is_none());
    assert_eq!(value["decoded_position_frames"], 0);
    assert!(value["audible_position_frames"].is_null());
    assert_eq!(value["dsd_playback"]["source_position_bits"], 16);
    assert_eq!(value["dsd_playback"]["decoded_position_bits"], 48);
    assert_eq!(value["dsd_playback"]["qualification"], "unverified");
    assert!(value["dsd_playback"].get("format").is_none());
}
#[test]
fn unqualified_dsd_refuses_unmute_and_pcm_processing_without_state_mutation() {
    let mut s = prepared();
    for (id, payload) in [
        ("unmute", Command::Mute { muted: false }),
        (
            "gain",
            Command::VolumeSet {
                millidecibels: -1000,
                ramp_frames: 0,
            },
        ),
        (
            "ramp",
            Command::VolumeSet {
                millidecibels: 0,
                ramp_frames: 16,
            },
        ),
        ("zero", Command::VolumeSilence),
        ("pcm-seek", Command::Seek { position_frames: 0 }),
    ] {
        let before = s.snapshot();
        assert_eq!(
            s.submit(command(&s, id, payload)).status,
            ReceiptStatus::Rejected,
            "{id}"
        );
        assert_eq!(s.snapshot(), before, "{id}");
    }
}
#[test]
fn bit_seek_checks_alignment_bounds_and_domain_before_advancing_epochs() {
    let mut s = prepared();
    for (id, bits) in [("unaligned", 1), ("past-end", 80)] {
        let before = s.snapshot();
        assert_eq!(
            s.submit(command(
                &s,
                id,
                Command::SeekDsd {
                    position_bits: bits
                }
            ))
            .status,
            ReceiptStatus::Rejected
        );
        assert_eq!(s.snapshot(), before);
    }
    let before = s.snapshot();
    assert_eq!(
        s.submit(command(&s, "end", Command::SeekDsd { position_bits: 64 }))
            .status,
        ReceiptStatus::Accepted
    );
    assert_eq!(s.snapshot().epochs.stream, before.epochs.stream + 1);
    assert_eq!(s.snapshot().decoded_position_frames, 0);
    let mut plain = Session::new(ZoneId::new("main").unwrap(), ControlLimits::default());
    assert_eq!(
        plain
            .submit(command(
                &plain,
                "wrong-domain",
                Command::SeekDsd { position_bits: 0 }
            ))
            .status,
        ReceiptStatus::Rejected
    );
}
#[test]
fn dsd_readiness_is_reset_on_restore_and_stale_updates_cannot_change_it() {
    let mut s = prepared();
    let state = s.snapshot();
    let id = state.active_entry_id.unwrap();
    assert!(
        s.set_queue_preparation(
            state.epochs.next_stream().unwrap(),
            &id,
            PreparationStatus::DsdReady {
                format: source(),
                total_source_bits: 64
            }
        )
        .is_err()
    );
    let checkpoint = serde_json::from_value(serde_json::to_value(s.checkpoint()).unwrap()).unwrap();
    let restored = Session::restore_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("replacement").unwrap(),
        ControlLimits::default(),
        checkpoint,
    )
    .unwrap();
    assert_eq!(
        restored.queue_snapshot().entries[0].status,
        PreparationStatus::Unprepared
    );
    assert!(
        serde_json::to_value(restored.snapshot())
            .unwrap()
            .get("dsd_playback")
            .is_none()
    );
}

#[test]
fn typed_progress_rejects_another_source_boundary_inside_a_current_event_envelope() {
    let mut s = prepared();
    let before = s.snapshot();
    let mut observation = before.observation.dsd().unwrap().clone();
    observation.source_epochs.session += 1;
    assert!(
        s.advance(EngineEvent {
            epochs: before.epochs,
            monotonic_ns: 2,
            kind: EngineEventKind::LocalProgress {
                observation: LocalPlaybackObservation::from(observation)
            }
        })
        .is_err()
    );
    assert_eq!(s.snapshot(), before);
}

#[test]
fn dsd_shutdown_waits_for_the_matching_source_close_observation() {
    let mut s = prepared();
    let before = s.snapshot();
    let shutdown = command(&s, "shutdown-dsd", Command::OwnerShutdown);
    assert_eq!(s.submit(shutdown.clone()).status, ReceiptStatus::Accepted);
    assert_eq!(s.pending_local_stop_request(), Some(&shutdown.request_id));
    let closing = s.snapshot();
    assert!(closing.shutdown_requested);
    assert!(closing.source.is_some());
    let observation = LocalPlaybackObservation::from(before.observation.dsd().unwrap().clone());
    s.advance(EngineEvent {
        epochs: closing.epochs,
        monotonic_ns: 2,
        kind: EngineEventKind::LocalStopped {
            observation: Some(observation),
        },
    })
    .unwrap();
    assert!(s.pending_local_stop_request().is_none());
    assert!(s.snapshot().source.is_none());
    assert_eq!(s.submit(shutdown).status, ReceiptStatus::Applied);
}
