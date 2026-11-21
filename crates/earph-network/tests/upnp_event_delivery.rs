// ABOUTME: Runs real Core-to-NOTIFY end-to-end flows on an owned private interface.
// ABOUTME: Checks independent callback authority, failover, FIN ordering, and cancellation.
#[path = "support/upnp_events.rs"]
mod fixture;
use earph_core::{control::*, renderer_events::*};
use fixture::*;
use std::{io::Write, time::Duration};
#[test]
fn notify_without_play_follows_response_fin_and_preserves_initial_snapshot() {
    let callback = listener();
    let mut owner = Owner::new(vec![origin(&callback)]);
    let (mut response, pending) =
        owner.pending(RendererService::AvTransport, &[url(&callback, "/events")]);
    let info = owner.admit(&pending);
    owner.stage(0, "latest metadata");
    assert!(
        owner
            .session
            .begin_renderer_delivery(&info.binding, owner.now())
            .is_err()
    );
    assert!(callback.accept().is_err());
    assert!(owner.surface.try_recv_response_completion().is_none());
    assert!(pending.respond(Ok(Some(&info))));
    let bytes = read_http(&mut response);
    assert!(bytes.starts_with(b"HTTP/1.1 200"));
    let ack = wait(|| owner.surface.try_recv_response_completion());
    assert!(ack.success);
    owner
        .session
        .acknowledge_renderer_subscription_response(&ack.binding, true, owner.now())
        .unwrap();
    let delivery = owner.dispatch(&info);
    assert_eq!(delivery.sequence, 0);
    assert!(delivery.snapshot.descriptor.is_none());
    let (mut notify, _) = wait(|| {
        owner.authorize();
        callback.accept().ok()
    });
    let bytes = read_http(&mut notify);
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.starts_with("NOTIFY /events HTTP/1.1\r\n"));
    assert!(text.contains("SEQ: 0\r\n"));
    assert!(text.contains("NO_MEDIA_PRESENT"));
    assert!(!text.contains("latest metadata"));
    assert!(text.contains(&format!("SID: {}\r\n", info.binding.sid)));
    notify
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
        .unwrap();
    owner.finish(owner.completion());
    assert!(owner.session.network_source().is_none());
    assert_eq!(owner.session.renderer_subscription_count(), 1);
    assert!(owner.surface.shutdown().joined);
}
#[test]
fn ordered_failover_uses_whole_urls_and_one_sequence() {
    let refused = listener();
    let failed_origin = origin(&refused);
    let failed_url = url(&refused, "/first");
    drop(refused);
    let callback = listener();
    let mut owner = Owner::new(vec![failed_origin, origin(&callback)]);
    let path = format!("/{}?opaque=@value", "x".repeat(7900));
    let mut urls = (0..7)
        .map(|i| format!("{failed_url}/{i}"))
        .collect::<Vec<_>>();
    urls.push(url(&callback, &path));
    let info = owner.subscribe(RendererService::RenderingControl, &urls);
    let delivery = owner.dispatch(&info);
    let (mut notify, _) = wait(|| {
        owner.authorize();
        callback.accept().ok()
    });
    let text = String::from_utf8(read_http(&mut notify)).unwrap();
    assert!(text.starts_with(&format!("NOTIFY {path} HTTP/1.1\r\n")));
    assert!(text.contains("SEQ: 0\r\n"));
    assert_eq!(delivery.sequence, 0);
    notify
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
        .unwrap();
    let result = owner.completion();
    assert_eq!(result.outcome, RendererDeliveryOutcome::Delivered);
    owner.finish(result);
    assert!(owner.surface.shutdown().joined);
}
#[test]
fn callback_revoke_cancels_a_real_owned_socket_then_joins_before_retirement() {
    let callback = listener();
    let mut owner = Owner::new(vec![origin(&callback)]);
    let info = owner.subscribe(RendererService::AvTransport, &[url(&callback, "/held")]);
    let delivery = owner.dispatch(&info);
    let (mut notify, _) = wait(|| {
        owner.authorize();
        callback.accept().ok()
    });
    read_http(&mut notify);
    assert_eq!(
        owner
            .local(Command::RendererCallbackGrantsRevoke {
                controller_id: info.binding.controller_id.clone(),
                expected_controller_generation: 1,
                expected_generation: 1
            })
            .status,
        ReceiptStatus::Applied
    );
    assert!(
        owner
            .session
            .renderer_delivery_authority(&delivery, owner.now())
            .is_none()
    );
    assert!(
        owner
            .surface
            .cancel_delivery(&delivery.binding, delivery.delivery_id)
    );
    let result = owner.completion();
    assert_eq!(result.outcome, RendererDeliveryOutcome::Cancelled);
    owner.finish(result);
    assert_eq!(owner.session.renderer_subscription_count(), 0);
    notify
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut byte = [0];
    use std::io::Read;
    assert_eq!(notify.read(&mut byte).unwrap(), 0);
    assert!(owner.surface.shutdown().joined);
}
#[test]
fn shared_ledger_refuses_seventh_large_sid_and_releases_after_retirement() {
    let callback = listener();
    let mut owner = Owner::new(vec![origin(&callback)]);
    let mut infos = Vec::new();
    for _ in 0..6 {
        let info = owner.subscribe(RendererService::AvTransport, &[url(&callback, "/events")]);
        let delivery = owner.dispatch(&info);
        let (mut socket, _) = wait(|| {
            owner.authorize();
            callback.accept().ok()
        });
        read_http(&mut socket);
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
            .unwrap();
        owner.finish(owner.completion());
        drop(delivery);
        infos.push(info);
    }
    assert_eq!(owner.surface.stats().reserved_bytes, 7_995_392);
    let (_socket, pending) =
        owner.pending(RendererService::AvTransport, &[url(&callback, "/events")]);
    assert!(pending.reserve_lease().is_err());
    assert_eq!(owner.session.renderer_subscription_count(), 6);
    assert!(pending.respond(Err(503)));
    assert!(owner.surface.stats().reserved_bytes <= 8 * 1024 * 1024);
    assert!(owner.surface.shutdown().joined);
}
#[test]
fn notify_412_retires_only_exact_sid_and_overflow_keeps_future_eligibility() {
    let callback = listener();
    let mut owner = Owner::new(vec![origin(&callback)]);
    let a = owner.subscribe(RendererService::AvTransport, &[url(&callback, "/a")]);
    let b = owner.subscribe(RendererService::RenderingControl, &[url(&callback, "/b")]);
    owner.dispatch(&a);
    let (mut socket, _) = wait(|| {
        owner.authorize();
        callback.accept().ok()
    });
    read_http(&mut socket);
    socket
        .write_all(b"HTTP/1.1 412 Precondition Failed\r\nContent-Length: 0\r\n\r\n")
        .unwrap();
    let result = owner.completion();
    assert_eq!(result.outcome, RendererDeliveryOutcome::InvalidSid);
    owner.finish(result);
    assert_eq!(owner.session.renderer_subscription_count(), 1);
    assert!(
        owner
            .session
            .begin_renderer_delivery(&a.binding, owner.now())
            .is_err()
    );
    owner.dispatch(&b);
    let (mut socket, _) = wait(|| {
        owner.authorize();
        callback.accept().ok()
    });
    read_http(&mut socket);
    socket
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2049\r\n\r\n")
        .unwrap();
    let result = owner.completion();
    assert_eq!(result.outcome, RendererDeliveryOutcome::ResponseOverflow);
    owner.finish(result);
    assert_eq!(owner.session.renderer_subscription_count(), 1);
    assert!(owner.surface.shutdown().joined);
}
#[test]
fn unsubscribe_preserves_authorized_initial_before_play_and_removes_latest() {
    let callback = listener();
    let mut owner = Owner::new(vec![origin(&callback)]);
    let info = owner.subscribe(
        RendererService::ConnectionManager,
        &[url(&callback, "/orphan")],
    );
    let c = owner.envelope(Command::RendererSubscription {
        controller_id: info.binding.controller_id.clone(),
        grant_generation: 1,
        callback_generation: 1,
        action: RendererSubscriptionAction::Unsubscribe {
            sid: info.binding.sid.clone(),
        },
    });
    let result = owner.session.submit_renderer_subscription_with_effects(
        c,
        owner.peer.into(),
        owner.now(),
        None,
        |_| Ok(()),
    );
    assert_eq!(result.receipt.status, ReceiptStatus::Applied);
    owner.dispatch(&info);
    let (mut socket, _) = wait(|| {
        owner.authorize();
        callback.accept().ok()
    });
    let text = String::from_utf8(read_http(&mut socket)).unwrap();
    assert!(text.contains("SEQ: 0\r\n"));
    assert!(text.contains("<CurrentConnectionIDs></CurrentConnectionIDs>"));
    socket
        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
        .unwrap();
    owner.finish(owner.completion());
    assert_eq!(owner.session.renderer_subscription_count(), 0);
    assert!(owner.surface.shutdown().joined);
}
#[test]
fn expired_absolute_delivery_deadline_cancels_without_resetting_failover() {
    let callback = listener();
    let mut owner = Owner::new(vec![origin(&callback)]);
    let info = owner.subscribe(
        RendererService::RenderingControl,
        &[url(&callback, "/held"), url(&callback, "/later")],
    );
    let delivery = owner
        .session
        .begin_renderer_delivery(&info.binding, owner.now())
        .unwrap();
    let late_now = delivery.deadline_monotonic_ns - 100_000_000;
    owner
        .surface
        .start_delivery(delivery.clone(), late_now)
        .unwrap();
    let (mut socket, _) = wait(|| {
        while let Some(attempt) = owner.surface.try_recv_callback_attempt() {
            let view = owner
                .session
                .renderer_delivery_authority(&attempt.delivery, late_now);
            assert!(attempt.authorize(view, late_now));
        }
        callback.accept().ok()
    });
    read_http(&mut socket);
    let result = wait(|| owner.surface.try_recv_delivery_completion());
    assert_eq!(result.outcome, RendererDeliveryOutcome::Timeout);
    owner.finish(result);
    assert!(
        callback.accept().is_err(),
        "failover must not receive a fresh thirty-second deadline"
    );
    assert!(owner.surface.shutdown().joined);
}
#[test]
fn fresh_authority_denial_prevents_connect_and_stale_view_cannot_resurrect_delivery() {
    let callback = listener();
    let mut owner = Owner::new(vec![origin(&callback)]);
    let info = owner.subscribe(RendererService::AvTransport, &[url(&callback, "/blocked")]);
    let delivery = owner.dispatch(&info);
    let attempt = wait(|| owner.surface.try_recv_callback_attempt());
    let mut view = owner
        .session
        .renderer_delivery_authority(&delivery, owner.now())
        .unwrap();
    view.binding.callback_generation += 1;
    assert!(attempt.authorize(Some(view), owner.now()));
    let result = owner.completion();
    assert_eq!(result.outcome, RendererDeliveryOutcome::Cancelled);
    owner.finish(result);
    assert!(callback.accept().is_err());
    assert!(owner.surface.shutdown().joined);
}
#[test]
fn large_opaque_metadata_crosses_real_notify_without_copy_or_truncation() {
    let callback = listener();
    let mut owner = Owner::new(vec![origin(&callback)]);
    let metadata = format!("{}& 🦀\t\r\n", "\"".repeat(12000));
    owner.stage(0, &metadata);
    let info = owner.subscribe(RendererService::AvTransport, &[url(&callback, "/large")]);
    let delivery = owner.dispatch(&info);
    let size = delivery.snapshot.encoded_size().unwrap();
    assert!(size > 240000 && size <= 262144);
    let (mut socket, _) = wait(|| {
        owner.authorize();
        callback.accept().ok()
    });
    let bytes = read_http(&mut socket);
    let end = bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    assert_eq!(bytes.len() - end, size);
    let mut expected = String::with_capacity(262144);
    delivery.snapshot.encode_into(&mut expected).unwrap();
    assert_eq!(&bytes[end..], expected.as_bytes());
    socket
        .write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\nOK\r\n0\r\n\r\n")
        .unwrap();
    let result = owner.completion();
    assert_eq!(result.outcome, RendererDeliveryOutcome::Delivered);
    owner.finish(result);
    assert!(owner.surface.shutdown().joined);
}
#[test]
fn failed_subscription_response_retires_exact_unconfirmed_initial_after_join() {
    let callback = listener();
    let mut owner = Owner::new(vec![origin(&callback)]);
    let (_socket, pending) =
        owner.pending(RendererService::AvTransport, &[url(&callback, "/never")]);
    let info = owner.admit(&pending);
    owner.surface.request_shutdown();
    let stats = wait(|| owner.surface.poll_shutdown());
    assert!(stats.joined);
    assert!(!pending.respond(Ok(Some(&info))));
    let ack = wait(|| owner.surface.try_recv_response_completion());
    assert!(!ack.success);
    assert_eq!(ack.binding, info.binding);
    owner
        .session
        .acknowledge_renderer_subscription_response(&ack.binding, false, owner.now())
        .unwrap();
    assert_eq!(owner.session.renderer_subscription_count(), 0);
    assert!(callback.accept().is_err());
}
#[test]
fn four_real_senders_cancel_and_join_before_shared_storage_retires() {
    let callback = listener();
    let mut owner = Owner::new(vec![origin(&callback)]);
    let mut deliveries = Vec::new();
    for _ in 0..4 {
        let info = owner.subscribe(RendererService::AvTransport, &[url(&callback, "/held")]);
        deliveries.push(owner.dispatch(&info));
    }
    let mut sockets = Vec::new();
    for _ in 0..4 {
        let (mut socket, _) = wait(|| {
            owner.authorize();
            callback.accept().ok()
        });
        read_http(&mut socket);
        sockets.push(socket);
    }
    assert_eq!(owner.session.renderer_subscription_count(), 4);
    let controller = deliveries[0].binding.controller_id.clone();
    assert_eq!(
        owner
            .local(Command::RendererCallbackGrantsRevoke {
                controller_id: controller,
                expected_controller_generation: 1,
                expected_generation: 1
            })
            .status,
        ReceiptStatus::Applied
    );
    let cancelled = owner
        .session
        .renderer_deliveries_needing_cancellation(owner.now());
    assert_eq!(cancelled.len(), 4);
    for delivery in &cancelled {
        assert!(
            owner
                .surface
                .cancel_delivery(&delivery.binding, delivery.delivery_id)
        );
    }
    drop(cancelled);
    for _ in 0..4 {
        let result = owner.completion();
        assert_eq!(result.outcome, RendererDeliveryOutcome::Cancelled);
        owner.finish(result);
    }
    drop(deliveries);
    assert_eq!(owner.session.renderer_subscription_count(), 0);
    wait(|| (owner.surface.stats().reserved_bytes == 262144).then_some(()));
    assert!(owner.surface.shutdown().joined);
}
fn soap(action: &str, args: &str) -> String {
    let namespace = "urn:schemas-upnp-org:service:AVTransport:1";
    let body = format!(
        "<s:Envelope xmlns:s=\"http://schemas.xmlsoap.org/soap/envelope/\" s:encodingStyle=\"http://schemas.xmlsoap.org/soap/encoding/\"><s:Body><u:{action} xmlns:u=\"{namespace}\">{args}</u:{action}></s:Body></s:Envelope>"
    );
    format!(
        "POST /upnp/avt/control HTTP/1.1\r\nHost: fixture\r\nContent-Type: text/xml\r\nSOAPACTION: \"{namespace}#{action}\"\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
}
#[test]
fn first_event_lease_cannot_consume_stop_credit_held_by_existing_ordinary_requests() {
    let callback = listener();
    let owner = Owner::new(vec![origin(&callback)]);
    let mut sockets = Vec::new();
    for _ in 0..2 {
        let mut socket = std::net::TcpStream::connect(owner.surface.local_addr()).unwrap();
        socket
            .write_all(soap("GetTransportInfo", "<InstanceID>0</InstanceID>").as_bytes())
            .unwrap();
        sockets.push(socket);
    }
    wait(|| (owner.surface.stats().queued == 2).then_some(()));
    let _actions = (0..2)
        .map(|_| wait(|| owner.surface.try_recv()))
        .collect::<Vec<_>>();
    let (_socket, pending) =
        owner.pending(RendererService::AvTransport, &[url(&callback, "/events")]);
    assert!(
        pending.reserve_lease().is_err(),
        "first subscription must preserve a Stop credit beyond all admitted ordinary requests"
    );
    assert!(pending.respond(Err(503)));
    assert!(owner.surface.shutdown().joined);
}
#[test]
fn redirect_response_does_not_change_callback_authority_or_event_body() {
    let first = listener();
    let second = listener();
    let redirect_target = listener();
    let mut owner = Owner::new(vec![origin(&first), origin(&second)]);
    let info = owner.subscribe(
        RendererService::RenderingControl,
        &[url(&first, "/first"), url(&second, "/second")],
    );
    owner.dispatch(&info);
    let (mut a, _) = wait(|| {
        owner.authorize();
        first.accept().ok()
    });
    let initial = read_http(&mut a);
    write!(
        a,
        "HTTP/1.1 302 Moved\r\nLocation: {}\r\nContent-Length: 0\r\n\r\n",
        url(&redirect_target, "/unauthorized")
    )
    .unwrap();
    let (mut b, _) = wait(|| {
        owner.authorize();
        second.accept().ok()
    });
    let failover = read_http(&mut b);
    let body = |bytes: Vec<u8>| {
        let end = bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
        bytes[end..].to_vec()
    };
    assert_eq!(body(initial), body(failover));
    assert!(redirect_target.accept().is_err());
    b.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n")
        .unwrap();
    let result = owner.completion();
    assert_eq!(result.outcome, RendererDeliveryOutcome::Delivered);
    owner.finish(result);
    assert!(owner.surface.shutdown().joined);
}
