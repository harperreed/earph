// ABOUTME: Exercises rotation epoch installation across the actual WebSocket flush boundary.
// ABOUTME: Reuses the existing real Snow peer without mocked crypto or copied transcripts.
use super::*;
use crate::sendspin::session_fixture as fixture;
use serde_json::json;
use tokio::{io::AsyncReadExt, runtime::Builder};
use tokio_tungstenite::tungstenite::protocol::Role;

#[test]
fn new_epoch_waits_for_complete_rotation_reply_flush() {
    Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
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
            let previous_hash = session.handshake_hash().unwrap();
            let previous_generation = session.handshake_generation();
            let (handshake, payload) = peer.rotation([7; 32], "pr", None);
            peer.send_json(&mut session, "noise/handshake", payload, 6)
                .unwrap();
            let mut pending = PendingWrite {
                write: session.next_write(6).unwrap().unwrap(),
                started: false,
            };
            let reply = peer.read_json(&pending.write.bytes);
            peer.install(handshake, &reply);
            let new_hash = peer.hash;
            assert_ne!(previous_hash, new_hash);
            let (transport, mut consumer) = tokio::io::duplex(1);
            let mut socket =
                WebSocketStream::from_raw_socket(transport, Role::Client, Some(websocket_config()))
                    .await;
            assert!(
                pending
                    .poll(
                        &mut socket,
                        &mut Context::from_waker(std::task::Waker::noop()),
                        false
                    )
                    .is_pending()
            );
            assert!(pending.started);
            assert_eq!(session.handshake_hash(), Some(previous_hash));
            assert_eq!(session.handshake_generation(), previous_generation);
            assert!(session.next_write(7).unwrap().is_none());
            let drain = async move {
                let mut bytes = Vec::new();
                consumer.read_to_end(&mut bytes).await.unwrap();
                assert!(bytes.len() > 1);
            };
            let send = async {
                poll_fn(|cx| pending.poll(&mut socket, cx, false))
                    .await
                    .unwrap();
                session
                    .complete_write(pending.write.id, WriteResult::Complete, 8)
                    .unwrap();
                assert_eq!(session.handshake_hash(), Some(new_hash));
                assert_eq!(session.handshake_generation(), previous_generation + 1);
                assert!(
                    session
                        .complete_write(pending.write.id, WriteResult::Complete, 9)
                        .unwrap()
                        .is_empty()
                );
                assert_eq!(session.handshake_generation(), previous_generation + 1);
                drop(socket);
            };
            tokio::time::timeout(
                Duration::from_secs(2),
                futures_util::future::join(send, drain),
            )
            .await
            .unwrap();
        });
}
