// ABOUTME: Tests bounded independent client lanes against the actual owner.
// ABOUTME: Catches queue blocking and worker leaks when completions remain undrained.
mod common;
use common::*;
use earph_core::{control::Command, ids::RequestId};
use earph_desktop::{DispatchError, Dispatcher, Lane, Request};

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
