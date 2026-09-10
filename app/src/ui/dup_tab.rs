use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

use eframe::egui;
use engine::duplicates::{DupEvent, DupOptions, DuplicateGroup};
use engine::fsops;

use crate::util::{self, eta, human_bytes, human_duration, Log};
use crate::worker::{self, Job};

pub struct DupTab {
    roots: Vec<PathBuf>,
    min_size_mb: f32,

    job: Option<Job<DupEvent>>,
    started_at: Option<Instant>,
    files_found: usize,
    hashing_done: usize,
    hashing_total: usize,
    groups: Vec<DuplicateGroup>,
    wasted_bytes: u64,
    selected: HashSet<PathBuf>,
    log: Log,
}

impl Default for DupTab {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            min_size_mb: 0.0,
            job: None,
            started_at: None,
            files_found: 0,
            hashing_done: 0,
            hashing_total: 0,
            groups: Vec::new(),
            wasted_bytes: 0,
            selected: HashSet::new(),
            log: Log::default(),
        }
    }
}

impl DupTab {
    pub fn is_running(&self) -> bool {
        self.job.is_some()
    }

    pub fn add_dropped(&mut self, paths: Vec<PathBuf>) {
        for p in paths {
            if let Some(dir) = crate::dnd::as_dir(&p) {
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
                DupEvent::Scanning { files_found } => self.files_found = files_found,
                DupEvent::Hashing { done, total } => {
                    self.hashing_done = done;
                    self.hashing_total = total;
                }
                DupEvent::GroupFound(group) => {
                    self.wasted_bytes += group.size * (group.paths.len() as u64 - 1);
                    self.groups.push(group);
                }
                DupEvent::Finished { groups, wasted_bytes } => {
                    self.wasted_bytes = wasted_bytes;
                    let elapsed = self.started_at.map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0);
                    self.log.push(format!(
                        "Found {groups} duplicate group(s), wasting {} — {}",
                        human_bytes(wasted_bytes),
                        human_duration(elapsed)
                    ));
                    crate::notify::notify(
                        "Duplicate scan finished",
                        &format!("{groups} group(s) found, {} wasted", human_bytes(wasted_bytes)),
                    );
                    finished = true;
                }
                DupEvent::Cancelled => {
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
        let options = DupOptions {
            min_size: (self.min_size_mb.max(0.0) * 1024.0 * 1024.0) as u64,
        };
        self.groups.clear();
        self.selected.clear();
        self.wasted_bytes = 0;
        self.files_found = 0;
        self.hashing_done = 0;
        self.hashing_total = 0;
        self.started_at = Some(Instant::now());
        self.log.clear();
        self.job = Some(worker::spawn_duplicates(self.roots.clone(), options));
    }

    fn scan_entire_disk(&mut self) {
        self.roots = engine::drives::list_drives();
        self.start();
    }

    fn select_all_but_first(&mut self) {
        self.selected.clear();
        for g in &self.groups {
            for p in g.paths.iter().skip(1) {
                self.selected.insert(p.clone());
            }
        }
    }

    fn delete_selected(&mut self) {
        if self.selected.is_empty() {
            return;
        }
        let paths: Vec<PathBuf> = self.selected.iter().cloned().collect();
        match fsops::delete_to_trash(&paths) {
            Ok(()) => {
                self.log.push(format!("Moved {} file(s) to trash.", paths.len()));
                for g in &mut self.groups {
                    g.paths.retain(|p| !self.selected.contains(p));
                }
                self.groups.retain(|g| g.paths.len() > 1);
                self.selected.clear();
            }
            Err(e) => self.log.push(format!("Delete failed: {e}")),
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.poll();

        ui.heading("Find Duplicates");
        ui.label("Scans folders and finds byte-identical files using a size → partial-hash → full-hash funnel.");
        ui.separator();

        ui.horizontal(|ui| {
            if ui.button("➕ Add Folder…").clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    self.roots.push(path);
                }
            }
            if let Some(drive) = util::drives_menu_button(ui) {
                if !self.roots.contains(&drive) {
                    self.roots.push(drive);
                }
            }
            if ui.button("🗑 Clear").clicked() {
                self.roots.clear();
            }
            ui.label("Min size (MB):");
            ui.add(egui::DragValue::new(&mut self.min_size_mb).range(0.0..=10000.0).speed(0.1));
        });

        ui.horizontal(|ui| {
            if ui
                .add_enabled(!self.is_running(), egui::Button::new("🖴 Scan Entire Disk"))
                .on_hover_text("Scan every attached drive, ignoring any folders added above")
                .clicked()
            {
                self.scan_entire_disk();
            }
        });

        egui::ScrollArea::vertical()
            .id_salt("dup_roots_scroll")
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
            if ui.button("Select all but first in each group").clicked() {
                self.select_all_but_first();
            }
            if ui
                .add_enabled(!self.selected.is_empty(), egui::Button::new("🗑 Delete selected (to Trash)"))
                .clicked()
            {
                self.delete_selected();
            }
        });

        if self.is_running() {
            let elapsed = self.started_at.map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0);
            ui.label(format!(
                "Scanning… {} files found — {} elapsed",
                self.files_found,
                human_duration(elapsed)
            ));
            if self.hashing_total > 0 {
                ui.add(egui::ProgressBar::new(self.hashing_done as f32 / self.hashing_total as f32).show_percentage());
                if let Some(eta) = eta(elapsed, self.hashing_done as u64, self.hashing_total as u64) {
                    ui.weak(format!("~{eta} remaining"));
                }
            }
        }

        ui.separator();
        ui.label(format!(
            "{} duplicate group(s) — {} selected — {} wasted",
            self.groups.len(),
            self.selected.len(),
            human_bytes(self.wasted_bytes)
        ));

        egui::ScrollArea::vertical().id_salt("dup_groups_scroll").show(ui, |ui| {
            for (gi, group) in self.groups.iter().enumerate() {
                ui.group(|ui| {
                    ui.label(format!(
                        "Group {} — {} × {} ({} wasted)",
                        gi + 1,
                        group.paths.len(),
                        human_bytes(group.size),
                        human_bytes(group.size * (group.paths.len() as u64 - 1))
                    ));
                    for p in &group.paths {
                        let mut checked = self.selected.contains(p);
                        ui.horizontal(|ui| {
                            if ui.checkbox(&mut checked, "").changed() {
                                if checked {
                                    self.selected.insert(p.clone());
                                } else {
                                    self.selected.remove(p);
                                }
                            }
                            if ui.small_button("📂").on_hover_text("Show in Finder/Explorer").clicked() {
                                if let Err(e) = crate::reveal::reveal(p) {
                                    self.log.push(format!("Couldn't open folder: {e}"));
                                }
                            }
                            ui.label(p.display().to_string());
                        });
                    }
                });
            }
        });

        ui.separator();
        ui.label("Log:");
        egui::ScrollArea::vertical()
            .id_salt("dup_log_scroll")
            .max_height(120.0)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in self.log.iter() {
                    ui.label(line);
                }
            });
    }
}
