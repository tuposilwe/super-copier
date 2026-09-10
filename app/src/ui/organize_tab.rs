use std::path::PathBuf;

use eframe::egui;
use engine::fsops::{OrganizeEvent, OrganizeStrategy};

use crate::util::Log;
use crate::worker::{self, Job};

pub struct OrganizeTab {
    dir: Option<PathBuf>,
    strategy: OrganizeStrategy,
    job: Option<Job<OrganizeEvent>>,
    moved: usize,
    log: Log,
}

impl Default for OrganizeTab {
    fn default() -> Self {
        Self {
            dir: None,
            strategy: OrganizeStrategy::ByExtension,
            job: None,
            moved: 0,
            log: Log::default(),
        }
    }
}

impl OrganizeTab {
    pub fn is_running(&self) -> bool {
        self.job.is_some()
    }

    pub fn add_dropped(&mut self, paths: Vec<PathBuf>) {
        if let Some(dir) = paths.iter().find_map(|p| crate::dnd::as_dir(p)) {
            self.dir = Some(dir);
        }
    }

    fn poll(&mut self) {
        let Some(job) = &self.job else { return };
        let mut finished = false;
        for event in job.rx.try_iter() {
            match event {
                OrganizeEvent::FileMoved { from, to } => {
                    self.moved += 1;
                    self.log.push(format!("{} → {}", from.display(), to.display()));
                }
                OrganizeEvent::FileError { path, message } => {
                    self.log.push(format!("✗ {} — {message}", path.display()));
                }
                OrganizeEvent::Finished { moved } => {
                    self.log.push(format!("Done: moved {moved} file(s)."));
                    finished = true;
                }
            }
        }
        if finished {
            self.job = None;
        }
    }

    fn start(&mut self) {
        let Some(dir) = self.dir.clone() else {
            self.log.push("Choose a folder first.".to_string());
            return;
        };
        self.moved = 0;
        self.log.clear();
        self.job = Some(worker::spawn_organize(dir, self.strategy));
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.poll();

        ui.heading("Auto-Organize");
        ui.label("Sorts files directly inside a folder into subfolders by type and/or date.");
        ui.separator();

        ui.horizontal(|ui| {
            if ui.button("📂 Choose Folder…").clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    self.dir = Some(path);
                }
            }
            ui.label(
                self.dir
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "(no folder selected)".to_string()),
            );
        });

        ui.horizontal(|ui| {
            ui.label("Strategy:");
            egui::ComboBox::from_id_salt("organize_strategy")
                .selected_text(match self.strategy {
                    OrganizeStrategy::ByExtension => "By type",
                    OrganizeStrategy::ByDate => "By date (YYYY-MM)",
                    OrganizeStrategy::ByExtensionAndDate => "By type, then date",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.strategy, OrganizeStrategy::ByExtension, "By type");
                    ui.selectable_value(&mut self.strategy, OrganizeStrategy::ByDate, "By date (YYYY-MM)");
                    ui.selectable_value(
                        &mut self.strategy,
                        OrganizeStrategy::ByExtensionAndDate,
                        "By type, then date",
                    );
                });
        });

        ui.separator();

        ui.horizontal(|ui| {
            let running = self.is_running();
            if ui.add_enabled(!running, egui::Button::new("▶ Organize")).clicked() {
                self.start();
            }
            if ui.add_enabled(running, egui::Button::new("⏹ Cancel")).clicked() {
                if let Some(job) = &self.job {
                    job.cancel.cancel();
                }
            }
        });

        ui.label(format!("Moved: {}", self.moved));

        ui.separator();
        ui.label("Log:");
        egui::ScrollArea::vertical()
            .id_salt("organize_log_scroll")
            .max_height(300.0)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in self.log.iter() {
                    ui.label(line);
                }
            });
    }
}
