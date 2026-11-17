// ABOUTME: Exercises the versioned command decoder with real wire documents.
// ABOUTME: Rejects inert settings and malformed targets before session mutation.
use earph_core::{ErrorCode, control::*};
use serde_json::{Value, json};

#[test]
fn supported_wire_command_round_trips_without_changing_meaning() {
    let bytes = include_bytes!("../../../fixtures/contracts/start-v1.json");
    let command = decode_command(bytes, &ControlLimits::default()).unwrap();
    let restored = decode_command(
        &serde_json::to_vec(&command).unwrap(),
        &ControlLimits::default(),
    )
    .unwrap();
    assert_eq!(restored, command);
}

#[test]
fn unsupported_versions_fields_and_methods_fail() {
    let baseline: Value =
        serde_json::from_slice(include_bytes!("../../../fixtures/contracts/start-v1.json"))
            .unwrap();
    for (pointer, value) in [
        ("/schema_version", json!(0)),
        ("/schema_version", json!(2)),
        ("/schema_version", json!("1")),
        ("/request_id", json!("")),
        ("/payload/method", json!("graph.commit")),
        ("/payload/method", json!("volume.set")),
    ] {
        let mut input = baseline.clone();
        *input.pointer_mut(pointer).unwrap() = value;
        assert!(
            decode_command(
                &serde_json::to_vec(&input).unwrap(),
                &ControlLimits::default()
            )
            .is_err()
        );
    }
    for pointer in ["", "/payload"] {
        let mut input = baseline.clone();
        input.pointer_mut(pointer).unwrap()["required_future_feature"] = json!(true);
        assert!(
            decode_command(
                &serde_json::to_vec(&input).unwrap(),
                &ControlLimits::default()
            )
            .is_err()
        );
    }
    let mut input = baseline;
    input.as_object_mut().unwrap().remove("schema_version");
    assert!(
        decode_command(
            &serde_json::to_vec(&input).unwrap(),
            &ControlLimits::default()
        )
        .is_err()
    );
    assert_eq!(
        decode_command(&vec![b' '; 1048577], &ControlLimits::default())
            .unwrap_err()
            .code,
        ErrorCode::ResourceBudgetExceeded
    );
    assert!(
        decode_command(
            br#"{"schema_version":1,"schema_version":2}"#,
            &ControlLimits::default()
        )
        .is_err()
    );
}
