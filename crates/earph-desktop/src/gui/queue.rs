// ABOUTME: Shows observed local queue entries and submits explicit edits with captured context.
// ABOUTME: Keeps path entry usable without native dialogs, metadata invention, or source starts.
use super::{GuiApp, UiIntent, app::field};
use earph_core::control::Command;
use eframe::egui;
use std::{path::Path, sync::Arc};

pub(super) fn show(ui: &mut egui::Ui, app: &mut GuiApp) {
    ui.heading("Local queue");
    if let Some(caps) = app.controller.capabilities() {
        ui.label(if caps.sources.contains(&earph_core::control::SourceKind::LocalFile) { "Local file playback is advertised by this owner. Queue edits do not start playback." } else { "Local file playback is unavailable in this owner. Queue intent is separate from source readiness." });
    }
    ui.label("UPnP, Sendspin, and Music Assistant controls are unavailable in this desktop unit.");
    field(ui, "File path", "queue-file-path", &mut app.file, 2048);
    let editable = app.controller.queue_edit_available();
    let mut command = None;
    if ui
        .add_enabled(editable, egui::Button::new("Add file"))
        .clicked()
    {
        command = Some(Command::QueueAppend {
            path: app.file.clone(),
        });
    }
    field(
        ui,
        "Folder path",
        "queue-folder-path",
        &mut app.folder,
        2048,
    );
    ui.horizontal(|ui| {
        if ui
            .add_enabled(editable, egui::Button::new("Add folder"))
            .clicked()
        {
            command = Some(Command::QueueAppendDirectory {
                path: app.folder.clone(),
            });
        }
        if ui
            .add_enabled(
                app.controller.attached(),
                egui::Button::new("Refresh queue"),
            )
            .clicked()
        {
            app.controller.handle(UiIntent::Refresh);
        }
        if ui
            .add_enabled(editable, egui::Button::new("Clear selection"))
            .clicked()
        {
            command = Some(Command::QueueSelect { id: None });
        }
    });
    if app.controller.queue_stale() {
        ui.label("Queue is stale or awaiting its owner/revision fence.");
    }
    if let Some(queue) = app.controller.queue() {
        ui.label(format!(
            "{} entries. Selected: {}",
            queue.total,
            queue.selected.as_ref().map_or("none", |id| id.as_str())
        ));
        for (index, entry) in queue.entries.iter().enumerate() {
            ui.push_id(entry.id.as_str(), |ui| {
                ui.group(|ui| {
                    let name = Path::new(&entry.path)
                        .file_name()
                        .map_or_else(|| entry.path.clone(), |p| p.to_string_lossy().into_owned());
                    ui.label(format!("{}: {} ({})", index + 1, name, entry.id.as_str()));
                    ui.label(format!("Preparation: {:?}", entry.status));
                    if let Some(state) = app.controller.state() {
                        if state.active_entry_id.as_ref() == Some(&entry.id) {
                            ui.label("Active entry");
                        }
                        if state.reserved_next_entry_id.as_ref() == Some(&entry.id) {
                            ui.label("Reserved next entry");
                        }
                    }
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .add_enabled(editable, egui::Button::new("Select"))
                            .clicked()
                        {
                            command = Some(Command::QueueSelect {
                                id: Some(entry.id.clone()),
                            });
                        }
                        if ui
                            .add_enabled(editable && index > 0, egui::Button::new("Move up"))
                            .clicked()
                        {
                            command = Some(Command::QueueMove {
                                id: entry.id.clone(),
                                new_index: index - 1,
                            });
                        }
                        if ui
                            .add_enabled(
                                editable && index + 1 < queue.total,
                                egui::Button::new("Move down"),
                            )
                            .clicked()
                        {
                            command = Some(Command::QueueMove {
                                id: entry.id.clone(),
                                new_index: index + 1,
                            });
                        }
                        if ui
                            .add_enabled(editable, egui::Button::new("Remove"))
                            .clicked()
                        {
                            command = Some(Command::QueueRemove {
                                id: entry.id.clone(),
                            });
                        }
                    });
                    ui.collapsing("Local path", |ui| {
                        ui.label(&entry.path);
                    });
                });
            });
        }
    } else {
        ui.label("No fenced queue observation yet.");
    }
    if let Some(command) = command
        && let Some(state) = app.controller.queue_basis()
    {
        app.controller.handle(UiIntent::QueueEdit {
            basis: Arc::new(state.clone()),
            command,
        });
    }
}
