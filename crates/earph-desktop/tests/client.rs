// ABOUTME: Tests immutable desktop requests through the actual LocalClient and owner.
// ABOUTME: Catches stale-target repair, reply rewriting, and client-owned shutdown.
mod common;
use common::*;
use earph_core::{ErrorCode, control::Command, ids::RequestId};
use earph_desktop::{Lane, Reply, ReplyStatus, Request};

fn discovery_receipts(
    request: &Request,
    observation: earph_core::devices::DeviceObservation,
) -> (Vec<u8>, Vec<u8>) {
    use earph_core::{
        control::{ControlLimits, OutputBackend, ReceiptStatus},
        ids::{OwnerId, ZoneId},
        session::Session,
    };
    let mut owner = Session::new_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("controlled-owner").unwrap(),
        ControlLimits::default(),
    );
    owner.enable_device_discovery(OutputBackend::Alsa);
    let accepted = owner.submit(request.envelope().clone());
    assert_eq!(accepted.status, ReceiptStatus::Accepted);
    owner
        .complete_device_query(&owner.device_query_job().unwrap(), u64::MAX, observation)
        .unwrap();
    let receipt = owner.submit(request.envelope().clone());
    assert_eq!(owner.submit(request.envelope().clone()), receipt);
    (
        serde_json::to_vec(&serde_json::json!({"receipt":accepted})).unwrap(),
        serde_json::to_vec(&serde_json::json!({"receipt":receipt})).unwrap(),
    )
}

// This controlled peer tests wire corruption, not native discovery behavior.
fn request_with_reply(
    request: &Request,
    bytes: Vec<u8>,
) -> Result<Reply, earph_desktop::DeliveryFailure> {
    use std::{
        fs,
        io::{Read, Write},
        os::unix::{fs::PermissionsExt, net::UnixListener},
        path::PathBuf,
        thread,
        time::{Duration, Instant},
    };
    let runtime = PathBuf::from("/tmp").join(unique());
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let listener = UnixListener::bind(runtime.join("o-reply.sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    fs::set_permissions(
        runtime.join("o-reply.sock"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    fs::write(
        runtime.join("endpoint.json"),
        br#"{"schema_version":1,"socket":"o-reply.sock"}"#,
    )
    .unwrap();
    fs::set_permissions(
        runtime.join("endpoint.json"),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let exact = request.bytes().to_vec();
    let peer = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        Instant::now() < deadline,
                        "controlled receipt peer received no request"
                    );
                    thread::sleep(Duration::from_millis(1));
                }
                Err(error) => panic!("controlled receipt peer: {error}"),
            }
        };
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut header = [0; 4];
        stream.read_exact(&mut header).unwrap();
        assert_eq!(u32::from_be_bytes(header) as usize, exact.len());
        let mut received = vec![0; exact.len()];
        stream.read_exact(&mut received).unwrap();
        assert_eq!(received, exact);
        stream
            .write_all(&(bytes.len() as u32).to_be_bytes())
            .unwrap();
        stream.write_all(&bytes).unwrap();
    });
    let result = earph_desktop::OwnerClient::attach(runtime.to_str().unwrap().into())
        .unwrap()
        .request(request);
    peer.join().unwrap();
    result
}

#[test]
fn discovery_queries_require_the_matching_typed_terminal_result() {
    use earph_core::{control::OutputBackend, devices::DeviceObservation};
    use earph_desktop::DeliveryFailure;
    use serde_json::{Value, json};
    for (payload, observation) in [
        (
            Command::DevicesList {
                backend: OutputBackend::Alsa,
            },
            DeviceObservation::List {
                devices: vec![],
                complete: true,
                failures: vec![],
            },
        ),
        (
            Command::DevicesInspect {
                backend: OutputBackend::Alsa,
                endpoint: "null".into(),
            },
            DeviceObservation::Inspection {
                endpoint: "null".into(),
                advertisement: None,
                current_properties: None,
                complete: true,
                failures: vec![],
            },
        ),
    ] {
        let request = query(payload);
        let (accepted, bytes) = discovery_receipts(&request, observation);
        let Reply::Receipt(pending) = request_with_reply(&request, accepted.clone()).unwrap()
        else {
            panic!("accepted receipt");
        };
        assert_eq!(pending.status, ReplyStatus::Accepted);
        assert!(pending.result.is_none());
        assert_eq!(
            request_with_reply(&request, bytes.clone())
                .expect("actual discovery result must match")
                .bytes(),
            bytes
        );
        let original: Value = serde_json::from_slice(&bytes).unwrap();
        let is_inspection = matches!(request.envelope().payload, Command::DevicesInspect { .. });
        for (pointer, replacement) in [
            ("/receipt/result/value/backend", json!("core_audio")),
            (
                "/receipt/result/value/observation",
                if is_inspection {
                    json!({"kind":"list","devices":[],"complete":true,"failures":[]})
                } else {
                    json!({"kind":"inspection","endpoint":"null","advertisement":null,"current_properties":null,"complete":true,"failures":[]})
                },
            ),
            (
                "/receipt/result",
                json!({"kind":"queue","value":{"entries":[],"selected":null,"offset":0,"total":0}}),
            ),
            ("/receipt/result", Value::Null),
            ("/receipt/status", json!("accepted")),
            ("/receipt/status", json!("failed")),
            ("/receipt/request_id", json!("wrong-request")),
            ("/receipt/zone_id", json!("wrong-zone")),
            ("/receipt/session_id", json!("wrong-session")),
        ] {
            let mut corrupt = original.clone();
            *corrupt.pointer_mut(pointer).unwrap() = replacement;
            let raw = serde_json::to_vec(&corrupt).unwrap();
            let Err(DeliveryFailure::Protocol(error)) = request_with_reply(&request, raw.clone())
            else {
                panic!("wrong result admitted: {pointer}");
            };
            assert_eq!(&*error.bytes, raw);
        }
        if is_inspection {
            let mut corrupt = original;
            corrupt["receipt"]["result"]["value"]["observation"]["endpoint"] = json!("other");
            let raw = serde_json::to_vec(&corrupt).unwrap();
            let Err(DeliveryFailure::Protocol(error)) = request_with_reply(&request, raw.clone())
            else {
                panic!("inspection endpoint changed");
            };
            assert_eq!(&*error.bytes, raw);
        }
    }
}

#[test]
fn discovery_failure_receipts_keep_typed_errors_and_validate_the_requested_backend() {
    use earph_core::{
        control::OutputBackend,
        devices::{DeviceObservation, DeviceQueryError},
    };
    for payload in [
        Command::DevicesList {
            backend: OutputBackend::Alsa,
        },
        Command::DevicesInspect {
            backend: OutputBackend::Alsa,
            endpoint: "null".into(),
        },
    ] {
        let request = query(payload);
        for code in [ErrorCode::DeviceUnavailable, ErrorCode::OperationCancelled] {
            let (_, bytes) = discovery_receipts(
                &request,
                DeviceObservation::Failed {
                    error: DeviceQueryError {
                        code,
                        message: "controlled native failure".into(),
                        native_status: None,
                        native_object_id: None,
                    },
                    cleanup_confirmed: true,
                },
            );
            let Reply::Receipt(receipt) = request_with_reply(&request, bytes.clone()).unwrap()
            else {
                panic!("terminal failure");
            };
            assert_eq!(
                receipt.status,
                if code == ErrorCode::OperationCancelled {
                    ReplyStatus::Cancelled
                } else {
                    ReplyStatus::Failed
                }
            );
            assert_eq!(receipt.error.unwrap().code, code);
            assert!(matches!(
                receipt.result,
                Some(earph_desktop::ResultView::Devices(_))
            ));
            let original: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            for (pointer, replacement) in [
                (
                    "/receipt/result/value/backend",
                    serde_json::json!("core_audio"),
                ),
                (
                    "/receipt/result",
                    serde_json::json!({"kind":"queue","value":{"entries":[],"selected":null,"offset":0,"total":0}}),
                ),
            ] {
                let mut corrupt = original.clone();
                *corrupt.pointer_mut(pointer).unwrap() = replacement;
                let raw = serde_json::to_vec(&corrupt).unwrap();
                let Err(earph_desktop::DeliveryFailure::Protocol(error)) =
                    request_with_reply(&request, raw.clone())
                else {
                    panic!("wrong failure result admitted: {pointer}");
                };
                assert_eq!(&*error.bytes, raw);
            }
        }
    }
}

#[test]
fn embedded_owner_advertises_empty_discovery_and_retains_capability_rejection() {
    use earph_core::control::OutputBackend;
    let owner = Owner::start();
    let client = owner.client();
    let before = owner.state();
    let Reply::Receipt(caps) = client.request(&query(Command::Capabilities)).unwrap() else {
        panic!("caps");
    };
    let Some(earph_desktop::ResultView::Capabilities(caps)) = caps.result else {
        panic!("typed caps");
    };
    assert!(caps.devices.backends.is_empty());
    for command in [
        Command::DevicesList {
            backend: OutputBackend::Alsa,
        },
        Command::DevicesInspect {
            backend: OutputBackend::Alsa,
            endpoint: "null".into(),
        },
    ] {
        let request = query(command);
        let reply = client.request(&request).unwrap();
        assert_eq!(reply.bytes(), client.request(&request).unwrap().bytes());
        let Reply::Receipt(receipt) = reply else {
            panic!("rejected receipt");
        };
        assert_eq!(receipt.status, ReplyStatus::Rejected);
        assert_eq!(
            receipt.error.unwrap().code,
            ErrorCode::CapabilityUnsupported
        );
        assert!(receipt.result.is_none());
    }
    assert_eq!(owner.state().revision, before.revision);
    assert_eq!(owner.state().epochs, before.epochs);
    assert!(owner.state().muted && !owner.state().output_consented);
}

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
fn start_requests_capture_a_new_session_without_repairing_stale_context() {
    use earph_core::{
        control::SourceKind,
        ids::{DeviceId, SessionId},
    };
    use earph_desktop::RequestError;
    let owner = Owner::start();
    let client = owner.client();
    let state = owner.state();
    assert!(state.session_id.is_none());
    let session = SessionId::new(unique()).unwrap();
    let start = Request::start(
        RequestId::new(unique()).unwrap(),
        &state,
        session.clone(),
        Command::Start {
            source: SourceKind::LocalFile,
            device_id: DeviceId::new("owned-output").unwrap(),
            takeover: false,
            seekable: true,
        },
    )
    .unwrap();
    assert_eq!(start.envelope().session_id.as_ref(), Some(&session));
    assert_eq!(start.envelope().owner_id, state.owner_id);
    assert_eq!(start.envelope().zone_id, state.zone_id);
    assert_eq!(start.envelope().expected_revision, Some(state.revision));
    assert_eq!(start.lane(), Lane::Ordinary);
    assert!(state.session_id.is_none());
    let exact = start.bytes().to_vec();
    let append = Request::mutation(
        RequestId::new(unique()).unwrap(),
        &state,
        Command::QueueAppend {
            path: "owned-not-opened.wav".into(),
        },
    )
    .unwrap();
    client.request(&append).unwrap();
    let Reply::Receipt(refused) = client.request(&start).unwrap() else {
        panic!("actual stale Start receipt");
    };
    assert_eq!(refused.status, ReplyStatus::Rejected);
    assert_eq!(refused.error.unwrap().code, ErrorCode::StaleRevision);
    assert_eq!(start.bytes(), exact);
    assert!(owner.state().session_id.is_none());
    assert_eq!(
        Request::start(
            RequestId::new(unique()).unwrap(),
            &state,
            session.clone(),
            Command::Stop
        )
        .unwrap_err(),
        RequestError::NotStart
    );
    let mut no_owner = state;
    no_owner.owner_id = None;
    assert_eq!(
        Request::start(
            RequestId::new(unique()).unwrap(),
            &no_owner,
            session,
            Command::Start {
                source: SourceKind::LocalFile,
                device_id: DeviceId::new("owned-output").unwrap(),
                takeover: false,
                seekable: true
            }
        )
        .unwrap_err(),
        RequestError::MissingOwner
    );
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
