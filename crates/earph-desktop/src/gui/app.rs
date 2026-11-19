// ABOUTME: Renders the native Earph controller with opaque local Light and Dark choices.
// ABOUTME: Owns local drafts while the workflow retains actual owner observations.
use super::{
    GuiController, UiIntent, connect, graph_editor::GraphIntentEditor, output, profiles, queue,
    receipts, signal_path, status, transport,
};
use eframe::egui;
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum View {
    Connect,
    Queue,
    Output,
    Profiles,
    SignalPath,
}
pub struct GuiApp {
    pub(super) controller: GuiController,
    pub(super) runtime: String,
    pub(super) executable: String,
    pub(super) file: String,
    pub(super) folder: String,
    pub(super) output: output::Draft,
    pub(super) transport: transport::Draft,
    pub(super) profiles: profiles::Draft,
    pub(super) graph: GraphIntentEditor,
    view: View,
    dark: bool,
    applied_theme: Option<bool>,
    pub(super) confirm_shutdown: bool,
}
impl GuiApp {
    pub fn new(controller: GuiController) -> Self {
        let executable = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|p| p.join("earph-engine")))
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        Self {
            controller,
            runtime: String::new(),
            executable,
            file: String::new(),
            folder: String::new(),
            output: output::Draft::default(),
            transport: transport::Draft::default(),
            profiles: profiles::Draft::default(),
            graph: GraphIntentEditor::default(),
            view: View::Connect,
            dark: true,
            applied_theme: None,
            confirm_shutdown: false,
        }
    }
    pub fn draw(&mut self, ui: &mut egui::Ui) {
        if self.applied_theme != Some(self.dark) {
            ui.ctx().options_mut(|o| o.sync_window_theme = false);
            ui.ctx().set_theme(if self.dark {
                egui::Theme::Dark
            } else {
                egui::Theme::Light
            });
            self.applied_theme = Some(self.dark);
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(ui.style()).fill(ui.visuals().panel_fill))
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.heading("Earph");
                    ui.selectable_value(&mut self.dark, false, "Light");
                    ui.selectable_value(&mut self.dark, true, "Dark");
                    if ui.button("Close controller").clicked() {
                        self.controller.handle(UiIntent::CloseController);
                    }
                });
                status::show(ui, &mut self.controller);
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    for (view, title) in [
                        (View::Connect, "Connect"),
                        (View::Queue, "Queue"),
                        (View::Output, "Output / Safety"),
                        (View::Profiles, "Profiles / DSP"),
                        (View::SignalPath, "Signal Path / Recovery"),
                    ] {
                        ui.selectable_value(&mut self.view, view, title);
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    match self.view {
                        View::Connect => connect::show(ui, self),
                        View::Queue => queue::show(ui, self),
                        View::Output => output::show(ui, self),
                        View::Profiles => profiles::show(ui, self),
                        View::SignalPath => signal_path::show(ui, &mut self.controller),
                    }
                    ui.separator();
                    receipts::show(ui, &mut self.controller);
                });
            });
    }
}
impl eframe::App for GuiApp {
    fn clear_color(&self, visuals: &egui::Visuals) -> [f32; 4] {
        visuals.panel_fill.to_normalized_gamma_f32()
    }
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.controller.tick(Instant::now());
        if ui.ctx().input(|i| i.viewport().close_requested()) && !self.controller.ready_to_close() {
            self.controller.handle(UiIntent::CloseController);
            ui.ctx()
                .send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
        self.draw(ui);
        if self.controller.ready_to_close() {
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ui.ctx()
            .request_repaint_after(Duration::from_millis(if self.controller.busy() {
                50
            } else {
                250
            }));
    }
}
pub fn run() -> eframe::Result {
    eframe::run_native(
        "Earph",
        eframe::NativeOptions {
            renderer: eframe::Renderer::Glow,
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1100.0, 820.0])
                .with_min_inner_size([760.0, 580.0]),
            ..Default::default()
        },
        Box::new(|_cc| Ok(Box::new(GuiApp::new(GuiController::new())))),
    )
}
pub(super) fn field(ui: &mut egui::Ui, label: &str, id: &str, text: &mut String, limit: usize) {
    let label = ui.label(label);
    ui.add(
        egui::TextEdit::singleline(text)
            .id(egui::Id::unique(id))
            .char_limit(limit)
            .desired_width(f32::INFINITY),
    )
    .labelled_by(label.id);
}
pub(super) fn executable(text: &str) -> PathBuf {
    PathBuf::from(text)
}
