// ABOUTME: Checks graph preparation against real captured playback facts and DSP execution.
// ABOUTME: Exercises bounded ownership, semantic hashes, cancellation, and retirement lifetimes.
use earph_engine::{assets, stream_render};
#[path = "support/graphs.rs"]
mod graph_fixtures;
mod support;
mod graph {
    include!("../src/graph.rs");

    pub(super) fn wait_for_validation_publication(service: &GraphService) {
        let end = Instant::now() + std::time::Duration::from_secs(5);
        // A validation job releases its Retirement owner only after publishing its completion.
        while Arc::strong_count(&service.retirement) != 1 {
            assert!(
                Instant::now() < end,
                "validation did not publish its completion"
            );
            thread::yield_now();
        }
    }

    #[test]
    fn shutdown_does_not_wait_for_an_external_owner_during_retirement_accounting() {
        let state = crate::snapshot();
        let request = crate::request(&state, serde_json::json!({"kind":"bypass"}));
        let mut service = GraphService::spawn(None).unwrap();
        service
            .submit(request, state, crate::active_source())
            .unwrap();
        let pipeline = crate::completion(&mut service)
            .result
            .unwrap()
            .pipeline
            .unwrap()
            .activate();
        let accounting = service.retirement.accounting.clone();
        assert_eq!(accounting.owners.load(Ordering::Acquire), 1);
        // Select the valid intermediate state after B releases its owner, before its retirement acknowledgement.
        // A remains owned by this caller and cannot retire until service shutdown returns.
        accounting.retiring.store(1, Ordering::Release);
        let (finished_tx, finished_rx) = mpsc::sync_channel(1);
        let shutdown = thread::spawn(move || {
            drop(service);
            finished_tx.send(()).unwrap();
        });
        let returned = finished_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .is_ok();
        // Complete B's selected accounting interval, then release A even when the regression fails.
        accounting.retiring.store(0, Ordering::Release);
        drop(pipeline);
        shutdown.join().unwrap();
        assert!(
            returned,
            "service shutdown waited for a pipeline still held by its caller"
        );
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while accounting.owners.load(Ordering::Acquire) != 0
            || accounting.retiring.load(Ordering::Acquire) != 0
            || accounting.resident.load(Ordering::Acquire) != 0
        {
            assert!(
                Instant::now() < deadline,
                "pipeline did not finish retiring"
            );
            thread::yield_now();
        }
        assert_eq!(accounting.owners.load(Ordering::Acquire), 0);
        assert_eq!(accounting.retiring.load(Ordering::Acquire), 0);
        assert_eq!(accounting.resident.load(Ordering::Acquire), 0);
    }

    #[test]
    fn cancellation_before_compile_is_deterministic() {
        let state = crate::snapshot();
        let request = crate::request(&state, serde_json::json!({"kind":"bypass"}));
        let service = GraphService::spawn(None).unwrap();
        for source_cancelled in [false, true] {
            let cancellation = GraphCancellation(Arc::new(AtomicBool::new(!source_cancelled)));
            let source = Arc::new(AtomicBool::new(source_cancelled));
            let result = prepare(
                &request,
                &state,
                &cancellation,
                &source,
                &service.retirement,
                Instant::now(),
                None,
            );
            assert_eq!(result.err().unwrap().code, ErrorCode::OperationCancelled);
            assert_eq!(service.resources().resident_bytes, 0);
        }
    }

    #[test]
    fn dropping_pipeline_marks_render_cancelled_before_queueing_retirement() {
        let state = crate::snapshot();
        let request = crate::request(&state, serde_json::json!({"kind":"bypass"}));
        let mut service = GraphService::spawn(None).unwrap();
        service
            .submit(request, state, crate::active_source())
            .unwrap();
        let mut pipeline = crate::completion(&mut service)
            .result
            .unwrap()
            .pipeline
            .unwrap();
        let executor = pipeline.retirement.clone();
        // Hold the real prepared payload in an unconsumed retirement mailbox.
        // This exposes the fixed lifecycle state before any destructor can run.
        let mailbox = Arc::new(Mailbox::default());
        let queue = Arc::new(Retirement {
            mailbox: mailbox.clone(),
            join: Mutex::new(None),
            thread_id: executor.thread_id,
            accounting: executor.accounting.clone(),
        });
        pipeline.retirement = queue.clone();
        drop(pipeline);
        let retired = mailbox.state.lock().unwrap().retired[0].take().unwrap();
        let Mode::Float64(render) = &retired.payload.mode else {
            panic!("float renderer required");
        };
        let lifecycle = render.graph().lifecycle();
        let accounting = executor.accounting.clone();
        thread::spawn(move || retire(retired, &accounting))
            .join()
            .unwrap();
        crate::retired(&service);
        assert_eq!(lifecycle, earph_dsp::GraphLifecycle::Cancelled);
    }

    #[test]
    fn admission_waits_for_retirement_acknowledgement() {
        let state = crate::snapshot();
        let request = crate::request(&state, serde_json::json!({"kind":"bypass"}));
        let mut service = GraphService::spawn(None).unwrap();
        service
            .submit(request.clone(), state.clone(), crate::active_source())
            .unwrap();
        let pipeline = crate::completion(&mut service)
            .result
            .unwrap()
            .pipeline
            .unwrap()
            .activate();
        let mailbox = service.retirement.mailbox.clone();
        let guard = mailbox.state.lock().unwrap();
        let retirement = thread::spawn(move || drop(pipeline));
        let end = Instant::now() + std::time::Duration::from_secs(5);
        while service.resources().pending_retirements == 0 {
            assert!(Instant::now() < end);
            thread::yield_now();
        }
        assert_ne!(service.resources().resident_bytes, 0);
        assert_eq!(
            service
                .submit(request, state, crate::active_source())
                .err()
                .unwrap()
                .code,
            ErrorCode::SourceBusy
        );
        drop(guard);
        retirement.join().unwrap();
        crate::retired(&service);
    }
}
#[allow(
    dead_code,
    reason = "The companion render_bridge tests cover the complete bridge API."
)]
#[path = "../src/render.rs"]
mod render;

use earph_core::{
    ErrorCode,
    audio::{ChannelLayout, MemoryBudget, PcmBlock, PcmFormat, PcmRepresentation},
    control::{ControlLimits, OutputBackend, OutputConfiguration, OutputRoute},
    graph::{GraphDocument, GraphProcessingRequest, PreservationDecision, QuantizationPolicy},
    graph_control::{GraphBinding, GraphIntent, GraphJobRequest, SignalTransformation},
    ids::*,
    safety::{FidelityPolicy, VolumeMode},
    session::{
        BackendObservation, PlaybackObservation, PreservationState, Session, SessionSnapshot,
    },
};
use graph::{GraphCompletion, GraphService};
use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

fn snapshot() -> SessionSnapshot {
    let mut state = Session::new_owner(
        ZoneId::new("zone").unwrap(),
        OwnerId::new("owner").unwrap(),
        ControlLimits::default(),
    )
    .snapshot();
    state.source = Some(earph_core::control::SourceKind::LocalFile);
    state.session_id = Some(SessionId::new("session").unwrap());
    state.output_id = Some(DeviceId::new("device").unwrap());
    state.software_gain_db = -12.0;
    state.output_consented = true;
    state.output = Some(OutputConfiguration {
        backend: OutputBackend::Alsa,
        route: OutputRoute::System,
        endpoint: "null".into(),
        device_id: DeviceId::new("device").unwrap(),
        profile_id: ProfileId::new("profile").unwrap(),
        period_frames: 64,
        buffer_frames: 256,
        volume_mode: VolumeMode::Software,
        fidelity: FidelityPolicy::CompatibleConversion,
    });
    let format = PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Signed16).unwrap();
    state.playback = Some(PlaybackObservation {
        output_activity: None,
        processing: None,
        format,
        submitted_frames: 0,
        consumed_frames: None,
        backend_delay_frames: None,
        consumption_basis: "native test fixture",
        source_entry_id: QueueEntryId::new("entry").unwrap(),
        source_position_frames: 0,
        audible_entry_id: None,
        audible_source_position_frames: None,
        input_peak: None,
        output_peak: None,
        modified: false,
        backend: Some(BackendObservation {
            device_identity_sha256: None,
            backend: OutputBackend::Alsa,
            audio_storage_bytes: 256,
            client_format: format.into(),
            device_format: Some(format.into()),
            measurement_boundary: "native client",
            preservation: PreservationState::Unverified,
        }),
    });
    state
}
fn active_source() -> Arc<std::sync::atomic::AtomicBool> {
    Arc::new(std::sync::atomic::AtomicBool::new(false))
}

#[test]
fn retained_profile_metadata_refuses_a_graph_before_pipeline_publication() {
    for reported in [64 * 1024 * 1024 + 1, 256 * 1024 * 1024, u64::MAX] {
        let mut state = snapshot();
        state.profile_storage_bytes = reported;
        let request = request(&state, serde_json::json!({"kind":"bypass"}));
        let mut service = GraphService::spawn(None).unwrap();
        service.submit(request, state, active_source()).unwrap();
        let result = completion(&mut service);
        assert_eq!(
            result.result.err().unwrap().code,
            earph_core::ErrorCode::ResourceBudgetExceeded
        );
        assert_eq!(service.resources().resident_bytes, 0);
    }
}

#[test]
fn graph_compilation_reserves_profile_history_growth_without_charging_unused_profiles() {
    for profile_bytes in [0, 1024] {
        let mut state = snapshot();
        state.profile_storage_bytes = profile_bytes;
        let mut job = request(
            &state,
            serde_json::json!({"kind":"delay","delay_frames":14000000}),
        );
        job.validate_only = true;
        let mut service = GraphService::spawn(None).unwrap();
        service.submit(job, state, active_source()).unwrap();
        let completed = completion(&mut service);
        if profile_bytes == 0 {
            assert!(completed.result.is_ok());
        } else {
            assert_eq!(
                completed.result.err().unwrap().code,
                earph_core::ErrorCode::ResourceBudgetExceeded
            );
        }
    }
}
fn request(state: &SessionSnapshot, effect: serde_json::Value) -> GraphJobRequest {
    let port = serde_json::json!({"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"float64","max_quantum_frames":64});
    let document: GraphDocument = serde_json::from_value(serde_json::json!({
        "schema_version":1,
        "nodes":[{"id":"in","kind":{"kind":"input"},"format":port}, {"id":"effect","kind":effect,"format":port}, {"id":"out","kind":{"kind":"output"},"format":port}],
        "edges":[{"from":"in","to":"effect","from_port":0,"to_port":0},{"from":"effect","to":"out","from_port":0,"to_port":0}],"output":"out"
    })).unwrap();
    GraphJobRequest {
        profile_application: None,
        request_id: RequestId::new("job").unwrap(),
        binding: GraphBinding {
            owner_id: state.owner_id.clone().unwrap(),
            session_id: state.session_id.clone().unwrap(),
            epochs: state.epochs,
            device_epoch: state.device_epoch,
            expected_revision: state.revision,
        },
        intent: Arc::new(GraphIntent {
            graph: Arc::new(document),
            processing: GraphProcessingRequest::Float64 {
                quantization: QuantizationPolicy::RoundTiesAwaySaturateNoDither,
            },
        }),
        validate_only: false,
        deadline_monotonic_ns: 5_000_000_000,
    }
}
fn completion(service: &mut GraphService) -> GraphCompletion {
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(result) = service.try_completion() {
            return result;
        }
        assert!(Instant::now() < end, "graph preparation failed to complete");
        thread::yield_now();
    }
}
fn retired(service: &GraphService) {
    let end = Instant::now() + Duration::from_secs(5);
    while service.resources().resident_bytes != 0
        || service.resources().preparing_or_candidate
        || service.resources().pending_retirements != 0
    {
        assert!(
            Instant::now() < end,
            "graph retirement failed to acknowledge"
        );
        thread::yield_now();
    }
}

fn with_profile(mut request: GraphJobRequest) -> GraphJobRequest {
    use earph_core::{profile_control::*, profiles::*, safety::VolumePolicy};
    let hash = ProfileObjectHash::try_from("c".repeat(64)).unwrap();
    request.profile_application = Some(Arc::new(ProfileApplicationBinding {
        volume_mode: earph_core::safety::VolumeMode::Software,
        fidelity: earph_core::safety::FidelityPolicy::CompatibleConversion,
        device_identity_sha256: None,
        profile: ProfileCatalogReference {
            id: ProfileId::new("headphones").unwrap(),
            revision: 1,
            profile_sha256: hash,
        },
        graph_sha256: hash,
        source: earph_core::control::SourceKind::LocalFile,
        effective_policy: VolumePolicy::new(-20.0, None).unwrap(),
        software_gain_db: -20.0,
        latency: ProfileLatencyIntent {
            mode: ProfileLatencyMode::Balanced,
            max_resident_bytes: 256 << 20,
            max_preparation_bytes: 256 << 20,
            max_latency_frames: None,
        },
    }));
    request
}

fn fixed_profile_request() -> (GraphJobRequest, SessionSnapshot) {
    use earph_core::safety::{OutputContext, VolumeMode, VolumePolicy};
    let mut state = snapshot();
    let output = state.output.as_mut().unwrap();
    output.volume_mode = VolumeMode::Fixed;
    state.software_gain_db = 0.0;
    state.gain_ramp_frames = 0;
    state.volume_policy.arm_fixed(OutputContext {
        device_id: output.device_id.clone(),
        profile_id: output.profile_id.clone(),
    });
    let mut job = with_profile(request(&state, serde_json::json!({"kind":"bypass"})));
    let application = Arc::make_mut(job.profile_application.as_mut().unwrap());
    application.volume_mode = VolumeMode::Fixed;
    application.software_gain_db = 0.0;
    application.effective_policy = VolumePolicy::default();
    (job, state)
}
#[test]
fn fixed_profile_compiles_prospective_unarmed_policy_without_relaxing_ordinary_graph_authorization()
{
    use earph_core::{ErrorCode, safety::*};
    let mut service = GraphService::spawn(None).unwrap();
    let (job, state) = fixed_profile_request();
    service
        .submit(job.clone(), state.clone(), active_source())
        .unwrap();
    let prepared = completion(&mut service).result.unwrap();
    let application = prepared.preview.profile_application.as_ref().unwrap();
    assert_eq!(application.volume_mode, VolumeMode::Fixed);
    assert!(
        application
            .effective_policy
            .validate_unmute(
                &OutputContext {
                    device_id: state.output.as_ref().unwrap().device_id.clone(),
                    profile_id: application.profile.id.clone()
                },
                VolumeMode::Fixed,
                SampleDomain::Pcm,
                application.fidelity,
                0.0,
                None
            )
            .is_err()
    );
    drop(prepared);
    retired(&service);
    for purpose in ["ordinary", "profile"] {
        let (mut job, mut state) = fixed_profile_request();
        state.volume_policy.disarm();
        if purpose == "ordinary" {
            job.profile_application = None;
        }
        service.submit(job, state, active_source()).unwrap();
        assert_eq!(
            completion(&mut service).result.err().unwrap().code,
            ErrorCode::SafetyPolicyBlocked,
            "{purpose}"
        );
        retired(&service);
    }
}
#[test]
fn fixed_profile_service_refuses_mode_policy_ramp_headroom_and_cancelled_source() {
    use earph_core::{ErrorCode, safety::*};
    for (condition, expected) in [
        ("consent", ErrorCode::SafetyPolicyBlocked),
        ("ramp", ErrorCode::SafetyPolicyBlocked),
        ("cap", ErrorCode::SafetyPolicyBlocked),
        ("hardware", ErrorCode::SafetyPolicyBlocked),
        ("mode", ErrorCode::StaleSession),
        ("fidelity", ErrorCode::StaleSession),
        ("headroom", ErrorCode::SafetyPolicyBlocked),
        ("cancel", ErrorCode::OperationCancelled),
    ] {
        let (mut job, mut state) = fixed_profile_request();
        let cancel = active_source();
        match condition {
            "consent" => state.output_consented = false,
            "ramp" => state.gain_ramp_frames = 64,
            "cap" => {
                Arc::make_mut(job.profile_application.as_mut().unwrap()).effective_policy =
                    VolumePolicy::new(-20.0, None).unwrap()
            }
            "hardware" => {
                Arc::make_mut(job.profile_application.as_mut().unwrap()).effective_policy =
                    VolumePolicy::new(0.0, Some(-30.0)).unwrap()
            }
            "mode" => {
                Arc::make_mut(job.profile_application.as_mut().unwrap()).volume_mode =
                    VolumeMode::Software
            }
            "fidelity" => {
                Arc::make_mut(job.profile_application.as_mut().unwrap()).fidelity =
                    FidelityPolicy::ExpertOverride
            }
            "headroom" => {
                job.intent =
                    request(&state, serde_json::json!({"kind":"gain","gain_linear":2.0})).intent
            }
            "cancel" => cancel.store(true, std::sync::atomic::Ordering::Release),
            _ => unreachable!(),
        }
        let mut service = GraphService::spawn(None).unwrap();
        if condition == "cancel" {
            assert_eq!(
                service.submit(job, state, cancel).err().unwrap().code,
                expected
            );
            assert!(service.try_completion().is_none());
            continue;
        }
        service.submit(job, state, cancel).unwrap();
        assert_eq!(
            completion(&mut service).result.err().unwrap().code,
            expected,
            "{condition}"
        );
        retired(&service);
    }
}

#[test]
fn profile_hashes_bind_supported_mode_and_fidelity_with_the_same_kernel() {
    use earph_core::safety::*;
    let mut service = GraphService::spawn(None).unwrap();
    let mut hashes = vec![];
    let mut kernels = vec![];
    for (mode, fidelity) in [
        (VolumeMode::Software, FidelityPolicy::CompatibleConversion),
        (VolumeMode::Fixed, FidelityPolicy::CompatibleConversion),
        (VolumeMode::Fixed, FidelityPolicy::ExpertOverride),
    ] {
        let (mut job, mut state) = fixed_profile_request();
        state.output.as_mut().unwrap().volume_mode = mode;
        state.output.as_mut().unwrap().fidelity = fidelity;
        let application = Arc::make_mut(job.profile_application.as_mut().unwrap());
        application.volume_mode = mode;
        application.fidelity = fidelity;
        job.validate_only = true;
        service.submit(job, state, active_source()).unwrap();
        let result = completion(&mut service).result.unwrap();
        hashes.push(result.preview.graph_hash.clone());
        kernels.push(result.preview.kernel_hash.clone());
        graph::wait_for_validation_publication(&service);
    }
    assert!(kernels.windows(2).all(|pair| pair[0] == pair[1]));
    assert_ne!(hashes[0], hashes[1]);
    assert_ne!(hashes[1], hashes[2]);
}

#[test]
fn profile_preparation_semantic_hash_binds_immutable_revision_and_caps() {
    let state = snapshot();
    let base = with_profile(request(&state, serde_json::json!({"kind":"bypass"})));
    let mut service = GraphService::spawn(None).unwrap();
    let mut hashes = vec![];
    for (revision, cap) in [(1, -20.0), (2, -20.0), (2, -30.0)] {
        let mut job = base.clone();
        job.validate_only = true;
        let application = Arc::make_mut(job.profile_application.as_mut().unwrap());
        application.profile.revision = revision;
        application.effective_policy = earph_core::safety::VolumePolicy::new(cap, None).unwrap();
        application.software_gain_db = cap;
        service.submit(job, state.clone(), active_source()).unwrap();
        let prepared = completion(&mut service).result.unwrap();
        hashes.push(prepared.preview.graph_hash.clone());
        assert!(prepared.pipeline.is_none());
        graph::wait_for_validation_publication(&service);
    }
    assert_ne!(
        hashes[0], hashes[1],
        "same kernel must not erase the selected saved revision"
    );
    assert_ne!(hashes[1], hashes[2]);
}

#[test]
fn profile_preparation_rejects_substituted_native_identity() {
    let mut state = snapshot();
    state.output.as_mut().unwrap().backend = earph_core::control::OutputBackend::CoreAudio;
    let backend = state.playback.as_mut().unwrap().backend.as_mut().unwrap();
    backend.backend = earph_core::control::OutputBackend::CoreAudio;
    backend.device_identity_sha256 = Some("1".repeat(64).try_into().unwrap());
    let mut job = with_profile(request(&state, serde_json::json!({"kind":"bypass"})));
    Arc::make_mut(job.profile_application.as_mut().unwrap()).device_identity_sha256 =
        Some("2".repeat(64).try_into().unwrap());
    let mut service = GraphService::spawn(None).unwrap();
    service.submit(job, state, active_source()).unwrap();
    assert_eq!(
        completion(&mut service).result.err().unwrap().code,
        earph_core::ErrorCode::StaleSession
    );
}

#[test]
fn profile_semantic_hash_tracks_observed_native_identity_without_hardware_claims() {
    let mut service = GraphService::spawn(None).unwrap();
    let mut hashes = vec![];
    for n in [1, 2] {
        let mut state = snapshot();
        state.output.as_mut().unwrap().backend = earph_core::control::OutputBackend::CoreAudio;
        let identity = Some(format!("{n:064x}").try_into().unwrap());
        let backend = state.playback.as_mut().unwrap().backend.as_mut().unwrap();
        backend.backend = earph_core::control::OutputBackend::CoreAudio;
        backend.device_identity_sha256 = identity;
        let mut job = with_profile(request(&state, serde_json::json!({"kind":"bypass"})));
        job.validate_only = true;
        Arc::make_mut(job.profile_application.as_mut().unwrap()).device_identity_sha256 = identity;
        service.submit(job, state, active_source()).unwrap();
        let result = completion(&mut service).result.unwrap();
        hashes.push(result.preview.graph_hash.clone());
        assert!(result.pipeline.is_none());
        graph::wait_for_validation_publication(&service);
    }
    assert_ne!(hashes[0], hashes[1]);
}

#[test]
fn live_profile_preparation_enforces_declared_memory_limits() {
    let state = snapshot();
    for (resident, preparation) in [(1, 256 << 20), (1, 1)] {
        let mut job = with_profile(request(&state, serde_json::json!({"kind":"bypass"})));
        let application = Arc::make_mut(job.profile_application.as_mut().unwrap());
        application.latency.max_resident_bytes = resident;
        application.latency.max_preparation_bytes = preparation;
        let mut service = GraphService::spawn(None).unwrap();
        service.submit(job, state.clone(), active_source()).unwrap();
        let error = completion(&mut service)
            .result
            .err()
            .expect("declared limits must admit the actual pipeline");
        assert_eq!(error.code, ErrorCode::ResourceBudgetExceeded);
        retired(&service);
    }
}

#[test]
fn live_profile_limits_admit_the_exact_known_resident_and_preparation_boundary() {
    let state = snapshot();
    let base = with_profile(request(&state, serde_json::json!({"kind":"bypass"})));
    let mut service = GraphService::spawn(None).unwrap();
    service
        .submit(base.clone(), state.clone(), active_source())
        .unwrap();
    let prepared = completion(&mut service).result.unwrap();
    let usage = prepared.preview.resources;
    let resident = usage.pipeline_bytes + usage.backend_audio_bytes + 256;
    drop(prepared);
    retired(&service);
    for (resident_limit, preparation_limit, pass) in [
        (resident, usage.aggregate_peak_bytes, true),
        (resident - 1, usage.aggregate_peak_bytes, false),
        (resident, usage.aggregate_peak_bytes - 1, false),
    ] {
        let mut job = base.clone();
        let application = Arc::make_mut(job.profile_application.as_mut().unwrap());
        application.latency.max_resident_bytes = resident_limit;
        application.latency.max_preparation_bytes = preparation_limit;
        service.submit(job, state.clone(), active_source()).unwrap();
        let result = completion(&mut service).result;
        if pass {
            drop(result.unwrap());
        } else {
            assert_eq!(
                result.err().unwrap().code,
                ErrorCode::ResourceBudgetExceeded
            );
        }
        retired(&service);
    }
}

#[test]
fn live_profile_src_cannot_claim_a_finite_aggregate_latency_ceiling() {
    let mut state = snapshot();
    state.software_gain_db = -48.0;
    let mut job = with_profile(request(&state, serde_json::json!({"kind":"bypass"})));
    Arc::make_mut(job.profile_application.as_mut().unwrap()).software_gain_db = -48.0;
    Arc::make_mut(&mut job.intent).graph =
        Arc::new(serde_json::from_value(graph_fixtures::src_roundtrip()).unwrap());
    Arc::make_mut(job.profile_application.as_mut().unwrap())
        .latency
        .max_latency_frames = Some(1024);
    let mut service = GraphService::spawn(None).unwrap();
    service.submit(job, state, active_source()).unwrap();
    let error = completion(&mut service)
        .result
        .err()
        .expect("finite aggregate latency remains unqualified");
    assert_eq!(error.code, ErrorCode::CapabilityUnsupported);
    retired(&service);
}

#[test]
fn profile_preparation_cannot_substitute_backend_facts_for_an_actual_local_source() {
    let mut state = snapshot();
    state.source = None;
    let job = with_profile(request(&state, serde_json::json!({"kind":"bypass"})));
    let mut service = GraphService::spawn(None).unwrap();
    service.submit(job, state, active_source()).unwrap();
    assert_eq!(
        completion(&mut service)
            .result
            .err()
            .expect("an open backend alone is not a local source")
            .code,
        ErrorCode::StaleSession
    );
    retired(&service);
}

#[test]
fn graph_jobs_preserve_the_live_source_token_and_reject_retired_sources() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let state = snapshot();
    let job = request(&state, serde_json::json!({"kind":"bypass"}));
    let source = Arc::new(AtomicBool::new(false));
    let mut service = GraphService::spawn(None).unwrap();
    let job_cancel = service
        .submit(job.clone(), state.clone(), source.clone())
        .unwrap();
    let pipeline = completion(&mut service).result.unwrap().pipeline.unwrap();
    job_cancel.cancel();
    assert!(!source.load(Ordering::Acquire));
    drop(pipeline);
    retired(&service);
    assert!(!source.load(Ordering::Acquire));
    source.store(true, Ordering::Release);
    assert_eq!(
        service.submit(job, state, source).err().unwrap().code,
        ErrorCode::OperationCancelled
    );
    assert!(!service.resources().preparing_or_candidate);
    assert_eq!(service.resources().resident_bytes, 0);
}

#[test]
fn validates_real_src_roundtrip_with_per_stage_timing_and_no_runtime_owner() {
    let mut state = snapshot();
    state.software_gain_db = -80.0;
    let mut job = request(&state, serde_json::json!({"kind":"bypass"}));
    let graph = serde_json::from_value(graph_fixtures::src_roundtrip()).unwrap();
    Arc::make_mut(&mut job.intent).graph = Arc::new(graph);
    job.validate_only = true;
    let mut service = GraphService::spawn(None).unwrap();
    service
        .submit(job.clone(), state.clone(), active_source())
        .unwrap();
    let success = completion(&mut service).result.unwrap();
    assert!(success.pipeline.is_none());
    success.preview.validate().unwrap();
    let usage = success.preview.resources;
    assert_eq!(
        usage.pipeline_bytes,
        usage.kernel_audio_bytes
            + usage.host_input_bytes
            + usage.host_output_bytes
            + usage.native_period_bytes
            + usage.metadata_bytes
            + usage.stream_bridge_bytes
    );
    let preview = serde_json::to_value(success.preview).unwrap();
    assert_eq!(preview["timing_kind"], "streaming");
    assert_eq!(preview["stages"].as_array().unwrap().len(), 4);
    assert!(preview.get("latency_frames").is_none());
    assert!(preview.get("tail_frames").is_none());
    assert_eq!(service.resources().resident_bytes, 0);
    assert!(!service.resources().preparing_or_candidate);
    for case in 1..4 {
        let mut job = job.clone();
        let mut state = state.clone();
        let expected = match case {
            1 => {
                state.output.as_mut().unwrap().fidelity = FidelityPolicy::StrictPreservation;
                ErrorCode::FidelityPolicyConflict
            }
            2 => {
                Arc::make_mut(&mut job.intent).processing = GraphProcessingRequest::IntegerBypass;
                ErrorCode::FidelityPolicyConflict
            }
            _ => {
                state
                    .playback
                    .as_mut()
                    .unwrap()
                    .backend
                    .as_mut()
                    .unwrap()
                    .audio_storage_bytes = 256 << 20;
                ErrorCode::ResourceBudgetExceeded
            }
        };
        service.submit(job, state, active_source()).unwrap();
        assert_eq!(
            completion(&mut service).result.err().unwrap().code,
            expected,
            "case {case}"
        );
        assert_eq!(service.resources().resident_bytes, 0);
        assert!(!service.resources().preparing_or_candidate);
    }
}

#[test]
fn changing_activity_does_not_change_prepared_graph_identity_or_resources() {
    use earph_core::session::{OutputActivity, OutputActivityBoundary, OutputActivityScope};
    let mut previews = Vec::new();
    for count in [0, u64::MAX] {
        let mut state = snapshot();
        state.playback.as_mut().unwrap().output_activity = Some(OutputActivity {
            scope: OutputActivityScope::OutputInstanceAcrossNativeReopens,
            measurement_boundary: OutputActivityBoundary::CoreAudioSdkEntryCalls,
            origin_epochs: state.epochs,
            native_open_successes: count,
            sdk_write_attempts: count,
            accepted_frames: count,
            sdk_start_attempts: count,
            counter_overflow: count == u64::MAX,
            close_confirmed: count != 0,
        });
        let job = request(&state, serde_json::json!({"kind":"bypass"}));
        let mut service = GraphService::spawn(None).unwrap();
        service.submit(job, state, active_source()).unwrap();
        let prepared = completion(&mut service).result.unwrap();
        assert!(prepared.pipeline.is_some());
        previews.push(prepared.preview);
    }
    assert_eq!(previews[0], previews[1]);
}

#[test]
fn captured_backend_storage_is_charged_without_changing_graph_identity() {
    let mut previews = Vec::new();
    for bytes in [0, 1 << 20] {
        let mut state = snapshot();
        state
            .playback
            .as_mut()
            .unwrap()
            .backend
            .as_mut()
            .unwrap()
            .audio_storage_bytes = bytes;
        let mut job = request(&state, serde_json::json!({"kind":"bypass"}));
        job.validate_only = true;
        let mut service = GraphService::spawn(None).unwrap();
        service.submit(job, state, active_source()).unwrap();
        let preview = completion(&mut service).result.unwrap().preview;
        assert_eq!(preview.resources.backend_audio_bytes, bytes);
        previews.push(preview);
    }
    assert_eq!(previews[0].graph_hash, previews[1].graph_hash);
    assert_eq!(previews[0].kernel_hash, previews[1].kernel_hash);
    assert_eq!(
        previews[1].resources.aggregate_peak_bytes - previews[0].resources.aggregate_peak_bytes,
        1 << 20
    );
}

#[test]
fn coreaudio_admission_reserves_the_owned_ring_overlap_during_reopen() {
    let mut state = snapshot();
    state.output.as_mut().unwrap().backend = OutputBackend::CoreAudio;
    let backend = state.playback.as_mut().unwrap().backend.as_mut().unwrap();
    backend.backend = OutputBackend::CoreAudio;
    backend.audio_storage_bytes = 1024;
    let mut job = request(&state, serde_json::json!({"kind":"bypass"}));
    job.validate_only = true;
    let mut service = GraphService::spawn(None).unwrap();
    service.submit(job, state, active_source()).unwrap();
    let preview = completion(&mut service).result.unwrap().preview;
    assert_eq!(
        preview.resources.aggregate_peak_bytes,
        preview.resources.pipeline_bytes + 256 + 2048 + 4 * 1024 * 1024
    );
}

#[test]
fn prepared_src_owns_the_actual_source_token_and_retirement_does_not_cancel_it() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let mut state = snapshot();
    state.software_gain_db = -80.0;
    let mut job = request(&state, serde_json::json!({"kind":"bypass"}));
    Arc::make_mut(&mut job.intent).graph =
        Arc::new(serde_json::from_value(graph_fixtures::src_roundtrip()).unwrap());
    let source = Arc::new(AtomicBool::new(false));
    let mut service = GraphService::spawn(None).unwrap();
    service
        .submit(job.clone(), state.clone(), source.clone())
        .unwrap();
    let success = completion(&mut service).result.unwrap();
    let pipeline = success.pipeline.unwrap();
    let render = pipeline.stream_render().unwrap();
    let value = serde_json::to_value(pipeline.preview()).unwrap();
    assert_eq!(
        value["stages"][1]["kind"]["truncated_library_output_delay_frames"],
        render
            .stream()
            .src_metadata(1)
            .unwrap()
            .truncated_output_delay as u64
    );
    drop(pipeline);
    retired(&service);
    assert!(!source.load(Ordering::Acquire));
    assert_eq!(Arc::strong_count(&source), 1);
    service.submit(job, state, source.clone()).unwrap();
    let mut pipeline = completion(&mut service).result.unwrap().pipeline.unwrap();
    source.store(true, Ordering::Release);
    assert_eq!(
        pipeline
            .stream_render_mut()
            .unwrap()
            .pump(None)
            .unwrap_err()
            .code,
        ErrorCode::OperationCancelled
    );
    drop(pipeline);
    retired(&service);
}
#[test]
fn resolves_corrections_and_retires_frozen_filter_storage() {
    use std::{fs, os::unix::fs::PermissionsExt};
    let runtime = support::runtime();
    fs::create_dir(&runtime).unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    let source = runtime.join("correction.wav");
    let mut bytes = Vec::new();
    bytes.extend(b"RIFF");
    bytes.extend(84_u32.to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16_u32.to_le_bytes());
    bytes.extend(3_u16.to_le_bytes());
    bytes.extend(2_u16.to_le_bytes());
    bytes.extend(48000_u32.to_le_bytes());
    bytes.extend(768000_u32.to_le_bytes());
    bytes.extend(16_u16.to_le_bytes());
    bytes.extend(64_u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(48_u32.to_le_bytes());
    for v in [0.5_f64, 0.25, -0.25, 0.0, 0.0, 0.0] {
        bytes.extend(v.to_le_bytes());
    }
    fs::write(&source, bytes).unwrap();
    let reference = assets::import(&runtime, &source).unwrap();
    let mut service = GraphService::spawn(Some(runtime.clone())).unwrap();
    let state = snapshot();
    let job = request(
        &state,
        serde_json::json!({"kind":"convolution","asset":reference}),
    );
    service
        .submit(job.clone(), state.clone(), crate::active_source())
        .unwrap();
    let success = completion(&mut service).result.unwrap();
    let hash = success.preview.kernel_hash.clone();
    assert_eq!(success.preview.timing.latency_frames(), Some(256));
    assert_eq!(success.preview.timing.tail_frames(), Some(258));
    assert!(
        success.preview.channel_peak_bounds[0] >= 0.75
            && success.preview.channel_peak_bounds[1] >= 0.25
    );
    let mut pipeline = success.pipeline.unwrap();
    assert!(pipeline.resources().kernel_audio_bytes > 33000);
    fs::write(
        runtime
            .join("assets")
            .join(format!("{}.wav", reference.sha256.as_str())),
        b"changed stored object",
    )
    .unwrap();
    assert_eq!(pipeline.preview().kernel_hash, hash);
    let mut input = PcmBlock::prepare(
        state.playback.as_ref().unwrap().format,
        64,
        state.epochs,
        MemoryBudget::default(),
    )
    .unwrap();
    input.set_span(state.epochs, 0, 0, 1).unwrap();
    input.set_integer_sample(0, 8192).unwrap();
    input.set_integer_sample(1, -8192).unwrap();
    let render = pipeline.render_mut().unwrap();
    render.set_gain_target(Some(0.0), 0).unwrap();
    render.process(&input, 0, 1).unwrap();
    let mut actual = Vec::new();
    loop {
        let period = render.period();
        for scalar in 0..period.header().frame_count as usize * 2 {
            actual.push(period.integer_sample(scalar).unwrap());
        }
        if render.finish().unwrap().valid_frames == 0 {
            break;
        }
    }
    // The original stereo taps retain their 256-frame partition delay after disk corruption.
    let mut expected = vec![0; 259 * 2];
    expected[256 * 2] = 4096;
    expected[256 * 2 + 1] = -2048;
    expected[257 * 2] = -2048;
    assert_eq!(actual, expected);
    drop(pipeline);
    retired(&service);
    service.submit(job, state, crate::active_source()).unwrap();
    assert!(completion(&mut service).result.is_err());
    retired(&service);
}

#[test]
fn integer_routes_reject_filter_intent_before_resolving_correction_files() {
    let state = snapshot();
    let mut job = request(
        &state,
        serde_json::json!({"kind":"convolution","asset":{"sha256":"0".repeat(64),"sample_rate_hz":48000,"channels":"stereo","taps_per_channel":1,"purpose":"correction"}}),
    );
    let intent = Arc::make_mut(&mut job.intent);
    intent.processing = GraphProcessingRequest::IntegerBypass;
    for node in &mut Arc::make_mut(&mut intent.graph).nodes {
        node.format.precision = PcmRepresentation::Signed16;
    }
    let mut service = GraphService::spawn(None).unwrap();
    service.submit(job, state, crate::active_source()).unwrap();
    assert_eq!(
        completion(&mut service).result.err().unwrap().code,
        ErrorCode::FidelityPolicyConflict
    );
    retired(&service);
}
#[test]
fn prepares_and_renders_native_integer_audio_after_graph_and_shared_volume() {
    let state = snapshot();
    let mut service = GraphService::spawn(None).unwrap();
    service
        .submit(
            request(&state, serde_json::json!({"kind":"gain","gain_linear":2.0})),
            state.clone(),
            crate::active_source(),
        )
        .unwrap();
    let completed = completion(&mut service);
    assert_eq!(completed.request_id.as_str(), "job");
    assert_eq!(completed.binding.owner_id, state.owner_id.clone().unwrap());
    let mut success = completed.result.unwrap();
    assert_eq!(
        success.preview.source_format,
        state.playback.as_ref().unwrap().format
    );
    assert_eq!(success.preview.channel_peak_bounds, [2.0, 2.0]);
    assert!(success.preview.required_headroom_db >= 6.020599913279624);
    assert!(matches!(
        success.preview.transformations.first(),
        Some(SignalTransformation::IntegerToFloat64)
    ));
    assert!(matches!(
        success.preview.transformations.last(),
        Some(SignalTransformation::Quantization { .. })
    ));
    let mut pipeline = success.pipeline.take().unwrap().activate();
    assert_eq!(pipeline.binding(), &success.preview.binding);
    assert_eq!(pipeline.resources(), success.preview.resources);
    let mut input = PcmBlock::prepare(
        state.playback.as_ref().unwrap().format,
        64,
        state.epochs,
        MemoryBudget::default(),
    )
    .unwrap();
    input.set_span(state.epochs, 0, 0, 1).unwrap();
    input.set_integer_sample(0, 2000).unwrap();
    input.set_integer_sample(1, -2000).unwrap();
    assert_eq!(
        pipeline.render().unwrap().resources().audio_payload_bytes as u64,
        success.preview.resources.kernel_audio_bytes
            + success.preview.resources.host_input_bytes
            + success.preview.resources.host_output_bytes
            + success.preview.resources.native_period_bytes
    );
    let render = pipeline.render_mut().unwrap();
    render.set_gain_target(Some(-6.020599913279624), 0).unwrap();
    assert_eq!(render.process(&input, 0, 1).unwrap().saturation_count, 0);
    assert_eq!(render.period().integer_sample(0), Some(2000));
    assert_eq!(render.period().integer_sample(1), Some(-2000));
    pipeline.reset(state.epochs.next_stream().unwrap()).unwrap();
    assert_eq!(pipeline.preview().graph_hash, success.preview.graph_hash);
    drop(pipeline);
    retired(&service);
}
#[test]
fn validation_returns_preview_without_pipeline_or_resident_owner() {
    let state = snapshot();
    let mut job = request(&state, serde_json::json!({"kind":"gain","gain_linear":4.0}));
    job.validate_only = true;
    let mut service = GraphService::spawn(None).unwrap();
    service.submit(job, state, crate::active_source()).unwrap();
    let success = completion(&mut service).result.unwrap();
    assert!(success.pipeline.is_none());
    assert_eq!(success.preview.channel_peak_bounds, [4.0, 4.0]);
    retired(&service);
}
#[test]
fn rejects_insufficient_headroom_and_unbounded_delay_before_preparation() {
    for (effect, code) in [
        (
            serde_json::json!({"kind":"gain","gain_linear":8.0}),
            ErrorCode::SafetyPolicyBlocked,
        ),
        (
            serde_json::json!({"kind":"delay","delay_frames":33554432_u64}),
            ErrorCode::ResourceBudgetExceeded,
        ),
    ] {
        let state = snapshot();
        let mut service = GraphService::spawn(None).unwrap();
        service
            .submit(request(&state, effect), state, crate::active_source())
            .unwrap();
        assert_eq!(completion(&mut service).result.err().unwrap().code, code);
        retired(&service);
    }
}
#[test]
fn integer_bypass_uses_strict_compiler_and_no_float_render() {
    let mut state = snapshot();
    state.software_gain_db = 0.0;
    state.output.as_mut().unwrap().fidelity = FidelityPolicy::StrictPreservation;
    let mut job = request(&state, serde_json::json!({"kind":"bypass"}));
    let intent = Arc::make_mut(&mut job.intent);
    intent.processing = GraphProcessingRequest::IntegerBypass;
    for node in &mut Arc::make_mut(&mut intent.graph).nodes {
        node.format.precision = PcmRepresentation::Signed16;
    }
    let mut service = GraphService::spawn(None).unwrap();
    service
        .submit(job.clone(), state.clone(), crate::active_source())
        .unwrap();
    let mut pipeline = completion(&mut service).result.unwrap().pipeline.unwrap();
    assert_eq!(
        pipeline.preview().preservation,
        PreservationDecision::RetainInteger
    );
    assert!(pipeline.preview().transformations.is_empty());
    assert!(pipeline.render_mut().is_none());
    drop(pipeline);
    retired(&service);
    Arc::make_mut(&mut Arc::make_mut(&mut job.intent).graph).nodes[1].kind =
        earph_core::graph::NodeKind::Gain {
            gain_linear: 1.0.try_into().unwrap(),
        };
    service.submit(job, state, crate::active_source()).unwrap();
    assert_eq!(
        completion(&mut service).result.err().unwrap().code,
        ErrorCode::FidelityPolicyConflict
    );
    retired(&service);
}

#[test]
fn integer_meter_is_rejected_before_creating_a_silent_unobserved_pipeline() {
    let mut state = snapshot();
    state.software_gain_db = 0.0;
    state.output.as_mut().unwrap().fidelity = FidelityPolicy::StrictPreservation;
    let mut job = request(&state, serde_json::json!({"kind":"meter"}));
    let intent = Arc::make_mut(&mut job.intent);
    intent.processing = GraphProcessingRequest::IntegerBypass;
    for node in &mut Arc::make_mut(&mut intent.graph).nodes {
        node.format.precision = PcmRepresentation::Signed16;
    }
    let mut service = GraphService::spawn(None).unwrap();
    service.submit(job, state, crate::active_source()).unwrap();
    assert_eq!(
        completion(&mut service).result.err().unwrap().code,
        ErrorCode::CapabilityUnsupported
    );
    retired(&service);
    assert_eq!(service.resources().resident_bytes, 0);
}

#[test]
fn strict_integer_bypass_rejects_a_gain_ramp_before_preparation() {
    let mut state = snapshot();
    state.software_gain_db = 0.0;
    state.gain_ramp_frames = 32;
    state.output.as_mut().unwrap().fidelity = FidelityPolicy::StrictPreservation;
    let mut job = request(&state, serde_json::json!({"kind":"bypass"}));
    let intent = Arc::make_mut(&mut job.intent);
    intent.processing = GraphProcessingRequest::IntegerBypass;
    for node in &mut Arc::make_mut(&mut intent.graph).nodes {
        node.format.precision = PcmRepresentation::Signed16;
    }
    let mut service = GraphService::spawn(None).unwrap();
    service.submit(job, state, crate::active_source()).unwrap();
    assert_eq!(
        completion(&mut service).result.err().unwrap().code,
        ErrorCode::FidelityPolicyConflict
    );
    retired(&service);
}

#[test]
fn integer_bypass_preview_reports_captured_attenuation_and_unity_ramps() {
    for (gain_db, ramp_frames, volume_expected) in
        [(-12.0, 0, true), (0.0, 32, true), (0.0, 0, false)]
    {
        let mut state = snapshot();
        state.software_gain_db = gain_db;
        state.gain_ramp_frames = ramp_frames;
        let mut job = request(&state, serde_json::json!({"kind":"bypass"}));
        let intent = Arc::make_mut(&mut job.intent);
        intent.processing = GraphProcessingRequest::IntegerBypass;
        for node in &mut Arc::make_mut(&mut intent.graph).nodes {
            node.format.precision = PcmRepresentation::Signed16;
        }
        let mut service = GraphService::spawn(None).unwrap();
        service.submit(job, state, crate::active_source()).unwrap();
        let pipeline = completion(&mut service).result.unwrap().pipeline.unwrap();
        assert_eq!(
            pipeline.preview().transformations,
            if volume_expected {
                vec![SignalTransformation::SoftwareVolume]
            } else {
                vec![]
            }
        );
        assert!(pipeline.render().is_none());
        drop(pipeline);
        retired(&service);
    }
}
#[test]
fn activation_hash_distinguishes_gain_ramps_without_changing_the_kernel() {
    let mut previews = Vec::new();
    for ramp_frames in [0, 32] {
        let mut state = snapshot();
        state.software_gain_db = 0.0;
        state.gain_ramp_frames = ramp_frames;
        let mut job = request(&state, serde_json::json!({"kind":"bypass"}));
        let intent = Arc::make_mut(&mut job.intent);
        intent.processing = GraphProcessingRequest::IntegerBypass;
        for node in &mut Arc::make_mut(&mut intent.graph).nodes {
            node.format.precision = PcmRepresentation::Signed16;
        }
        let mut service = GraphService::spawn(None).unwrap();
        service.submit(job, state, crate::active_source()).unwrap();
        let success = completion(&mut service).result.unwrap();
        previews.push(success.preview.clone());
        drop(success);
        retired(&service);
    }
    assert_ne!(previews[0].transformations, previews[1].transformations);
    assert_eq!(previews[0].kernel_hash, previews[1].kernel_hash);
    assert_ne!(previews[0].graph_hash, previews[1].graph_hash);
}
#[test]
fn semantic_hash_excludes_binding_and_changes_for_safety_native_evidence() {
    fn hash(state: SessionSnapshot) -> String {
        let mut service = GraphService::spawn(None).unwrap();
        service
            .submit(
                request(&state, serde_json::json!({"kind":"bypass"})),
                state,
                crate::active_source(),
            )
            .unwrap();
        let success = completion(&mut service).result.unwrap();
        let hash = success.preview.graph_hash.clone();
        drop(success);
        retired(&service);
        hash
    }
    let state = snapshot();
    let first = hash(state.clone());
    let mut changed = state.clone();
    changed.revision += 10;
    changed.epochs.stream += 1;
    changed.device_epoch += 1;
    changed.monotonic_ns = 1;
    changed.session_id = Some(SessionId::new("different-session").unwrap());
    assert_eq!(first, hash(changed));
    let mut changed = state.clone();
    changed.software_gain_db = -18.0;
    assert_ne!(first, hash(changed));
    let mut changed = state;
    changed
        .playback
        .as_mut()
        .unwrap()
        .backend
        .as_mut()
        .unwrap()
        .measurement_boundary = "changed evidence";
    assert_ne!(first, hash(changed));
}
#[test]
fn candidate_gate_and_retirement_allow_only_one_next_candidate() {
    let state = snapshot();
    let job = request(&state, serde_json::json!({"kind":"bypass"}));
    let mut service = GraphService::spawn(None).unwrap();
    service
        .submit(job.clone(), state.clone(), crate::active_source())
        .unwrap();
    assert_eq!(
        service
            .submit(job.clone(), state.clone(), crate::active_source())
            .err()
            .unwrap()
            .code,
        ErrorCode::SourceBusy
    );
    let pipeline = completion(&mut service).result.unwrap().pipeline.unwrap();
    assert_eq!(
        service
            .submit(job.clone(), state.clone(), crate::active_source())
            .err()
            .unwrap()
            .code,
        ErrorCode::SourceBusy
    );
    let active = pipeline.activate();
    service.submit(job, state, crate::active_source()).unwrap();
    let candidate = completion(&mut service).result.unwrap().pipeline.unwrap();
    assert!(
        service.resources().resident_bytes
            >= active.resources().pipeline_bytes + candidate.resources().pipeline_bytes
    );
    drop(candidate);
    drop(active);
    retired(&service);
}
#[test]
fn dropping_service_before_pipeline_preserves_off_thread_retirement() {
    let state = snapshot();
    let mut service = GraphService::spawn(None).unwrap();
    service
        .submit(
            request(&state, serde_json::json!({"kind":"bypass"})),
            state,
            crate::active_source(),
        )
        .unwrap();
    let pipeline = completion(&mut service).result.unwrap().pipeline.unwrap();
    drop(service);
    let join = thread::spawn(move || drop(pipeline));
    join.join().unwrap();
}
#[test]
fn stale_capture_and_expired_job_do_not_allocate_pipeline() {
    let state = snapshot();
    let mut job = request(&state, serde_json::json!({"kind":"bypass"}));
    job.binding.expected_revision += 1;
    let mut service = GraphService::spawn(None).unwrap();
    service
        .submit(job, state.clone(), crate::active_source())
        .unwrap();
    assert_eq!(
        completion(&mut service).result.err().unwrap().code,
        ErrorCode::StaleSession
    );
    retired(&service);
    let mut expired = request(&state, serde_json::json!({"kind":"bypass"}));
    expired.deadline_monotonic_ns = state.monotonic_ns;
    service
        .submit(expired, state, crate::active_source())
        .unwrap();
    assert!(completion(&mut service).result.is_err());
    retired(&service);
}

#[test]
fn prepared_bounds_cover_matrix_sum_gain_and_compensation_without_assuming_cancellation() {
    let state = snapshot();
    let mut job = request(&state, serde_json::json!({"kind":"bypass"}));
    job.validate_only = true;
    let graph = Arc::make_mut(&mut Arc::make_mut(&mut job.intent).graph);
    let port = graph.nodes[0].format;
    let matrix = NodeId::new("matrix").unwrap();
    let gain = NodeId::new("gain").unwrap();
    graph.nodes[1].kind = earph_core::graph::NodeKind::Sum;
    graph.nodes.push(earph_core::graph::Node {
        id: matrix.clone(),
        format: port,
        kind: earph_core::graph::NodeKind::Matrix {
            input_channels: ChannelLayout::Stereo,
            coefficients: vec![-1.0, 2.0, 0.5, -0.25]
                .into_iter()
                .map(|value| value.try_into().unwrap())
                .collect(),
        },
    });
    graph.nodes.push(earph_core::graph::Node {
        id: gain.clone(),
        format: port,
        kind: earph_core::graph::NodeKind::Gain {
            gain_linear: 2.0.try_into().unwrap(),
        },
    });
    let delayed = NodeId::new("delayed").unwrap();
    graph.nodes.push(earph_core::graph::Node {
        id: delayed.clone(),
        format: port,
        kind: earph_core::graph::NodeKind::Delay { delay_frames: 3 },
    });
    graph.edges[0].from = delayed.clone();
    graph.edges.extend([
        earph_core::graph::Edge {
            from: NodeId::new("in").unwrap(),
            to: delayed,
            from_port: 0,
            to_port: 0,
        },
        earph_core::graph::Edge {
            from: NodeId::new("in").unwrap(),
            to: matrix.clone(),
            from_port: 0,
            to_port: 0,
        },
        earph_core::graph::Edge {
            from: matrix,
            to: gain.clone(),
            from_port: 0,
            to_port: 0,
        },
        earph_core::graph::Edge {
            from: gain,
            to: NodeId::new("effect").unwrap(),
            from_port: 0,
            to_port: 1,
        },
    ]);
    let mut service = GraphService::spawn(None).unwrap();
    service.submit(job, state, crate::active_source()).unwrap();
    let success = completion(&mut service).result.unwrap();
    assert_eq!(success.preview.timing.latency_frames(), Some(3));
    assert!(success.preview.transformations.iter().any(|entry| matches!(
        entry,
        SignalTransformation::Kernel {
            transformation: earph_core::graph::Transformation::Compensation {
                delay_frames: 3,
                ..
            }
        }
    )));
    assert!(success.preview.channel_peak_bounds[0] >= 7.0);
    assert!(success.preview.channel_peak_bounds[1] >= 2.5);
    assert!(success.preview.channel_peak_bounds[0] < 7.00000000001);
    retired(&service);
}

#[test]
fn rejects_nonfinite_conservative_bounds_and_rate_mismatch() {
    let state = snapshot();
    let mut job = request(
        &state,
        serde_json::json!({"kind":"matrix","input_channels":"stereo","coefficients":[f64::MAX,f64::MAX,0.0,1.0]}),
    );
    job.validate_only = true;
    let mut service = GraphService::spawn(None).unwrap();
    service
        .submit(job, state.clone(), crate::active_source())
        .unwrap();
    assert_eq!(
        completion(&mut service).result.err().unwrap().code,
        ErrorCode::GraphInvalid
    );
    retired(&service);
    let mut job = request(&state, serde_json::json!({"kind":"bypass"}));
    for node in &mut Arc::make_mut(&mut Arc::make_mut(&mut job.intent).graph).nodes {
        node.format.sample_rate_hz = 44100;
    }
    service.submit(job, state, crate::active_source()).unwrap();
    assert_eq!(
        completion(&mut service).result.err().unwrap().code,
        ErrorCode::FormatUnsupported
    );
    retired(&service);
}

#[test]
fn sdk_float32_evidence_does_not_change_integer_adapter_input() {
    let mut state = snapshot();
    state.output.as_mut().unwrap().backend = OutputBackend::CoreAudio;
    let backend = state.playback.as_mut().unwrap().backend.as_mut().unwrap();
    backend.backend = OutputBackend::CoreAudio;
    backend.client_format.representation =
        earph_core::session::ObservedSampleRepresentation::Float32;
    backend.client_format.bytes_per_frame = 8;
    let mut service = GraphService::spawn(None).unwrap();
    service
        .submit(
            request(&state, serde_json::json!({"kind":"bypass"})),
            state.clone(),
            crate::active_source(),
        )
        .unwrap();
    let success = completion(&mut service).result.unwrap();
    assert_eq!(
        success.preview.native_output_format,
        state.playback.as_ref().unwrap().format
    );
    assert_eq!(
        success.preview.backend,
        state.playback.as_ref().unwrap().backend
    );
    assert!(matches!(
        success.preview.transformations.last(),
        Some(SignalTransformation::BackendAdapterConversion {
            client_representation: earph_core::session::ObservedSampleRepresentation::Float32
        })
    ));
    drop(success);
    retired(&service);
}

#[test]
fn unconsumed_validation_completion_keeps_candidate_slot_reserved() {
    let state = snapshot();
    let mut job = request(&state, serde_json::json!({"kind":"bypass"}));
    job.validate_only = true;
    let mut service = GraphService::spawn(None).unwrap();
    service
        .submit(job.clone(), state.clone(), crate::active_source())
        .unwrap();
    graph::wait_for_validation_publication(&service);
    assert_eq!(
        service
            .submit(job, state, crate::active_source())
            .err()
            .unwrap()
            .code,
        ErrorCode::SourceBusy
    );
    assert!(completion(&mut service).result.unwrap().pipeline.is_none());
    retired(&service);
}

#[test]
fn two_owned_pipelines_prevent_third_candidate_even_after_activation() {
    let state = snapshot();
    let job = request(&state, serde_json::json!({"kind":"bypass"}));
    let mut service = GraphService::spawn(None).unwrap();
    service
        .submit(job.clone(), state.clone(), crate::active_source())
        .unwrap();
    let first = completion(&mut service)
        .result
        .unwrap()
        .pipeline
        .unwrap()
        .activate();
    service
        .submit(job.clone(), state.clone(), crate::active_source())
        .unwrap();
    let second = completion(&mut service)
        .result
        .unwrap()
        .pipeline
        .unwrap()
        .activate();
    assert_eq!(
        service
            .submit(job, state, crate::active_source())
            .err()
            .unwrap()
            .code,
        ErrorCode::SourceBusy
    );
    drop(first);
    drop(second);
    retired(&service);
}

#[test]
fn racing_cancellation_retires_every_late_success() {
    let state = snapshot();
    let mut service = GraphService::spawn(None).unwrap();
    let cancel = service
        .submit(
            request(&state, serde_json::json!({"kind":"bypass"})),
            state,
            crate::active_source(),
        )
        .unwrap();
    cancel.cancel();
    let result = completion(&mut service).result;
    if let Err(error) = &result {
        assert_eq!(error.code, ErrorCode::OperationCancelled);
    }
    drop(result);
    retired(&service);
}

#[test]
fn repeated_cross_thread_retirement_acknowledges_every_owner() {
    let state = snapshot();
    let mut service = GraphService::spawn(None).unwrap();
    for _ in 0..32 {
        service
            .submit(
                request(
                    &state,
                    serde_json::json!({"kind":"delay","delay_frames":128}),
                ),
                state.clone(),
                crate::active_source(),
            )
            .unwrap();
        let pipeline = completion(&mut service)
            .result
            .unwrap()
            .pipeline
            .unwrap()
            .activate();
        thread::spawn(move || drop(pipeline)).join().unwrap();
        retired(&service);
    }
}

#[test]
fn nonfinite_backend_evidence_cannot_collapse_into_a_json_null_hash() {
    let mut state = snapshot();
    state
        .playback
        .as_mut()
        .unwrap()
        .backend
        .as_mut()
        .unwrap()
        .client_format
        .sample_rate_hz = f64::NAN;
    let mut service = GraphService::spawn(None).unwrap();
    service
        .submit(
            request(&state, serde_json::json!({"kind":"bypass"})),
            state,
            crate::active_source(),
        )
        .unwrap();
    assert_eq!(
        completion(&mut service).result.err().unwrap().code,
        ErrorCode::FormatUnsupported
    );
    retired(&service);
}

#[test]
fn same_graph_hash_survives_existing_active_storage_and_accounts_route_period() {
    let state = snapshot();
    let job = request(&state, serde_json::json!({"kind":"bypass"}));
    let mut service = GraphService::spawn(None).unwrap();
    service
        .submit(job.clone(), state.clone(), crate::active_source())
        .unwrap();
    let first = completion(&mut service)
        .result
        .unwrap()
        .pipeline
        .unwrap()
        .activate();
    let route = state
        .playback
        .as_ref()
        .unwrap()
        .format
        .storage_bytes(64)
        .unwrap() as u64;
    assert_eq!(
        first.resources().aggregate_peak_bytes,
        first.resources().pipeline_bytes
            + first.resources().backend_audio_bytes
            + route
            + 4 * 1024 * 1024
    );
    service.submit(job, state, crate::active_source()).unwrap();
    let second = completion(&mut service).result.unwrap().pipeline.unwrap();
    assert_eq!(first.preview().graph_hash, second.preview().graph_hash);
    assert_eq!(
        second.resources().aggregate_peak_bytes,
        first.resources().pipeline_bytes
            + second.resources().pipeline_bytes
            + second.resources().backend_audio_bytes
            + route
            + 4 * 1024 * 1024
    );
    drop(first);
    drop(second);
    retired(&service);
}

#[test]
fn caller_vector_spare_capacity_is_admitted_before_compilation() {
    let state = snapshot();
    let mut job = request(&state, serde_json::json!({"kind":"bypass"}));
    Arc::make_mut(&mut Arc::make_mut(&mut job.intent).graph)
        .nodes
        .reserve_exact(129);
    let mut service = GraphService::spawn(None).unwrap();
    service.submit(job, state, crate::active_source()).unwrap();
    assert_eq!(
        completion(&mut service).result.err().unwrap().code,
        ErrorCode::ResourceBudgetExceeded
    );
    retired(&service);
}

#[test]
fn retained_id_spare_capacity_is_charged_using_actual_backing_storage() {
    let state = snapshot();
    let mut base = request(&state, serde_json::json!({"kind":"bypass"}));
    base.validate_only = true;
    let original_capacity = base.intent.graph.nodes[0].id.capacity_bytes();
    let mut reserved = base.clone();
    let mut identifier = String::with_capacity(8192);
    identifier.push_str("in");
    let extra = identifier.capacity() - original_capacity;
    Arc::make_mut(&mut Arc::make_mut(&mut reserved.intent).graph).nodes[0].id =
        NodeId::new(identifier).unwrap();
    let mut service = GraphService::spawn(None).unwrap();
    service
        .submit(base, state.clone(), crate::active_source())
        .unwrap();
    let base_preview = completion(&mut service).result.unwrap().preview;
    service
        .submit(reserved, state, crate::active_source())
        .unwrap();
    let reserved_preview = completion(&mut service).result.unwrap().preview;
    assert_eq!(
        reserved_preview.resources.metadata_bytes,
        base_preview.resources.metadata_bytes + extra as u64
    );
    retired(&service);
}
