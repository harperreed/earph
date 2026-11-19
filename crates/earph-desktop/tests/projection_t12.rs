// ABOUTME: Tests strict T12 views against actual Core-serialized producer types.
// ABOUTME: Distinguishes controlled serialization fixtures from real owner execution.
use earph_core::{
    EngineError, ErrorCode,
    control::{Command, CommandEnvelope, ControlLimits, QueryResult, Receipt, ReceiptStatus},
    graph_control::GraphDiagnostic,
    ids::{OwnerId, RequestId, ZoneId},
    profile_control::{PackageFilesystemState, ProfilePackageIssue, ProfilePackageReason},
    session::Session,
};
use earph_core::{
    audio::*, control::*, graph::*, graph_control::*, ids::*, profile_control::*,
    profile_packages::*, profiles::*, safety::*, session::*,
};
use earph_desktop::decode_reply;
use serde_json::Value;
use std::sync::Arc;

fn hash(byte: char) -> ProfileObjectHash {
    byte.to_string().repeat(64).try_into().unwrap()
}
fn intent() -> Arc<GraphIntent> {
    let port = serde_json::json!({"domain":"pcm","sample_rate_hz":48000,"channels":"stereo","precision":"signed16","max_quantum_frames":64});
    Arc::new(serde_json::from_value(serde_json::json!({"graph":{"schema_version":1,"nodes":[{"id":"in","kind":{"kind":"input"},"format":port},{"id":"out","kind":{"kind":"output"},"format":port}],"edges":[{"from":"in","to":"out","from_port":0,"to_port":0}],"output":"out"},"processing":{"kind":"integer_bypass"}})).unwrap())
}
fn profile() -> Arc<ProfileRecord> {
    Arc::new(ProfileRecord {
        profile: Arc::new(ProfileDocument {
            schema_version: 1,
            id: ProfileId::new("fixture-profile").unwrap(),
            revision: 1,
            name: "Controlled serialized intent".into(),
            graph_sha256: hash('b'),
            source_policy: ProfileSourcePolicy::LocalFile,
            fidelity: FidelityPolicy::CompatibleConversion,
            output: ProfileOutputSelector {
                backend: OutputBackend::Alsa,
                route: OutputRoute::System,
                device_id: DeviceId::new("configured-output").unwrap(),
                manual_setup: None,
            },
            volume: ProfileVolumeIntent {
                mode: VolumeMode::Software,
                software_cap_db: -20.0,
                hardware_cap_db: None,
            },
            latency: ProfileLatencyIntent {
                mode: ProfileLatencyMode::Balanced,
                max_resident_bytes: 1048576,
                max_preparation_bytes: 2097152,
                max_latency_frames: None,
            },
            corrections: vec![],
        }),
        profile_sha256: hash('a'),
        graph: intent(),
        corrections: Arc::new(vec![]),
        correction_preview: None,
    })
}
fn backend(identity: bool) -> BackendObservation {
    let format = PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Signed16).unwrap();
    BackendObservation {
        backend: OutputBackend::Alsa,
        device_identity_sha256: identity.then(|| hash('c')),
        audio_storage_bytes: 1024,
        client_format: format.into(),
        device_format: None,
        measurement_boundary: "controlled Core serializer; not a native device",
        preservation: PreservationState::Unverified,
    }
}
fn preview() -> Arc<GraphPreview> {
    let format = PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Signed16).unwrap();
    Arc::new(GraphPreview {
        profile_application: Some(Arc::new(ProfileApplicationBinding {
            profile: profile().reference(),
            device_identity_sha256: Some(hash('c')),
            volume_mode: VolumeMode::Software,
            fidelity: FidelityPolicy::CompatibleConversion,
            graph_sha256: hash('b'),
            source: SourceKind::LocalFile,
            effective_policy: VolumePolicy::new(-20.0, None).unwrap(),
            software_gain_db: -40.0,
            latency: profile().profile.latency.clone(),
        })),
        backend: Some(backend(true)),
        graph_hash: "d".repeat(64),
        kernel_hash: "e".repeat(64),
        implementation_version: "controlled serializer".into(),
        binding: GraphBinding {
            owner_id: OwnerId::new("projection-owner").unwrap(),
            session_id: SessionId::new("controlled-session").unwrap(),
            epochs: Epochs::default(),
            device_epoch: 0,
            expected_revision: 0,
        },
        source_format: format,
        native_output_format: format,
        processing_input_format: format,
        processing_output_format: format,
        quantum_frames: 64,
        timing: GraphTiming::SameRate {
            latency_frames: 0,
            tail_frames: 0,
            tail_policy: GraphTailPolicy::AuthoredFiniteSupport,
            terminal_output_bounds: vec![0.0, 0.0],
            node_tails: vec![],
        },
        preservation: PreservationDecision::RetainInteger,
        transformations: vec![SignalTransformation::SoftwareVolume],
        resources: GraphResidentUsage::default(),
        channel_peak_bounds: vec![1.0, 1.0],
        required_headroom_db: 0.0,
        transition: GraphTransition::MutedRebuffer,
    })
}
fn controlled_results() -> Vec<QueryResult> {
    let record = profile();
    let prepared = GraphPrepared {
        token: PreparedGraphId::new("controlled-token").unwrap(),
        expires_monotonic_ns: 30000000000,
        preview: preview(),
    };
    let manifest = PackageManifest {
        schema_version: 1,
        profile: record.reference(),
        objects: vec![
            PackageObject {
                kind: PackageObjectKind::Profile,
                sha256: hash('a'),
                bytes: 100,
            },
            PackageObject {
                kind: PackageObjectKind::Graph,
                sha256: hash('b'),
                bytes: 100,
            },
        ],
    };
    let manifest_bytes = serde_json::to_vec(&manifest).unwrap().len() as u64;
    let package = ProfilePackageResult {
        operation: ProfilePackageStage::Previewed,
        review_sources: manifest
            .objects
            .iter()
            .map(|object| PackageReviewSource {
                object: object.clone(),
                local_path: format!("/controlled-package/{}", object.file_name()),
            })
            .collect(),
        manifest,
        manifest_sha256: hash('f'),
        manifest_bytes,
        total_bytes: manifest_bytes + 200,
        measurement_review_required: false,
        content_review_required: true,
    };
    package.validate().unwrap();
    vec![
        QueryResult::ProfileSaved(record.clone()),
        QueryResult::Profile(record.clone()),
        QueryResult::Profiles(ProfilePage {
            entries: vec![ProfileSummary {
                id: record.profile.id.clone(),
                revision: 1,
                name: record.profile.name.clone(),
                profile_sha256: record.profile_sha256,
            }],
            total: 1,
            next_offset: None,
            operation: ProfileSaveInspection {
                stage: None,
                request_id: None,
                deadline_monotonic_ns: None,
            },
        }),
        QueryResult::ProfileResolution(ProfileResolution::SelectionRequired {
            reason: ProfileSelectionIssue::NoVerifiedMatch,
            candidates: vec![ProfileRevision {
                id: record.profile.id.clone(),
                revision: 1,
            }],
        }),
        QueryResult::ProfileBound(ProfileAssociation {
            backend: OutputBackend::CoreAudio,
            route: OutputRoute::System,
            device_identity_sha256: hash('c'),
            profile_id: record.profile.id.clone(),
        }),
        QueryResult::ProfilePackage(Arc::new(package)),
        QueryResult::FirAsset(earph_core::dsp_nodes::FirAssetReference {
            sha256: earph_core::dsp_nodes::FirSha256::new("a".repeat(64)).unwrap(),
            sample_rate_hz: 48000,
            channels: ChannelLayout::Stereo,
            taps_per_channel: 1,
            purpose: earph_core::dsp_nodes::FirPurpose::Correction,
        }),
        QueryResult::GraphPreview(preview()),
        QueryResult::GraphPrepared(prepared.clone()),
        QueryResult::GraphInspection(GraphInspection {
            service: GraphServiceObservation::default(),
            recovery_required: false,
            graph_epoch: 0,
            active: Some(preview()),
            configured: Some(intent()),
            pending_request_id: None,
            pending_stage: None,
            prepared: Some(prepared),
        }),
    ]
}

fn session() -> Session {
    Session::new_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("projection-owner").unwrap(),
        ControlLimits::default(),
    )
}
fn query(command: Command) -> Vec<u8> {
    let mut owner = session();
    let receipt = owner.submit(CommandEnvelope {
        schema_version: 1,
        request_id: RequestId::new("projection-query").unwrap(),
        zone_id: ZoneId::new("main").unwrap(),
        owner_id: None,
        session_id: None,
        expected_revision: None,
        payload: command,
    });
    serde_json::to_vec(&serde_json::json!({"receipt": receipt})).unwrap()
}
fn result(result: QueryResult, status: ReceiptStatus) -> Vec<u8> {
    let state = session().snapshot();
    let receipt = Receipt {
        request_id: RequestId::new("controlled-result").unwrap(),
        zone_id: state.zone_id,
        session_id: state.session_id,
        status,
        error: (status == ReceiptStatus::Failed).then_some(EngineError::new(
            ErrorCode::AssetInvalid,
            "controlled producer failure",
        )),
        revision: state.revision,
        epochs: state.epochs,
        activation_frame: None,
        result: Some(result),
    };
    serde_json::to_vec(&serde_json::json!({"receipt": receipt})).unwrap()
}
fn rejects_removed(bytes: &[u8], pointer: &str, field: &str) {
    let mut reply: Value = serde_json::from_slice(bytes).unwrap();
    reply
        .pointer_mut(pointer)
        .unwrap()
        .as_object_mut()
        .unwrap()
        .remove(field)
        .unwrap();
    let bytes = serde_json::to_vec(&reply).unwrap();
    let error = decode_reply(bytes.clone()).expect_err("missing required producer field must fail");
    assert_eq!(&*error.bytes, bytes);
}

#[test]
fn integrated_state_and_capabilities_decode_without_dropping_new_fields() {
    for command in [Command::State, Command::Capabilities] {
        let bytes = query(command);
        let decoded =
            decode_reply(bytes.clone()).expect("actual integrated Core reply must decode");
        assert_eq!(decoded.bytes(), bytes);
    }
}

#[test]
fn output_activity_capabilities_require_the_actual_scope_and_boundary() {
    let bytes = query(Command::Capabilities);
    decode_reply(bytes.clone()).expect("actual output activity capability must decode");
    rejects_removed(&bytes, "/receipt/result/value", "output_activity");
    for key in ["backends", "scope", "measurement_boundary"] {
        rejects_removed(&bytes, "/receipt/result/value/output_activity", key);
    }
    let original: Value = serde_json::from_slice(&bytes).unwrap();
    for key in ["scope", "measurement_boundary"] {
        let mut invalid = original.clone();
        invalid["receipt"]["result"]["value"]["output_activity"][key] = "invented".into();
        assert!(decode_reply(serde_json::to_vec(&invalid).unwrap()).is_err());
    }
    let mut unknown = original;
    unknown["receipt"]["result"]["value"]["output_activity"]["invented"] = true.into();
    assert!(decode_reply(serde_json::to_vec(&unknown).unwrap()).is_err());
}

#[test]
fn controlled_output_activity_keeps_u64_totals_and_requires_nullable_samples() {
    let activity = OutputActivity {
        scope: OutputActivityScope::OutputInstanceAcrossNativeReopens,
        measurement_boundary: OutputActivityBoundary::CoreAudioSdkEntryCalls,
        origin_epochs: Epochs {
            session: 7,
            stream: 9,
        },
        native_open_successes: u64::MAX,
        sdk_write_attempts: u64::MAX,
        accepted_frames: u64::MAX,
        sdk_start_attempts: u64::MAX,
        counter_overflow: true,
        close_confirmed: true,
    };
    for sample in [Some(activity), None] {
        let mut state = session().snapshot();
        state.playback = Some(PlaybackObservation {
            processing: None,
            format: PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Signed16)
                .unwrap(),
            submitted_frames: 0,
            consumed_frames: None,
            backend_delay_frames: None,
            consumption_basis: "controlled Core serializer; no native measurement",
            source_entry_id: QueueEntryId::new("activity-fixture").unwrap(),
            source_position_frames: 0,
            audible_entry_id: None,
            audible_source_position_frames: None,
            input_peak: None,
            output_peak: None,
            modified: false,
            backend: None,
            output_activity: sample,
        });
        let bytes = result(QueryResult::State(Box::new(state)), ReceiptStatus::Applied);
        let decoded = decode_reply(bytes.clone()).expect("actual activity sample must decode");
        assert_eq!(decoded.bytes(), bytes);
        rejects_removed(&bytes, "/receipt/result/value/playback", "output_activity");
        let earph_desktop::Reply::Receipt(receipt) = decoded else {
            panic!("actual State receipt");
        };
        let projected = receipt
            .state()
            .unwrap()
            .playback
            .as_ref()
            .unwrap()
            .output_activity
            .as_ref();
        if sample.is_none() {
            assert!(projected.is_none());
            continue;
        }
        let projected = projected.unwrap();
        assert_eq!(projected.origin_epochs, activity.origin_epochs);
        assert_eq!(projected.native_open_successes, u64::MAX);
        assert_eq!(projected.sdk_write_attempts, u64::MAX);
        assert_eq!(projected.accepted_frames, u64::MAX);
        assert_eq!(projected.sdk_start_attempts, u64::MAX);
        assert!(projected.counter_overflow && projected.close_confirmed);
        for key in [
            "scope",
            "measurement_boundary",
            "origin_epochs",
            "native_open_successes",
            "sdk_write_attempts",
            "accepted_frames",
            "sdk_start_attempts",
            "counter_overflow",
            "close_confirmed",
        ] {
            rejects_removed(
                &bytes,
                "/receipt/result/value/playback/output_activity",
                key,
            );
        }
        let original: Value = serde_json::from_slice(&bytes).unwrap();
        for pointer in [
            "/receipt/result/value/playback/output_activity",
            "/receipt/result/value/playback/output_activity/origin_epochs",
        ] {
            let mut invalid = original.clone();
            invalid
                .pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("invented".into(), true.into());
            assert!(decode_reply(serde_json::to_vec(&invalid).unwrap()).is_err());
        }
        for key in ["scope", "measurement_boundary"] {
            let mut invalid = original.clone();
            invalid["receipt"]["result"]["value"]["playback"]["output_activity"][key] =
                "invented".into();
            assert!(decode_reply(serde_json::to_vec(&invalid).unwrap()).is_err());
        }
    }
}

#[test]
fn new_state_and_capability_keys_are_required_even_when_nullable() {
    let state = query(Command::State);
    for field in ["local_volume_policy", "configured_profile"] {
        rejects_removed(&state, "/receipt/result/value", field);
    }
    let caps = query(Command::Capabilities);
    for field in ["exact_association_backends", "application", "package"] {
        rejects_removed(&caps, "/receipt/result/value/profiles", field);
    }
    for field in [
        "volume_modes",
        "fixed_requires_current_arming",
        "fixed_requires_unity_gain",
        "fixed_requires_zero_gain_ramp",
        "requires_current_output",
        "requires_output_consent",
        "requires_matching_fidelity",
        "live_selected_profile_update",
        "finite_aggregate_src_latency",
    ] {
        rejects_removed(&caps, "/receipt/result/value/profiles/application", field);
    }
}

#[test]
fn graph_diagnostic_requires_nullable_target_keys_and_rejects_unknowns() {
    let bytes = result(
        QueryResult::GraphDiagnostic(GraphDiagnostic::from(EngineError::new(
            ErrorCode::GraphInvalid,
            "controlled diagnostic",
        ))),
        ReceiptStatus::Failed,
    );
    decode_reply(bytes.clone()).unwrap();
    for field in ["node", "edge"] {
        rejects_removed(&bytes, "/receipt/result/value", field);
    }
    let mut value: Value = serde_json::from_slice(&bytes).unwrap();
    value["receipt"]["result"]["value"]["invented"] = true.into();
    assert!(decode_reply(serde_json::to_vec(&value).unwrap()).is_err());
}

#[test]
fn package_failure_preserves_its_typed_issue_and_explicit_null_object() {
    let bytes = result(
        QueryResult::ProfilePackageIssue(ProfilePackageIssue {
            reason: ProfilePackageReason::IoFailure,
            object: None,
            filesystem_state: PackageFilesystemState::InspectRequired,
        }),
        ReceiptStatus::Failed,
    );
    let decoded =
        decode_reply(bytes.clone()).expect("failed package result is still an actual result");
    assert_eq!(decoded.bytes(), bytes);
    rejects_removed(&bytes, "/receipt/result/value", "object");
}

#[test]
fn actual_graph_inspection_requires_each_nullable_observation() {
    let bytes = query(Command::GraphInspect {});
    decode_reply(bytes.clone()).unwrap();
    for field in [
        "active",
        "configured",
        "pending_request_id",
        "pending_stage",
        "prepared",
    ] {
        rejects_removed(&bytes, "/receipt/result/value", field);
    }
}

#[test]
fn all_controlled_core_result_branches_decode_and_keep_exact_bytes() {
    for produced in controlled_results() {
        let bytes = result(produced, ReceiptStatus::Applied);
        let decoded = decode_reply(bytes.clone()).expect("actual Core result type must project");
        assert_eq!(decoded.bytes(), bytes);
    }
}

fn object_paths(value: &Value, path: String, output: &mut Vec<String>) {
    match value {
        Value::Object(fields) => {
            output.push(path.clone());
            for (key, value) in fields {
                object_paths(value, format!("{path}/{key}"), output);
            }
        }
        Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                object_paths(value, format!("{path}/{index}"), output);
            }
        }
        _ => {}
    }
}

#[test]
fn controlled_profile_graph_and_package_objects_reject_unknown_fields_and_missing_nulls() {
    for produced in controlled_results() {
        let bytes = result(produced, ReceiptStatus::Applied);
        let original: Value = serde_json::from_slice(&bytes).unwrap();
        let mut paths = vec![];
        object_paths(&original, String::new(), &mut paths);
        for path in paths {
            let object = original.pointer(&path).unwrap().as_object().unwrap();
            for (field, value) in object {
                if value.is_null() {
                    rejects_removed(&bytes, &path, field);
                }
            }
            let mut corrupt = original.clone();
            corrupt
                .pointer_mut(&path)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("invented".into(), true.into());
            assert!(
                decode_reply(serde_json::to_vec(&corrupt).unwrap()).is_err(),
                "unknown producer field admitted at {path}"
            );
        }
    }
}

#[test]
fn controlled_playback_observations_preserve_nulls_and_omitted_processing_fields() {
    let mut state = session().snapshot();
    state.configured_profile = Some(profile().reference());
    state.local_volume_policy = VolumePolicy::new(-10.0, None).unwrap();
    state.volume_policy = VolumePolicy::new(-20.0, None).unwrap();
    state.playback = Some(PlaybackObservation {
        processing: Some(PlaybackProcessingObservation {
            resident_bytes: 1024,
            audio_payload_bytes: None,
            failure_node: None,
            graph_hash: "d".repeat(64),
            timing: PlaybackTiming::SameRate {
                latency_frames: 0,
                tail_frames: 0,
            },
            streaming: None,
            source_boundary: None,
            saturation_count: 0,
            graph_peak: None,
            prequantization_peak: None,
        }),
        format: PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Signed16).unwrap(),
        submitted_frames: 64,
        consumed_frames: None,
        backend_delay_frames: None,
        consumption_basis: "controlled Core serializer",
        source_entry_id: QueueEntryId::new("entry").unwrap(),
        source_position_frames: 64,
        audible_entry_id: None,
        audible_source_position_frames: None,
        input_peak: None,
        output_peak: None,
        modified: false,
        backend: Some(backend(false)),
        output_activity: None,
    });
    let bytes = result(QueryResult::State(Box::new(state)), ReceiptStatus::Applied);
    decode_reply(bytes.clone())
        .expect("actual Core nullable/omitted playback contract must decode");
    for field in [
        "processing",
        "consumed_frames",
        "backend_delay_frames",
        "audible_entry_id",
        "audible_source_position_frames",
        "input_peak",
        "output_peak",
        "backend",
    ] {
        rejects_removed(&bytes, "/receipt/result/value/playback", field);
    }
    for field in [
        "audio_payload_bytes",
        "failure_node",
        "graph_peak",
        "prequantization_peak",
    ] {
        rejects_removed(&bytes, "/receipt/result/value/playback/processing", field);
    }
    for field in ["device_identity_sha256", "device_format"] {
        rejects_removed(&bytes, "/receipt/result/value/playback/backend", field);
    }
}

#[test]
fn non_null_correction_preview_consumes_the_actual_normalization_enum() {
    let mut record = (*profile()).clone();
    Arc::make_mut(&mut record.profile)
        .corrections
        .push(ProfileCorrectionReference {
            kind: ProfileCorrectionKind::TextReport,
            object_sha256: hash('f'),
        });
    record.corrections = Arc::new(vec![ProfileCorrectionObject {
        kind: ProfileCorrectionKind::TextReport,
        object_sha256: hash('f'),
        document_json: Arc::from("{}"),
    }]);
    record.correction_preview = Some(Arc::new(ProfileCorrectionPreview {
        context: ProfileCorrectionContext {
            sample_rate_hz: 48000,
            channels: ChannelLayout::Stereo,
            quantum_frames: 64,
        },
        preamp_db: FiniteGraphScalar::new(-6.0).unwrap(),
        channel_peak_bounds: vec![FiniteGraphScalar::new(1.0).unwrap(); 2],
        required_headroom_db: FiniteGraphScalar::new(0.0).unwrap(),
        normalization: CorrectionNormalization::Preserved,
    }));
    let bytes = result(
        QueryResult::Profile(Arc::new(record)),
        ReceiptStatus::Applied,
    );
    assert!(
        decode_reply(bytes).is_ok(),
        "actual Core correction normalization failed projection"
    );
}

#[test]
fn streaming_graph_producer_fields_require_explicit_nullable_src_observations() {
    let mut preview = (*preview()).clone();
    let format = preview.native_output_format;
    preview.profile_application = None;
    preview.backend = None;
    preview.timing = GraphTiming::Streaming {
        stages: Arc::from(vec![CompiledStreamStage {
            original_nodes: vec![NodeId::new("src").unwrap()],
            input_format: format,
            output_format: format,
            input_peak_bounds: vec![1.0; 2],
            output_peak_bounds: vec![1.0; 2],
            kind: CompiledStreamStageKind::Src {
                parameters: earph_core::dsp_nodes::SrcParameters {
                    input_rate: 48000,
                    output_rate: 48000,
                    channels: ChannelLayout::Stereo,
                    preset: earph_core::dsp_nodes::SrcPreset::Responsive,
                    chunk_frames: 64,
                    filter_length: 64,
                    phases: 32,
                    explicit_pre_scaling_cutoff: None,
                    pre_scaling_cutoff: 0.95,
                    drain_zero_calls: 1,
                    max_output_frames: 128,
                    guard_time_numerator: 1,
                    library_max_relative_ratio: 1.01,
                    resources: earph_core::dsp_nodes::SrcResources {
                        resident_bytes: 1024,
                        preparation_peak_bytes: 2048,
                    },
                },
                coefficient_fingerprint: [1; 32],
                parameter_fingerprint: [2; 32],
                guard_time_denominator: 1,
                maximum_adapter_finish_output_frames: 64,
                truncated_library_output_delay_frames: None,
            },
        }]),
    };
    let bytes = result(
        QueryResult::GraphPreview(Arc::new(preview)),
        ReceiptStatus::Applied,
    );
    assert!(
        decode_reply(bytes.clone()).is_ok(),
        "actual Core streaming fields failed projection"
    );
    rejects_removed(&bytes, "/receipt/result/value", "profile_application");
    rejects_removed(&bytes, "/receipt/result/value", "backend");
    rejects_removed(
        &bytes,
        "/receipt/result/value/stages/0/kind",
        "truncated_library_output_delay_frames",
    );
    rejects_removed(
        &bytes,
        "/receipt/result/value/stages/0/kind/parameters",
        "explicit_pre_scaling_cutoff",
    );
    let mut missing: Value = serde_json::from_slice(&bytes).unwrap();
    missing["receipt"]["result"]["value"]["stages"][0]["kind"]["parameters"]["invented"] =
        true.into();
    assert!(decode_reply(serde_json::to_vec(&missing).unwrap()).is_err());
}

#[test]
fn controlled_streaming_runtime_and_observed_device_format_remain_separate() {
    let mut state = session().snapshot();
    let mut observed = backend(false);
    observed.device_format = Some(ObservedStreamFormat {
        sample_rate_hz: 96000.0,
        channels: 2,
        representation: ObservedSampleRepresentation::Float32,
        bytes_per_frame: 8,
        interleaved: true,
    });
    let format = PcmFormat::new(48000, ChannelLayout::Stereo, PcmRepresentation::Signed16).unwrap();
    let stages: Arc<[CompiledStreamStage]> = Arc::from(vec![CompiledStreamStage {
        original_nodes: vec![NodeId::new("island").unwrap()],
        input_format: format,
        output_format: format,
        input_peak_bounds: vec![1.0; 2],
        output_peak_bounds: vec![1.0; 2],
        kind: CompiledStreamStageKind::Island {
            kernel_hash: "e".repeat(64),
            local_latency_frames: 8,
            emitted_tail_frames: 16,
            bounded_iir_truncation: true,
            terminal_output_bounds: vec![0.0; 2],
        },
    }]);
    state.playback = Some(PlaybackObservation {
        processing: Some(PlaybackProcessingObservation {
            resident_bytes: 1024,
            audio_payload_bytes: Some(512),
            failure_node: None,
            graph_hash: "d".repeat(64),
            timing: PlaybackTiming::Streaming { stages },
            streaming: Some(StreamingRuntimeObservation {
                source_epochs: Epochs::default(),
                output_epochs: Epochs {
                    session: 0,
                    stream: 1,
                },
                source_accepted_frames: 64,
                generated_output_frames: 128,
                submission_frontier_frames: 96,
                backend_origin_frames: 0,
                retained_start_frames: 32,
            }),
            source_boundary: Some(Arc::new(StreamingReplacementBoundary {
                source_entry_id: QueueEntryId::new("previous-entry").unwrap(),
                source_position_frames: 16,
                source_epochs: Epochs::default(),
                discarded_output_frames: 8,
            })),
            saturation_count: 0,
            graph_peak: Some(0.0),
            prequantization_peak: Some(0.0),
        }),
        format,
        submitted_frames: 96,
        consumed_frames: Some(32),
        backend_delay_frames: Some(64),
        consumption_basis: "controlled serializer",
        source_entry_id: QueueEntryId::new("current-entry").unwrap(),
        source_position_frames: 64,
        audible_entry_id: None,
        audible_source_position_frames: None,
        input_peak: Some(0.0),
        output_peak: Some(0.0),
        modified: true,
        backend: Some(observed),
        output_activity: None,
    });
    let bytes = result(QueryResult::State(Box::new(state)), ReceiptStatus::Applied);
    let earph_desktop::Reply::Receipt(receipt) = decode_reply(bytes.clone()).unwrap() else {
        panic!("typed State");
    };
    let playback = receipt.state().unwrap().playback.as_ref().unwrap();
    let backend = playback.backend.as_ref().unwrap();
    assert_eq!(backend.client_format.sample_rate_hz, 48000.0);
    assert_eq!(
        backend.device_format.as_ref().unwrap().sample_rate_hz,
        96000.0
    );
    assert_eq!(
        backend.device_format.as_ref().unwrap().representation,
        earph_desktop::ObservedSampleRepresentationView::Float32
    );
    let processing = playback.processing.as_ref().unwrap();
    assert_eq!(
        processing
            .streaming
            .as_ref()
            .unwrap()
            .source_accepted_frames,
        64
    );
    assert_eq!(
        processing
            .streaming
            .as_ref()
            .unwrap()
            .generated_output_frames,
        128
    );
    assert_eq!(
        processing
            .source_boundary
            .as_ref()
            .unwrap()
            .discarded_output_frames,
        8
    );
    let mut value: Value = serde_json::from_slice(&bytes).unwrap();
    value["receipt"]["result"]["value"]["playback"]["processing"]["streaming"] = Value::Null;
    assert!(
        decode_reply(serde_json::to_vec(&value).unwrap()).is_err(),
        "explicit null must not stand in for an omitted producer observation"
    );
}
