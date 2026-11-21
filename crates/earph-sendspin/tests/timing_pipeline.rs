// ABOUTME: Exercises real Sendspin message producers, decoders, and timing consumers.
// ABOUTME: Tests portable in-process contracts without native or output-clock claims.
#[path = "support/session_fixture.rs"]
mod fixture;
use earph_sendspin::{
    decode_player_audio,
    message::{MessageLimits, Outgoing, Reassembler},
    session::*,
    timing::*,
};
use serde_json::json;

fn sync() -> TimeSync {
    TimeSync::new(
        Binding::new([11; 32], 4, 1),
        TimePolicy::new(250_000, 9_000_000, 20_000_000).unwrap(),
    )
}
fn token(s: &mut TimeSync, t: i64) -> ProbeToken {
    let Action::PrepareProbe(token) = s.poll(t).unwrap().action else {
        panic!("probe expected")
    };
    token
}
fn mapped() -> TimeSync {
    let mut s = sync();
    for i in 0..8 {
        let t = 1_000_000 + i * 10_000;
        let token = token(&mut s, t);
        s.probe_started(token, t).unwrap();
        s.reply(
            token,
            ServerTime {
                client_transmitted: t,
                server_received: t + 10_002,
                server_transmitted: t + 10_102,
            },
            t + 104,
        )
        .unwrap();
    }
    s
}
#[test]
fn copied_mapping_checks_binding_current_time_and_expiry_without_changing_targets() {
    let mut sync = mapped();
    let binding = Binding::new([11; 32], 4, 1);
    let facts = sync.usable_mapping(1_900_000).unwrap();
    assert_eq!(facts.binding(), binding);
    assert_eq!(facts.mapping_revision(), 1);
    assert_eq!(
        facts
            .map_server_time(2_000_000, 1_900_000, binding)
            .unwrap(),
        sync.map_server_time(2_000_000, 1_900_000).unwrap()
    );
    assert_eq!(
        facts
            .map_client_time(1_990_000, 1_900_000, binding)
            .unwrap()
            .time_us,
        2_000_000
    );
    assert_eq!(
        facts
            .port_target(2_000_000, 100, 1_900_000, binding)
            .unwrap()
            .time_us,
        1_890_000
    );
    assert_eq!(
        facts.map_server_time(2_000_000, 1_899_999, binding),
        Err(TimingError::ClockRegression)
    );
    assert_eq!(
        facts.map_server_time(2_000_000, 1_900_000, Binding::new([12; 32], 4, 1)),
        Err(TimingError::BindingMismatch)
    );
    assert!(
        facts
            .map_server_time(2_000_000, facts.expires_at_us(), binding)
            .is_ok()
    );
    assert_eq!(
        facts.map_server_time(2_000_000, facts.expires_at_us() + 1, binding),
        Err(TimingError::Stale)
    );
    assert!(!format!("{facts:?}").contains("11, 11"));
}
#[test]
fn port_target_keeps_numeric_validation_before_mutable_clock_observation() {
    let mut sync = sync();
    let frame = earph_sendspin::PlayerAudioFrame {
        server_timestamp_us: MAX_TIME_US + 1,
        send_ahead_us: 1,
        payload: &[],
    };
    assert_eq!(
        sync.port_target(frame, 0, 1),
        Err(TimingError::NumericRange)
    );
    assert_eq!(sync.snapshot(0).unwrap().status, Status::Uninitialized);
}
#[test]
fn latest_mapping_revision_remaps_retained_server_timestamp_without_retiming_old_facts() {
    let mut sync = mapped();
    let binding = Binding::new([11; 32], 4, 1);
    let old = sync.usable_mapping(1_900_000).unwrap();
    for i in 0..8 {
        let t = 10_000_000 + i * 10_000;
        let probe = token(&mut sync, t);
        sync.probe_started(probe, t).unwrap();
        sync.reply(
            probe,
            ServerTime {
                client_transmitted: t,
                server_received: t + 20_002,
                server_transmitted: t + 20_102,
            },
            t + 104,
        )
        .unwrap();
    }
    let current = sync.usable_mapping(10_100_000).unwrap();
    assert_eq!(current.mapping_revision(), old.mapping_revision() + 1);
    let server_timestamp = 11_000_000;
    assert_eq!(
        old.port_target(server_timestamp, 100, 10_100_000, binding)
            .unwrap()
            .time_us,
        10_890_000
    );
    assert_eq!(
        current
            .port_target(server_timestamp, 100, 10_100_000, binding)
            .unwrap()
            .time_us,
        10_880_000
    );
    assert_eq!(
        current
            .port_target(server_timestamp, 0, 10_100_000, binding)
            .unwrap()
            .time_us,
        10_980_000
    );
}
#[test]
fn decoded_id4_target_subtracts_local_delay_once_and_lead_only_affects_diagnostics() {
    let mut s = mapped();
    let mut reader = Reassembler::new(MessageLimits::default());
    let mut body = 2_000_000_i64.to_be_bytes().to_vec();
    body.extend_from_slice(&100_000_u32.to_be_bytes());
    body.extend_from_slice(&[0x55, 0xaa]);
    let mut producer = Outgoing::new(4, body, MessageLimits::default()).unwrap();
    let record = producer.next_record().unwrap();
    let complete = reader
        .push(&record, fixture::stamp_ms(1900))
        .unwrap()
        .unwrap();
    let mut plaintext = vec![complete.id];
    plaintext.extend_from_slice(&complete.body);
    let frame = decode_player_audio(&plaintext, plaintext.len()).unwrap();
    assert_eq!(frame.payload, &[0x55, 0xaa]);
    assert_eq!(
        s.port_target(frame, 100, 1_900_000).unwrap().time_us,
        1_890_000
    );
    assert_eq!(
        s.arrival_delay(frame, 1_900_000, 1_900_000).unwrap(),
        Some(10_000)
    );
    for lead in [0, u32::MAX, 1, 123_456] {
        let mut other = frame;
        other.send_ahead_us = lead;
        assert_eq!(
            s.port_target(other, 100, 1_900_000).unwrap().time_us,
            1_890_000
        );
        if lead == 0 || lead == u32::MAX {
            assert_eq!(s.arrival_delay(other, 1_900_000, 1_900_000).unwrap(), None);
        }
    }
    assert_eq!(
        s.port_target(frame, 5001, 1_900_000),
        Err(TimingError::DelayRange)
    );
}
#[test]
fn portable_end_to_end_drives_actual_session_producers_timeout_rotation_and_retirement() {
    let (mut session, mut peer) = fixture::initial();
    let events = peer
        .send_json(
            &mut session,
            "server/activate",
            json!({"activities":[],"active_roles":[]}),
            4,
        )
        .unwrap();
    session.admit(fixture::admission(&events), true, 4).unwrap();
    let mut s = TimeSync::new(
        Binding::new(peer.public, 4, 1),
        TimePolicy::new(250_000, 9_000_000, 20_000_000).unwrap(),
    );
    for i in 0..8 {
        let t = 1_000_000 + i * 300_000;
        let token = token(&mut s, t);
        let request = s.probe_started(token, t).unwrap();
        let write = session
            .time_probe_write(
                earph_sendspin::message::ReceiveStamp::new(request.client_transmitted).unwrap(),
            )
            .unwrap()
            .unwrap();
        let sent = peer.read_json(&write.bytes);
        assert_eq!(sent["payload"]["client_transmitted"], t);
        session
            .complete_write(write.id, WriteResult::Complete, 1000 + i as u64 * 300)
            .unwrap();
        if i == 2 {
            s.poll(t + 250_000).unwrap();
            continue;
        }
        let mut plaintext = vec![0];
        plaintext.extend(serde_json::to_vec(&json!({"type":"server/time","payload":{"client_transmitted":t,"server_received":t+10_002,"server_transmitted":t+10_102}})).unwrap());
        let record = peer.encrypt(&plaintext);
        let events = session
            .receive(
                TransportKind::Binary,
                &record,
                earph_sendspin::message::ReceiveStamp::new(t + 104).unwrap(),
            )
            .unwrap();
        let message = events
            .iter()
            .find_map(|event| {
                if let SessionEvent::Message(message) = event {
                    Some(message)
                } else {
                    None
                }
            })
            .unwrap();
        let envelope: serde_json::Value = serde_json::from_slice(&message.body).unwrap();
        let reply: ServerTime = serde_json::from_value(envelope["payload"].clone()).unwrap();
        // The canonical transport capture reaches the timing reducer without reconstruction.
        s.reply(token, reply, message.arrival.microseconds())
            .unwrap();
    }
    let before = s.snapshot(4_000_000).unwrap();
    assert_eq!(before.estimate.unwrap().offset_us, 10_000.);
    assert_eq!(before.diagnostics.timeouts, 1);
    let (handshake, payload) = peer.rotation([7; 32], "pr", None);
    peer.send_json(&mut session, "noise/handshake", payload, 4001)
        .unwrap();
    assert!(
        session
            .time_probe_write(earph_sendspin::message::ReceiveStamp::new(4_001_000).unwrap())
            .unwrap()
            .is_none()
    );
    s.set_suppressed(true, 4_001_000).unwrap();
    assert_eq!(s.poll(4_001_001).unwrap().action, Action::NoAction);
    let write = session.next_write(4002).unwrap().unwrap();
    let reply = peer.read_json(&write.bytes);
    peer.install(handshake, &reply);
    session
        .complete_write(write.id, WriteResult::Complete, 4002)
        .unwrap();
    let events = peer
        .send_json(
            &mut session,
            "server/activate",
            json!({"activities":[],"active_roles":[]}),
            4003,
        )
        .unwrap();
    session
        .admit(fixture::admission(&events), true, 4003)
        .unwrap();
    s.set_suppressed(false, 4_003_000).unwrap();
    assert_eq!(s.snapshot(4_003_000).unwrap().estimate, before.estimate);
    assert!(matches!(
        s.poll(4_003_000).unwrap().action,
        Action::PrepareProbe(_)
    ));
    s.retire();
    assert_eq!(
        s.map_server_time(5_000_000, 4_003_000),
        Err(TimingError::Retired)
    );
    assert_eq!(s.poll(4_003_000).unwrap().action, Action::NoAction);
    session.close();
}
