use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;

use eframe::egui;
use engine::copy::{CopyEvent, CopyOptions, OverwritePolicy};
use engine::fsops;

use crate::dnd;
use crate::util::{eta, human_bytes, human_duration, human_rate, Log};
use crate::worker::{self, Job};

#[derive(PartialEq, Eq, Clone, Copy)]
enum Mode {
    Copy,
    Move,
    Rename,
}

struct ActiveFile {
    bytes_done: u64,
    size: u64,
}

/// Running totals across every batch of the current session (there can be
/// more than one job at once — see [`CopyTab::spawn_new_batch`]).
#[derive(Default, Clone, Copy)]
struct Totals {
    files_copied: usize,
    files_skipped: usize,
    files_failed: usize,
    bytes_copied: u64,
}

pub struct CopyTab {
    mode: Mode,
    sources: Vec<PathBuf>,
    dest: Option<PathBuf>,
    overwrite: OverwritePolicy,
    verify: bool,
    preserve_times: bool,
    use_fast_path: bool,
    rename_pattern: String,

    /// Usually one, but a new batch is spawned alongside the rest whenever
    /// files are added while a transfer is already running, so more than
    /// one can be in flight together.
    jobs: Vec<Job<CopyEvent>>,
    /// Sources already handed to some job this session (across every
    /// batch) — the delta between this and `sources` is what's new since
    /// the last batch was spawned.
    queued: HashSet<PathBuf>,
    started_at: Option<Instant>,
    total_files: usize,
    total_bytes: u64,
    files_done: usize,
    /// Bytes belonging to files that have fully finished copying.
    bytes_done_complete: u64,
    /// Files currently mid-transfer (there can be several at once — the
    /// engine copies multiple files concurrently, and there can be
    /// multiple batches too).
    active: HashMap<PathBuf, ActiveFile>,
    /// Set once every batch in the session has finished.
    totals: Option<Totals>,
    log: Log,
}

impl Default for CopyTab {
    fn default() -> Self {
        Self {
            mode: Mode::Copy,
            sources: Vec::new(),
            dest: None,
            overwrite: OverwritePolicy::SkipIfSameSize,
            verify: false,
            preserve_times: true,
            use_fast_path: true,
            rename_pattern: "{name}.{ext}".to_string(),
            jobs: Vec::new(),
            queued: HashSet::new(),
            started_at: None,
            total_files: 0,
            total_bytes: 0,
            files_done: 0,
            bytes_done_complete: 0,
            active: HashMap::new(),
            totals: None,
            log: Log::default(),
        }
    }
}

impl CopyTab {
    pub fn is_running(&self) -> bool {
        !self.jobs.is_empty()
    }

    /// Switches to Move mode — used when this tab is reached via the
    /// "Move with Super Copier" Explorer context menu entry.
    pub fn set_move_mode(&mut self) {
        self.mode = Mode::Move;
    }

    fn bytes_in_flight(&self) -> u64 {
        self.active.values().map(|f| f.bytes_done).sum()
    }

    fn poll(&mut self) {
        if self.jobs.is_empty() {
            return;
        }
        let mut finished_indices = Vec::new();
        for (i, job) in self.jobs.iter().enumerate() {
            let mut batch_finished = false;
            for event in job.rx.try_iter() {
                match event {
                    CopyEvent::Started {
                        total_files,
                        total_bytes,
                    } => {
                        self.total_files += total_files;
                        self.total_bytes += total_bytes;
                        self.log.push(format!(
                            "Starting: {total_files} files, {}",
                            human_bytes(total_bytes)
                        ));
                    }
                    CopyEvent::FileStarted { path, size } => {
                        self.active.insert(path, ActiveFile { bytes_done: 0, size });
                    }
                    CopyEvent::FileProgress {
                        path,
                        bytes_done,
                        size,
                    } => {
                        self.active.insert(path, ActiveFile { bytes_done, size });
                    }
                    CopyEvent::FileVerified { .. } => {}
                    CopyEvent::FileDone { path } => {
                        self.files_done += 1;
                        if let Some(f) = self.active.remove(&path) {
                            self.bytes_done_complete += f.size;
                        }
                        self.log.push(format!("✓ {}", path.display()));
                    }
                    CopyEvent::FileSkipped { path, reason } => {
                        self.files_done += 1;
                        self.active.remove(&path);
                        self.log.push(format!("↷ skipped {} ({reason})", path.display()));
                    }
                    CopyEvent::FileError { path, message } => {
                        self.active.remove(&path);
                        self.log.push(format!("✗ {} — {message}", path.display()));
                    }
                    CopyEvent::Finished(summary) => {
                        let totals = self.totals.get_or_insert_with(Totals::default);
                        totals.files_copied += summary.files_copied;
                        totals.files_skipped += summary.files_skipped;
                        totals.files_failed += summary.files_failed;
                        totals.bytes_copied += summary.bytes_copied;
                        batch_finished = true;
                    }
                    CopyEvent::Cancelled => {
                        batch_finished = true;
                    }
                }
            }
            if batch_finished {
                finished_indices.push(i);
            }
        }
        for i in finished_indices.into_iter().rev() {
            self.jobs.remove(i);
        }

        if self.jobs.is_empty() {
            // The whole session (every batch) is done.
            self.queued.clear();
            if let Some(totals) = self.totals {
                let elapsed = self.started_at.map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0);
                self.log.push(format!(
                    "Done: {} transferred, {} skipped, {} failed in {}",
                    totals.files_copied,
                    totals.files_skipped,
                    totals.files_failed,
                    human_duration(elapsed)
                ));
                let verb = match self.mode {
                    Mode::Copy => "Copy",
                    Mode::Move => "Move",
                    Mode::Rename => "Transfer",
                };
                crate::notify::notify(
                    &format!("{verb} finished"),
                    &format!(
                        "{} transferred, {} skipped, {} failed",
                        totals.files_copied, totals.files_skipped, totals.files_failed
                    ),
                );
            } else {
                self.log.push("Cancelled.".to_string());
            }
        }
    }

    /// Starts a fresh session: resets every running total and queues all
    /// current sources. Only called when nothing is currently running (the
    /// Start button is disabled otherwise).
    fn start(&mut self) {
        if self.sources.is_empty() {
            self.log.push("Drag in at least one file or folder first.".to_string());
            return;
        }
        if self.dest.is_none() {
            self.log.push("Choose a destination folder first.".to_string());
            return;
        }
        self.total_files = 0;
        self.total_bytes = 0;
        self.files_done = 0;
        self.bytes_done_complete = 0;
        self.active.clear();
        self.totals = None;
        self.queued.clear();
        self.log.clear();
        self.started_at = Some(Instant::now());
        self.spawn_new_batch(self.sources.clone());
    }

    /// Spawns a job for exactly `paths` (already known not to be queued)
    /// and marks them as queued. Used both by `start()` for the first
    /// batch and, mid-session, for files added after that.
    fn spawn_new_batch(&mut self, paths: Vec<PathBuf>) {
        let Some(dest) = self.dest.clone() else { return };
        for p in &paths {
            self.queued.insert(p.clone());
        }
        let options = CopyOptions {
            overwrite: self.overwrite,
            verify: self.verify,
            preserve_times: self.preserve_times,
            threads: 0,
            use_fast_path: self.use_fast_path,
        };
        let job = match self.mode {
            Mode::Copy => worker::spawn_copy(paths, dest, options),
            Mode::Move => worker::spawn_move(paths, dest, options),
            Mode::Rename => return,
        };
        self.jobs.push(job);
    }

    /// While a transfer is running, folds any sources added since the last
    /// batch into a new one immediately, instead of making the user wait
    /// for the current batch to finish or click Start again.
    fn absorb_new_sources(&mut self) {
        if !self.is_running() || self.dest.is_none() {
            return;
        }
        let new_paths: Vec<PathBuf> = self
            .sources
            .iter()
            .filter(|p| !self.queued.contains(*p))
            .cloned()
            .collect();
        if new_paths.is_empty() {
            return;
        }
        self.log.push(format!(
            "+{} item(s) added mid-transfer — picking them up now",
            new_paths.len()
        ));
        self.spawn_new_batch(new_paths);
    }

    fn cancel_all(&self) {
        for job in &self.jobs {
            job.cancel.cancel();
        }
    }

    fn rename_preview(&self) -> Vec<(String, String)> {
        self.sources
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("");
                let ext = p.extension().and_then(|s| s.to_str()).unwrap_or("");
                let old_name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                let new_name = fsops::apply_rename_pattern(&self.rename_pattern, stem, ext, i + 1);
                (old_name, new_name)
            })
            .collect()
    }

    fn do_rename(&mut self) {
        if self.sources.is_empty() {
            self.log.push("Drag in at least one file or folder to rename first.".to_string());
            return;
        }
        self.log.clear();
        match fsops::rename_batch(&self.sources, &self.rename_pattern) {
            Ok(new_paths) => {
                for (old, new) in self.sources.iter().zip(new_paths.iter()) {
                    self.log.push(format!("✓ {} → {}", old.display(), new.display()));
                }
                self.sources = new_paths;
            }
            Err(e) => self.log.push(format!("✗ Rename failed: {e}")),
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, dropped: Vec<PathBuf>) {
        self.poll();

        let hovering = dnd::hovering_files(ui.ctx());

        // Mode: the one choice that matters up front.
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.mode, Mode::Copy, "📄  Copy");
            ui.selectable_value(&mut self.mode, Mode::Move, "✂  Move");
            ui.selectable_value(&mut self.mode, Mode::Rename, "✏  Rename");
        });
        ui.add_space(6.0);

        // The big, obvious drop target.
        let source_resp = self.ui_source_drop_zone(ui, hovering);

        // Destination — its own drop target, only relevant outside Rename mode.
        let dest_resp = if self.mode != Mode::Rename {
            ui.add_space(6.0);
            Some(dnd::drop_zone(ui, "📂  Destination", &self.dest, hovering))
        } else {
            None
        };

        if source_resp.clicked() {
            if let Some(paths) = rfd::FileDialog::new().pick_files() {
                self.sources.extend(paths);
            }
        }
        if let Some(resp) = &dest_resp {
            if resp.clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    self.dest = Some(path);
                }
            }
        }

        // Route this frame's OS drop: onto the destination box sets the
        // destination, anywhere else adds sources.
        if !dropped.is_empty() {
            let drop_pos = ui.ctx().input(|i| i.pointer.interact_pos().or_else(|| i.pointer.hover_pos()));
            let dropped_on_dest = dest_resp
                .as_ref()
                .is_some_and(|r| drop_pos.is_some_and(|p| r.rect.contains(p)));
            if dropped_on_dest {
                if let Some(dir) = dropped.first().and_then(|p| dnd::as_dir(p)) {
                    self.dest = Some(dir);
                }
            } else {
                self.sources.extend(dropped);
            }
        }

        // If a transfer is already running, any sources that just appeared
        // (from the drop above, or from "Add Files…" below) get folded in
        // right away instead of waiting for a fresh Start.
        self.absorb_new_sources();

        if !self.sources.is_empty() {
            ui.add_space(6.0);
            self.ui_source_list(ui);
        }

        ui.add_space(8.0);

        if self.mode == Mode::Rename {
            self.ui_rename(ui);
        } else {
            self.ui_transfer(ui);
        }

        if !self.log.is_empty() {
            ui.add_space(4.0);
            egui::CollapsingHeader::new("Log").default_open(false).show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .id_salt("copy_log_scroll")
                    .max_height(180.0)
                    .stick_to_bottom(true)
                    .show(ui, |ui| {
                        for line in self.log.iter() {
                            ui.label(line);
                        }
                    });
            });
        }
    }

    fn ui_source_drop_zone(&self, ui: &mut egui::Ui, hovering: bool) -> egui::Response {
        let fill = if hovering {
            ui.visuals().selection.bg_fill.linear_multiply(0.3)
        } else {
            ui.visuals().faint_bg_color
        };
        let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
        let inner = egui::Frame::group(ui.style())
            .fill(fill)
            .stroke(stroke)
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                ui.set_min_height(70.0);
                ui.vertical_centered(|ui| {
                    ui.add_space(10.0);
                    if self.sources.is_empty() {
                        ui.label("Drag & drop files or folders here");
                        ui.weak("or click to browse");
                    } else if self.is_running() {
                        ui.label(format!(
                            "{} item(s) — drop more to add them to the running transfer",
                            self.sources.len()
                        ));
                    } else {
                        ui.label(format!("{} item(s) added — drop more, or click to add files", self.sources.len()));
                    }
                    ui.add_space(10.0);
                });
            });
        ui.interact(inner.response.rect, ui.id().with("source_drop_zone"), egui::Sense::click())
    }

    fn ui_source_list(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label(format!("{} item(s):", self.sources.len()));
            if ui.add_enabled(!self.is_running(), egui::Button::new("Clear")).clicked() {
                self.sources.clear();
            }
        });
        egui::ScrollArea::vertical()
            .id_salt("sources_scroll")
            .max_height(100.0)
            .show(ui, |ui| {
                let mut remove_idx = None;
                for (i, p) in self.sources.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if self.is_running() && self.queued.contains(p) {
                            ui.weak("✓");
                        }
                        ui.label(p.display().to_string());
                        if !self.is_running() && ui.small_button("✕").clicked() {
                            remove_idx = Some(i);
                        }
                    });
                }
                if let Some(i) = remove_idx {
                    self.sources.remove(i);
                }
            });
    }

    fn ui_transfer(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let running = self.is_running();
            let start_label = match self.mode {
                Mode::Copy => "▶  Copy",
                Mode::Move => "▶  Move",
                Mode::Rename => unreachable!(),
            };
            if ui
                .add_enabled(!running, egui::Button::new(start_label).min_size(egui::vec2(100.0, 28.0)))
                .clicked()
            {
                self.start();
            }
            if ui.add_enabled(running, egui::Button::new("⏹ Cancel")).clicked() {
                self.cancel_all();
            }
            if running {
                ui.weak(format!(
                    "{} batch{} running",
                    self.jobs.len(),
                    if self.jobs.len() == 1 { "" } else { "es" }
                ));
            }
        });

        egui::CollapsingHeader::new("⚙ Advanced options").default_open(false).show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label("On conflict:");
                egui::ComboBox::from_id_salt("overwrite_policy")
                    .selected_text(match self.overwrite {
                        OverwritePolicy::Always => "Overwrite",
                        OverwritePolicy::Skip => "Skip existing",
                        OverwritePolicy::SkipIfSameSize => "Skip if same size (resume)",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut self.overwrite,
                            OverwritePolicy::SkipIfSameSize,
                            "Skip if same size (resume)",
                        );
                        ui.selectable_value(&mut self.overwrite, OverwritePolicy::Skip, "Skip existing");
                        ui.selectable_value(&mut self.overwrite, OverwritePolicy::Always, "Overwrite");
                    });
            });
            ui.checkbox(&mut self.verify, "Verify (hash check)");
            ui.checkbox(&mut self.preserve_times, "Preserve timestamps");
            ui.checkbox(&mut self.use_fast_path, "Use OS fast-copy");
        });

        if self.total_files > 0 {
            ui.add_space(6.0);
            let bytes_done = self.bytes_done_complete + self.bytes_in_flight();
            let frac = if self.total_bytes > 0 {
                bytes_done as f32 / self.total_bytes as f32
            } else {
                0.0
            };
            ui.label(format!(
                "{}/{} files — {} / {}",
                self.files_done,
                self.total_files,
                human_bytes(bytes_done),
                human_bytes(self.total_bytes)
            ));
            ui.add(egui::ProgressBar::new(frac.clamp(0.0, 1.0)).show_percentage());

            if let Some(started) = self.started_at {
                let secs = started.elapsed().as_secs_f64().max(0.001);
                let rate = bytes_done as f64 / secs;
                ui.horizontal(|ui| {
                    ui.weak(format!("{} elapsed", human_duration(secs)));
                    ui.weak("·");
                    ui.weak(human_rate(rate));
                    if let Some(eta) = eta(secs, bytes_done, self.total_bytes) {
                        ui.weak("·");
                        ui.weak(format!("~{eta} remaining"));
                    }
                });
            }
        }

        if !self.is_running() {
            if let Some(totals) = self.totals {
                ui.colored_label(
                    egui::Color32::from_rgb(90, 200, 120),
                    format!(
                        "✓ Done: {} transferred, {} skipped, {} failed — {}",
                        totals.files_copied,
                        totals.files_skipped,
                        totals.files_failed,
                        human_bytes(totals.bytes_copied)
                    ),
                );
            }
        }
    }

    fn ui_rename(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Pattern:");
            ui.text_edit_singleline(&mut self.rename_pattern);
        });
        ui.weak("{name} = original name · {ext} = extension · {n}/{nn}/{nnn} = numbered");

        if ui.button("✏  Rename").clicked() {
            self.do_rename();
        }

        if !self.sources.is_empty() {
            ui.add_space(6.0);
            egui::ScrollArea::vertical()
                .id_salt("rename_preview_scroll")
                .max_height(140.0)
                .show(ui, |ui| {
                    for (old_name, new_name) in self.rename_preview() {
                        ui.label(format!("{old_name} → {new_name}"));
                    }
                });
        }
    }
}
