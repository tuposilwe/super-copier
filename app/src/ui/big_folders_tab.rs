use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

use eframe::egui;
use engine::big_folders::{BigFolderEvent, BigFoldersOptions, FolderEntry};

use crate::dialog::DeferredPicker;
use crate::dnd;
use crate::util::{self, human_bytes, human_duration, Log};
use crate::worker::{self, Job};

#[derive(PartialEq, Clone, Copy)]
enum SortBy {
    SizeDesc,
    NameAsc,
}

pub struct BigFoldersTab {
    roots: Vec<PathBuf>,
    min_size_mb: f32,
    search: String,
    sort_by: SortBy,

    job: Option<Job<BigFolderEvent>>,
    started_at: Option<Instant>,
    files_scanned: usize,
    results: Vec<FolderEntry>,
    /// Indices into `results`, filtered by `search` and sorted by
    /// `sort_by`. Cached instead of recomputed every frame — see the same
    /// pattern (and the freeze it fixed) in large_files_tab.rs.
    filtered_indices: Vec<usize>,
    filtered_cache_key: Option<(String, SortBy, usize)>,
    selected: HashSet<PathBuf>,
    /// Paths currently being sent to the trash on a background thread (see
    /// `worker::spawn_delete_to_trash`), with the channel reporting back
    /// whether it worked.
    deleting: Option<worker::DeleteJob>,
    picker: DeferredPicker,
    log: Log,
}

impl Default for BigFoldersTab {
    fn default() -> Self {
        Self {
            roots: Vec::new(),
            min_size_mb: 500.0,
            search: String::new(),
            sort_by: SortBy::SizeDesc,
            job: None,
            started_at: None,
            files_scanned: 0,
            results: Vec::new(),
            filtered_indices: Vec::new(),
            filtered_cache_key: None,
            selected: HashSet::new(),
            deleting: None,
            picker: DeferredPicker::default(),
            log: Log::default(),
        }
    }
}

impl BigFoldersTab {
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
        if let Some((paths, rx)) = &self.deleting {
            if let Ok(result) = rx.try_recv() {
                match result {
                    Ok(count) => {
                        self.log.push(format!("Moved {count} folder(s) to trash."));
                        let deleted: HashSet<PathBuf> = paths.iter().cloned().collect();
                        self.results.retain(|f| !deleted.contains(&f.path));
                        self.selected.retain(|p| !deleted.contains(p));
                    }
                    Err(e) => self.log.push(format!("Delete failed: {e}")),
                }
                self.deleting = None;
            }
        }

        let Some(job) = &self.job else { return };
        let mut finished = false;
        for event in job.rx.try_iter() {
            match event {
                BigFolderEvent::Scanning { files_scanned } => self.files_scanned = files_scanned,
                BigFolderEvent::Found(entry) => self.results.push(entry),
                BigFolderEvent::Finished { count } => {
                    let elapsed = self.started_at.map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0);
                    self.log.push(format!("Found {count} folder(s) over the threshold — {}", human_duration(elapsed)));
                    crate::notify::notify("Big Folders scan finished", &format!("{count} folder(s) found"));
                    finished = true;
                }
                BigFolderEvent::Cancelled => {
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
        let options = BigFoldersOptions {
            min_size: (self.min_size_mb.max(0.0) * 1024.0 * 1024.0) as u64,
        };
        self.results.clear();
        self.selected.clear();
        self.files_scanned = 0;
        self.started_at = Some(Instant::now());
        self.log.clear();
        self.job = Some(worker::spawn_big_folders(self.roots.clone(), options));
    }

    fn scan_entire_disk(&mut self) {
        self.roots = engine::drives::list_drives();
        self.start();
    }

    /// Rebuilds `filtered_indices` from `results` if the search text, sort
    /// mode, or result count has changed since the last call.
    fn refresh_filtered(&mut self) {
        let key = (self.search.clone(), self.sort_by, self.results.len());
        if self.filtered_cache_key.as_ref() == Some(&key) {
            return;
        }
        let needle = self.search.trim().to_lowercase();
        let mut indices: Vec<usize> = self
            .results
            .iter()
            .enumerate()
            .filter(|(_, f)| needle.is_empty() || f.path.to_string_lossy().to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect();
        match self.sort_by {
            SortBy::SizeDesc => indices.sort_by_key(|&i| std::cmp::Reverse(self.results[i].size)),
            SortBy::NameAsc => {
                indices.sort_by(|&a, &b| self.results[a].path.file_name().cmp(&self.results[b].path.file_name()))
            }
        }
        self.filtered_indices = indices;
        self.filtered_cache_key = Some(key);
    }

    fn delete_selected(&mut self) {
        if self.selected.is_empty() || self.deleting.is_some() {
            return;
        }
        let paths: Vec<PathBuf> = self.selected.iter().cloned().collect();
        let rx = worker::spawn_delete_to_trash(paths.clone());
        self.deleting = Some((paths, rx));
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.poll();
        if let Some(paths) = self.picker.poll() {
            self.roots.extend(paths);
        }
        if self.deleting.is_some() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
        }

        ui.heading("Big Folders");
        ui.label("Scans folders and reports which subfolders are actually taking up space — cumulative size, including everything nested inside, not just top-level files.");
        ui.separator();

        ui.horizontal(|ui| {
            if ui.button("➕ Add Folder…").clicked() {
                self.picker.request_folder();
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
            ui.add(egui::DragValue::new(&mut self.min_size_mb).range(1.0..=1_000_000.0).speed(10.0));
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
            .id_salt("bigfolders_roots_scroll")
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
            let elapsed = self.started_at.map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0);
            ui.label(format!("Scanning… {} files checked — {} elapsed", self.files_scanned, human_duration(elapsed)));
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
            egui::ComboBox::from_id_salt("bigfolders_sort")
                .selected_text(match self.sort_by {
                    SortBy::SizeDesc => "Largest first",
                    SortBy::NameAsc => "Name",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.sort_by, SortBy::SizeDesc, "Largest first");
                    ui.selectable_value(&mut self.sort_by, SortBy::NameAsc, "Name");
                });
        });

        self.refresh_filtered();

        ui.horizontal(|ui| {
            ui.label(format!("{} of {} folder(s)", self.filtered_indices.len(), self.results.len()));
            if ui.button("Select all shown").clicked() {
                for &idx in &self.filtered_indices {
                    self.selected.insert(self.results[idx].path.clone());
                }
            }
            if ui.button("Clear selection").clicked() {
                self.selected.clear();
            }
            if ui
                .add_enabled(
                    !self.selected.is_empty() && self.deleting.is_none(),
                    egui::Button::new("🗑 Delete selected (to Trash)"),
                )
                .clicked()
            {
                self.delete_selected();
            }
            if self.deleting.is_some() {
                ui.weak("Deleting…");
            }
        });

        // Same freeze risk as Big Files/Search/Duplicates on a whole-disk
        // scan — show_rows only builds widgets for rows in the viewport.
        let row_height = ui.spacing().interact_size.y;
        egui::ScrollArea::vertical().id_salt("bigfolders_results_scroll").show_rows(
            ui,
            row_height,
            self.filtered_indices.len(),
            |ui, row_range| {
                for &idx in &self.filtered_indices[row_range] {
                    let entry = &self.results[idx];
                    let mut checked = self.selected.contains(&entry.path);
                    ui.horizontal(|ui| {
                        if ui.checkbox(&mut checked, "").changed() {
                            if checked {
                                self.selected.insert(entry.path.clone());
                            } else {
                                self.selected.remove(&entry.path);
                            }
                        }
                        if ui.small_button("📂").on_hover_text("Show in Finder/Explorer").clicked() {
                            if let Err(e) = crate::reveal::reveal(&entry.path) {
                                self.log.push(format!("Couldn't open folder: {e}"));
                            }
                        }
                        ui.monospace(human_bytes(entry.size));
                        ui.weak(format!("({} files)", entry.file_count));
                        ui.label(entry.path.display().to_string());
                    });
                }
            },
        );

        ui.separator();
        ui.label("Log:");
        egui::ScrollArea::vertical()
            .id_salt("bigfolders_log_scroll")
            .max_height(100.0)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in self.log.iter() {
                    ui.label(line);
                }
            });
    }
}
