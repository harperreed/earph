// ABOUTME: Exercises desktop presentation through real owner receipts and revisions.
// ABOUTME: Catches optimistic application, stale repair, unbounded history, and shutdown on detach.
mod common;
use common::*;
use earph_core::{control::Command, ids::RequestId};
use earph_desktop::{DesktopClient, Presentation, Request, Submission};
use std::{
    thread,
    time::{Duration, Instant},
};

fn settle(client: &mut DesktopClient, id: &RequestId) {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        client.poll();
        if client
            .model()
            .record(id)
            .is_some_and(|record| !record.in_flight)
        {
            return;
        }
        assert!(Instant::now() < deadline, "desktop request did not settle");
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn snapshot_coalescing_preserves_each_explicit_zone() {
    use earph_core::{ErrorCode, ids::ZoneId};
    use earph_desktop::{DispatchError, Lane, Reply, SubmitError};
    let owner = Owner::start();
    let mut client = DesktopClient::attach(owner.runtime.to_str().unwrap().to_owned()).unwrap();
    let main = query(Command::State);
    let main_id = main.envelope().request_id.clone();
    client.submit(main).unwrap();
    assert_eq!(
        client.submit(query(Command::State)).unwrap(),
        Submission::Coalesced(main_id.clone())
    );
    let other_id = RequestId::new(unique()).unwrap();
    let other = Request::query(
        other_id.clone(),
        ZoneId::new("other").unwrap(),
        Command::State,
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        match client.submit(other.clone()) {
            Ok(Submission::Admitted) => break,
            Err(SubmitError::Dispatch(DispatchError::Full(Lane::Ordinary))) => {
                assert!(client.model().record(&other_id).is_none());
                assert!(client.model().record(&main_id).unwrap().in_flight);
                assert!(Instant::now() < deadline, "ordinary queue did not advance");
                thread::yield_now();
            }
            outcome => panic!("wrong-zone query changed admission: {outcome:?}"),
        }
    }
    settle(&mut client, &main_id);
    settle(&mut client, &other_id);
    let record = client.model().record(&other_id).unwrap();
    assert_eq!(record.request.envelope().zone_id.as_str(), "other");
    assert_eq!(record.presentation, Presentation::Rejected);
    let Some(Ok(Reply::Receipt(receipt))) = &record.outcome else {
        panic!("missing actual owner rejection");
    };
    assert_eq!(
        receipt.error.as_ref().unwrap().code,
        ErrorCode::StaleSession
    );
    assert_eq!(
        client
            .model()
            .latest_state()
            .unwrap()
            .state
            .zone_id
            .as_str(),
        "main"
    );
    let (_, shutdown) = client.disconnect();
    shutdown.join().unwrap();
}

#[test]
fn snapshot_coalescing_and_stale_rejection_never_invent_applied_state() {
    let owner = Owner::start();
    let mut client = DesktopClient::attach(owner.runtime.to_str().unwrap().to_owned()).unwrap();
    let first = query(Command::State);
    let id = first.envelope().request_id.clone();
    assert_eq!(client.submit(first).unwrap(), Submission::Admitted);
    assert_eq!(
        client.model().record(&id).unwrap().presentation,
        Presentation::Queued
    );
    assert_eq!(
        client.submit(query(Command::State)).unwrap(),
        Submission::Coalesced(id.clone())
    );
    settle(&mut client, &id);
    let before = client.model().latest_state().unwrap().state.clone();
    assert_eq!(before.revision, 0);
    let append = Request::mutation(
        RequestId::new(unique()).unwrap(),
        &before,
        Command::QueueAppend {
            path: "first.wav".into(),
        },
    )
    .unwrap();
    let append_id = append.envelope().request_id.clone();
    client.submit(append).unwrap();
    settle(&mut client, &append_id);
    assert_eq!(
        client.model().record(&append_id).unwrap().presentation,
        Presentation::Applied
    );
    assert_eq!(
        client.model().latest_state().unwrap().state.revision,
        0,
        "a command receipt is not a snapshot"
    );
    let stale = Request::mutation(
        RequestId::new(unique()).unwrap(),
        &before,
        Command::QueueAppend {
            path: "stale.wav".into(),
        },
    )
    .unwrap();
    let stale_id = stale.envelope().request_id.clone();
    client.submit(stale).unwrap();
    settle(&mut client, &stale_id);
    let rejected = client.model().record(&stale_id).unwrap();
    assert_eq!(rejected.presentation, Presentation::Rejected);
    assert_eq!(rejected.request.envelope().expected_revision, Some(0));
    assert!(client.reconcile(&stale_id).is_err());
    let (_, shutdown) = client.disconnect();
    shutdown.join().unwrap();
    assert_eq!(owner.state().revision, 1);
    assert!(owner.state().muted);
    assert!(!owner.state().shutdown_requested);
}

#[test]
fn completed_read_history_stays_bounded_and_does_not_block_new_queries() {
    let owner = Owner::start();
    let mut client = DesktopClient::attach(owner.runtime.to_str().unwrap().to_owned()).unwrap();
    let first = query(Command::Capabilities);
    let first_id = first.envelope().request_id.clone();
    client.submit(first).unwrap();
    settle(&mut client, &first_id);
    for _ in 0..40 {
        let request = query(Command::Capabilities);
        let id = request.envelope().request_id.clone();
        client.submit(request).unwrap();
        settle(&mut client, &id);
    }
    assert!(client.model().records().len() <= 16);
    assert!(client.model().record(&first_id).is_none());
    let (_, shutdown) = client.disconnect();
    shutdown.join().unwrap();
}

#[test]
fn owner_restart_blocks_reconciliation_of_an_old_target_without_retargeting_it() {
    let mut owner = Owner::start();
    let old_state = owner.state();
    let runtime = owner.runtime.clone();
    let mut client = DesktopClient::attach(runtime.to_str().unwrap().to_owned()).unwrap();
    let read = query(Command::State);
    let id = read.envelope().request_id.clone();
    client.submit(read).unwrap();
    settle(&mut client, &id);
    owner.stop();
    let request = Request::mutation(
        RequestId::new(unique()).unwrap(),
        &old_state,
        Command::QueueAppend {
            path: "uncertain.wav".into(),
        },
    )
    .unwrap();
    let retained = request.bytes().to_vec();
    let request_id = request.envelope().request_id.clone();
    client.submit(request).unwrap();
    settle(&mut client, &request_id);
    assert_eq!(
        client.model().record(&request_id).unwrap().presentation,
        Presentation::Uncertain
    );
    let restarted = Owner::at(runtime);
    let restarted_revision = restarted.state().revision;
    let read = query(Command::State);
    let read_id = read.envelope().request_id.clone();
    client.submit(read).unwrap();
    settle(&mut client, &read_id);
    assert_ne!(
        client.model().latest_state().unwrap().state.owner_id,
        old_state.owner_id
    );
    assert!(client.reconcile(&request_id).is_err());
    let record = client.model().record(&request_id).unwrap();
    assert!(record.owner_changed);
    assert_eq!(record.request.bytes(), retained);
    let (_, shutdown) = client.disconnect();
    shutdown.join().unwrap();
    assert_eq!(restarted.state().revision, restarted_revision);
}

#[test]
fn unresolved_ordinary_history_cannot_exhaust_reserved_safety_admission() {
    let runtime = std::path::PathBuf::from("/tmp").join(unique());
    let mut client = DesktopClient::attach(runtime.to_str().unwrap().to_owned()).unwrap();
    let mut unresolved = Vec::new();
    loop {
        let request = query(Command::Capabilities);
        let id = request.envelope().request_id.clone();
        match client.submit(request) {
            Ok(_) => {
                settle(&mut client, &id);
                unresolved.push(id);
            }
            Err(earph_desktop::SubmitError::HistoryFull) => break,
            other => panic!("unexpected admission: {other:?}"),
        }
        assert!(unresolved.len() <= 16, "history must remain bounded");
    }
    assert!(!unresolved.is_empty());
    let owner = Owner::at(runtime);
    let emergency = Request::mutation(
        RequestId::new(unique()).unwrap(),
        &owner.state(),
        Command::Mute { muted: true },
    )
    .unwrap();
    let emergency_id = emergency.envelope().request_id.clone();
    client
        .submit(emergency)
        .expect("ordinary uncertainty must leave safety history capacity");
    settle(&mut client, &emergency_id);
    assert_eq!(
        client.model().record(&emergency_id).unwrap().presentation,
        Presentation::Applied
    );
    for id in unresolved {
        assert_eq!(
            client.model().record(&id).unwrap().presentation,
            Presentation::Uncertain
        );
    }
    assert!(client.model().records().len() <= 16);
    let (_, shutdown) = client.disconnect();
    shutdown.join().unwrap();
}
