use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

use eframe::egui;
use engine::copy::{CopyEvent, CopyOptions};
use engine::search::{SearchEvent, SearchMatch, SearchOptions};

use crate::dialog::DeferredPicker;
use crate::dnd;
use crate::util::{self, human_bytes, human_duration, Log};
use crate::worker::{self, Job};

#[derive(Default)]
pub struct SearchTab {
    roots: Vec<PathBuf>,
    query: String,
    case_sensitive: bool,
    include_dirs: bool,

    job: Option<Job<SearchEvent>>,
    started_at: Option<Instant>,
    files_scanned: usize,
    results: Vec<SearchMatch>,
    selected: HashSet<PathBuf>,
    /// Paths currently being sent to the trash on a background thread (see
    /// `worker::spawn_delete_to_trash`), with the channel reporting back
    /// whether it worked.
    deleting: Option<worker::DeleteJob>,
    /// The originally requested source paths plus the job moving them —
    /// kept so that on completion we can check which ones no longer exist
    /// at their old location (the simplest correct way to tell success
    /// from failure for both files and whole directory trees) and drop
    /// only those from the result list.
    moving: Option<(Vec<PathBuf>, Job<CopyEvent>)>,
    picker: DeferredPicker,
    move_dest_picker: DeferredPicker,
    log: Log,
}


impl SearchTab {
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
                        self.log.push(format!("Moved {count} item(s) to trash."));
                        let deleted: HashSet<PathBuf> = paths.iter().cloned().collect();
                        self.results.retain(|m| !deleted.contains(&m.path));
                        self.selected.retain(|p| !deleted.contains(p));
                    }
                    Err(e) => self.log.push(format!("Delete failed: {e}")),
                }
                self.deleting = None;
            }
        }

        if let Some((paths, job)) = &self.moving {
            let mut move_finished = false;
            for event in job.rx.try_iter() {
                match event {
                    CopyEvent::Finished(summary) => {
                        self.log.push(format!(
                            "Moved {} file(s), {} failed.",
                            summary.files_copied, summary.files_failed
                        ));
                        move_finished = true;
                    }
                    CopyEvent::Cancelled => {
                        self.log.push("Move cancelled.".to_string());
                        move_finished = true;
                    }
                    CopyEvent::FileError { path, message } => {
                        self.log.push(format!("✗ {} — {message}", path.display()));
                    }
                    _ => {}
                }
            }
            if move_finished {
                // A file/dir no longer existing at its old location is the
                // simplest correct signal that it was actually moved —
                // works the same way whether it was a single file or a
                // whole directory tree, and naturally leaves anything that
                // failed still showing in the results.
                let moved: HashSet<PathBuf> = paths.iter().filter(|p| !p.exists()).cloned().collect();
                self.results.retain(|m| !moved.contains(&m.path));
                self.selected.retain(|p| !moved.contains(p));
                self.moving = None;
            }
        }

        let Some(job) = &self.job else { return };
        let mut finished = false;
        for event in job.rx.try_iter() {
            match event {
                SearchEvent::Scanning { files_scanned } => self.files_scanned = files_scanned,
                SearchEvent::Found(m) => self.results.push(m),
                SearchEvent::Finished { count } => {
                    let elapsed = self.started_at.map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0);
                    self.log.push(format!("Found {count} match(es) in {}.", human_duration(elapsed)));
                    crate::notify::notify("Search finished", &format!("{count} match(es) for \"{}\"", self.query));
                    finished = true;
                }
                SearchEvent::Cancelled => {
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
        if self.query.trim().is_empty() {
            self.log.push("Type something to search for first.".to_string());
            return;
        }
        let whole_disk = self.roots.is_empty();
        let roots = if whole_disk { engine::drives::list_drives() } else { self.roots.clone() };
        let options = SearchOptions {
            query: self.query.clone(),
            case_sensitive: self.case_sensitive,
            include_dirs: self.include_dirs,
        };
        self.results.clear();
        self.selected.clear();
        self.files_scanned = 0;
        self.started_at = Some(Instant::now());
        self.log.clear();
        self.log.push(if whole_disk {
            "Searching the entire disk…".to_string()
        } else {
            format!("Searching {} folder(s)…", roots.len())
        });
        self.job = Some(worker::spawn_search(roots, options));
    }

    fn scan_entire_disk(&mut self) {
        self.roots.clear();
        self.start();
    }

    fn delete_selected(&mut self) {
        if self.selected.is_empty() || self.deleting.is_some() {
            return;
        }
        let paths: Vec<PathBuf> = self.selected.iter().cloned().collect();
        let rx = worker::spawn_delete_to_trash(paths.clone());
        self.deleting = Some((paths, rx));
    }

    fn move_selected(&mut self, dest: PathBuf) {
        if self.selected.is_empty() || self.moving.is_some() {
            return;
        }
        let paths: Vec<PathBuf> = self.selected.iter().cloned().collect();
        self.log.push(format!("Moving {} item(s) to {}…", paths.len(), dest.display()));
        let job = worker::spawn_move(paths.clone(), dest, CopyOptions::default());
        self.moving = Some((paths, job));
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.poll();
        if let Some(paths) = self.picker.poll() {
            self.roots.extend(paths);
        }
        if let Some(mut paths) = self.move_dest_picker.poll() {
            if let Some(dest) = paths.pop() {
                self.move_selected(dest);
            }
        }
        if self.deleting.is_some() || self.moving.is_some() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
        }
        let running = self.is_running();

        ui.heading("Search");
        ui.label("Find files (and optionally folders) by name — in chosen folders, or across the entire disk.");
        ui.separator();

        ui.horizontal(|ui| {
            ui.label("🔎");
            let resp = ui.add_enabled(
                !running,
                egui::TextEdit::singleline(&mut self.query).hint_text("File name contains…"),
            );
            let enter_pressed = resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.add_enabled(!running, egui::Button::new("Search")).clicked() || (enter_pressed && !running) {
                self.start();
            }
            if ui.add_enabled(running, egui::Button::new("⏹ Cancel")).clicked() {
                if let Some(job) = &self.job {
                    job.cancel.cancel();
                }
            }
        });
        ui.horizontal(|ui| {
            ui.checkbox(&mut self.case_sensitive, "Case sensitive");
            ui.checkbox(&mut self.include_dirs, "Include folders");
        });

        ui.separator();

        ui.horizontal(|ui| {
            ui.label(if self.roots.is_empty() {
                "Scope: entire disk".to_string()
            } else {
                format!("Scope: {} folder(s)", self.roots.len())
            });
            if ui.button("➕ Add Folder…").clicked() {
                self.picker.request_folder();
            }
            if let Some(drive) = util::drives_menu_button(ui) {
                if !self.roots.contains(&drive) {
                    self.roots.push(drive);
                }
            }
            if ui.button("🗑 Reset scope to entire disk").clicked() {
                self.roots.clear();
            }
            if ui
                .add_enabled(!running, egui::Button::new("🖴 Scan Entire Disk"))
                .on_hover_text("Search every attached drive, ignoring any folders added above")
                .clicked()
            {
                self.scan_entire_disk();
            }
        });

        if !self.roots.is_empty() {
            egui::ScrollArea::vertical()
                .id_salt("search_roots_scroll")
                .max_height(60.0)
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
        }

        if running {
            ui.separator();
            let elapsed = self.started_at.map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0);
            ui.label(format!(
                "Scanning… {} checked, {} match so far — {} elapsed",
                self.files_scanned,
                self.results.len(),
                human_duration(elapsed)
            ));
        }

        ui.separator();

        ui.horizontal(|ui| {
            ui.label(format!("{} result(s)", self.results.len()));
            if ui.button("Select all").clicked() {
                for m in &self.results {
                    self.selected.insert(m.path.clone());
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
            if ui
                .add_enabled(
                    !self.selected.is_empty() && self.moving.is_none(),
                    egui::Button::new("📦 Move selected to folder…"),
                )
                .clicked()
            {
                self.move_dest_picker.request_folder();
            }
            if self.deleting.is_some() {
                ui.weak("Deleting…");
            }
            if self.moving.is_some() {
                ui.weak("Moving…");
            }
        });

        // With whole-disk searches routinely returning hundreds of thousands
        // of matches, laying out every row as a live widget every frame
        // (the old plain-loop-in-a-ScrollArea approach) freezes the UI.
        // show_rows only builds widgets for rows actually in the viewport.
        let row_height = ui.spacing().interact_size.y;
        egui::ScrollArea::vertical().id_salt("search_results_scroll").show_rows(
            ui,
            row_height,
            self.results.len(),
            |ui, row_range| {
                for m in &self.results[row_range] {
                    let mut checked = self.selected.contains(&m.path);
                    ui.horizontal(|ui| {
                        if ui.checkbox(&mut checked, "").changed() {
                            if checked {
                                self.selected.insert(m.path.clone());
                            } else {
                                self.selected.remove(&m.path);
                            }
                        }
                        if ui.small_button("📂").on_hover_text("Show in Finder/Explorer").clicked() {
                            if let Err(e) = crate::reveal::reveal(&m.path) {
                                self.log.push(format!("Couldn't open folder: {e}"));
                            }
                        }
                        ui.label(if m.is_dir { "📁" } else { "📄" });
                        if m.is_dir {
                            ui.monospace("—");
                        } else {
                            ui.monospace(human_bytes(m.size));
                        }
                        ui.label(m.path.display().to_string());
                    });
                }
            },
        );

        ui.separator();
        ui.label("Log:");
        egui::ScrollArea::vertical()
            .id_salt("search_log_scroll")
            .max_height(100.0)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in self.log.iter() {
                    ui.label(line);
                }
            });
    }
}
