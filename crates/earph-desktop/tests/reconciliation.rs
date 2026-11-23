// ABOUTME: Reconciles real applied owner commands after deliberately lost transport replies.
// ABOUTME: Tests pending profile saves and safety progress without a fake application mode.
mod common;
use common::proxy::Proxy;
use common::*;
use earph_core::{control::Command, ids::RequestId};
use earph_desktop::{DesktopClient, Presentation, Reply, Request};
use std::{
    thread,
    time::{Duration, Instant},
};

fn settle(client: &mut DesktopClient, id: &RequestId) {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        client.poll();
        if !client.model().record(id).unwrap().in_flight {
            break;
        }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn a_lost_real_owner_reply_replays_exact_bytes_and_applies_the_mutation_once() {
    let owner = Owner::start();
    let proxy = Proxy::drop_first_append(owner.runtime.clone());
    let mut client = DesktopClient::attach(proxy.runtime.to_str().unwrap().to_owned()).unwrap();
    let request = Request::mutation(
        RequestId::new("lost-append").unwrap(),
        &owner.state(),
        Command::QueueAppend {
            path: "owned.wav".into(),
        },
    )
    .unwrap();
    let bytes = request.bytes().to_vec();
    let id = request.envelope().request_id.clone();
    client.submit(request).unwrap();
    settle(&mut client, &id);
    assert_eq!(
        owner.state().revision,
        1,
        "the actual owner applied before losing the reply"
    );
    assert_eq!(
        client.model().record(&id).unwrap().presentation,
        Presentation::Uncertain
    );
    client.reconcile(&id).unwrap();
    settle(&mut client, &id);
    assert_eq!(
        client.model().record(&id).unwrap().presentation,
        Presentation::Applied
    );
    let frames = proxy.frames.lock().unwrap();
    assert_eq!(frames.as_slice(), [bytes.clone(), bytes]);
    assert_eq!(owner.state().revision, 1);
    drop(frames);
    let (_, shutdown) = client.disconnect();
    shutdown.join().unwrap();
}

#[test]
fn profile_acceptance_stays_pending_until_the_same_saved_intent_receipt_applies() {
    let owner = Owner::start();
    let mut client = DesktopClient::attach(owner.runtime.to_str().unwrap().to_owned()).unwrap();
    let before = owner.state();
    let request = Request::mutation(
        RequestId::new("profile-save").unwrap(),
        &before,
        profile_payload(),
    )
    .unwrap();
    let id = request.envelope().request_id.clone();
    let bytes = request.bytes().to_vec();
    client.submit(request).unwrap();
    settle(&mut client, &id);
    assert_eq!(
        client.model().record(&id).unwrap().presentation,
        Presentation::Accepted
    );
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        client.reconcile(&id).unwrap();
        settle(&mut client, &id);
        if client.model().record(&id).unwrap().presentation != Presentation::Accepted {
            break;
        }
        assert!(Instant::now() < deadline);
    }
    let record = client.model().record(&id).unwrap();
    assert_eq!(record.presentation, Presentation::Applied);
    assert_eq!(record.request.bytes(), bytes);
    let Some(Ok(Reply::Receipt(receipt))) = &record.outcome else {
        panic!("saved receipt")
    };
    assert!(matches!(
        receipt.result,
        Some(earph_desktop::ResultView::ProfileSaved(_))
    ));
    let after = owner.state();
    assert!(after.muted);
    assert_eq!(after.software_gain_db, before.software_gain_db);
    assert_eq!(after.output, before.output);
    assert!(!after.output_consented);
    let (_, shutdown) = client.disconnect();
    shutdown.join().unwrap();
}

#[test]
fn safety_can_finish_while_an_ordinary_transport_request_is_held_elsewhere() {
    let owner = Owner::start();
    let proxy = Proxy::hold_first_capabilities(owner.runtime.clone());
    let mut client = DesktopClient::attach(proxy.runtime.to_str().unwrap().to_owned()).unwrap();
    client.submit(query(Command::Capabilities)).unwrap();
    proxy.entered.recv_timeout(Duration::from_secs(2)).unwrap();
    let safety = Request::mutation(
        RequestId::new("emergency-mute").unwrap(),
        &owner.state(),
        Command::Mute { muted: true },
    )
    .unwrap();
    let id = safety.envelope().request_id.clone();
    client.submit(safety).unwrap();
    settle(&mut client, &id);
    assert_eq!(
        client.model().record(&id).unwrap().presentation,
        Presentation::Applied
    );
    assert!(owner.state().muted);
    proxy.release();
    let (_, shutdown) = client.disconnect();
    shutdown.join().unwrap();
}

#[test]
fn replaying_an_old_retained_snapshot_cannot_replace_a_newer_observation() {
    let owner = Owner::start();
    let proxy = Proxy::drop_first_state(owner.runtime.clone());
    let mut client = DesktopClient::attach(proxy.runtime.to_str().unwrap().to_owned()).unwrap();
    let old = query(Command::State);
    let old_id = old.envelope().request_id.clone();
    client.submit(old).unwrap();
    settle(&mut client, &old_id);
    assert_eq!(
        client.model().record(&old_id).unwrap().presentation,
        Presentation::Uncertain
    );
    let mutation = Request::mutation(
        RequestId::new(unique()).unwrap(),
        &owner.state(),
        Command::QueueAppend {
            path: "fresh.wav".into(),
        },
    )
    .unwrap();
    owner.client().request(&mutation).unwrap();
    let fresh = query(Command::State);
    let fresh_id = fresh.envelope().request_id.clone();
    client.submit(fresh).unwrap();
    settle(&mut client, &fresh_id);
    assert_eq!(client.model().latest_state().unwrap().state.revision, 1);
    client.reconcile(&old_id).unwrap();
    settle(&mut client, &old_id);
    assert_eq!(
        client.model().record(&old_id).unwrap().presentation,
        Presentation::Applied
    );
    assert_eq!(client.model().latest_state().unwrap().state.revision, 1);
    assert_eq!(client.model().latest_state().unwrap().request_id, fresh_id);
    let (_, shutdown) = client.disconnect();
    shutdown.join().unwrap();
}

#[test]
fn disconnect_discards_queued_mutations_and_keeps_their_intent_uncertain() {
    let owner = Owner::start();
    let proxy = Proxy::hold_first_capabilities(owner.runtime.clone());
    let mut client = DesktopClient::attach(proxy.runtime.to_str().unwrap().to_owned()).unwrap();
    client.submit(query(Command::Capabilities)).unwrap();
    proxy.entered.recv_timeout(Duration::from_secs(2)).unwrap();
    let queued = Request::mutation(
        RequestId::new(unique()).unwrap(),
        &owner.state(),
        Command::QueueAppend {
            path: "must-stay-queued.wav".into(),
        },
    )
    .unwrap();
    let queued_id = queued.envelope().request_id.clone();
    client.submit(queued).unwrap();
    let (history, shutdown) = client.disconnect();
    assert!(!history.attached);
    assert_eq!(
        history.record(&queued_id).unwrap().presentation,
        Presentation::Uncertain
    );
    assert!(!history.record(&queued_id).unwrap().in_flight);
    proxy.release();
    shutdown.join().unwrap();
    assert_eq!(owner.state().revision, 0);
    let Reply::Receipt(reply) = owner
        .client()
        .request(&query(Command::QueueGet {
            offset: 0,
            limit: 64,
        }))
        .unwrap()
    else {
        panic!("queue")
    };
    let Some(earph_desktop::ResultView::Queue(queue)) = reply.result else {
        panic!("queue body")
    };
    assert!(queue.entries.is_empty());
    assert!(!owner.state().shutdown_requested);
}
