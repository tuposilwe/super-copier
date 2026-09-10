use std::collections::HashSet;
use std::path::PathBuf;

use eframe::egui;
use engine::fsops;
use engine::large_files::{FileEntry, LargeFileEvent, LargeFilesOptions};

use crate::dnd;
use crate::util::{human_bytes, Log};
use crate::worker::{self, Job};

#[derive(PartialEq, Clone, Copy)]
enum SortBy {
    SizeDesc,
    NameAsc,
}

pub struct LargeFilesTab {
    roots: Vec<PathBuf>,
    min_size_mb: f32,
    search: String,
    sort_by: SortBy,

    job: Option<Job<LargeFileEvent>>,
    files_scanned: usize,
    results: Vec<FileEntry>,
    selected: HashSet<PathBuf>,
    total_bytes: u64,
    log: Log,
}

impl Default for LargeFilesTab {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            min_size_mb: 100.0,
            search: String::new(),
            sort_by: SortBy::SizeDesc,
            job: None,
            files_scanned: 0,
            results: Vec::new(),
            selected: HashSet::new(),
            total_bytes: 0,
            log: Log::default(),
        }
    }
}

impl LargeFilesTab {
    pub fn is_running(&self) -> bool {
        self.job.is_some()
    }

    pub fn add_dropped(&mut self, paths: Vec<PathBuf>) {
        for p in paths {
            if let Some(dir) = dnd::as_dir(&p) {
                if !self.roots.contains(&dir) {
                    self.roots.push(dir);
                }
            }
        }
    }

    fn poll(&mut self) {
        let Some(job) = &self.job else { return };
        let mut finished = false;
        for event in job.rx.try_iter() {
            match event {
                LargeFileEvent::Scanning { files_scanned } => self.files_scanned = files_scanned,
                LargeFileEvent::Found(entry) => {
                    self.total_bytes += entry.size;
                    self.results.push(entry);
                }
                LargeFileEvent::Finished { count, total_bytes } => {
                    self.total_bytes = total_bytes;
                    self.log.push(format!("Found {count} file(s) over the threshold, {}", human_bytes(total_bytes)));
                    finished = true;
                }
                LargeFileEvent::Cancelled => {
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
        if self.roots.is_empty() {
            self.log.push("Add at least one folder to scan first.".to_string());
            return;
        }
        let options = LargeFilesOptions {
            min_size: (self.min_size_mb.max(0.0) * 1024.0 * 1024.0) as u64,
        };
        self.results.clear();
        self.selected.clear();
        self.total_bytes = 0;
        self.files_scanned = 0;
        self.log.clear();
        self.job = Some(worker::spawn_large_files(self.roots.clone(), options));
    }

    /// Applies the live search filter and chosen sort over the already
    /// scanned results — instant, since it never touches the filesystem.
    fn filtered_sorted(&self) -> Vec<FileEntry> {
        let needle = self.search.trim().to_lowercase();
        let mut list: Vec<FileEntry> = self
            .results
            .iter()
            .filter(|f| needle.is_empty() || f.path.to_string_lossy().to_lowercase().contains(&needle))
            .cloned()
            .collect();
        match self.sort_by {
            SortBy::SizeDesc => list.sort_by_key(|f| std::cmp::Reverse(f.size)),
            SortBy::NameAsc => list.sort_by(|a, b| a.path.file_name().cmp(&b.path.file_name())),
        }
        list
    }

    fn delete_selected(&mut self) {
        if self.selected.is_empty() {
            return;
        }
        let paths: Vec<PathBuf> = self.selected.iter().cloned().collect();
        match fsops::delete_to_trash(&paths) {
            Ok(()) => {
                self.log.push(format!("Moved {} file(s) to trash.", paths.len()));
                self.results.retain(|f| !self.selected.contains(&f.path));
                self.selected.clear();
            }
            Err(e) => self.log.push(format!("Delete failed: {e}")),
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.poll();

        ui.heading("Big Files");
        ui.label("Scans folders for files above a size threshold, then search and sort the results.");
        ui.separator();

        ui.horizontal(|ui| {
            if ui.button("➕ Add Folder…").clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    self.roots.push(path);
                }
            }
            if ui.button("🗑 Clear").clicked() {
                self.roots.clear();
            }
            ui.label("Min size (MB):");
            ui.add(egui::DragValue::new(&mut self.min_size_mb).range(1.0..=1_000_000.0).speed(10.0));
        });

        egui::ScrollArea::vertical()
            .id_salt("bigfiles_roots_scroll")
            .max_height(80.0)
            .show(ui, |ui| {
                let mut remove_idx = None;
                for (i, p) in self.roots.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(p.display().to_string());
                        if ui.small_button("✕").clicked() {
                            remove_idx = Some(i);
                        }
                    });
                }
                if let Some(i) = remove_idx {
                    self.roots.remove(i);
                }
            });

        ui.separator();

        ui.horizontal(|ui| {
            let running = self.is_running();
            if ui.add_enabled(!running, egui::Button::new("▶ Scan")).clicked() {
                self.start();
            }
            if ui.add_enabled(running, egui::Button::new("⏹ Cancel")).clicked() {
                if let Some(job) = &self.job {
                    job.cancel.cancel();
                }
            }
        });

        if self.is_running() {
            ui.label(format!(
                "Scanning… {} files checked, {} match so far",
                self.files_scanned,
                self.results.len()
            ));
        }

        ui.separator();

        ui.horizontal(|ui| {
            ui.label("🔎 Search:");
            ui.text_edit_singleline(&mut self.search);
            if ui.small_button("✕").clicked() {
                self.search.clear();
            }
            ui.separator();
            ui.label("Sort:");
            egui::ComboBox::from_id_salt("bigfiles_sort")
                .selected_text(match self.sort_by {
                    SortBy::SizeDesc => "Largest first",
                    SortBy::NameAsc => "Name",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.sort_by, SortBy::SizeDesc, "Largest first");
                    ui.selectable_value(&mut self.sort_by, SortBy::NameAsc, "Name");
                });
        });

        let filtered = self.filtered_sorted();

        ui.horizontal(|ui| {
            ui.label(format!(
                "{} of {} file(s) — {} total",
                filtered.len(),
                self.results.len(),
                human_bytes(self.total_bytes)
            ));
            if ui.button("Select all shown").clicked() {
                for f in &filtered {
                    self.selected.insert(f.path.clone());
                }
            }
            if ui.button("Clear selection").clicked() {
                self.selected.clear();
            }
            if ui
                .add_enabled(!self.selected.is_empty(), egui::Button::new("🗑 Delete selected (to Trash)"))
                .clicked()
            {
                self.delete_selected();
            }
        });

        egui::ScrollArea::vertical().id_salt("bigfiles_results_scroll").show(ui, |ui| {
            for entry in &filtered {
                let mut checked = self.selected.contains(&entry.path);
                ui.horizontal(|ui| {
                    if ui.checkbox(&mut checked, "").changed() {
                        if checked {
                            self.selected.insert(entry.path.clone());
                        } else {
                            self.selected.remove(&entry.path);
                        }
                    }
                    ui.monospace(human_bytes(entry.size));
                    ui.label(entry.path.display().to_string());
                });
            }
        });

        ui.separator();
        ui.label("Log:");
        egui::ScrollArea::vertical()
            .id_salt("bigfiles_log_scroll")
            .max_height(100.0)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in self.log.iter() {
                    ui.label(line);
                }
            });
    }
}
