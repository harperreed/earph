// ABOUTME: Exercises bounded GUI capture when real ordinary IPC already fills its dispatcher.
// ABOUTME: Reuses the actual owner and held-read transport without introducing a product test mode.
use super::test_common as common;
use super::*;
#[path = "../gui_receipt_fixture/mod.rs"]
mod receipt_fixture;
use common::{Owner, unique};
use eframe::egui;

struct ReleaseRead<'a>(&'a receipt_fixture::ReceiptTransport);
impl Drop for ReleaseRead<'_> {
    fn drop(&mut self) {
        self.0.release_state();
    }
}

#[test]
fn ordinary_send_backpressure_retains_an_unsent_diagnostic_capture() {
    let mut owner = Owner::start();
    let transport = receipt_fixture::ReceiptTransport::observation(owner.runtime.clone());
    let mut gui = GuiController::new();
    let _release = ReleaseRead(&transport);
    gui.handle(UiIntent::Attach {
        runtime: transport.runtime.to_str().unwrap().into(),
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    while !gui.connected() || gui.busy() {
        gui.tick(Instant::now());
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    let basis = Arc::new(gui.state().unwrap().clone());
    transport.hold_next_state();
    let first =
        Request::query(request_id().unwrap(), basis.zone_id.clone(), Command::State).unwrap();
    gui.client.as_mut().unwrap().submit(first).unwrap();
    while !transport.state_held() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    let queued =
        Request::diagnostic_query(request_id().unwrap(), &basis, Command::MetricsGet {}).unwrap();
    let reconciled_id = queued.envelope().request_id.clone();
    let original_bytes = queued.bytes().to_vec();
    gui.client.as_mut().unwrap().submit(queued).unwrap();
    gui.handle(UiIntent::DiagnosticAction {
        basis,
        command: Command::SignalPathGet {},
    });
    let retained = matches!(gui.waiting_queue_edit(), Some(Command::SignalPathGet {}));
    let error = gui.error().map(str::to_owned);
    transport.release_state();
    assert!(
        retained,
        "ordinary backpressure discarded an unsent capture: {error:?}"
    );
    while gui.diagnostic_signal_path().is_none() || gui.queue_edit_pending() {
        gui.tick(Instant::now());
        assert!(Instant::now() < deadline, "{:?}", gui.error());
        thread::sleep(Duration::from_millis(2));
    }
    while gui.busy() {
        gui.tick(Instant::now());
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    transport.hold_next_state();
    let basis = gui.state().unwrap();
    let first =
        Request::query(request_id().unwrap(), basis.zone_id.clone(), Command::State).unwrap();
    let queued =
        Request::diagnostic_query(request_id().unwrap(), basis, Command::MetricsGet {}).unwrap();
    gui.client.as_mut().unwrap().submit(first).unwrap();
    while !transport.state_held() {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    gui.client.as_mut().unwrap().submit(queued).unwrap();
    gui.handle(UiIntent::Reconcile(reconciled_id.clone()));
    let mut app = crate::gui::GuiApp::new(gui);
    let _release_ui = ReleaseRead(&transport);
    let context = egui::Context::default();
    context.enable_accesskit();
    let mut output = context.run_ui(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1800.0, 10000.0),
            )),
            ..Default::default()
        },
        |ui| app.draw(ui),
    );
    output.textures_delta.clear();
    transport.release_state();
    assert_eq!(
        app.controller.waiting_reconciliation(),
        Some(&reconciled_id)
    );
    let tree = output.platform_output.accesskit_update.unwrap();
    let waiting = format!(
        "Exact reconciliation waiting (not resent): {}",
        reconciled_id.as_str()
    );
    let names: Vec<_> = tree
        .nodes
        .iter()
        .filter_map(|(_, node)| node.label().or_else(|| node.value()))
        .collect();
    assert!(
        names.contains(&waiting.as_str()),
        "waiting caption missing from actual tree: {names:?}"
    );
    for label in ["Mute", "Stop"] {
        assert!(
            tree.nodes
                .iter()
                .any(|(_, node)| node.label() == Some(label) && !node.is_disabled())
        );
    }
    let mut gui = app.controller;
    while gui.waiting_reconciliation().is_some()
        || gui.model().record(&reconciled_id).unwrap().in_flight
    {
        gui.tick(Instant::now());
        assert!(Instant::now() < deadline, "{:?}", gui.error());
        thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(
        gui.model().record(&reconciled_id).unwrap().request.bytes(),
        original_bytes
    );
    gui.handle(UiIntent::CloseController);
    while !gui.ready_to_close() {
        gui.tick(Instant::now());
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(2));
    }
    owner.stop();
}
