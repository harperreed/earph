// ABOUTME: Projects actual Core discovery receipts and corrupts their wire structure.
// ABOUTME: Preserves incomplete observations, explicit unknowns, and exact integer values.
use earph_core::{
    ErrorCode,
    control::{Command, CommandEnvelope, ControlLimits, OutputBackend, ReceiptStatus},
    devices::*,
    ids::{OwnerId, RequestId, ZoneId},
    session::Session,
};
use earph_desktop::decode_reply;
use serde_json::{Value, json};

fn query(command: Command) -> CommandEnvelope {
    CommandEnvelope {
        schema_version: 1,
        request_id: RequestId::new("discovery-view").unwrap(),
        zone_id: ZoneId::new("main").unwrap(),
        owner_id: None,
        session_id: None,
        expected_revision: None,
        payload: command,
    }
}
fn session(backend: OutputBackend) -> Session {
    let mut session = Session::new_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("discovery-view-owner").unwrap(),
        ControlLimits::default(),
    );
    session.enable_device_discovery(backend);
    session
}
fn produced(backend: OutputBackend, observation: DeviceObservation) -> Vec<u8> {
    let mut session = session(backend);
    let before = session.snapshot();
    let command = query(match &observation {
        DeviceObservation::Inspection { endpoint, .. } => Command::DevicesInspect {
            backend,
            endpoint: endpoint.clone(),
        },
        _ => Command::DevicesList { backend },
    });
    let accepted = session.submit(command.clone());
    assert_eq!(accepted.status, ReceiptStatus::Accepted);
    let job = session.device_query_job().unwrap();
    assert_eq!(session.submit(command.clone()), accepted);
    session
        .complete_device_query(&job, u64::MAX, observation)
        .unwrap();
    let terminal = session.submit(command.clone());
    assert_eq!(session.submit(command), terminal);
    assert_eq!(session.snapshot(), before);
    serde_json::to_vec(&json!({"receipt": terminal})).unwrap()
}
fn failure() -> DeviceQueryError {
    DeviceQueryError {
        code: ErrorCode::DeviceUnavailable,
        message: "controlled property unavailable".into(),
        native_status: None,
        native_object_id: Some(u32::MAX),
    }
}
fn summary(backend: OutputBackend, endpoint: String) -> DeviceSummary {
    DeviceSummary::advertised(
        backend,
        endpoint,
        Some("Duplicate label".into()),
        None,
        None,
    )
}
fn format() -> DeviceFormat {
    DeviceFormat {
        sample_rate_hz: 48000.0,
        format_id: u32::MAX,
        format_flags: u32::MAX,
        bytes_per_packet: 8,
        frames_per_packet: 1,
        bytes_per_frame: 8,
        channels_per_frame: 2,
        bits_per_channel: 32,
    }
}
fn range() -> DeviceValueRange {
    DeviceValueRange {
        minimum: 44100.0,
        maximum: 192000.0,
    }
}
fn documents() -> Vec<Vec<u8>> {
    let backend = OutputBackend::CoreAudio;
    let mut session = session(backend);
    let caps = session.submit(query(Command::Capabilities));
    let inspection = DeviceObservation::Inspection {
        endpoint: "owned-uid".into(),
        advertisement: Some(summary(backend, "owned-uid".into())),
        current_properties: Some(Box::new(DeviceProperties {
            object_id: u32::MAX,
            uid: "owned-uid".into(),
            name: "Controlled properties, no native observation".into(),
            alive: true,
            nominal_rate_hz: 48000.0,
            nominal_rate_settable: false,
            buffer_frames: 128,
            buffer_frame_range: range(),
            hog_owner_pid: None,
            hog_mode_settable: None,
            output_streams: vec![DeviceStream {
                object_id: u32::MAX,
                physical: format(),
                virtual_format: format(),
                physical_settable: false,
                virtual_settable: true,
                available_physical: vec![DeviceRangedFormat {
                    format: format(),
                    sample_rates: range(),
                }],
                available_virtual: vec![DeviceRangedFormat {
                    format: format(),
                    sample_rates: range(),
                }],
            }],
        })),
        complete: false,
        failures: vec![failure()],
    };
    let mut documents = vec![
        serde_json::to_vec(&json!({"receipt": caps})).unwrap(),
        produced(backend, inspection),
        produced(
            backend,
            DeviceObservation::List {
                devices: vec![
                    summary(backend, "a".repeat(257)),
                    summary(backend, "owned-uid".into()),
                    summary(backend, "second-uid".into()),
                ],
                complete: false,
                failures: vec![failure()],
            },
        ),
        produced(
            OutputBackend::Alsa,
            DeviceObservation::Inspection {
                endpoint: "unadvertised".into(),
                advertisement: None,
                current_properties: None,
                complete: true,
                failures: vec![],
            },
        ),
        produced(
            OutputBackend::Alsa,
            DeviceObservation::List {
                devices: vec![],
                complete: true,
                failures: vec![],
            },
        ),
    ];
    for code in [ErrorCode::DeviceUnavailable, ErrorCode::OperationCancelled] {
        let mut error = failure();
        error.code = code;
        error.native_status = Some(i32::MIN);
        error.native_object_id = None;
        documents.push(produced(
            backend,
            DeviceObservation::Failed {
                error,
                cleanup_confirmed: true,
            },
        ));
    }
    documents
}

#[test]
fn actual_core_discovery_variants_preserve_exact_reply_bytes() {
    for bytes in documents() {
        let reply =
            decode_reply(bytes.clone()).expect("actual Core discovery receipt must project");
        assert_eq!(reply.bytes(), bytes);
    }
}

#[test]
fn typed_discovery_views_preserve_unknown_partial_failed_and_empty_observations() {
    use earph_desktop::{DeviceObservationView, Reply, ReplyStatus, ResultView};
    let documents = documents();
    for (index, bytes) in documents.iter().enumerate().skip(1) {
        let Reply::Receipt(receipt) = decode_reply(bytes.clone()).unwrap() else {
            panic!("receipt");
        };
        let Some(ResultView::Devices(result)) = receipt.result else {
            panic!("typed devices");
        };
        assert_eq!(result.owner_id.as_str(), "discovery-view-owner");
        assert_eq!(result.completed_monotonic_ns, u64::MAX);
        match (&result.observation, index) {
            (
                DeviceObservationView::Inspection {
                    advertisement: Some(advertisement),
                    current_properties: Some(properties),
                    complete,
                    failures,
                    ..
                },
                1,
            ) => {
                assert!(!complete);
                assert_eq!(failures[0].native_object_id, Some(u32::MAX));
                assert_eq!(failures[0].native_status, None);
                assert_eq!(properties.object_id, u32::MAX);
                assert_eq!(properties.hog_owner_pid, None);
                assert_eq!(properties.hog_mode_settable, None);
                assert_eq!(properties.output_streams[0].physical.format_flags, u32::MAX);
                assert_eq!(advertisement.description, None);
            }
            (
                DeviceObservationView::List {
                    devices,
                    complete,
                    failures,
                },
                2,
            ) => {
                assert!(!complete && !failures.is_empty());
                assert_eq!(devices[0].endpoint, "a".repeat(257));
                assert_eq!(devices[0].selection, None);
                assert_eq!(devices[0].selection_error, Some(ErrorCode::AssetInvalid));
                assert_eq!(devices[1].display_name, devices[2].display_name);
                assert_ne!(devices[1].endpoint, devices[2].endpoint);
            }
            (
                DeviceObservationView::Inspection {
                    advertisement,
                    current_properties,
                    complete,
                    failures,
                    ..
                },
                3,
            ) => {
                assert!(advertisement.is_none() && current_properties.is_none());
                assert!(*complete && failures.is_empty());
            }
            (
                DeviceObservationView::List {
                    devices,
                    complete,
                    failures,
                },
                4,
            ) => {
                assert!(devices.is_empty() && *complete && failures.is_empty());
                assert_eq!(receipt.status, ReplyStatus::Applied);
            }
            (
                DeviceObservationView::Failed {
                    error,
                    cleanup_confirmed,
                },
                5 | 6,
            ) => {
                assert!(*cleanup_confirmed);
                assert_eq!(error.native_status, Some(i32::MIN));
                assert_eq!(error.native_object_id, None);
                assert_eq!(
                    receipt.status,
                    if index == 5 {
                        ReplyStatus::Failed
                    } else {
                        ReplyStatus::Cancelled
                    }
                );
            }
            observation => panic!("wrong projected observation: {observation:?}"),
        }
    }
}

#[test]
fn discovery_unsigned_counters_keep_zero_and_u64_maximum_without_float_rounding() {
    use earph_desktop::{Reply, ResultView};
    let original: Value = serde_json::from_slice(&documents()[0]).unwrap();
    for value in [0, u64::MAX] {
        let mut caps = original.clone();
        for field in [
            "max_concurrent_queries",
            "max_entries",
            "max_text_bytes",
            "max_result_bytes",
            "max_retained_result_bytes",
            "query_timeout_ms",
        ] {
            caps["receipt"]["result"]["value"]["devices"][field] = json!(value);
        }
        let Reply::Receipt(receipt) = decode_reply(serde_json::to_vec(&caps).unwrap()).unwrap()
        else {
            panic!("receipt");
        };
        let Some(ResultView::Capabilities(caps)) = receipt.result else {
            panic!("capabilities");
        };
        assert_eq!(
            [
                caps.devices.max_concurrent_queries,
                caps.devices.max_entries,
                caps.devices.max_text_bytes,
                caps.devices.max_result_bytes,
                caps.devices.max_retained_result_bytes,
                caps.devices.query_timeout_ms
            ],
            [value; 6]
        );
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
fn rejects(value: &Value, context: &str) {
    let bytes = serde_json::to_vec(value).unwrap();
    let error = decode_reply(bytes.clone()).expect_err(context);
    assert_eq!(&*error.bytes, bytes);
}

#[test]
fn every_discovery_object_requires_all_fields_and_rejects_unknown_fields() {
    for bytes in documents() {
        decode_reply(bytes.clone()).expect("valid producer baseline");
        let original: Value = serde_json::from_slice(&bytes).unwrap();
        let mut paths = vec![];
        object_paths(
            &original["receipt"]["result"],
            "/receipt/result".into(),
            &mut paths,
        );
        for path in paths {
            for field in original.pointer(&path).unwrap().as_object().unwrap().keys() {
                let mut corrupt = original.clone();
                corrupt
                    .pointer_mut(&path)
                    .unwrap()
                    .as_object_mut()
                    .unwrap()
                    .remove(field);
                rejects(&corrupt, &format!("missing required field {path}/{field}"));
            }
            let mut corrupt = original.clone();
            corrupt
                .pointer_mut(&path)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("invented".into(), true.into());
            rejects(&corrupt, &format!("unknown field at {path}"));
        }
    }
}

#[test]
fn discovery_rejects_unknown_enums_and_malformed_scalar_types() {
    let documents = documents();
    let inspection: Value = serde_json::from_slice(&documents[1]).unwrap();
    for (path, replacement) in [
        ("backend", json!("invented")),
        ("read_boundary", json!("invented")),
        ("observation/kind", json!("invented")),
        (
            "observation/advertisement/selection/backend",
            json!("invented"),
        ),
        ("observation/failures/0/code", json!("INVENTED")),
        ("completed_monotonic_ns", json!("18446744073709551615")),
        ("completed_monotonic_ns", json!(-1)),
        ("completed_monotonic_ns", json!(1.5)),
        ("completed_monotonic_ns", json!(18446744073709551616.0)),
        ("observation/complete", json!("false")),
        ("observation/current_properties/object_id", json!(-1)),
        (
            "observation/current_properties/object_id",
            json!(4294967296u64),
        ),
        (
            "observation/current_properties/hog_owner_pid",
            json!(2147483648u64),
        ),
        ("observation/current_properties/hog_mode_settable", json!(0)),
        (
            "observation/current_properties/nominal_rate_hz",
            json!("48000"),
        ),
        (
            "observation/current_properties/output_streams/0/physical/format_flags",
            json!(1.5),
        ),
        (
            "observation/failures/0/native_status",
            json!(-2147483649i64),
        ),
        ("observation/advertisement/description", json!(1)),
    ] {
        let mut corrupt = inspection.clone();
        *corrupt
            .pointer_mut(&format!("/receipt/result/value/{path}"))
            .unwrap() = replacement;
        rejects(&corrupt, path);
    }
    let mut corrupt: Value = serde_json::from_slice(&documents[2]).unwrap();
    corrupt["receipt"]["result"]["value"]["observation"]["devices"][0]["selection_error"] =
        json!("INVENTED");
    rejects(&corrupt, "unknown selection error");
    let mut caps: Value = serde_json::from_slice(&documents[0]).unwrap();
    caps["receipt"]["result"]["value"]["devices"]["backends"][0] = json!("invented");
    rejects(&caps, "unknown capability backend");
}
