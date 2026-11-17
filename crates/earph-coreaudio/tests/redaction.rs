// ABOUTME: Checks portable report redaction against actual JSON producers and files.
// ABOUTME: Removes device identifiers while retaining format observations and source hashes.
use earph_coreaudio::redact_record;
use serde_json::json;

#[test]
fn redaction_removes_identifiers_without_rewriting_observed_formats() {
    let original = json!({"schema_version":1,"command":"precision-check","status":"conflict",
        "assessment":{"requested":{"device_uid":"private-device-uid"},
            "observed":{"uid":"private-device-uid","name":"Doctor Biz personal device",
                "object_id":42,"output_streams":[{"physical":{"format_flags":9,"bits_per_channel":32}}]}},
        "signals_submitted":0});
    let bytes = serde_json::to_vec(&original).unwrap();
    let result = redact_record(&bytes).unwrap();
    assert_eq!(
        result["assessment"]["requested"]["device_uid"],
        "redacted-device"
    );
    assert_eq!(result["assessment"]["observed"]["uid"], "redacted-device");
    assert_eq!(
        result["assessment"]["observed"]["name"],
        "redacted-device-name"
    );
    assert_eq!(
        result["assessment"]["observed"]["output_streams"],
        original["assessment"]["observed"]["output_streams"]
    );
    assert_eq!(
        result["source_record_sha256"],
        earph_harness::sha256(&bytes)
    );
    assert_eq!(result["identifiers_redacted"], true);
    assert!(
        !serde_json::to_string(&result)
            .unwrap()
            .contains("private-device-uid")
    );
    assert_eq!(
        original["assessment"]["observed"]["uid"],
        "private-device-uid"
    );
}

#[test]
fn unsupported_or_nonrecord_inputs_fail_without_a_redacted_success() {
    for bytes in [
        b"[]".as_slice(),
        b"{bad json",
        br#"{"schema_version":2,"command":"devices"}"#,
        br#"{"schema_version":1,"command":"unrelated"}"#,
    ] {
        assert!(redact_record(bytes).is_err());
    }
}
