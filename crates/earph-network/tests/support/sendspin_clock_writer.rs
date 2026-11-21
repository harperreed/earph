// ABOUTME: Exercises late probe stamping behind session queues and real socket backpressure.
// ABOUTME: Verifies no emission or Noise write begins before WebSocket readiness.
use super::*;
use crate::sendspin::session_fixture as fixture;
use futures_util::StreamExt;
use serde_json::json;
use tokio_tungstenite::tungstenite::protocol::Role;
#[test]
fn clock_probe_samples_after_application_queue_and_socket_readiness() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let domain = super::super::MonotonicDomain::new(1).unwrap();
            let mut clock = ClockDriver::new(ClockConfiguration {
                domain: domain.clone(),
                policy: earph_sendspin::timing::TimePolicy::new(250_000, 9_000_000, 20_000_000)
                    .unwrap(),
            });
            let (mut session, mut peer) = fixture::initial();
            let events = peer
                .send_json(&mut session, "server/activate", json!({"activities":[]}), 4)
                .unwrap();
            session.admit(fixture::admission(&events), true, 5).unwrap();
            tokio::time::sleep(Duration::from_millis(6)).await;
            clock
                .advance(session.clock_context(), domain.sample().unwrap())
                .unwrap();
            session
                .send(
                    ApplicationMessage::json("client/goodbye", json!({"reason":"done"})),
                    domain.milliseconds(),
                )
                .unwrap();
            let (transport, remote) = tokio::io::duplex(1);
            let mut socket =
                WebSocketStream::from_raw_socket(transport, Role::Client, Some(websocket_config()))
                    .await;
            let mut cx = Context::from_waker(std::task::Waker::noop());
            assert!(matches!(
                poll_clock_write(&mut socket, &mut cx, &mut session, &mut clock),
                Poll::Ready(Ok(None))
            ));
            let queued = session.next_write(domain.milliseconds()).unwrap().unwrap();
            assert_eq!(peer.read_json(&queued.bytes)["type"], "client/goodbye");
            session
                .complete_write(queued.id, WriteResult::Complete, domain.milliseconds())
                .unwrap();
            Pin::new(&mut socket)
                .start_send(Message::Pong(vec![1; 32].into()))
                .unwrap();
            assert!(poll_clock_write(&mut socket, &mut cx, &mut session, &mut clock).is_pending());
            let waiting = domain.sample().unwrap();
            tokio::time::sleep(Duration::from_millis(10)).await;
            let blocked = clock
                .advance(session.clock_context(), domain.sample().unwrap())
                .unwrap()
                .unwrap();
            assert_eq!(blocked.snapshot.diagnostics.emitted_requests, 0);
            assert!(session.next_write(domain.milliseconds()).unwrap().is_none());
            let receiver = async {
                let mut remote = WebSocketStream::from_raw_socket(remote, Role::Server, None).await;
                assert!(matches!(remote.next().await, Some(Ok(Message::Pong(_)))));
                let Some(Ok(Message::Binary(cipher))) = remote.next().await else {
                    panic!("clock ciphertext expected")
                };
                let message = peer.read_json(&cipher);
                assert_eq!(message["type"], "client/time");
                message["payload"]["client_transmitted"].as_i64().unwrap()
            };
            let sender = async {
                let mut pending =
                    poll_fn(|cx| poll_clock_write(&mut socket, cx, &mut session, &mut clock))
                        .await
                        .unwrap()
                        .unwrap();
                assert!(pending.started);
                assert_eq!(
                    clock
                        .advance(session.clock_context(), domain.sample().unwrap())
                        .unwrap()
                        .unwrap()
                        .snapshot
                        .diagnostics
                        .emitted_requests,
                    1
                );
                poll_fn(|cx| pending.poll(&mut socket, cx, false))
                    .await
                    .unwrap();
                session
                    .complete_write(
                        pending.write.id,
                        WriteResult::Complete,
                        domain.milliseconds(),
                    )
                    .unwrap();
            };
            let (t1, ()) = tokio::time::timeout(
                Duration::from_secs(2),
                futures_util::future::join(receiver, sender),
            )
            .await
            .unwrap();
            assert!(
                t1 > waiting.microseconds() + 9_000,
                "T1 must follow the blocked interval"
            );
        });
}
