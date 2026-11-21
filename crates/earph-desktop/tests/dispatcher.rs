// ABOUTME: Tests bounded independent client lanes against the actual owner.
// ABOUTME: Catches queue blocking and worker leaks when completions remain undrained.
mod common;
use common::*;
use earph_core::{control::Command, ids::RequestId};
use earph_desktop::{DispatchError, Dispatcher, Lane, Request};
use std::{
    thread,
    time::{Duration, Instant},
};

#[test]
fn two_safety_requests_admit_while_the_worker_cannot_dequeue() {
    let owner = Owner::start();
    let dispatcher = Dispatcher::attach(owner.runtime.to_str().unwrap().into()).unwrap();
    // Four completions fill the bounded result channel. The fifth blocks its producer.
    for _ in 0..5 {
        let before = owner.state();
        let request = Request::mutation(
            RequestId::new(unique()).unwrap(),
            &before,
            Command::Mute { muted: true },
        )
        .unwrap();
        assert_eq!(dispatcher.try_submit(request), Ok(()));
        let deadline = Instant::now() + Duration::from_secs(3);
        while owner.state().revision == before.revision {
            assert!(Instant::now() < deadline, "owned mute did not apply");
            thread::sleep(Duration::from_millis(1));
        }
    }
    let state = owner.state();
    let request =
        |command| Request::mutation(RequestId::new(unique()).unwrap(), &state, command).unwrap();
    let first = dispatcher.try_submit(request(Command::Mute { muted: true }));
    let second = dispatcher.try_submit(request(Command::Stop));
    let third = dispatcher.try_submit(request(Command::Mute { muted: true }));
    // Release the completion consumer before asserting, including on the failing baseline.
    dispatcher.disconnect().join().unwrap();
    assert_eq!(first, Ok(()));
    assert_eq!(
        second,
        Ok(()),
        "both controller safety slots need admission without a dequeue"
    );
    assert_eq!(
        third,
        Err(DispatchError::Full(Lane::Safety)),
        "the safety queue must stay bounded"
    );
    let after = owner.state();
    assert!(after.muted && after.output.is_none() && !after.output_consented);
    assert!(!after.shutdown_requested);
}

#[test]
fn saturated_ordinary_work_does_not_consume_safety_admission_or_leak_workers() {
    let owner = Owner::start();
    let dispatcher = Dispatcher::attach(owner.runtime.to_str().unwrap().to_owned()).unwrap();
    let mut ordinary_full = false;
    for _ in 0..8 {
        match dispatcher.try_submit(query(Command::Capabilities)) {
            Ok(()) => {}
            Err(DispatchError::Full(Lane::Ordinary)) => {
                ordinary_full = true;
                break;
            }
            value => panic!("unexpected ordinary admission: {value:?}"),
        }
    }
    assert!(
        ordinary_full,
        "ordinary requests must remain bounded without a consumer"
    );
    let state = owner.state();
    let safety = || {
        Request::mutation(
            RequestId::new(unique()).unwrap(),
            &state,
            Command::Mute { muted: true },
        )
        .unwrap()
    };
    assert_eq!(dispatcher.try_submit(safety()), Ok(()));
    let mut safety_full = false;
    for _ in 0..8 {
        match dispatcher.try_submit(safety()) {
            Ok(()) => {}
            Err(DispatchError::Full(Lane::Safety)) => {
                safety_full = true;
                break;
            }
            value => panic!("unexpected safety admission: {value:?}"),
        }
    }
    assert!(safety_full, "safety requests must also remain bounded");
    // There is deliberately no completion consumer. Disconnect must unblock worker sends.
    dispatcher.disconnect().join().unwrap();
    assert!(!owner.state().shutdown_requested);
    assert_eq!(owner.state().output, None);
}
