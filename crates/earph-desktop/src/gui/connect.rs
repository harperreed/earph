// ABOUTME: Presents explicit owner attachment, launch, disconnect, and shutdown controls.
// ABOUTME: Keeps runtime paths and executable drafts separate from connection observations.
use super::{
    GuiApp, UiIntent,
    app::{executable, field},
};
use eframe::egui;

pub(super) fn show(ui: &mut egui::Ui, app: &mut GuiApp) {
    ui.heading("Connect to the local owner");
    ui.label(
        "Enter an absolute private runtime path of at most 60 UTF-8 bytes. Its parent must exist.",
    );
    field(
        ui,
        "Runtime directory",
        "runtime-directory",
        &mut app.runtime,
        2048,
    );
    field(
        ui,
        "Owner executable",
        "owner-executable",
        &mut app.executable,
        4096,
    );
    ui.label(
        "Start local engine launches this exact executable. Attachment never starts an engine.",
    );
    ui.horizontal_wrapped(|ui| {
        let can_attach = !app.controller.attached() && !app.controller.busy();
        if ui
            .add_enabled(can_attach, egui::Button::new("Attach"))
            .clicked()
        {
            app.controller.handle(UiIntent::Attach {
                runtime: app.runtime.clone(),
            });
        }
        if ui
            .add_enabled(can_attach, egui::Button::new("Start local engine"))
            .clicked()
        {
            app.controller.handle(UiIntent::Launch {
                runtime: app.runtime.clone(),
                executable: executable(&app.executable),
            });
        }
        if ui
            .add_enabled(
                app.controller.attached(),
                egui::Button::new("Reconnect / Refresh"),
            )
            .clicked()
        {
            app.controller.handle(UiIntent::Refresh);
        }
        if ui
            .add_enabled(app.controller.attached(), egui::Button::new("Disconnect"))
            .clicked()
        {
            app.controller.handle(UiIntent::Disconnect);
        }
    });
    ui.label(format!(
        "Host observation: {:?}",
        app.controller.host_status()
    ));
    if let Some(runtime) = app.controller.runtime() {
        ui.label(format!("Attached path: {runtime}"));
    }
    ui.label("Quitting Earph stops playback and the local engine.");
    ui.label("Disconnect does not cancel a command already sent to the owner.");
    if ui
        .add_enabled(
            app.controller.connected(),
            egui::Button::new("Stop engine and quit…"),
        )
        .clicked()
    {
        app.confirm_shutdown = true;
    }
    if app.confirm_shutdown {
        ui.group(|ui| {
            ui.label("Shutdown affects playback and every attached controller.");
            ui.label("Confirm only if you want the shared local engine to stop.");
            ui.horizontal(|ui| {
                if ui.button("Confirm engine shutdown").clicked() {
                    app.confirm_shutdown = false;
                    app.controller.handle(UiIntent::ShutdownAndQuit);
                }
                if ui.button("Keep engine running").clicked() {
                    app.confirm_shutdown = false;
                }
            });
        });
    }
}
