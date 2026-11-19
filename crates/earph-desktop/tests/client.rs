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
