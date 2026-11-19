// ABOUTME: Tests immutable desktop requests through the actual LocalClient and owner.
// ABOUTME: Catches stale-target repair, reply rewriting, and client-owned shutdown.
mod common;
use common::*;
use earph_core::{ErrorCode, control::Command, ids::RequestId};
use earph_desktop::{Lane, Reply, ReplyStatus, Request};

#[test]
fn request_builders_capture_targets_and_replay_the_identical_owner_envelope() {
    let owner = Owner::start();
    let client = owner.client();
    let state = owner.state();
    let request = Request::mutation(
        RequestId::new("append-exact").unwrap(),
        &state,
        Command::QueueAppend {
            path: "owned-雪.wav".into(),
        },
    )
    .unwrap();
    assert_eq!(request.lane(), Lane::Ordinary);
    let envelope: serde_json::Value = serde_json::from_slice(request.bytes()).unwrap();
    assert_eq!(
        envelope["owner_id"],
        state.owner_id.as_ref().unwrap().as_str()
    );
    assert_eq!(envelope["expected_revision"], 0);
    assert_eq!(envelope["session_id"], serde_json::Value::Null);
    let reply = client.request(&request).unwrap();
    assert_eq!(
        reply.bytes(),
        earph_engine::request(&owner.runtime, request.bytes()).unwrap()
    );
    assert_eq!(reply.bytes(), client.request(&request).unwrap().bytes());
    let stale = Request::mutation(
        RequestId::new("append-stale").unwrap(),
        &state,
        Command::QueueAppend {
            path: "other.wav".into(),
        },
    )
    .unwrap();
    let Reply::Receipt(receipt) = client.request(&stale).unwrap() else {
        panic!("receipt")
    };
    assert_eq!(receipt.status, ReplyStatus::Rejected);
    assert_eq!(receipt.error.unwrap().code, ErrorCode::StaleRevision);
    assert_eq!(stale.envelope().expected_revision, Some(0));
}

#[test]
fn only_shared_safety_commands_use_the_independent_lane() {
    let owner = Owner::start();
    let state = owner.state();
    for (payload, expected) in [
        (Command::Stop, Lane::Safety),
        (Command::Mute { muted: true }, Lane::Safety),
        (Command::Mute { muted: false }, Lane::Ordinary),
        (
            Command::VolumeSet {
                millidecibels: -40000,
                ramp_frames: 0,
            },
            Lane::Ordinary,
        ),
    ] {
        assert_eq!(
            Request::mutation(RequestId::new(unique()).unwrap(), &state, payload)
                .unwrap()
                .lane(),
            expected
        );
    }
    assert!(Request::mutation(RequestId::new(unique()).unwrap(), &state, Command::State).is_err());
    assert!(
        Request::query(
            RequestId::new(unique()).unwrap(),
            state.zone_id.clone(),
            Command::Stop
        )
        .is_err()
    );
}

#[test]
fn dropping_an_attached_adapter_never_changes_owner_or_safety_state() {
    let owner = Owner::start();
    let before = owner.state();
    let client = owner.client();
    client.request(&query(Command::Capabilities)).unwrap();
    drop(client);
    let after = owner.state();
    assert_eq!(after.owner_id, before.owner_id);
    assert_eq!(after.revision, before.revision);
    assert!(after.muted);
    assert!(!after.shutdown_requested);
    assert_eq!(after.output, None);
    assert!(!after.output_consented);
}

fn terminal(client: &earph_desktop::OwnerClient, request: &Request) -> earph_desktop::ReceiptView {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(8);
    loop {
        let Reply::Receipt(receipt) = client.request(request).unwrap() else {
            panic!("actual owner receipt");
        };
        if receipt.status != ReplyStatus::Accepted {
            return receipt;
        }
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[test]
fn actual_profile_queries_project_records_pages_resolution_and_graph_state() {
    use earph_desktop::{ProfileResolutionView, ProfileSelectionReasonView, ResultView};
    let owner = Owner::start();
    let client = owner.client();
    let request = Request::mutation(
        RequestId::new(unique()).unwrap(),
        &owner.state(),
        profile_payload(),
    )
    .unwrap();
    let saved = terminal(&client, &request);
    let Some(ResultView::ProfileSaved(record)) = saved.result else {
        panic!("typed saved intent");
    };
    let profile_id = record.profile.id.clone();
    let Reply::Receipt(page) = client
        .request(&query(Command::ProfilesList {
            offset: 0,
            limit: 16,
        }))
        .unwrap()
    else {
        panic!("profile page");
    };
    let Some(ResultView::Profiles(page)) = page.result else {
        panic!("typed page");
    };
    assert_eq!(page.entries[0].profile_sha256, record.profile_sha256);
    assert!(page.next_offset.is_none());
    assert!(
        page.operation.stage.is_none()
            && page.operation.request_id.is_none()
            && page.operation.deadline_monotonic_ns.is_none()
    );
    let Reply::Receipt(get) = client
        .request(&query(Command::ProfilesGet {
            id: profile_id.clone(),
        }))
        .unwrap()
    else {
        panic!("profile get");
    };
    let Some(ResultView::Profile(retrieved)) = get.result else {
        panic!("typed profile");
    };
    assert_eq!(record, retrieved);
    let output = &record.profile.output;
    let Reply::Receipt(resolution) = client
        .request(&query(Command::ProfilesResolve {
            manual_profile_id: Some(profile_id.clone()),
            output: earph_core::profiles::ProfileOutputSelector {
                backend: output.backend,
                route: output.route,
                device_id: output.device_id.clone(),
                manual_setup: output.manual_setup.clone(),
            },
        }))
        .unwrap()
    else {
        panic!("resolution");
    };
    assert!(
        matches!(resolution.result, Some(ResultView::ProfileResolution(ProfileResolutionView::Selected { profile, reason: ProfileSelectionReasonView::Manual })) if profile.id == profile_id)
    );
    let Reply::Receipt(inspection) = client.request(&query(Command::GraphInspect {})).unwrap()
    else {
        panic!("graph inspection");
    };
    let Some(ResultView::GraphInspection(inspection)) = inspection.result else {
        panic!("typed graph inspection");
    };
    assert!(
        inspection.active.is_none()
            && inspection.configured.is_none()
            && inspection.prepared.is_none()
    );
    assert!(owner.state().configured_profile.is_none());
    assert!(owner.state().muted && owner.state().output.is_none());
}

#[test]
fn actual_fir_import_projects_the_owner_computed_reference_without_starting_audio() {
    let owner = Owner::start();
    let client = owner.client();
    let mut wave = Vec::new();
    wave.extend(b"RIFF");
    wave.extend(60_u32.to_le_bytes());
    wave.extend(b"WAVEfmt ");
    wave.extend(16_u32.to_le_bytes());
    wave.extend(3_u16.to_le_bytes());
    wave.extend(1_u16.to_le_bytes());
    wave.extend(48000_u32.to_le_bytes());
    wave.extend(384000_u32.to_le_bytes());
    wave.extend(8_u16.to_le_bytes());
    wave.extend(64_u16.to_le_bytes());
    wave.extend(b"data");
    wave.extend(24_u32.to_le_bytes());
    for value in [0.5_f64, -0.25, 0.0] {
        wave.extend(value.to_le_bytes());
    }
    let source = owner.runtime.join("owned-fir.wav");
    std::fs::write(&source, &wave).unwrap();
    let request = Request::mutation(
        RequestId::new(unique()).unwrap(),
        &owner.state(),
        Command::FirImport {
            path: source.to_str().unwrap().into(),
            purpose: earph_core::dsp_nodes::FirPurpose::Correction,
        },
    )
    .unwrap();
    let receipt = terminal(&client, &request);
    assert_eq!(receipt.status, ReplyStatus::Applied);
    let Some(earph_desktop::ResultView::FirAsset(asset)) = receipt.result else {
        panic!("typed actual FIR reference");
    };
    assert_eq!(asset.sample_rate_hz, 48000);
    assert_eq!(asset.channels, earph_core::audio::ChannelLayout::Mono);
    assert_eq!(asset.taps_per_channel, 3);
    assert_eq!(
        std::fs::read(
            owner
                .runtime
                .join("assets")
                .join(format!("{}.wav", asset.sha256.as_str()))
        )
        .unwrap(),
        wave
    );
    assert_eq!(std::fs::read(source).unwrap(), wave);
    let Command::ProfilesCreate { draft, .. } = profile_payload() else {
        unreachable!();
    };
    let import = Request::mutation(
        RequestId::new(unique()).unwrap(),
        &owner.state(),
        Command::ProfilesImport {
            id: earph_core::ids::ProfileId::new("fir-profile").unwrap(),
            expected_profile_revision: None,
            draft,
            input: std::sync::Arc::new(earph_core::profile_control::ProfileCorrectionInput::Fir {
                path: owner.runtime.join("owned-fir.wav").to_str().unwrap().into(),
                purpose: earph_core::dsp_nodes::FirPurpose::Correction,
                context: earph_core::profile_control::ProfileCorrectionContext {
                    sample_rate_hz: 48000,
                    channels: earph_core::audio::ChannelLayout::Mono,
                    quantum_frames: 64,
                },
                metadata_json: "{}".into(),
                preamp_db: earph_core::graph::FiniteGraphScalar::new(-6.0).unwrap(),
            }),
        },
    )
    .unwrap();
    let imported = terminal(&client, &import);
    assert_eq!(imported.status, ReplyStatus::Applied, "{imported:?}");
    let Some(earph_desktop::ResultView::ProfileSaved(record)) = imported.result else {
        panic!("typed actual correction profile");
    };
    assert_eq!(record.corrections.len(), 1);
    assert_eq!(
        record.correction_preview.as_ref().unwrap().normalization,
        earph_core::profile_control::CorrectionNormalization::Preserved
    );
    assert_eq!(
        record.correction_preview.as_ref().unwrap().preamp_db.get(),
        -6.0
    );
    assert!(
        owner.state().muted && owner.state().output.is_none() && owner.state().playback.is_none()
    );
}
