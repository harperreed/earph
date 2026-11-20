// ABOUTME: Parses real Core renderer receipts through the strict Desktop consumer.
// ABOUTME: Preserves disabled host behavior without introducing a protocol or output owner.
use earph_core::{control::*, ids::*, session::Session};
use earph_desktop::{Reply, decode_reply};
use serde_json::{Value, json};

fn reply() -> Vec<u8> {
    let mut s = Session::new_owner(
        ZoneId::new("main").unwrap(),
        OwnerId::new("owner").unwrap(),
        ControlLimits::default(),
    );
    let c = decode_command(
        &serde_json::to_vec(&json!({
            "schema_version":1,"request_id":"renderer-query","zone_id":"main","owner_id":"owner",
            "session_id":null,"expected_revision":0,"payload":{"method":"renderer.get"}
        }))
        .unwrap(),
        &ControlLimits::default(),
    )
    .unwrap();
    serde_json::to_vec(&json!({"receipt":s.submit(c)})).unwrap()
}

#[test]
fn actual_core_renderer_reply_is_a_strict_redacted_projection() {
    let bytes = reply();
    let projected = decode_reply(bytes.clone()).expect("actual Core renderer query must project");
    assert_eq!(projected.bytes(), bytes);
    assert!(matches!(projected, Reply::Receipt(_)));
    let mut invalid: Value = serde_json::from_slice(&bytes).unwrap();
    invalid["receipt"]["result"]["value"]["controller_count"] = json!(1);
    assert!(decode_reply(serde_json::to_vec(&invalid).unwrap()).is_err());
    let mut invalid: Value = serde_json::from_slice(&bytes).unwrap();
    invalid["receipt"]["status"] = json!("accepted");
    assert!(decode_reply(serde_json::to_vec(&invalid).unwrap()).is_err());
    let mut invalid: Value = serde_json::from_slice(&bytes).unwrap();
    invalid["receipt"]["result"]["value"]["peers"] = json!(["192.168.1.2"]);
    assert!(decode_reply(serde_json::to_vec(&invalid).unwrap()).is_err());
}
