use std::path::PathBuf;

use eframe::egui;
use engine::sync::{SyncEvent, SyncOptions, SyncSummary};

use crate::dnd;
use crate::util::Log;
use crate::worker::{self, Job};

#[derive(Default)]
pub struct SyncTab {
    src: Option<PathBuf>,
    dst: Option<PathBuf>,
    mirror: bool,
    verify: bool,

    job: Option<Job<SyncEvent>>,
    planned: Option<(usize, usize, usize)>,
    summary: Option<SyncSummary>,
    log: Log,
}


impl SyncTab {
    pub fn is_running(&self) -> bool {
        self.job.is_some()
    }

    fn poll(&mut self) {
        let Some(job) = &self.job else { return };
        let mut finished = false;
        for event in job.rx.try_iter() {
            match event {
                SyncEvent::Planned {
                    to_copy,
                    to_update,
                    to_delete,
                } => {
                    self.planned = Some((to_copy, to_update, to_delete));
                    self.log.push(format!(
                        "Plan: {to_copy} to copy, {to_update} to update, {to_delete} to delete"
                    ));
                }
                SyncEvent::FileCopied { path } => self.log.push(format!("+ {}", path.display())),
                SyncEvent::FileUpdated { path } => self.log.push(format!("↻ {}", path.display())),
                SyncEvent::FileDeleted { path } => self.log.push(format!("- {}", path.display())),
                SyncEvent::FileError { path, message } => {
                    self.log.push(format!("✗ {} — {message}", path.display()))
                }
                SyncEvent::Finished(summary) => {
                    self.log.push(format!(
                        "Done: {} copied, {} updated, {} deleted, {} failed",
                        summary.copied, summary.updated, summary.deleted, summary.failed
                    ));
                    self.summary = Some(summary);
                    finished = true;
                }
                SyncEvent::Cancelled => {
                    self.log.push("Cancelled.".to_string());
                    finished = true;
                }
            }
        }
        if finished {
            self.job = None;
        }
    }

    fn start(&mut self) {
        let (Some(src), Some(dst)) = (self.src.clone(), self.dst.clone()) else {
            self.log.push("Choose both a source and destination folder first.".to_string());
            return;
        };
        let options = SyncOptions {
            mirror: self.mirror,
            verify: self.verify,
        };
        self.planned = None;
        self.summary = None;
        self.log.clear();
        self.job = Some(worker::spawn_sync(src, dst, options));
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, dropped: Vec<PathBuf>) {
        self.poll();

        ui.heading("Sync / Mirror");
        ui.label("One-way folder sync: brings the destination up to date with the source. Drag a folder onto either box below, or click to pick one.");
        ui.separator();

        let hovering = dnd::hovering_files(ui.ctx());

        let src_resp = drop_zone(ui, "📂 Source…", &self.src, hovering);
        if src_resp.clicked() {
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                self.src = Some(path);
            }
        }

        let dst_resp = drop_zone(ui, "📂 Destination…", &self.dst, hovering);
        if dst_resp.clicked() {
            if let Some(path) = rfd::FileDialog::new().pick_folder() {
                self.dst = Some(path);
            }
        }

        if let Some(dir) = dropped.first().and_then(|p| dnd::as_dir(p)) {
            let drop_pos = ui.ctx().input(|i| i.pointer.interact_pos().or_else(|| i.pointer.hover_pos()));
            match drop_pos {
                Some(pos) if src_resp.rect.contains(pos) => self.src = Some(dir),
                Some(pos) if dst_resp.rect.contains(pos) => self.dst = Some(dir),
                _ if self.src.is_none() => self.src = Some(dir),
                _ => self.dst = Some(dir),
            }
        }

        ui.horizontal(|ui| {
            ui.checkbox(&mut self.mirror, "Mirror (delete extra files in destination)");
            ui.checkbox(&mut self.verify, "Verify (hash check)");
        });

        ui.separator();

        ui.horizontal(|ui| {
            let running = self.is_running();
            if ui.add_enabled(!running, egui::Button::new("▶ Sync")).clicked() {
                self.start();
            }
            if ui.add_enabled(running, egui::Button::new("⏹ Cancel")).clicked() {
                if let Some(job) = &self.job {
                    job.cancel.cancel();
                }
            }
        });

        if let Some((to_copy, to_update, to_delete)) = self.planned {
            ui.label(format!(
                "Plan: {to_copy} to copy, {to_update} to update, {to_delete} to delete"
            ));
        }

        if let Some(summary) = &self.summary {
            ui.colored_label(
                egui::Color32::from_rgb(90, 200, 120),
                format!(
                    "Finished: {} copied, {} updated, {} deleted, {} failed",
                    summary.copied, summary.updated, summary.deleted, summary.failed
                ),
            );
        }

        ui.separator();
        ui.label("Log:");
        egui::ScrollArea::vertical()
            .id_salt("sync_log_scroll")
            .max_height(300.0)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in self.log.iter() {
                    ui.label(line);
                }
            });
    }
}

/// A clickable box that also acts as a drop target; returns a response
/// whose `.rect` the caller can hit-test against the drop position and
/// whose `.clicked()` opens a folder picker.
fn drop_zone(ui: &mut egui::Ui, label: &str, current: &Option<PathBuf>, hovering_files: bool) -> egui::Response {
    let fill = if hovering_files {
        ui.visuals().selection.bg_fill.linear_multiply(0.3)
    } else {
        ui.visuals().faint_bg_color
    };
    let inner = egui::Frame::group(ui.style()).fill(fill).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(label);
            ui.label(
                current
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "(none — click or drop a folder here)".to_string()),
            );
        });
    });
    ui.interact(inner.response.rect, ui.id().with(label), egui::Sense::click())
}
