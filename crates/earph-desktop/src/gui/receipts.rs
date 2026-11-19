// ABOUTME: Presents bounded actual receipts without collapsing acceptance and application.
// ABOUTME: Offers exact reconciliation and retains uncertain outcomes as local details.
use super::{GuiController, UiIntent};
use crate::{Presentation, Reply};
use eframe::egui;

pub(super) fn show(ui: &mut egui::Ui, controller: &mut GuiController) {
    ui.heading("Requests and receipts");
    ui.label("Accepted means pending owner work. Applied describes the operation, not physical audio presentation.");
    let mut reconcile = None;
    for record in controller.receipts() {
        let id = &record.request.envelope().request_id;
        let state = if record.presentation == Presentation::Uncertain {
            "Outcome unknown".into()
        } else {
            format!("{:?}", record.presentation)
        };
        ui.collapsing(
            format!(
                "{} · {state}{}",
                id.as_str(),
                if record.in_flight {
                    " · delivery pending"
                } else {
                    ""
                }
            ),
            |ui| {
                ui.label(format!("Command: {:?}", record.request.envelope().payload));
                if record.owner_changed {
                    ui.label("Owner changed. Reconciliation of the old target is blocked.");
                }
                match record.outcome {
                    Some(Ok(Reply::Receipt(receipt))) => {
                        ui.label(format!(
                            "Status {:?}; revision {}; epochs {:?}; activation frame {}",
                            receipt.status,
                            receipt.revision,
                            receipt.epochs,
                            receipt
                                .activation_frame
                                .map_or_else(|| "unknown".into(), |n| n.to_string())
                        ));
                        if let Some(error) = &receipt.error {
                            ui.label(format!("{:?}: {}", error.code, error.message));
                        }
                    }
                    Some(Ok(Reply::OwnerError(error))) => {
                        ui.label(format!(
                            "Top-level owner error {:?}: {}",
                            error.error.code, error.error.message
                        ));
                    }
                    Some(Err(error)) => {
                        ui.label(format!("Client / protocol failure: {error:?}"));
                    }
                    None => {
                        ui.label("No owner outcome received.");
                    }
                }
                if controller.attached()
                    && !record.in_flight
                    && !record.owner_changed
                    && matches!(
                        record.presentation,
                        Presentation::Accepted | Presentation::Uncertain
                    )
                    && ui.button("Reconcile exact request").clicked()
                {
                    reconcile = Some(id.clone());
                }
                ui.collapsing("Exact local request", |ui| {
                    ui.label(String::from_utf8_lossy(record.request.bytes()));
                });
                if let Some(Ok(reply)) = record.outcome {
                    ui.collapsing("Exact local reply", |ui| {
                        ui.label(String::from_utf8_lossy(reply.bytes()));
                    });
                }
            },
        );
    }
    if let Some(id) = reconcile {
        controller.handle(UiIntent::Reconcile(id));
    }
}
