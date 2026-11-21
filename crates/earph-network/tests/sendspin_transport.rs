// ABOUTME: Exercises the actual outbound roleless WebSocket owner on admitted sockets.
// ABOUTME: Verifies upgrade policy, complete writes, cancellation, and retained ownership.
use earph_core::ids::OwnerId;
use earph_network::sendspin::*;
use earph_sendspin::{pairing::*, session::*};
use serde_json::json;
use std::{
    net::{IpAddr, TcpListener, UdpSocket},
    thread,
    time::{Duration, Instant},
};
use tokio_tungstenite::tungstenite::{
    accept_hdr,
    handshake::server::{Request, Response},
};
struct Services;
impl SendspinServices for Services {
    fn admission(&mut self, _: AdmissionProposal) -> Operation<Result<bool, DriverError>> {
        Box::pin(async { Ok(true) })
    }
    fn candidate(&mut self, _: AttemptId) -> Operation<Result<Candidate, DriverError>> {
        Box::pin(async { Ok(Candidate::new([90; 32])) })
    }
    fn commit(&mut self, _: AttemptId, _: RecordUpdate) -> Operation<Result<(), CommitOutcome>> {
        Box::pin(async { Ok(()) })
    }
}
fn interface() -> IpAddr {
    let s = UdpSocket::bind("0.0.0.0:0").unwrap();
    s.connect("192.0.2.1:9").unwrap();
    let ip = s.local_addr().unwrap().ip();
    assert!(matches!(ip,IpAddr::V4(p) if p.is_private()));
    ip
}
fn config() -> SessionConfig {
    SessionConfig {
        private: [5; 32],
        pairing_psk: [7; 32],
        connection_generation: 1,
        hello: json!({"name":"fixture","supported_roles":[]}),
        catalog: RecordCatalog::new(5).unwrap(),
        protected: vec![],
        limits: roleless_session_limits(),
    }
}
fn permit(listener: &TcpListener) -> SendspinConnectPermit {
    SendspinConnectPermit::new(
        OwnerId::new("fixture").unwrap(),
        1,
        1,
        Instant::now() + Duration::from_secs(10),
        &format!(
            "ws://fixture.invalid:{}/sendspin",
            listener.local_addr().unwrap().port()
        ),
        vec![listener.local_addr().unwrap().ip()],
    )
    .unwrap()
}
fn wait(observer: &SendspinObserver) -> Observation {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let o = observer.observation();
        if o.terminal.is_some() {
            return o;
        }
        assert!(Instant::now() < deadline, "terminal deadline: {o:?}");
        thread::sleep(Duration::from_millis(2));
    }
}
#[test]
fn a_clean_peer_close_receives_its_queued_response() {
    let listener = TcpListener::bind((interface(), 0)).unwrap();
    let p = permit(&listener);
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (close_tx, close_rx) = std::sync::mpsc::channel();
    let peer = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut socket = tokio_tungstenite::tungstenite::accept(stream).unwrap();
        assert!(socket.read().unwrap().is_text());
        ready_tx.send(()).unwrap();
        close_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        socket.close(None).unwrap();
        assert!(
            socket
                .read()
                .expect("valid peer Close must receive its queued response")
                .is_close()
        );
    });
    let mut worker = SendspinWorker::default();
    let handle = worker
        .start(
            p.clone(),
            &p,
            config(),
            Box::new(Services),
            DriverLimits::default(),
        )
        .unwrap();
    let observer = handle.observer();
    ready_rx.recv_timeout(Duration::from_secs(3)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while observer.observation().writes_completed != 1 || observer.observation().pending_write {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    close_tx.send(()).unwrap();
    assert_eq!(wait(&observer).terminal, Some(DriverError::PeerClosed));
    worker.retire().unwrap();
    peer.join().unwrap();
    assert_eq!(observer.observation().writes_completed, 1);
    assert!(observer.observation().joined);
}
#[test]
// Tungstenite requires its unboxed HTTP error response in this callback signature.
#[allow(clippy::result_large_err)]
fn exact_upgrade_owns_one_socket_and_rejects_invalid_session_input() {
    let listener = TcpListener::bind((interface(), 0)).unwrap();
    let p = permit(&listener);
    let server = thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let mut ws = accept_hdr(s, |request: &Request, response: Response| {
            assert_eq!(request.uri().path(), "/sendspin");
            assert!(
                request
                    .headers()
                    .get("host")
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with("fixture.invalid:")
            );
            for key in ["origin", "authorization", "cookie", "proxy-authorization"] {
                assert!(request.headers().get(key).is_none());
            }
            Ok(response)
        })
        .unwrap();
        let first = ws.read().unwrap();
        assert!(first.is_text());
        let v: serde_json::Value = serde_json::from_str(first.to_text().unwrap()).unwrap();
        assert_eq!(v["type"], "client/init");
        ws.send("invalid session input".into()).unwrap();
    });
    let mut worker = SendspinWorker::default();
    let handle = worker
        .start(
            p.clone(),
            &p,
            config(),
            Box::new(Services),
            DriverLimits::default(),
        )
        .unwrap();
    assert!(matches!(
        worker.start(
            p.clone(),
            &p,
            config(),
            Box::new(Services),
            DriverLimits::default()
        ),
        Err(DriverError::Busy)
    ));
    let observer = handle.observer();
    assert_eq!(wait(&observer).terminal, Some(DriverError::Protocol));
    worker.retire().unwrap();
    assert!(observer.observation().joined);
    server.join().unwrap();
}

#[test]
fn stale_grants_local_roles_and_zero_generations_fail_before_dial() {
    let listener = TcpListener::bind((interface(), 0)).unwrap();
    let p = permit(&listener);
    let changed = SendspinConnectPermit::new(
        OwnerId::new("fixture").unwrap(),
        1,
        2,
        Instant::now() + Duration::from_secs(10),
        &format!(
            "ws://fixture.invalid:{}/sendspin",
            listener.local_addr().unwrap().port()
        ),
        vec![listener.local_addr().unwrap().ip()],
    )
    .unwrap();
    let mut w = SendspinWorker::default();
    assert!(matches!(
        w.start(
            p.clone(),
            &changed,
            config(),
            Box::new(Services),
            DriverLimits::default()
        ),
        Err(DriverError::Admission)
    ));
    let mut c = config();
    c.hello["supported_roles"] = json!(["player@v1"]);
    assert!(matches!(
        w.start(
            p.clone(),
            &p,
            c,
            Box::new(Services),
            DriverLimits::default()
        ),
        Err(DriverError::Admission)
    ));
    assert!(
        SendspinConnectPermit::new(
            OwnerId::new("fixture").unwrap(),
            0,
            1,
            Instant::now() + Duration::from_secs(1),
            "ws://fixture:42/",
            vec![interface()]
        )
        .is_err()
    );
}

#[test]
fn roleless_hello_cannot_carry_a_player_format_offer() {
    let listener = TcpListener::bind((interface(), 0)).unwrap();
    let p = permit(&listener);
    let mut c = config();
    c.hello["player@v1_support"] = json!({"supported_formats":[{"codec":"pcm"}]});
    let mut w = SendspinWorker::default();
    assert!(matches!(
        w.start(
            p.clone(),
            &p,
            c,
            Box::new(Services),
            DriverLimits::default()
        ),
        Err(DriverError::Admission)
    ));
}
#[test]
fn cancellation_bypasses_full_producer_queue_and_observers_do_not_cancel() {
    let listener = TcpListener::bind((interface(), 0)).unwrap();
    let p = permit(&listener);
    let mut w = SendspinWorker::default();
    let h = w
        .start(
            p.clone(),
            &p,
            config(),
            Box::new(Services),
            DriverLimits {
                commands: 2,
                producer_bytes: 256,
                ..DriverLimits::default()
            },
        )
        .unwrap();
    let o = h.observer();
    drop(h.observer());
    let private = h
        .try_send(ApplicationMessage::json(
            "client/state",
            json!({"token":"SECRET-token"}),
        ))
        .unwrap_err();
    assert_eq!(private.error, DriverError::Protocol);
    assert!(!format!("{private:?}").contains("SECRET"));
    for _ in 0..2 {
        h.try_send(ApplicationMessage::json(
            "client/time",
            json!({"counter":1}),
        ))
        .unwrap();
    }
    let rejected = h
        .try_send(ApplicationMessage::json(
            "client/time",
            json!({"counter":2}),
        ))
        .unwrap_err();
    assert_eq!(rejected.error, DriverError::Budget);
    assert!(!rejected.message.body.is_empty());
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&rejected.message.body).unwrap()["payload"]["counter"],
        2
    );
    assert_eq!(o.observation().command_entries, 2);
    assert!(o.observation().terminal.is_none());
    h.cancel();
    w.retire().unwrap();
    let state = o.observation();
    assert_eq!(state.terminal, Some(DriverError::Cancelled));
    assert!(state.joined);
    assert_eq!(state.producer_bytes, 0);
    assert_eq!(state.command_entries, 0);
    assert!(state.producer_high_water <= 256);
}
#[test]
fn stalled_upgrade_expires_without_reset_and_cancels() {
    for cancel in [false, true] {
        let listener = TcpListener::bind((interface(), 0)).unwrap();
        let p = permit(&listener);
        let mut w = SendspinWorker::default();
        let h = w
            .start(
                p.clone(),
                &p,
                config(),
                Box::new(Services),
                DriverLimits {
                    upgrade_ms: 40,
                    ..DriverLimits::default()
                },
            )
            .unwrap();
        let o = h.observer();
        if cancel {
            h.cancel();
        }
        assert_eq!(
            wait(&o).terminal,
            Some(if cancel {
                DriverError::Cancelled
            } else {
                DriverError::Deadline
            })
        );
        w.retire().unwrap();
        assert!(o.observation().joined);
    }
}
#[test]
fn rejected_redirect_is_not_followed() {
    use std::io::{Read, Write};
    let listener = TcpListener::bind((interface(), 0)).unwrap();
    let p = permit(&listener);
    let server = thread::spawn(move || {
        let (mut s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut b = [0; 4096];
        assert!(s.read(&mut b).unwrap() > 0);
        s.write_all(
            b"HTTP/1.1 302 Found\r\nLocation: ws://127.0.0.1:42/\r\nContent-Length: 0\r\n\r\n",
        )
        .unwrap();
    });
    let mut w = SendspinWorker::default();
    let h = w
        .start(
            p.clone(),
            &p,
            config(),
            Box::new(Services),
            DriverLimits::default(),
        )
        .unwrap();
    let o = h.observer();
    assert_eq!(wait(&o).terminal, Some(DriverError::Upgrade));
    w.retire().unwrap();
    server.join().unwrap();
}
#[test]
fn malformed_and_oversized_frames_fail_closed() {
    use std::io::Write;
    let cases: Vec<(Vec<u8>, DriverError)> = vec![
        (vec![0xc1, 1, b'a'], DriverError::Protocol),
        (vec![0x81, 1, 0xff], DriverError::Protocol),
        (vec![0x81, 0x81, 0, 0, 0, 0, b'a'], DriverError::Protocol),
        (
            {
                let mut f = vec![0x82, 127];
                f.extend_from_slice(&100001u64.to_be_bytes());
                f
            },
            DriverError::Budget,
        ),
        (
            {
                let mut f = vec![0x02, 126];
                f.extend_from_slice(&60000u16.to_be_bytes());
                f.extend_from_slice(&vec![42; 60000]);
                f.extend_from_slice(&[0x80, 126]);
                f.extend_from_slice(&40001u16.to_be_bytes());
                f.extend_from_slice(&vec![42; 40001]);
                f
            },
            DriverError::Budget,
        ),
        (
            {
                let mut f = vec![0x82, 127];
                f.extend_from_slice(&65536u64.to_be_bytes());
                f.extend_from_slice(&vec![42; 65536]);
                f
            },
            DriverError::Budget,
        ),
    ];
    for (frame, expected) in cases {
        let listener = TcpListener::bind((interface(), 0)).unwrap();
        let p = permit(&listener);
        let server = thread::spawn(move || {
            let (s, _) = listener.accept().unwrap();
            s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            let mut ws = tokio_tungstenite::tungstenite::accept(s).unwrap();
            let _ = ws.read().unwrap();
            ws.get_mut().write_all(&frame).unwrap();
            thread::sleep(Duration::from_millis(25));
        });
        let mut w = SendspinWorker::default();
        let h = w
            .start(
                p.clone(),
                &p,
                config(),
                Box::new(Services),
                DriverLimits::default(),
            )
            .unwrap();
        let o = h.observer();
        assert_eq!(wait(&o).terminal, Some(expected));
        w.retire().unwrap();
        assert!(o.observation().joined);
        server.join().unwrap();
    }
}
#[test]
fn fragmented_message_waits_for_final_frame_and_services_interleaved_ping() {
    use std::io::Write;
    let listener = TcpListener::bind((interface(), 0)).unwrap();
    let p = permit(&listener);
    let (tx, rx) = std::sync::mpsc::channel();
    let (release, gate) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut ws = tokio_tungstenite::tungstenite::accept(s).unwrap();
        let _ = ws.read().unwrap();
        ws.get_mut()
            .write_all(&[0x01, 1, b'b', 0x89, 1, b'p'])
            .unwrap();
        assert!(ws.read().unwrap().is_pong());
        tx.send(()).unwrap();
        gate.recv().unwrap();
        ws.get_mut().write_all(&[0x80, 2, b'a', b'd']).unwrap();
    });
    let mut w = SendspinWorker::default();
    let h = w
        .start(
            p.clone(),
            &p,
            config(),
            Box::new(Services),
            DriverLimits::default(),
        )
        .unwrap();
    let o = h.observer();
    rx.recv_timeout(Duration::from_secs(3)).unwrap();
    assert!(o.observation().terminal.is_none());
    assert_eq!(o.observation().phase, SessionPhase::AwaitServerInit);
    release.send(()).unwrap();
    assert_eq!(wait(&o).terminal, Some(DriverError::Protocol));
    w.retire().unwrap();
    server.join().unwrap();
}
#[test]
fn cancellation_after_flush_sends_a_bounded_websocket_close() {
    let listener = TcpListener::bind((interface(), 0)).unwrap();
    let p = permit(&listener);
    let (tx, rx) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut ws = tokio_tungstenite::tungstenite::accept(s).unwrap();
        assert!(ws.read().unwrap().is_text());
        tx.send(()).unwrap();
        assert!(ws.read().unwrap().is_close());
    });
    let mut w = SendspinWorker::default();
    let h = w
        .start(
            p.clone(),
            &p,
            config(),
            Box::new(Services),
            DriverLimits::default(),
        )
        .unwrap();
    rx.recv_timeout(Duration::from_secs(3)).unwrap();
    h.cancel();
    w.retire().unwrap();
    server.join().unwrap();
}
#[test]
fn a_missing_pong_expires_the_absolute_control_deadline() {
    use std::io::Read;
    let listener = TcpListener::bind((interface(), 0)).unwrap();
    let p = permit(&listener);
    let server = thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut ws = tokio_tungstenite::tungstenite::accept(s).unwrap();
        assert!(ws.read().unwrap().is_text());
        let mut bytes = [0; 64];
        let n = ws.get_mut().read(&mut bytes).unwrap();
        assert!(n >= 6);
        assert_eq!(bytes[0], 0x89);
        while ws.get_mut().read(&mut bytes).unwrap_or(0) > 0 {}
    });
    let mut w = SendspinWorker::default();
    let h = w
        .start(
            p.clone(),
            &p,
            config(),
            Box::new(Services),
            DriverLimits {
                ping_ms: 20,
                pong_ms: 20,
                ..DriverLimits::default()
            },
        )
        .unwrap();
    let o = h.observer();
    assert_eq!(wait(&o).terminal, Some(DriverError::Deadline));
    w.retire().unwrap();
    server.join().unwrap();
}
#[test]
fn grant_expiry_closes_an_established_idle_connection() {
    let listener = TcpListener::bind((interface(), 0)).unwrap();
    let p = SendspinConnectPermit::new(
        OwnerId::new("fixture").unwrap(),
        1,
        1,
        Instant::now() + Duration::from_millis(150),
        &format!(
            "ws://fixture.invalid:{}/sendspin",
            listener.local_addr().unwrap().port()
        ),
        vec![listener.local_addr().unwrap().ip()],
    )
    .unwrap();
    let server = thread::spawn(move || {
        let (s, _) = listener.accept().unwrap();
        s.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut ws = tokio_tungstenite::tungstenite::accept(s).unwrap();
        assert!(ws.read().unwrap().is_text());
        assert!(ws.read().unwrap().is_close());
    });
    let mut w = SendspinWorker::default();
    let h = w
        .start(
            p.clone(),
            &p,
            config(),
            Box::new(Services),
            DriverLimits::default(),
        )
        .unwrap();
    let o = h.observer();
    assert_eq!(wait(&o).terminal, Some(DriverError::Expired));
    w.retire().unwrap();
    server.join().unwrap();
}
