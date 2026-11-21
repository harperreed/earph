// ABOUTME: Parses real canonical URI staging replies through the desktop's strict projection.
// ABOUTME: Rejects missing staging fields and never reconstructs raw network input.
use earph_core::{control::*, ids::*, session::Session};
use earph_desktop::{Reply, ReplyStatus, Request, decode_reply};
use serde_json::{Value, json};

fn owner() -> Session {
    Session::new_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("source-owner").unwrap(),
        ControlLimits::default(),
    )
}
fn state(session: &mut Session) -> earph_desktop::StateView {
    let receipt = session.submit(CommandEnvelope {
        schema_version: 1,
        request_id: RequestId::new("state").unwrap(),
        zone_id: ZoneId::new("main").unwrap(),
        owner_id: None,
        session_id: None,
        expected_revision: None,
        payload: Command::State,
    });
    let Reply::Receipt(receipt) =
        decode_reply(serde_json::to_vec(&json!({"receipt":receipt})).unwrap()).unwrap()
    else {
        panic!("receipt")
    };
    receipt.state().unwrap().clone()
}
fn staged_reply() -> Vec<u8> {
    let mut owner = owner();
    let request = Request::mutation(
        RequestId::new("stage").unwrap(),
        &state(&mut owner),
        Command::SourceStageUri {
            uri: "http://example.invalid/?token=private".into(),
            metadata: "private metadata".into(),
        },
    )
    .unwrap();
    let command = decode_command(request.bytes(), &ControlLimits::default()).unwrap();
    let receipt = owner.submit(command);
    assert_eq!(receipt.status, ReceiptStatus::Applied);
    serde_json::to_vec(&json!({"receipt":receipt})).unwrap()
}

#[test]
fn desktop_request_and_projection_consume_real_staging_without_exposing_input() {
    let bytes = staged_reply();
    let reply = decode_reply(bytes.clone()).expect("canonical staging reply must project");
    assert_eq!(reply.bytes(), bytes);
    let Reply::Receipt(receipt) = reply else {
        panic!("receipt")
    };
    assert_eq!(receipt.status, ReplyStatus::Applied);
    assert!(receipt.result.is_some());
    assert!(!String::from_utf8(bytes).unwrap().contains("private"));
}

#[test]
fn staging_projection_requires_nullable_fields_and_unadmitted_bounded_identity() {
    let original: Value = serde_json::from_slice(&staged_reply()).unwrap();
    for (key, value) in [("generation", json!(0)), ("staged", Value::Null)] {
        let mut missing = original.clone();
        missing["receipt"]["result"]["value"]
            .as_object_mut()
            .unwrap()
            .remove(key);
        assert!(decode_reply(serde_json::to_vec(&missing).unwrap()).is_err());
        if key == "generation" {
            let mut bad = original.clone();
            bad["receipt"]["result"]["value"][key] = value;
            assert!(decode_reply(serde_json::to_vec(&bad).unwrap()).is_err());
        }
    }
    for (key, value) in [
        ("media_id", json!("staged-2")),
        ("admission", json!("admitted")),
        ("uri_bytes", json!(0)),
        ("uri_bytes", json!(8193)),
        ("metadata_bytes", json!(65537)),
        ("uri", json!("http://private.invalid/?token=secret")),
    ] {
        let mut bad = original.clone();
        bad["receipt"]["result"]["value"]["staged"][key] = value;
        assert!(
            decode_reply(serde_json::to_vec(&bad).unwrap()).is_err(),
            "invalid {key}"
        );
    }
    let mut failed = original;
    failed["receipt"]["status"] = json!("failed");
    assert!(decode_reply(serde_json::to_vec(&failed).unwrap()).is_err());
}
