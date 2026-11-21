// ABOUTME: Exercises live clock ownership against authenticated real Snow session messages.
// ABOUTME: Uses fixed microsecond vectors to isolate ordering from host scheduling.
use super::*;
use crate::sendspin::session_fixture as fixture;
use earph_sendspin::timing::Status;
use earph_sendspin::{
    message::Complete,
    session::{ClientSession, SessionEvent, WriteResult},
};
use serde_json::json;
fn stamp(us: i64) -> ReceiveStamp {
    ReceiveStamp::new(us).unwrap()
}
fn clock(domain: u64) -> ClockDriver {
    ClockDriver::new(ClockConfiguration {
        domain: MonotonicDomain::new(domain).unwrap(),
        policy: TimePolicy::new(1_000, 100_000, 200_000).unwrap(),
    })
}
fn active() -> (ClientSession, fixture::Peer) {
    let (mut session, mut peer) = fixture::initial();
    let events = peer
        .send_json(
            &mut session,
            "server/activate",
            json!({"activities":[],"active_roles":[]}),
            4,
        )
        .unwrap();
    session.admit(fixture::admission(&events), true, 5).unwrap();
    (session, peer)
}
fn message(t1: i64, t2: i64, t3: i64, t4: i64) -> Complete {
    Complete {id:0,body:serde_json::to_vec(&json!({"type":"server/time","payload":{"client_transmitted":t1,"server_received":t2,"server_transmitted":t3}})).unwrap(),arrival:stamp(t4)}
}
fn emit(clock: &mut ClockDriver, session: &mut ClientSession, peer: &mut fixture::Peer, t1: i64) {
    clock.advance(session.clock_context(), stamp(t1)).unwrap();
    let token = clock.offered_at(stamp(t1 + 1)).unwrap().unwrap();
    let write = session.time_probe_write(stamp(t1 + 1)).unwrap().unwrap();
    assert_eq!(
        peer.read_json(&write.bytes)["payload"]["client_transmitted"],
        t1 + 1
    );
    clock.started(token, stamp(t1 + 1)).unwrap();
    session
        .complete_write(
            write.id,
            WriteResult::Complete,
            stamp(t1 + 1).milliseconds(),
        )
        .unwrap();
}
fn map(
    clock: &mut ClockDriver,
    session: &mut ClientSession,
    peer: &mut fixture::Peer,
) -> ClockObservation {
    for i in 0..8 {
        let t = 1_000_000 + i * 1_000;
        emit(clock, session, peer, t);
        let reply = message(t + 1, t + 10_003, t + 10_103, t + 105);
        assert!(clock.consume(session.clock_context(), &reply).unwrap());
    }
    clock
        .advance(session.clock_context(), stamp(1_007_105))
        .unwrap()
        .unwrap()
}
#[test]
fn burst_uses_one_filter_update_and_redacts_the_mapping() {
    let (mut session, mut peer) = active();
    let mut clock = clock(1);
    let observed = map(&mut clock, &mut session, &mut peer);
    assert_eq!(observed.snapshot.count, 1);
    assert_eq!(observed.snapshot.mapping_revision, 1);
    assert_eq!(observed.snapshot.estimate.unwrap().offset_us, 10_000.);
    assert_eq!(
        observed.snapshot.estimate.unwrap().last_update_us,
        1_000_105
    );
    assert_eq!(observed.snapshot.diagnostics.emitted_requests, 8);
    assert_eq!(observed.snapshot.diagnostics.accepted_measurements, 1);
    assert!(!format!("{observed:?}").contains("10000"));
    assert!(!format!("{observed:?}").contains("offset_us"));
}
#[test]
fn captured_matching_reply_wins_before_a_later_timeout_tick() {
    let (mut session, mut peer) = active();
    let mut clock = clock(1);
    emit(&mut clock, &mut session, &mut peer, 1_000_000);
    assert!(
        clock
            .consume(
                session.clock_context(),
                &message(1_000_001, 1_010_003, 1_010_103, 1_000_999)
            )
            .unwrap()
    );
    let observed = clock
        .advance(session.clock_context(), stamp(1_001_100))
        .unwrap()
        .unwrap();
    assert_eq!(observed.snapshot.diagnostics.timeouts, 0);
    assert_eq!(observed.snapshot.diagnostics.emitted_requests, 1);
    assert!(clock.offered_at(stamp(1_001_101)).unwrap().is_some());
}
#[test]
fn invalid_duplicate_and_unmatched_replies_never_extend_pending_work() {
    let (mut session, mut peer) = active();
    let mut clock = clock(1);
    emit(&mut clock, &mut session, &mut peer, 1_000_000);
    for payload in [
        json!({"client_transmitted":1_000_001,"server_received":true,"server_transmitted":1}),
        json!({"client_transmitted":1_000_001,"server_received":1,"server_transmitted":1.5}),
        json!({"client_transmitted":1_000_001,"server_received":MAX_TIME_US+1,"server_transmitted":1}),
        json!({"client_transmitted":1_000_001,"server_received":1_010_003,"server_transmitted":1_010_002}),
        json!({"client_transmitted":999,"server_received":1_010_003,"server_transmitted":1_010_103}),
    ] {
        let message = Complete {
            id: 0,
            body: serde_json::to_vec(&json!({"type":"server/time","payload":payload})).unwrap(),
            arrival: stamp(1_000_105),
        };
        assert!(clock.consume(session.clock_context(), &message).unwrap());
    }
    let observed = clock
        .advance(session.clock_context(), stamp(1_001_001))
        .unwrap()
        .unwrap();
    assert_eq!(observed.snapshot.count, 0);
    assert_eq!(observed.snapshot.diagnostics.timeouts, 1);
    assert!(
        clock
            .consume(
                session.clock_context(),
                &message(1_000_001, 1_010_003, 1_010_103, 1_000_105)
            )
            .unwrap()
    );
    assert_eq!(
        clock
            .advance(session.clock_context(), stamp(1_001_002))
            .unwrap()
            .unwrap()
            .snapshot
            .count,
        0
    );
}
#[test]
fn blocked_admission_abandons_burst_and_retains_original_captured_reply() {
    let (mut session, mut peer) = active();
    let mut clock = clock(1);
    let before = map(&mut clock, &mut session, &mut peer);
    emit(&mut clock, &mut session, &mut peer, 1_100_000);
    let events = peer
        .send_json(
            &mut session,
            "server/activate",
            json!({"activities":[]}),
            1100,
        )
        .unwrap();
    let id = fixture::admission(&events);
    clock
        .advance(session.clock_context(), stamp(1_100_005))
        .unwrap();
    let payload = json!({"type":"server/time","payload":{"client_transmitted":1_100_001,"server_received":1_110_003,"server_transmitted":1_110_103}});
    let mut plain = vec![0];
    plain.extend(serde_json::to_vec(&payload).unwrap());
    let cipher = peer.encrypt(&plain);
    assert!(
        session
            .receive(
                earph_sendspin::session::TransportKind::Binary,
                &cipher,
                stamp(1_100_037)
            )
            .unwrap()
            .is_empty()
    );
    clock
        .advance(session.clock_context(), stamp(1_100_050))
        .unwrap();
    let released = session.admit(id, true, 1100).unwrap();
    let reply = released
        .iter()
        .find_map(|e| match e {
            SessionEvent::Message(m) => Some(m),
            _ => None,
        })
        .unwrap();
    assert_eq!(reply.arrival.microseconds(), 1_100_037);
    assert!(clock.consume(session.clock_context(), reply).unwrap());
    let after = clock
        .advance(session.clock_context(), stamp(1_100_100))
        .unwrap()
        .unwrap();
    assert_eq!(after.snapshot.estimate, before.snapshot.estimate);
    assert_eq!(
        after.snapshot.mapping_revision,
        before.snapshot.mapping_revision
    );
    assert_eq!(after.snapshot.diagnostics.incomplete_bursts, 1);
    assert!(clock.offered_at(stamp(1_100_101)).unwrap().is_some());
}
#[test]
fn rotation_suppresses_probes_and_preserves_established_filter() {
    let (mut session, mut peer) = active();
    let mut clock = clock(1);
    let before = map(&mut clock, &mut session, &mut peer);
    emit(&mut clock, &mut session, &mut peer, 1_100_000);
    let (handshake, payload) = peer.rotation([7; 32], "pr", None);
    peer.send_json(&mut session, "noise/handshake", payload, 1100)
        .unwrap();
    clock
        .advance(session.clock_context(), stamp(1_100_005))
        .unwrap();
    assert!(clock.offered_at(stamp(1_100_010)).unwrap().is_none());
    let write = session.next_write(1100).unwrap().unwrap();
    let reply = peer.read_json(&write.bytes);
    peer.install(handshake, &reply);
    session
        .complete_write(write.id, WriteResult::Complete, 1100)
        .unwrap();
    let quiet = clock
        .advance(session.clock_context(), stamp(1_100_020))
        .unwrap()
        .unwrap();
    assert_eq!(quiet.snapshot.estimate, before.snapshot.estimate);
    assert_eq!(quiet.snapshot.diagnostics.incomplete_bursts, 1);
    let events = peer
        .send_json(
            &mut session,
            "server/activate",
            json!({"activities":[]}),
            1100,
        )
        .unwrap();
    session
        .admit(fixture::admission(&events), true, 1100)
        .unwrap();
    clock
        .advance(session.clock_context(), stamp(1_100_030))
        .unwrap();
    assert!(clock.offered_at(stamp(1_100_031)).unwrap().is_some());
}
#[test]
fn replaced_domain_has_a_fresh_binding_and_no_previous_mapping() {
    assert!(MonotonicDomain::new(0).is_err());
    let (mut session, mut peer) = active();
    let mut original = clock(1);
    let before = map(&mut original, &mut session, &mut peer);
    let mut replacement = clock(2);
    let after = replacement
        .advance(session.clock_context(), stamp(1_100_000))
        .unwrap()
        .unwrap();
    assert_ne!(after.snapshot.binding, before.snapshot.binding);
    assert_eq!(after.snapshot.count, 0);
    assert_eq!(after.snapshot.mapping_revision, 0);
    replacement
        .consume(
            session.clock_context(),
            &message(1_000_001, 1_010_003, 1_010_103, 1_000_105),
        )
        .unwrap();
    assert_eq!(
        replacement
            .advance(session.clock_context(), stamp(1_100_001))
            .unwrap()
            .unwrap()
            .snapshot
            .count,
        0
    );
    let retired = original.retire(stamp(1_100_000)).unwrap().unwrap();
    assert_eq!(retired.snapshot.status, Status::Retired);
}

#[test]
fn another_authenticated_connection_cannot_answer_the_pending_probe() {
    use earph_sendspin::{pairing::RecordCatalog, session::SessionConfig};
    let (mut session, mut peer) = active();
    let mut clock = clock(1);
    emit(&mut clock, &mut session, &mut peer, 1_000_000);
    let foreign = ClientSession::new(
        SessionConfig {
            private: [9; 32],
            pairing_psk: [7; 32],
            connection_generation: 9,
            hello: json!({"name":"foreign binding","supported_roles":[]}),
            catalog: RecordCatalog::new(5).unwrap(),
            protected: vec![],
            limits: Default::default(),
        },
        0,
    )
    .unwrap();
    let (foreign, _) = fixture::initial_from(foreign);
    assert!(
        clock
            .consume(
                foreign.clock_context(),
                &message(1_000_001, 1_010_003, 1_010_103, 1_000_105)
            )
            .unwrap()
    );
    assert!(clock.offered_at(stamp(1_000_200)).unwrap().is_none());
    let observed = clock
        .advance(session.clock_context(), stamp(1_001_001))
        .unwrap()
        .unwrap();
    assert_eq!(observed.snapshot.diagnostics.timeouts, 1);
    assert_eq!(observed.snapshot.count, 0);
}

#[test]
fn copied_mapping_uses_current_filter_identity_and_expires_without_new_state() {
    let (mut session, mut peer) = active();
    let mut clock = clock(1);
    let observed = map(&mut clock, &mut session, &mut peer);
    let facts = observed.usable_mapping.unwrap();
    assert_eq!(facts.binding(), observed.snapshot.binding);
    assert_eq!(facts.mapping_revision(), observed.snapshot.mapping_revision);
    assert_eq!(
        facts
            .map_server_time(1_020_000, 1_007_105, facts.binding())
            .unwrap()
            .time_us,
        1_010_000
    );
    assert!(
        facts
            .map_server_time(1_020_000, facts.expires_at_us() + 1, facts.binding())
            .is_err()
    );
    let stale = clock
        .advance(session.clock_context(), stamp(1_200_106))
        .unwrap()
        .unwrap();
    assert!(stale.usable_mapping.is_none());
    assert_eq!(stale.snapshot.count, 1);
}
