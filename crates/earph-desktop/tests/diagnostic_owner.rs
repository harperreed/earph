// ABOUTME: Checks history and exact preview replay through the standalone owner and real shared client.
// ABOUTME: Keeps output unopened and joins the owned process through the existing fixture.
mod common;
use common::{process::ProcessOwner, query, unique};
use earph_core::{control::Command, diagnostics::DiagnosticBundle, ids::RequestId};
use earph_desktop::{OwnerClient, Reply, Request, ResultView};

#[test]
fn standalone_diagnostics_history_and_preview_preserve_exact_bytes_without_output() {
    let mut owner = ProcessOwner::start();
    println!(
        "owned owner PID: {}; test PID: {}",
        owner.child.id(),
        std::process::id()
    );
    let client = OwnerClient::attach(owner.runtime.to_str().unwrap().into()).unwrap();
    let Reply::Receipt(initial) = client.request(&query(Command::State)).unwrap() else {
        panic!("state receipt")
    };
    let state = initial.state().unwrap().clone();
    assert!(state.muted && !state.output_consented && state.output.is_none());
    let history = Request::diagnostic_query(
        RequestId::new(unique()).unwrap(),
        &state,
        Command::EventsGet { cursor: None },
    )
    .unwrap();
    let Reply::Receipt(history) = client.request(&history).unwrap() else {
        panic!("history receipt")
    };
    let Some(ResultView::Events(history)) = history.result else {
        panic!("history result")
    };
    assert!(history.report.events.is_empty() && !history.report.complete_audit_log);
    let request = Request::diagnostic_query(
        RequestId::new(unique()).unwrap(),
        &state,
        Command::DiagnosticsPreview {},
    )
    .unwrap();
    let reply = client.request(&request).unwrap();
    let Reply::Receipt(preview) = &reply else {
        panic!("preview receipt")
    };
    let Some(ResultView::DiagnosticPreview(preview)) = &preview.result else {
        panic!("preview result")
    };
    let bundle = DiagnosticBundle::decode(preview.report.bytes.as_bytes()).unwrap();
    assert_eq!(bundle.cut, bundle.history.cut);
    assert_eq!(bundle.cut, bundle.signal_path.cut);
    assert!(bundle.signal_path.requested.muted);
    assert!(bundle.signal_path.requested.output.is_none());
    assert!(bundle.signal_path.resolved.is_none() && bundle.signal_path.observed.is_none());
    assert_eq!(client.request(&request).unwrap().bytes(), reply.bytes());
    let Reply::Receipt(after) = client.request(&query(Command::State)).unwrap() else {
        panic!("state receipt")
    };
    let after = after.state().unwrap();
    assert_eq!(after.revision, state.revision);
    assert_eq!(after.epochs, state.epochs);
    assert!(after.muted && !after.output_consented && after.output.is_none());
    owner.stop().unwrap();
    println!(
        "owned owner numeric exit: {:?}",
        owner.child.try_wait().unwrap().unwrap().code()
    );
}
