// ABOUTME: Drives real Session graph preparation and activation without platform resources.
// ABOUTME: Supplies controlled domain observations while callers provide compiler or contract previews.
use earph_core::{
    audio::{ChannelLayout, PcmFormat, PcmRepresentation},
    control::*,
    graph::{GraphDocument, GraphProcessingRequest, QuantizationPolicy},
    graph_control::*,
    ids::*,
    profiles::ProfileObjectHash,
    queue::PreparationStatus,
    safety::{FidelityPolicy, VolumeMode},
    session::*,
};
use serde_json::json;
use std::sync::Arc;

pub fn command(session: &Session, id: &str, payload: Command) -> CommandEnvelope {
    let state = session.snapshot();
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

pub fn preparing(endpoint: &str, device_identity: Option<ProfileObjectHash>) -> Session {
    let mut session = Session::new_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("private-owner").unwrap(),
        ControlLimits::default(),
    );
    session.enable_local_playback();
    let device_id = DeviceId::new("private-device").unwrap();
    let profile_id = ProfileId::new("private-profile").unwrap();
    let output = OutputConfiguration {
        backend: OutputBackend::Alsa,
        route: OutputRoute::System,
        endpoint: endpoint.into(),
        device_id: device_id.clone(),
        profile_id: profile_id.clone(),
        period_frames: 64,
        buffer_frames: 256,
        volume_mode: VolumeMode::Software,
        fidelity: FidelityPolicy::CompatibleConversion,
    };
    for (id, payload) in [
        ("output", Command::OutputSelect { output }),
        (
            "consent",
            Command::OutputConsent {
                device_id: device_id.clone(),
                profile_id,
            },
        ),
        (
            "queue",
            Command::QueueAppend {
                path: "/private/input.wav".into(),
            },
        ),
    ] {
        let receipt = session.submit(command(&session, id, payload));
        assert_eq!(receipt.status, ReceiptStatus::Applied, "{receipt:?}");
    }
    let mut start = command(
        &session,
        "start",
        Command::Start {
            source: SourceKind::LocalFile,
            device_id,
            takeover: false,
            seekable: true,
        },
    );
    start.session_id = Some(SessionId::new("private-session").unwrap());
    assert_eq!(session.submit(start).status, ReceiptStatus::Accepted);
    let pause = command(&session, "pause-preparation", Command::Pause);
    assert_eq!(session.submit(pause).status, ReceiptStatus::Applied);
    let source_entry_id = session.snapshot().active_entry_id.unwrap();
    let format = PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Signed16).unwrap();
    let observation = PlaybackObservation {
        processing: None,
        format,
        submitted_frames: 0,
        consumed_frames: None,
        backend_delay_frames: None,
        consumption_basis: "controlled domain observation; no device opened",
        source_entry_id: source_entry_id.clone(),
        source_position_frames: 0,
        audible_entry_id: None,
        audible_source_position_frames: None,
        input_peak: None,
        output_peak: None,
        modified: false,
        output_activity: None,
        backend: Some(BackendObservation {
            backend: OutputBackend::Alsa,
            device_identity_sha256: device_identity,
            audio_storage_bytes: 256,
            client_format: format.into(),
            device_format: Some(format.into()),
            measurement_boundary: "alsa_client_negotiation",
            preservation: PreservationState::Unverified,
        }),
    };
    session
        .set_queue_preparation(
            session.snapshot().epochs,
            &source_entry_id,
            PreparationStatus::Ready {
                format,
                total_frames: 48000,
            },
        )
        .unwrap();
    session
        .advance(EngineEvent {
            epochs: session.snapshot().epochs,
            monotonic_ns: 1,
            kind: EngineEventKind::LocalPrepared { observation },
        })
        .unwrap();
    assert_eq!(session.snapshot().transport, TransportState::Paused);
    let port = json!({"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"float64","max_quantum_frames":64});
    let graph: GraphDocument = serde_json::from_value(json!({
        "schema_version":1,"output":"private-out","nodes":[
            {"id":"private-in","kind":{"kind":"input"},"format":port},
            {"id":"private-gain","kind":{"kind":"gain","gain_linear":0.5},"format":port},
            {"id":"private-out","kind":{"kind":"output"},"format":port}],
        "edges":[{"from":"private-in","to":"private-gain","from_port":0,"to_port":0},
            {"from":"private-gain","to":"private-out","from_port":0,"to_port":0}]
    }))
    .unwrap();
    let prepare = command(
        &session,
        "prepare",
        Command::GraphPrepare {
            graph: Arc::new(graph),
            processing: GraphProcessingRequest::Float64 {
                quantization: QuantizationPolicy::RoundTiesAwaySaturateNoDither,
            },
        },
    );
    let receipt = session.submit_graph(prepare, 2);
    assert_eq!(receipt.status, ReceiptStatus::Accepted, "{receipt:?}");
    session
}

pub fn activate(session: &mut Session, preview: Arc<GraphPreview>) -> Receipt {
    let job = session.graph_job().unwrap();
    session
        .complete_graph_preparation(&job.request_id, &job.binding, 3, Ok(preview.clone()))
        .unwrap();
    let prepared = session.graph_inspection().prepared.unwrap();
    let commit = command(
        session,
        "commit",
        Command::GraphCommit {
            token: prepared.token,
            graph_hash: preview.graph_hash.clone(),
        },
    );
    assert_eq!(
        session.submit_graph(commit.clone(), 4).status,
        ReceiptStatus::Accepted
    );
    session
        .complete_graph_activation(
            GraphActivationAck {
                request_id: commit.request_id,
                epochs: session.snapshot().epochs,
                graph_hash: preview.graph_hash.clone(),
                activation_frame: 0,
                observation: session.snapshot().playback.unwrap(),
                source_total_frames: 48000,
            },
            5,
            |_| Ok(()),
        )
        .unwrap();
    let receipt = session.submit(command(session, "signal-path", Command::SignalPathGet {}));
    assert_eq!(receipt.status, ReceiptStatus::Applied, "{receipt:?}");
    receipt
}
