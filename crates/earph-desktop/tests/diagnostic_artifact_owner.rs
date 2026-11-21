// ABOUTME: Reconciles a lost diagnostic export reply through the standalone Engine and shared desktop client.
// ABOUTME: Verifies exact retained files and replacement-owner inspection while leaving output unopened.
mod common;
use common::{process::ProcessOwner, proxy::Proxy, query, unique};
use earph_core::{control::Command, diagnostics::*, ids::RequestId};
use earph_desktop::{OwnerClient, Reply, ReplyStatus, Request, ResultView, StateView};
use std::{
    fs, thread,
    time::{Duration, Instant},
};

fn state(client: &OwnerClient) -> StateView {
    let Reply::Receipt(receipt) = client.request(&query(Command::State)).unwrap() else {
        panic!("state")
    };
    receipt.state().unwrap().clone()
}
fn settle(client: &OwnerClient, request: &Request) -> earph_desktop::ReceiptView {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let Reply::Receipt(receipt) = client.request(request).unwrap() else {
            panic!("artifact receipt")
        };
        if receipt.status != ReplyStatus::Accepted {
            return receipt;
        }
        assert!(Instant::now() < deadline, "artifact did not settle");
        thread::sleep(Duration::from_millis(2));
    }
}
fn artifact(
    client: &OwnerClient,
    state: &StateView,
    payload: Command,
) -> earph_desktop::ReceiptView {
    let request =
        Request::diagnostic_artifact(RequestId::new(unique()).unwrap(), state, payload).unwrap();
    settle(client, &request)
}

#[test]
fn lost_export_reply_reconciles_exact_files_and_a_replacement_owner_without_playback() {
    let mut owner = ProcessOwner::start();
    let proxy = Proxy::drop_first_diagnostic_export(owner.runtime.clone());
    let client = OwnerClient::attach(proxy.runtime.to_str().unwrap().into()).unwrap();
    let before = state(&client);
    assert!(before.muted && before.output.is_none() && !before.output_consented);
    let capture = Request::diagnostic_query(
        RequestId::new(unique()).unwrap(),
        &before,
        Command::DiagnosticsPreview {},
    )
    .unwrap();
    let Reply::Receipt(captured) = client.request(&capture).unwrap() else {
        panic!("preview")
    };
    let Some(ResultView::DiagnosticPreview(preview)) = captured.result else {
        panic!("typed preview")
    };
    let destination = owner.artifacts.canonicalize().unwrap().join("diagnostics");
    let request = Request::diagnostic_artifact(
        RequestId::new(unique()).unwrap(),
        &before,
        Command::DiagnosticsExport {
            directory: destination.to_str().unwrap().into(),
            expected_sha256: preview.report.sha256,
        },
    )
    .unwrap();
    fs::write(owner.artifacts.join("export-request.json"), request.bytes()).unwrap();
    assert!(
        client.request(&request).is_err(),
        "proxy must drop the actual export reply"
    );
    let receipt = settle(&client, &request);
    assert_eq!(receipt.status, ReplyStatus::Applied, "{:?}", receipt.error);
    let Some(ResultView::DiagnosticArtifact(published)) = &receipt.result else {
        panic!("typed publication")
    };
    assert_eq!(
        published.effect,
        DiagnosticFilesystemEffect::InstalledVerified
    );
    let payload = fs::read(destination.join("diagnostics.json")).unwrap();
    assert_eq!(payload, preview.report.bytes.as_bytes());
    let manifest_bytes = fs::read(destination.join("manifest.json")).unwrap();
    let mut names: Vec<_> = fs::read_dir(&destination)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    names.sort();
    assert_eq!(names, ["diagnostics.json", "manifest.json"]);
    let manifest = DiagnosticArtifactManifest::decode(&manifest_bytes).unwrap();
    manifest.validate_bundle(&payload).unwrap();
    let replay = client.request(&request).unwrap();
    assert_eq!(replay.bytes(), Reply::Receipt(receipt.clone()).bytes());
    let frames = proxy.frames.lock().unwrap();
    let exports: Vec<_> = frames
        .iter()
        .filter(|bytes| {
            serde_json::from_slice::<serde_json::Value>(bytes).unwrap()["payload"]["method"]
                == "diagnostics.export"
        })
        .collect();
    assert!(exports.len() >= 2);
    assert!(
        exports
            .iter()
            .all(|bytes| bytes.as_slice() == request.bytes())
    );
    drop(frames);
    let inspected = artifact(
        &client,
        &before,
        Command::DiagnosticsInspect {
            directory: destination.to_str().unwrap().into(),
            expected_sha256: preview.report.sha256,
        },
    );
    assert_eq!(inspected.status, ReplyStatus::Applied);
    let Some(ResultView::DiagnosticArtifact(inspected)) = inspected.result else {
        panic!("typed inspection")
    };
    assert_eq!(inspected.effect, DiagnosticFilesystemEffect::Unchanged);
    assert_eq!(inspected.inspection, Some(DiagnosticInspection::Matching));
    assert_eq!(
        fs::read(destination.join("diagnostics.json")).unwrap(),
        payload
    );
    assert_eq!(
        fs::read(destination.join("manifest.json")).unwrap(),
        manifest_bytes
    );
    let cancelled = artifact(
        &client,
        &before,
        Command::DiagnosticsCancel {
            request_id: request.envelope().request_id.clone(),
        },
    );
    assert_eq!(cancelled.status, ReplyStatus::Applied);
    let Some(ResultView::DiagnosticCancel(cancelled)) = cancelled.result else {
        panic!("typed cancellation")
    };
    assert_eq!(cancelled.state, DiagnosticCancelState::AlreadyTerminal);
    let after = state(&client);
    assert_eq!(after.revision, before.revision);
    assert_eq!(after.epochs, before.epochs);
    assert!(after.muted && after.output.is_none() && !after.output_consented);
    owner.stop().unwrap();
    assert_eq!(owner.child.try_wait().unwrap().unwrap().code(), Some(0));
    drop(proxy);

    let mut replacement = ProcessOwner::start();
    let client = OwnerClient::attach(replacement.runtime.to_str().unwrap().into()).unwrap();
    let current = state(&client);
    assert_ne!(current.owner_id, before.owner_id);
    let Reply::Receipt(stale) = client.request(&request).unwrap() else {
        panic!("stale owner refusal")
    };
    assert_eq!(stale.status, ReplyStatus::Rejected);
    let inspected = artifact(
        &client,
        &current,
        Command::DiagnosticsInspect {
            directory: destination.to_str().unwrap().into(),
            expected_sha256: preview.report.sha256,
        },
    );
    assert_eq!(inspected.status, ReplyStatus::Applied);
    let Some(ResultView::DiagnosticArtifact(inspected)) = inspected.result else {
        panic!("replacement inspection")
    };
    assert_eq!(inspected.inspection, Some(DiagnosticInspection::Matching));
    let after = state(&client);
    assert_eq!(after.revision, current.revision);
    assert!(after.muted && after.output.is_none() && !after.output_consented);
    replacement.stop().unwrap();
    assert_eq!(
        replacement.child.try_wait().unwrap().unwrap().code(),
        Some(0)
    );
    println!(
        "Diagnostic export artifacts retained: {}",
        owner.artifacts.display()
    );
}
