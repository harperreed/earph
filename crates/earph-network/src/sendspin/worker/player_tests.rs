// ABOUTME: Exercises typed player admission through authenticated session producers.
// ABOUTME: Keeps fragmented writes, policy revocation, and bounded lease ownership observable.
use super::*;
#[cfg(test)]
mod player_tests {
    use super::*;
    use crate::sendspin::session_fixture as fixture;
    use serde_json::json;
    struct Services;
    impl SendspinServices for Services {
        fn playback_authorized(&self) -> bool {
            true
        }
        fn admission(&mut self, _: AdmissionProposal) -> Operation<Result<bool, DriverError>> {
            Box::pin(async { Ok(true) })
        }
        fn candidate(&mut self, _: AttemptId) -> Operation<Result<Candidate, DriverError>> {
            panic!("no pairing")
        }
        fn commit(
            &mut self,
            _: AttemptId,
            _: earph_sendspin::pairing::RecordUpdate,
        ) -> Operation<Result<(), CommitOutcome>> {
            panic!("no commit")
        }
    }
    use earph_sendspin::session::AdmissionProposal;
    pub(super) fn pump() -> (Pump, fixture::Peer) {
        let catalog = earph_sendspin::pairing::RecordCatalog::from_committed(
            5,
            vec![earph_sendspin::pairing::PairingRecord {
                server_public: earph_sendspin::noise::static_public_key(&[11; 32]).unwrap(),
                psk: [90; 32],
                last_used_order: 1,
            }],
            vec![],
        )
        .unwrap();
        let (mut session, mut peer) = fixture::initial_from_with_record_limit(
            fixture::new_client_with_limits(
                catalog,
                earph_sendspin::session::SessionLimits {
                    messages: earph_sendspin::message::MessageLimits {
                        record_plaintext_bytes: 128,
                        ..Default::default()
                    },
                    ..Default::default()
                },
            ),
            128,
        );
        let events = peer
            .send_json(
                &mut session,
                "server/activate",
                json!({"activities":[],"active_roles":[]}),
                4,
            )
            .unwrap();
        session.admit(fixture::admission(&events), true, 5).unwrap();
        let (handshake, payload) = peer.rotation([90; 32], "lt", None);
        peer.send_json_fragmented(&mut session, "noise/handshake", payload, 6, 128)
            .unwrap();
        let mut reader = earph_sendspin::message::Reassembler::new(Default::default());
        loop {
            let write = session.next_write(6).unwrap().unwrap();
            let plain = peer.read(&write.bytes);
            let reply = reader.push(&plain, fixture::stamp_ms(6)).unwrap();
            session
                .complete_write(write.id, WriteResult::Complete, 6)
                .unwrap();
            if let Some(reply) = reply {
                peer.install(handshake, &serde_json::from_slice(&reply.body).unwrap());
                break;
            }
        }
        let mut shared = resource_tests::shared();
        let inner = Arc::get_mut(&mut shared).unwrap();
        inner.limits = DriverLimits::default();
        inner.state.get_mut().unwrap().observation.player = Some(PlayerObservation::default());
        let clock = ClockDriver::new(ClockConfiguration {
            domain: crate::sendspin::MonotonicDomain::new(1).unwrap(),
            policy: earph_sendspin::timing::TimePolicy::new(1000, 100000, 20_000_000).unwrap(),
        });
        (
            Pump {
                session,
                clock,
                services: Box::new(Services),
                shared,
                start: Instant::now(),
                admission: None,
                candidate: None,
                commit: None,
                profile: Profile::Player(PlayerSupport::new(1048576).unwrap()),
                player_stream_active: false,
                player_state: None,
            },
            peer,
        )
    }
    pub(super) fn activate(p: &mut Pump, peer: &mut fixture::Peer) {
        let events = peer
            .send_json(
                &mut p.session,
                "server/activate",
                json!({"activities":["playback"],"active_roles":["player@v1"]}),
                7,
            )
            .unwrap();
        let id = fixture::admission(&events);
        let events = p.session.admit(id, true, 8).unwrap();
        p.events(events).unwrap();
    }
    #[test]
    fn player_state_requires_mapping_and_each_activation_requires_a_fresh_object() {
        let (mut p, mut peer) = pump();
        activate(&mut p, &mut peer);
        let o = p.shared.lock().observation.clone();
        let player = o.player.unwrap();
        assert!(player.participating);
        assert_eq!(player.activation_generation, 1);
        assert!(!player.state_sent);
        assert_eq!(p.player_mapping().err(), Some(DriverError::Clock));
        let events = peer
            .send_json(
                &mut p.session,
                "server/activate",
                json!({"activities":[],"active_roles":[]}),
                9,
            )
            .unwrap();
        let events = p
            .session
            .admit(fixture::admission(&events), true, 10)
            .unwrap();
        p.events(events).unwrap();
        assert!(!p.shared.lock().observation.player.unwrap().participating);
        assert_eq!(
            p.shared
                .lock()
                .observation
                .player
                .unwrap()
                .activation_generation,
            2
        );
    }
    #[test]
    fn unavailable_audio_retains_ordered_accounting_without_pcm_admission() {
        let (mut p, mut peer) = pump();
        let activation = peer
            .send_json(
                &mut p.session,
                "server/activate",
                json!({"activities":["playback"],"active_roles":["player@v1"]}),
                7,
            )
            .unwrap();
        let admission = fixture::admission(&activation);
        let start=peer.send_json_fragmented(&mut p.session,"stream/start",json!({"server_transmitted":6000,"player":{"codec":"pcm","sample_rate":48000,"channels":2,"bit_depth":16}}),9,31).unwrap();
        assert!(start.is_empty());
        let mut plain = vec![4];
        plain.extend_from_slice(&7000i64.to_be_bytes());
        plain.extend_from_slice(&0u32.to_be_bytes());
        plain.extend_from_slice(&[1, 0, 2, 0]);
        let cipher = peer.encrypt(&plain);
        let events = p
            .session
            .receive(TransportKind::Binary, &cipher, fixture::stamp_ms(10))
            .unwrap();
        assert!(events.is_empty());
        for (kind, payload) in [
            ("stream/clear", json!({"server_transmitted":8000})),
            (
                "stream/clear",
                json!({"server_transmitted":9000,"from_timestamp":7000}),
            ),
            ("stream/end", json!({})),
        ] {
            let events = peer.send_json(&mut p.session, kind, payload, 11).unwrap();
            assert!(events.is_empty());
        }
        let released = p.session.admit(admission, true, 12).unwrap();
        p.events(released).unwrap();
        let observer = SendspinObserver {
            shared: p.shared.clone(),
        };
        let binding = earph_sendspin::player::PlayerBinding::new(
            earph_sendspin::timing::Binding::new(peer.public, 4, 1),
            1,
            1,
        );
        let mut timeline = earph_sendspin::player::PlayerTimeline::new(binding, Default::default());
        timeline.set_participation(true, true).unwrap();
        let mut kinds = vec![];
        while let Some(lease) = observer.next_event() {
            if let Some(event) = lease.player_event() {
                timeline
                    .apply(event, binding, lease.player_available(), None)
                    .unwrap();
                if matches!(event, PlayerEvent::Audio { .. }) {
                    assert!(!lease.player_available());
                    assert_eq!(timeline.usage().wire_bytes, 17);
                    assert_eq!(timeline.usage().allocated_frames, 0);
                }
                kinds.push(match event {
                    PlayerEvent::Start { .. } => "start",
                    PlayerEvent::Clear { .. } => "clear",
                    PlayerEvent::End { .. } => "end",
                    PlayerEvent::Audio { .. } => "audio",
                });
            }
        }
        assert_eq!(kinds, ["start", "audio", "clear", "clear", "end"]);
        let cipher = peer.encrypt(&[4, 0]);
        let events = p
            .session
            .receive(TransportKind::Binary, &cipher, fixture::stamp_ms(13))
            .unwrap();
        assert_eq!(p.events(events), Err(DriverError::Protocol));
    }
    #[test]
    fn inactive_role_discards_audio_without_inspecting_its_payload() {
        let (mut p, mut peer) = pump();
        let events = peer
            .send_json(
                &mut p.session,
                "server/activate",
                json!({"activities":[],"active_roles":[]}),
                7,
            )
            .unwrap();
        let events = p
            .session
            .admit(fixture::admission(&events), true, 8)
            .unwrap();
        p.events(events).unwrap();
        let cipher = peer.encrypt(&[4]);
        let events = p
            .session
            .receive(TransportKind::Binary, &cipher, fixture::stamp_ms(9))
            .unwrap();
        p.events(events).unwrap();
        let observer = SendspinObserver {
            shared: p.shared.clone(),
        };
        while let Some(event) = observer.next_event() {
            assert!(!matches!(event.event(),SessionEvent::Message(message) if message.id==4));
        }
    }
    #[test]
    fn concurrent_typed_consumers_allocate_nothing_and_retain_wire_credits() {
        let (mut p, mut peer) = pump();
        activate(&mut p, &mut peer);
        let observer = SendspinObserver {
            shared: p.shared.clone(),
        };
        while observer.next_event().is_some() {}
        for (kind, payload, now) in [
            (
                "stream/start",
                json!({"server_transmitted":9000,"player":{"codec":"pcm","sample_rate":48000,"channels":2,"bit_depth":16},"unknown":"x".repeat(8192)}),
                9,
            ),
            (
                "stream/clear",
                json!({"server_transmitted":10000,"unknown":"x".repeat(8192)}),
                10,
            ),
            ("stream/end", json!({"unknown":"x".repeat(8192)}), 11),
        ] {
            let events = peer
                .send_json_fragmented(&mut p.session, kind, payload, now, 128)
                .unwrap();
            p.events(events).unwrap();
            if kind == "stream/start" {
                let mut plain = vec![4];
                plain.extend_from_slice(&9500i64.to_be_bytes());
                plain.extend_from_slice(&0u32.to_be_bytes());
                plain.extend_from_slice(&[0x00, 0x80, 0xff, 0x7f]);
                let cipher = peer.encrypt(&plain);
                let events = p
                    .session
                    .receive(TransportKind::Binary, &cipher, fixture::stamp_ms(now))
                    .unwrap();
                p.events(events).unwrap();
            }
        }
        let mut leases = vec![];
        while let Some(lease) = observer.next_event() {
            leases.push(Arc::new(lease));
        }
        assert_eq!(leases.len(), 4);
        let before = observer.observation();
        assert_eq!(before.event_entries, 4);
        assert!(observer.player_metadata_capacity_bytes() > 0);
        let barrier = Arc::new(std::sync::Barrier::new(leases.len() + 1));
        let mut readers = vec![];
        for lease in &leases {
            let lease = lease.clone();
            let barrier = barrier.clone();
            readers.push(std::thread::spawn(move || {
                barrier.wait();
                crate::upnp::trace_allocations(|| {
                    let mut seen = 0;
                    for _ in 0..8 {
                        seen += match lease.player_event().unwrap() {
                            PlayerEvent::Start {
                                server_transmitted_us: 9000,
                                ..
                            } => 1,
                            PlayerEvent::Clear {
                                server_transmitted_us: 10000,
                                ..
                            } => 1,
                            PlayerEvent::End { .. } => 1,
                            PlayerEvent::Audio { frame, .. }
                                if frame.payload == [0x00, 0x80, 0xff, 0x7f] =>
                            {
                                1
                            }
                            _ => 0,
                        };
                    }
                    seen
                })
            }));
        }
        barrier.wait();
        let mut max_peak = 0;
        for reader in readers {
            let (seen, peak, overflow) = reader.join().unwrap();
            max_peak = max_peak.max(peak);
            assert_eq!(seen, 8);
            assert!(!overflow);
            assert_eq!(
                peak, 0,
                "typed consumers must not allocate another JSON Envelope"
            );
        }
        assert_eq!(observer.observation().event_bytes, before.event_bytes);
        assert_eq!(observer.observation().event_entries, 4);
        println!(
            "PLAYER_EVENT_ALLOCATION threads=4 transient_peak={max_peak} metadata_capacity={} retained_entries=4 retained_wire_bytes={}",
            observer.player_metadata_capacity_bytes(),
            before.event_bytes
        );
        drop(leases);
        assert_eq!(observer.observation().event_bytes, 0);
        assert_eq!(observer.observation().event_entries, 0);
    }
}

#[cfg(test)]
mod player_flush_tests {
    use super::player_tests::{activate, pump};
    use super::*;
    use futures_util::StreamExt;
    use serde_json::json;
    fn mapping(p: &mut Pump, peer: &mut crate::sendspin::session_fixture::Peer) -> UsableMapping {
        std::thread::sleep(Duration::from_millis(10));
        for _ in 0..8 {
            let t1 = p.clock.domain.sample().unwrap();
            p.clock.advance(p.session.clock_context(), t1).unwrap();
            let token = p.clock.offered_at(t1).unwrap().unwrap();
            let write = p.session.time_probe_write(t1).unwrap().unwrap();
            p.clock.started(token, t1).unwrap();
            let payload = peer.read_json(&write.bytes);
            assert_eq!(payload["type"], "client/time");
            p.session
                .complete_write(write.id, WriteResult::Complete, t1.milliseconds())
                .unwrap();
            let t4 = p.clock.domain.sample().unwrap();
            let mut plain = vec![0];
            plain.extend(serde_json::to_vec(&json!({"type":"server/time","payload":{"client_transmitted":t1.microseconds(),"server_received":t1.microseconds()+1000,"server_transmitted":t1.microseconds()+1000}})).unwrap());
            let cipher = peer.encrypt(&plain);
            let events = p
                .session
                .receive(TransportKind::Binary, &cipher, t4)
                .unwrap();
            p.events(events).unwrap();
            std::thread::sleep(Duration::from_millis(1));
        }
        p.shared
            .lock()
            .observation
            .clock
            .unwrap()
            .usable_mapping
            .unwrap()
    }
    #[test]
    fn fragmented_state_opens_audio_only_after_the_final_actual_flush() {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
            let (mut p,mut peer)=pump();
            activate(&mut p,&mut peer);
            let mapping=mapping(&mut p,&mut peer);
            let update=StateUpdate{generation:1,mapping,state:PlayerState::new(0,100,50).unwrap(),available:true};
            p.send_player_state(update).unwrap();
            assert!(p.session.application_pending());
            let mut fragments=0;
            let mut state_reader=earph_sendspin::message::Reassembler::new(Default::default());
            let mut delivered=None;
            while let Some(write)=p.session.next_write(p.clock.domain.milliseconds()).unwrap() {
                fragments+=1;
                let (wire,consumer)=tokio::io::duplex(1);
                let mut socket=WebSocketStream::from_raw_socket(wire,tokio_tungstenite::tungstenite::protocol::Role::Client,None).await;
                let mut receiver=WebSocketStream::from_raw_socket(consumer,tokio_tungstenite::tungstenite::protocol::Role::Server,None).await;
                let mut pending=PendingWrite{write,started:false};
                assert!(pending.poll(&mut socket,&mut Context::from_waker(std::task::Waker::noop()),false).is_pending());
                assert!(!p.shared.lock().observation.player.unwrap().state_sent);
                let (sent,received)=futures_util::future::join(poll_fn(|cx| pending.poll(&mut socket,cx,false)),receiver.next()).await;
                sent.unwrap();
                let Some(Ok(Message::Binary(cipher)))=received else {panic!("state binary frame missing")};
                let plain=peer.read(&cipher);
                if let Some(message)=state_reader.push(&plain,p.clock.domain.sample().unwrap()).unwrap() {
                    delivered=Some(serde_json::from_slice::<serde_json::Value>(&message.body).unwrap());
                }
                p.session.complete_write(pending.write.id,WriteResult::Complete,p.clock.domain.milliseconds()).unwrap();
                p.player_state_written().unwrap();
                assert_eq!(p.shared.lock().observation.player.unwrap().state_sent,!p.session.application_pending());
            }
            assert!(fragments>1);
            assert!(p.shared.lock().observation.player.unwrap().available);
            assert_eq!(delivered.unwrap(),json!({"type":"client/state","payload":{"player":{"available":true,"output_delay_ms":0,"required_lead_time_ms":100,"min_buffer_ms":50,"supported_commands":[]}}}));
        });
    }
    async fn flush(write: TransportWrite) -> TransportWrite {
        let (wire, consumer) = tokio::io::duplex(1);
        let mut socket = WebSocketStream::from_raw_socket(
            wire,
            tokio_tungstenite::tungstenite::protocol::Role::Client,
            None,
        )
        .await;
        let mut receiver = WebSocketStream::from_raw_socket(
            consumer,
            tokio_tungstenite::tungstenite::protocol::Role::Server,
            None,
        )
        .await;
        let mut pending = PendingWrite {
            write,
            started: false,
        };
        assert!(
            pending
                .poll(
                    &mut socket,
                    &mut Context::from_waker(std::task::Waker::noop()),
                    false
                )
                .is_pending()
        );
        let (sent, received) = futures_util::future::join(
            poll_fn(|cx| pending.poll(&mut socket, cx, false)),
            receiver.next(),
        )
        .await;
        sent.unwrap();
        assert!(matches!(received, Some(Ok(Message::Binary(_)))));
        pending.write
    }
    #[test]
    fn rotation_reply_flush_cannot_acknowledge_an_unsent_player_state() {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(async {
                let (mut p, mut peer) = pump();
                activate(&mut p, &mut peer);
                let mapping = mapping(&mut p, &mut peer);
                let update = StateUpdate {
                    generation: 1,
                    mapping,
                    state: PlayerState::new(0, 100, 50).unwrap(),
                    available: true,
                };
                let expected = update.message().body;
                p.send_player_state(update).unwrap();
                let (handshake, payload) = peer.rotation([90; 32], "lt", None);
                let now = p.clock.domain.milliseconds();
                let events = peer
                    .send_json_fragmented(&mut p.session, "noise/handshake", payload, now, 128)
                    .unwrap();
                p.events(events).unwrap();
                let mut reader = earph_sendspin::message::Reassembler::new(Default::default());
                loop {
                    let write = p
                        .session
                        .next_write(p.clock.domain.milliseconds())
                        .unwrap()
                        .unwrap();
                    let plain = peer.read(&write.bytes);
                    let reply = reader
                        .push(&plain, p.clock.domain.sample().unwrap())
                        .unwrap();
                    let write = flush(write).await;
                    let events = p
                        .session
                        .complete_write(
                            write.id,
                            WriteResult::Complete,
                            p.clock.domain.milliseconds(),
                        )
                        .unwrap();
                    p.events(events).unwrap();
                    p.player_state_written().unwrap();
                    assert!(
                        !p.shared.lock().observation.player.unwrap().state_sent,
                        "rotation reply did not carry the player state"
                    );
                    assert!(!p.shared.lock().observation.player.unwrap().available);
                    if let Some(reply) = reply {
                        peer.install(handshake, &serde_json::from_slice(&reply.body).unwrap());
                        break;
                    }
                }
                let observer = SendspinObserver {
                    shared: p.shared.clone(),
                };
                let mut unsent = 0;
                while let Some(lease) = observer.next_event() {
                    if let SessionEvent::Unsent(message) = lease.event() {
                        assert_eq!(message.body, expected);
                        unsent += 1;
                    }
                }
                assert_eq!(unsent, 1);
                let now = p.clock.domain.milliseconds();
                let events = peer
                    .send_json(
                        &mut p.session,
                        "server/activate",
                        json!({"activities":["playback"],"active_roles":["player@v1"]}),
                        now,
                    )
                    .unwrap();
                let events = p
                    .session
                    .admit(
                        crate::sendspin::session_fixture::admission(&events),
                        true,
                        now,
                    )
                    .unwrap();
                p.events(events).unwrap();
                assert!(!p.shared.lock().observation.player.unwrap().state_sent);
                let mapping = p
                    .shared
                    .lock()
                    .observation
                    .clock
                    .unwrap()
                    .usable_mapping
                    .unwrap();
                p.send_player_state(StateUpdate {
                    generation: 2,
                    mapping,
                    ..update
                })
                .unwrap();
                while let Some(write) = p.session.next_write(p.clock.domain.milliseconds()).unwrap()
                {
                    let write = flush(write).await;
                    let events = p
                        .session
                        .complete_write(
                            write.id,
                            WriteResult::Complete,
                            p.clock.domain.milliseconds(),
                        )
                        .unwrap();
                    p.events(events).unwrap();
                    p.player_state_written().unwrap();
                }
                assert!(p.shared.lock().observation.player.unwrap().state_sent);
                assert!(p.shared.lock().observation.player.unwrap().available);
            });
    }
    #[test]
    fn queued_state_cannot_survive_reactivation_or_native_revocation() {
        struct Revocable(Arc<std::sync::atomic::AtomicBool>);
        impl SendspinServices for Revocable {
            fn playback_authorized(&self) -> bool {
                self.0.load(std::sync::atomic::Ordering::Acquire)
            }
            fn admission(
                &mut self,
                _: earph_sendspin::session::AdmissionProposal,
            ) -> Operation<Result<bool, DriverError>> {
                Box::pin(async { Ok(true) })
            }
            fn candidate(&mut self, _: AttemptId) -> Operation<Result<Candidate, DriverError>> {
                panic!("no pairing")
            }
            fn commit(
                &mut self,
                _: AttemptId,
                _: earph_sendspin::pairing::RecordUpdate,
            ) -> Operation<Result<(), CommitOutcome>> {
                panic!("no commit")
            }
        }
        let (mut p, mut peer) = pump();
        activate(&mut p, &mut peer);
        let mapping = mapping(&mut p, &mut peer);
        let authority = Arc::new(std::sync::atomic::AtomicBool::new(true));
        p.services = Box::new(Revocable(authority.clone()));
        let update = StateUpdate {
            generation: 1,
            mapping,
            state: PlayerState::new(0, 100, 50).unwrap(),
            available: true,
        };
        p.send_player_state(update).unwrap();
        authority.store(false, std::sync::atomic::Ordering::Release);
        assert_eq!(p.check_pending_player_state(), Err(DriverError::Admission));
        authority.store(true, std::sync::atomic::Ordering::Release);
        let now = p.clock.domain.milliseconds();
        let events = peer
            .send_json(
                &mut p.session,
                "server/activate",
                json!({"activities":["playback"],"active_roles":["player@v1"]}),
                now,
            )
            .unwrap();
        let events = p
            .session
            .admit(
                crate::sendspin::session_fixture::admission(&events),
                true,
                now,
            )
            .unwrap();
        p.events(events).unwrap();
        assert_eq!(p.check_pending_player_state(), Err(DriverError::Admission));
        assert!(!p.shared.lock().observation.player.unwrap().state_sent);
    }
    #[test]
    fn cancellation_immediately_revokes_the_player_declaration() {
        let (mut p, mut peer) = pump();
        activate(&mut p, &mut peer);
        {
            let mut s = p.shared.lock();
            let player = s.observation.player.as_mut().unwrap();
            player.state_sent = true;
            player.available = true;
        }
        p.shared.cancel();
        let player = p.shared.lock().observation.player.unwrap();
        assert!(!player.participating);
        assert!(!player.state_sent);
        assert!(!player.available);
    }
    #[test]
    fn cancellation_releases_queued_audio_but_retained_lease_stays_charged() {
        let (p, _) = pump();
        let observer = SendspinObserver {
            shared: p.shared.clone(),
        };
        let audio = || {
            SessionEvent::Message(earph_sendspin::message::Complete {
                id: 4,
                body: vec![0; 16],
                arrival: p.clock.domain.sample().unwrap(),
            })
        };
        p.shared.publish(audio()).unwrap();
        let held = observer.next_event().unwrap();
        p.shared.publish(audio()).unwrap();
        assert_eq!(observer.observation().event_bytes, 32);
        p.shared.cancel();
        assert_eq!(observer.observation().event_bytes, 16);
        assert!(observer.next_event().is_none());
        drop(held);
        assert_eq!(observer.observation().event_bytes, 0);
        assert_eq!(observer.observation().event_entries, 0);
    }
}
