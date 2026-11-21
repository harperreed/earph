// ABOUTME: Presents bounded actual receipts without collapsing acceptance and application.
// ABOUTME: Offers exact reconciliation and retains uncertain outcomes as local details.
use super::{GuiController, UiIntent};
use crate::{Presentation, Reply};
use eframe::egui;

fn request_header(
    id: &earph_core::ids::RequestId,
    state: &str,
    in_flight: bool,
) -> egui::CollapsingHeader {
    egui::CollapsingHeader::new(format!(
        "{} · {state}{}",
        id.as_str(),
        if in_flight {
            " · delivery pending"
        } else {
            ""
        }
    ))
    .id_salt(id.as_str())
}

pub(super) fn show(ui: &mut egui::Ui, controller: &mut GuiController) {
    ui.heading("Requests and receipts");
    ui.label("Accepted means pending owner work. Applied describes the operation, not physical audio presentation.");
    let mut reconcile = None;
    for record in controller.receipts().filter(|record| {
        !record.request.is_snapshot_query()
            || !matches!(
                record.presentation,
                Presentation::Queued | Presentation::Applied
            )
    }) {
        let id = &record.request.envelope().request_id;
        let state = if record.presentation == Presentation::Uncertain {
            "Outcome unknown".into()
        } else {
            format!("{:?}", record.presentation)
        };
        request_header(id, &state, record.in_flight).show(ui, |ui| {
            if request_details(ui, &record, controller.attached()) {
                reconcile = Some(id.clone());
            }
        });
    }
    if let Some(id) = reconcile {
        controller.handle(UiIntent::Reconcile(id));
    }
}

fn request_details(ui: &mut egui::Ui, record: &super::Receipt<'_>, attached: bool) -> bool {
    let mut reconcile = false;
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
    if matches!(
        record.presentation,
        Presentation::Accepted | Presentation::Uncertain
    ) {
        reconcile = ui
            .add_enabled(
                attached && !record.in_flight && !record.owner_changed,
                egui::Button::new("Reconcile exact request"),
            )
            .clicked();
    }
    let id = record.request.envelope().request_id.as_str();
    ui.scope_builder(
        egui::UiBuilder::new().scope_id(egui::Id::unique((id, "exact-request"))),
        |ui| {
            ui.collapsing("Exact local request", |ui| {
                ui.label(String::from_utf8_lossy(record.request.bytes()));
            });
        },
    );
    if let Some(Ok(reply)) = record.outcome {
        ui.scope_builder(
            egui::UiBuilder::new().scope_id(egui::Id::unique((id, "exact-reply"))),
            |ui| {
                ui.collapsing("Exact local reply", |ui| {
                    ui.label(String::from_utf8_lossy(reply.bytes()));
                });
            },
        );
    }
    reconcile
}

#[cfg(test)]
mod tests {
    use super::*;
    use earph_core::ids::RequestId;

    #[test]
    fn expanded_exact_receipt_keeps_widget_identity_during_replay() {
        let request = crate::Request::query(
            RequestId::new("owned-replayed-request").unwrap(),
            earph_core::ids::ZoneId::new("main").unwrap(),
            earph_core::control::Command::State,
        )
        .unwrap();
        let receipt = earph_core::control::Receipt {
            request_id: request.envelope().request_id.clone(),
            zone_id: request.envelope().zone_id.clone(),
            session_id: None,
            status: earph_core::control::ReceiptStatus::Accepted,
            error: None,
            revision: 1,
            epochs: Default::default(),
            activation_frame: None,
            result: None,
        };
        let outcome = Some(Ok(crate::decode_reply(
            serde_json::to_vec(&serde_json::json!({"receipt": receipt})).unwrap(),
        )
        .unwrap()));
        let context = egui::Context::default();
        context.enable_accesskit();
        let draw = |in_flight, time, events| {
            let record = super::super::Receipt {
                request: &request,
                presentation: Presentation::Accepted,
                in_flight,
                owner_changed: false,
                outcome: &outcome,
            };
            let mut output = context.run_ui(
                egui::RawInput {
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| {
                    request_header(&request.envelope().request_id, "Accepted", in_flight)
                        .default_open(true)
                        .show(ui, |ui| {
                            request_details(ui, &record, true);
                        });
                },
            );
            output.textures_delta.clear();
            output.platform_output.accesskit_update.unwrap()
        };
        let named = |tree: &egui::accesskit::TreeUpdate, label| {
            tree.nodes
                .iter()
                .find(|(_, node)| {
                    node.role() != egui::accesskit::Role::TextRun
                        && node.label().or_else(|| node.value()) == Some(label)
                })
                .unwrap()
                .0
        };
        let tree = draw(false, 0.0, vec![]);
        let request_widget = named(&tree, "Exact local request");
        let reply_widget = named(&tree, "Exact local reply");
        let click = egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
            action: egui::accesskit::Action::Click,
            target_tree: tree.tree_id,
            target_node: reply_widget,
            data: None,
        });
        draw(false, 1.0, vec![click]);
        let expanded = draw(false, 2.0, vec![]);
        let raw = String::from_utf8_lossy(outcome.as_ref().unwrap().as_ref().unwrap().bytes());
        let raw_widget = named(&expanded, raw.as_ref());
        let during = draw(true, 3.0, vec![]);
        let replay_button = during
            .nodes
            .iter()
            .find(|(_, node)| node.label() == Some("Reconcile exact request"));
        assert!(
            replay_button.is_some(),
            "delivery polling removed the reconciliation control"
        );
        let (button_id, button) = replay_button.unwrap();
        assert_eq!(*button_id, named(&expanded, "Reconcile exact request"));
        assert!(
            button.is_disabled(),
            "a second replay remained available during delivery"
        );
        assert_eq!(
            named(&during, "Exact local request"),
            request_widget,
            "delivery polling replaced the exact request widget"
        );
        assert_eq!(
            named(&during, "Exact local reply"),
            reply_widget,
            "delivery polling replaced the exact reply widget"
        );
        assert_eq!(
            named(&during, raw.as_ref()),
            raw_widget,
            "delivery polling replaced the reviewed exact reply"
        );
    }

    #[test]
    fn request_details_keep_their_identity_across_delivery_and_status_changes() {
        let context = egui::Context::default();
        context.enable_accesskit();
        let id = RequestId::new("owned-pending-request").unwrap();
        let draw = |state: &str, in_flight, time, events| {
            let mut visible = false;
            let mut output = context.run_ui(
                egui::RawInput {
                    time: Some(time),
                    events,
                    ..Default::default()
                },
                |ui| {
                    visible = request_header(&id, state, in_flight)
                        .show(ui, |ui| {
                            ui.label("Retained request details");
                        })
                        .body_returned
                        .is_some();
                },
            );
            output.textures_delta.clear();
            (output.platform_output.accesskit_update.unwrap(), visible)
        };
        let (tree, _) = draw("Accepted", false, 0.0, vec![]);
        let target = tree
            .nodes
            .iter()
            .find(|(_, n)| n.label() == Some("owned-pending-request · Accepted"))
            .unwrap()
            .0;
        let click = egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
            action: egui::accesskit::Action::Click,
            target_tree: tree.tree_id,
            target_node: target,
            data: None,
        });
        draw("Accepted", false, 1.0, vec![click]);
        assert!(draw("Accepted", false, 2.0, vec![]).1);
        let (tree, visible) = draw("Accepted", true, 3.0, vec![]);
        assert!(
            visible,
            "delivery polling closed the user's request details"
        );
        let changed = tree
            .nodes
            .iter()
            .find(|(_, n)| n.label() == Some("owned-pending-request · Accepted · delivery pending"))
            .unwrap()
            .0;
        assert_eq!(
            changed, target,
            "delivery changed the request's accessibility identity"
        );
        assert!(
            draw("Applied", false, 4.0, vec![]).1,
            "terminal status closed the user's request details"
        );
    }
}
