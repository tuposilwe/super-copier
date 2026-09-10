use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use eframe::egui;
use engine::copy::{CopyEvent, CopyOptions, CopySummary, OverwritePolicy};
use engine::fsops;

use crate::util::{human_bytes, human_rate, Log};
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

pub struct CopyTab {
    mode: Mode,
    sources: Vec<PathBuf>,
    dest: Option<PathBuf>,
    overwrite: OverwritePolicy,
    verify: bool,
    preserve_times: bool,
    use_fast_path: bool,
    rename_pattern: String,

    job: Option<Job<CopyEvent>>,
    started_at: Option<Instant>,
    total_files: usize,
    total_bytes: u64,
    files_done: usize,
    /// Bytes belonging to files that have fully finished copying.
    bytes_done_complete: u64,
    /// Files currently mid-transfer (there can be several at once — the
    /// engine copies multiple files concurrently).
    active: HashMap<PathBuf, ActiveFile>,
    summary: Option<CopySummary>,
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
            job: None,
            started_at: None,
            total_files: 0,
            total_bytes: 0,
            files_done: 0,
            bytes_done_complete: 0,
            active: HashMap::new(),
            summary: None,
            log: Log::default(),
        }
    }
}

impl CopyTab {
    pub fn is_running(&self) -> bool {
        self.job.is_some()
    }

    pub fn add_dropped(&mut self, paths: Vec<PathBuf>) {
        self.sources.extend(paths);
    }

    fn bytes_in_flight(&self) -> u64 {
        self.active.values().map(|f| f.bytes_done).sum()
    }

    fn poll(&mut self) {
        let Some(job) = &self.job else { return };
        let mut finished = false;
        for event in job.rx.try_iter() {
            match event {
                CopyEvent::Started {
                    total_files,
                    total_bytes,
                } => {
                    self.total_files = total_files;
                    self.total_bytes = total_bytes;
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
                    self.summary = Some(summary.clone());
                    self.bytes_done_complete = summary.bytes_copied;
                    self.log.push(format!(
                        "Done: {} transferred, {} skipped, {} failed in {:.1}s",
                        summary.files_copied, summary.files_skipped, summary.files_failed, summary.elapsed_secs
                    ));
                    finished = true;
                }
                CopyEvent::Cancelled => {
                    self.log.push("Cancelled.".to_string());
                    finished = true;
                }
            }
        }
        if finished {
            self.job = None;
            self.active.clear();
        }
    }

    fn start(&mut self) {
        if self.sources.is_empty() {
            self.log.push("Add at least one source file or folder first.".to_string());
            return;
        }
        let Some(dest) = self.dest.clone() else {
            self.log.push("Choose a destination folder first.".to_string());
            return;
        };
        let options = CopyOptions {
            overwrite: self.overwrite,
            verify: self.verify,
            preserve_times: self.preserve_times,
            threads: 0,
            use_fast_path: self.use_fast_path,
        };
        self.total_files = 0;
        self.total_bytes = 0;
        self.files_done = 0;
        self.bytes_done_complete = 0;
        self.active.clear();
        self.summary = None;
        self.log.clear();
        self.started_at = Some(Instant::now());
        self.job = Some(match self.mode {
            Mode::Copy => worker::spawn_copy(self.sources.clone(), dest, options),
            Mode::Move => worker::spawn_move(self.sources.clone(), dest, options),
            Mode::Rename => unreachable!("rename does not use the transfer job"),
        });
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
            self.log.push("Add at least one file or folder to rename first.".to_string());
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

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        self.poll();

        ui.heading("Copy / Move / Rename");
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.mode, Mode::Copy, "📄 Copy");
            ui.selectable_value(&mut self.mode, Mode::Move, "✂ Move");
            ui.selectable_value(&mut self.mode, Mode::Rename, "✏ Rename");
        });
        ui.label(match self.mode {
            Mode::Copy => "Copy files and folders with multi-threaded, OS-accelerated transfers.",
            Mode::Move => "Move files and folders — instant same-volume rename, or copy-then-delete across volumes.",
            Mode::Rename => "Rename files in place, optionally as a batch using a pattern.",
        });
        ui.separator();

        ui.horizontal(|ui| {
            if ui.button("➕ Add Files…").clicked() {
                if let Some(paths) = rfd::FileDialog::new().pick_files() {
                    self.sources.extend(paths);
                }
            }
            if ui.button("➕ Add Folder…").clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    self.sources.push(path);
                }
            }
            if ui.button("🗑 Clear Sources").clicked() {
                self.sources.clear();
            }
        });

        egui::ScrollArea::vertical()
            .id_salt("sources_scroll")
            .max_height(120.0)
            .show(ui, |ui| {
                let mut remove_idx = None;
                for (i, p) in self.sources.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(p.display().to_string());
                        if ui.small_button("✕").clicked() {
                            remove_idx = Some(i);
                        }
                    });
                }
                if let Some(i) = remove_idx {
                    self.sources.remove(i);
                }
            });

        ui.separator();

        if self.mode == Mode::Rename {
            self.ui_rename(ui);
        } else {
            self.ui_transfer(ui);
        }

        ui.separator();
        ui.label("Log:");
        egui::ScrollArea::vertical()
            .id_salt("copy_log_scroll")
            .max_height(180.0)
            .stick_to_bottom(true)
            .show(ui, |ui| {
                for line in self.log.iter() {
                    ui.label(line);
                }
            });
    }

    fn ui_transfer(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("📂 Choose Destination…").clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    self.dest = Some(path);
                }
            }
            ui.label(
                self.dest
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "(no destination selected)".to_string()),
            );
        });

        ui.separator();

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
            ui.checkbox(&mut self.verify, "Verify (hash check)");
            ui.checkbox(&mut self.preserve_times, "Preserve timestamps");
            ui.checkbox(&mut self.use_fast_path, "Use OS fast-copy");
        });

        ui.separator();

        ui.horizontal(|ui| {
            let running = self.is_running();
            let start_label = match self.mode {
                Mode::Copy => "▶ Start Copy",
                Mode::Move => "▶ Start Move",
                Mode::Rename => unreachable!(),
            };
            if ui.add_enabled(!running, egui::Button::new(start_label)).clicked() {
                self.start();
            }
            if ui.add_enabled(running, egui::Button::new("⏹ Cancel")).clicked() {
                if let Some(job) = &self.job {
                    job.cancel.cancel();
                }
            }
        });

        ui.separator();

        if self.total_files > 0 {
            let bytes_done = self.bytes_done_complete + self.bytes_in_flight();
            let frac = if self.total_bytes > 0 {
                bytes_done as f32 / self.total_bytes as f32
            } else {
                0.0
            };
            ui.label(format!(
                "Overall: {}/{} files, {} / {}",
                self.files_done,
                self.total_files,
                human_bytes(bytes_done),
                human_bytes(self.total_bytes)
            ));
            ui.add(egui::ProgressBar::new(frac.clamp(0.0, 1.0)).show_percentage());

            if let Some(started) = self.started_at {
                let secs = started.elapsed().as_secs_f64().max(0.001);
                let rate = bytes_done as f64 / secs;
                ui.label(format!("Throughput: {}", human_rate(rate)));
            }

            if !self.active.is_empty() {
                ui.label(format!("Transferring {} file(s) in parallel:", self.active.len()));
                let mut active: Vec<(&PathBuf, &ActiveFile)> = self.active.iter().collect();
                active.sort_by_key(|a| std::cmp::Reverse(a.1.bytes_done));
                for (path, f) in active.into_iter().take(6) {
                    let frac = if f.size > 0 { f.bytes_done as f32 / f.size as f32 } else { 1.0 };
                    ui.horizontal(|ui| {
                        ui.add(egui::ProgressBar::new(frac.clamp(0.0, 1.0)).desired_width(120.0));
                        ui.label(path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default());
                    });
                }
            }
        }

        if let Some(summary) = &self.summary {
            ui.colored_label(
                egui::Color32::from_rgb(90, 200, 120),
                format!(
                    "Finished: {} transferred, {} skipped, {} failed — {} in {:.1}s",
                    summary.files_copied,
                    summary.files_skipped,
                    summary.files_failed,
                    human_bytes(summary.bytes_copied),
                    summary.elapsed_secs
                ),
            );
        }
    }

    fn ui_rename(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Pattern:");
            ui.text_edit_singleline(&mut self.rename_pattern);
        });
        ui.label("Tokens: {name} = original name, {ext} = extension, {n}/{nn}/{nnn} = zero-padded sequence number.");

        if ui.button("✏ Rename").clicked() {
            self.do_rename();
        }

        if !self.sources.is_empty() {
            ui.separator();
            ui.label("Preview:");
            egui::ScrollArea::vertical()
                .id_salt("rename_preview_scroll")
                .max_height(160.0)
                .show(ui, |ui| {
                    for (old_name, new_name) in self.rename_preview() {
                        ui.label(format!("{old_name} → {new_name}"));
                    }
                });
        }
    }
}
