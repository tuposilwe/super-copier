use std::path::{Path, PathBuf};
use std::time::Instant;

use eframe::egui;
use engine::archive::{inspect_zip, ArchiveEvent, ArchiveSummary, ZipInfo, ZipOptions};

use crate::dialog::DeferredPicker;
use crate::util::{eta, human_bytes, human_duration, human_rate, Log};
use crate::worker::{self, Job};

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Compress,
    Extract,
}

enum Outcome {
    Done { summary: ArchiveSummary, reveal: PathBuf },
    Failed(String),
    Cancelled,
}

const LEVELS: [(u8, &str); 4] = [(1, "Fast"), (6, "Normal"), (9, "Smallest"), (0, "None (store only)")];

pub struct ArchiveTab {
    mode: Mode,

    sources: Vec<PathBuf>,
    output: Option<PathBuf>,
    output_is_custom: bool,
    level: u8,

    zip: Option<PathBuf>,
    zip_info: Option<Result<ZipInfo, String>>,
    dest: Option<PathBuf>,
    dest_is_custom: bool,
    overwrite: bool,

    add_files: DeferredPicker,
    add_folder: DeferredPicker,
    save_zip: DeferredPicker,
    pick_zip: DeferredPicker,
    pick_dest: DeferredPicker,

    job: Option<Job<ArchiveEvent>>,
    job_reveal: Option<PathBuf>,
    started_at: Option<Instant>,
    done: u64,
    total: u64,
    last_error: Option<String>,
    cancel_requested: bool,
    outcome: Option<Outcome>,
    log: Log,
}

impl Default for ArchiveTab {
    fn default() -> Self {
        Self {
            mode: Mode::Compress,
            sources: Vec::new(),
            output: None,
            output_is_custom: false,
            level: 6,
            zip: None,
            zip_info: None,
            dest: None,
            dest_is_custom: false,
            overwrite: false,
            add_files: DeferredPicker::default(),
            add_folder: DeferredPicker::default(),
            save_zip: DeferredPicker::default(),
            pick_zip: DeferredPicker::default(),
            pick_dest: DeferredPicker::default(),
            job: None,
            job_reveal: None,
            started_at: None,
            done: 0,
            total: 0,
            last_error: None,
            cancel_requested: false,
            outcome: None,
            log: Log::default(),
        }
    }
}

/// Name to suggest for a new archive: the item's own name if there's just
/// one, otherwise a generic "Archive".
pub(crate) fn default_zip_name(sources: &[PathBuf]) -> String {
    let stem = match sources {
        [one] => one
            .file_stem()
            .or_else(|| one.file_name())
            .map(|s| s.to_string_lossy().into_owned()),
        _ => None,
    };
    format!("{}.zip", stem.filter(|s| !s.is_empty()).unwrap_or_else(|| "Archive".to_string()))
}

/// `path` itself if nothing is there yet, otherwise "name 2.ext", "name 3.ext"…
/// so a default choice never silently replaces something.
pub(crate) fn unique_path(path: PathBuf) -> PathBuf {
    if !path.exists() {
        return path;
    }
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = path.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (2..)
        .map(|n| path.with_file_name(format!("{stem} {n}{ext}")))
        .find(|p| !p.exists())
        .expect("an unused name exists")
}

fn is_zip(p: &Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case("zip"))
}

impl ArchiveTab {
    pub fn is_running(&self) -> bool {
        self.job.is_some()
    }

    fn add_sources(&mut self, paths: Vec<PathBuf>) {
        for p in paths {
            if !self.sources.contains(&p) {
                self.sources.push(p);
            }
        }
        self.refresh_default_output();
    }

    fn refresh_default_output(&mut self) {
        if self.output_is_custom {
            return;
        }
        self.output = self.sources.first().and_then(|first| {
            let dir = first.parent()?;
            Some(unique_path(dir.join(default_zip_name(&self.sources))))
        });
    }

    fn set_zip(&mut self, path: PathBuf) {
        self.zip_info = Some(inspect_zip(&path).map_err(|e| e.to_string()));
        if !self.dest_is_custom {
            self.dest = path.parent().map(|dir| {
                let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "Extracted".into());
                unique_path(dir.join(stem))
            });
        }
        self.zip = Some(path);
        self.outcome = None;
    }

    fn can_start(&self) -> bool {
        if self.job.is_some() {
            return false;
        }
        match self.mode {
            Mode::Compress => !self.sources.is_empty() && self.output.is_some(),
            Mode::Extract => matches!(self.zip_info, Some(Ok(_))) && self.dest.is_some(),
        }
    }

    fn start(&mut self) {
        self.log.clear();
        self.outcome = None;
        self.last_error = None;
        self.cancel_requested = false;
        self.done = 0;
        self.total = 0;
        self.started_at = Some(Instant::now());
        match self.mode {
            Mode::Compress => {
                let (Some(out), false) = (self.output.clone(), self.sources.is_empty()) else { return };
                self.log.push(format!("Creating {}", out.display()));
                self.job_reveal = Some(out.clone());
                self.job = Some(worker::spawn_zip(self.sources.clone(), out, ZipOptions { level: self.level }));
            }
            Mode::Extract => {
                let (Some(zip), Some(dest)) = (self.zip.clone(), self.dest.clone()) else { return };
                self.log.push(format!("Extracting {} to {}", zip.display(), dest.display()));
                self.job_reveal = Some(dest.clone());
                self.job = Some(worker::spawn_unzip(zip, dest, self.overwrite));
            }
        }
    }

    fn poll(&mut self) {
        let Some(job) = &self.job else { return };
        let mut finished = false;
        for event in job.rx.try_iter() {
            match event {
                ArchiveEvent::Started { files, bytes } => {
                    self.total = bytes;
                    self.log.push(format!("{files} file(s), {}", human_bytes(bytes)));
                }
                ArchiveEvent::Progress { done, total } => {
                    self.done = done;
                    self.total = total;
                }
                ArchiveEvent::FileSkipped { path, reason } => self.log.push(format!("skipped {path} — {reason}")),
                ArchiveEvent::FileError { path, message } => {
                    self.log.push(format!("✗ {path} — {message}"));
                    self.last_error = Some(format!("{path}: {message}"));
                }
                ArchiveEvent::Finished(summary) => {
                    let verb = if self.mode == Mode::Compress { "Zipped" } else { "Extracted" };
                    self.log.push(format!(
                        "{verb} {} file(s), {} — {}",
                        summary.files,
                        human_bytes(summary.bytes),
                        human_duration(summary.elapsed_secs)
                    ));
                    crate::notify::notify(
                        if self.mode == Mode::Compress { "Zip finished" } else { "Unzip finished" },
                        &format!("{} file(s)", summary.files),
                    );
                    let reveal = self.job_reveal.clone().unwrap_or_default();
                    self.outcome = Some(Outcome::Done { summary, reveal });
                    finished = true;
                }
                ArchiveEvent::Cancelled => {
                    // A fatal error is reported as a FileError followed by
                    // Cancelled; only a user-requested stop is a plain cancel.
                    self.outcome = Some(match (&self.last_error, self.cancel_requested) {
                        (Some(e), false) => Outcome::Failed(e.clone()),
                        _ => Outcome::Cancelled,
                    });
                    self.log.push("Stopped.");
                    finished = true;
                }
            }
        }
        if finished {
            self.job = None;
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, dropped: Vec<PathBuf>) {
        self.poll();
        if let Some(paths) = self.add_files.poll() {
            self.add_sources(paths);
        }
        if let Some(paths) = self.add_folder.poll() {
            self.add_sources(paths);
        }
        if let Some(mut paths) = self.save_zip.poll() {
            if let Some(p) = paths.pop() {
                self.output = Some(if is_zip(&p) { p } else { p.with_extension("zip") });
                self.output_is_custom = true;
            }
        }
        if let Some(mut paths) = self.pick_zip.poll() {
            if let Some(p) = paths.pop() {
                self.set_zip(p);
            }
        }
        if let Some(mut paths) = self.pick_dest.poll() {
            if let Some(p) = paths.pop() {
                self.dest = Some(p);
                self.dest_is_custom = true;
            }
        }
        if self.job.is_none() && !dropped.is_empty() {
            match self.mode {
                Mode::Compress => self.add_sources(dropped),
                Mode::Extract => {
                    if let Some(z) = dropped.into_iter().find(|p| is_zip(p)) {
                        self.set_zip(z);
                    }
                }
            }
        }
        if self.job.is_some() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
        let running = self.job.is_some();

        egui::ScrollArea::vertical().id_salt("archive_scroll").show(ui, |ui| {
            ui.heading("Zip / Unzip");
            ui.label("Compress files and folders into a .zip, or extract one. Drop files onto the window to add them.");
            ui.add_enabled_ui(!running, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.mode, Mode::Compress, "🗜 Compress");
                    ui.selectable_value(&mut self.mode, Mode::Extract, "📦 Extract");
                });
            });
            ui.separator();

            ui.add_enabled_ui(!running, |ui| match self.mode {
                Mode::Compress => self.compress_ui(ui),
                Mode::Extract => self.extract_ui(ui),
            });

            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let label = if self.mode == Mode::Compress { "▶ Create zip" } else { "▶ Extract" };
                if ui.add_enabled(self.can_start(), egui::Button::new(label)).clicked() {
                    self.start();
                }
                if ui.add_enabled(running, egui::Button::new("⏹ Cancel")).clicked() {
                    self.cancel_requested = true;
                    if let Some(job) = &self.job {
                        job.cancel.cancel();
                    }
                }
            });

            if running {
                let elapsed = self.started_at.map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0);
                if self.total > 0 {
                    ui.add(egui::ProgressBar::new(self.done as f32 / self.total as f32).show_percentage());
                    let mut line = format!("{} of {}", human_bytes(self.done), human_bytes(self.total));
                    if elapsed > 0.5 {
                        line.push_str(&format!(" — {}", human_rate(self.done as f64 / elapsed)));
                    }
                    if let Some(left) = eta(elapsed, self.done, self.total) {
                        line.push_str(&format!(" — ~{left} remaining"));
                    }
                    ui.weak(format!("{line} — {} elapsed", human_duration(elapsed)));
                } else {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.weak(format!("Working… {} elapsed", human_duration(elapsed)));
                    });
                }
            }

            match &self.outcome {
                Some(Outcome::Done { summary, reveal }) => {
                    let mut msg = format!(
                        "✔ Done — {} file(s), {}",
                        summary.files,
                        human_bytes(summary.bytes)
                    );
                    if summary.archive_bytes > 0 && summary.bytes > 0 {
                        msg.push_str(&format!(
                            " → {} ({:.0}% of original)",
                            human_bytes(summary.archive_bytes),
                            summary.archive_bytes as f64 / summary.bytes as f64 * 100.0
                        ));
                    }
                    ui.colored_label(egui::Color32::from_rgb(90, 200, 120), msg);
                    if summary.failed > 0 {
                        ui.colored_label(
                            egui::Color32::from_rgb(230, 170, 60),
                            format!("{} item(s) couldn't be processed — see the log.", summary.failed),
                        );
                    }
                    if summary.skipped > 0 {
                        ui.weak(format!("{} existing file(s) were left alone.", summary.skipped));
                    }
                    if ui.button("📂 Show in Finder/Explorer").clicked() {
                        if let Err(e) = crate::reveal::reveal(reveal) {
                            self.log.push(format!("Couldn't open folder: {e}"));
                        }
                    }
                }
                Some(Outcome::Failed(msg)) => {
                    ui.colored_label(egui::Color32::from_rgb(220, 80, 80), format!("✗ Failed: {msg}"));
                }
                Some(Outcome::Cancelled) => {
                    let note = if self.mode == Mode::Compress {
                        "Cancelled. No zip was created."
                    } else {
                        "Cancelled. Files extracted so far were left in place."
                    };
                    ui.colored_label(egui::Color32::from_rgb(230, 170, 60), note);
                }
                None => {}
            }

            ui.separator();
            ui.label("Log:");
            egui::ScrollArea::vertical()
                .id_salt("archive_log_scroll")
                .max_height(140.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in self.log.iter() {
                        ui.label(line);
                    }
                });
        });
    }

    fn compress_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("➕ Add files…").clicked() {
                self.add_files.request_files();
            }
            if ui.button("📁 Add folder…").clicked() {
                self.add_folder.request_folder();
            }
            if ui.button("🗑 Clear").clicked() {
                self.sources.clear();
                self.output_is_custom = false;
                self.refresh_default_output();
            }
        });

        if self.sources.is_empty() {
            ui.weak("Nothing added yet — pick files or folders, or drop them onto this window.");
        } else {
            let mut remove = None;
            egui::ScrollArea::vertical().id_salt("archive_sources").max_height(160.0).show(ui, |ui| {
                for (i, p) in self.sources.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.small_button("✕").clicked() {
                            remove = Some(i);
                        }
                        ui.label(if p.is_dir() { "📁" } else { "📄" });
                        ui.label(p.display().to_string());
                    });
                }
            });
            if let Some(i) = remove {
                self.sources.remove(i);
                self.refresh_default_output();
            }
            ui.weak(format!("{} item(s)", self.sources.len()));
        }

        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.label("Compression:");
            let current = LEVELS.iter().find(|(l, _)| *l == self.level).map(|(_, n)| *n).unwrap_or("Normal");
            egui::ComboBox::from_id_salt("zip_level").selected_text(current).show_ui(ui, |ui| {
                for (level, name) in LEVELS {
                    ui.selectable_value(&mut self.level, level, name);
                }
            });
        });
        ui.horizontal(|ui| {
            ui.label("Save as:");
            match &self.output {
                Some(p) => ui.label(p.display().to_string()),
                None => ui.weak("(add something first)"),
            };
            if ui.add_enabled(!self.sources.is_empty(), egui::Button::new("Change…")).clicked() {
                self.save_zip.request_save_zip(default_zip_name(&self.sources));
            }
        });
    }

    fn extract_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("📦 Choose .zip file…").clicked() {
                self.pick_zip.request_files();
            }
            match &self.zip {
                Some(p) => ui.label(p.display().to_string()),
                None => ui.weak("(none — click, or drop a .zip onto this window)"),
            };
        });
        match &self.zip_info {
            Some(Ok(info)) => {
                ui.label(format!(
                    "{} file(s) — {} once extracted ({} zipped)",
                    info.files,
                    human_bytes(info.uncompressed),
                    human_bytes(info.compressed)
                ));
            }
            Some(Err(e)) => {
                ui.colored_label(egui::Color32::from_rgb(220, 80, 80), format!("Can't read that as a zip file: {e}"));
            }
            None => {}
        }
        ui.horizontal(|ui| {
            ui.label("Extract to:");
            match &self.dest {
                Some(p) => ui.label(p.display().to_string()),
                None => ui.weak("(choose a zip first)"),
            };
            if ui.add_enabled(self.zip.is_some(), egui::Button::new("Change…")).clicked() {
                self.pick_dest.request_folder();
            }
        });
        ui.checkbox(&mut self.overwrite, "Replace files that already exist (otherwise they're skipped)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggested_names_follow_what_is_being_zipped() {
        assert_eq!(default_zip_name(&[PathBuf::from("/x/Holiday Photos")]), "Holiday Photos.zip");
        assert_eq!(default_zip_name(&[PathBuf::from("/x/report.pdf")]), "report.zip", "extension isn't doubled up");
        assert_eq!(default_zip_name(&[PathBuf::from("/a"), PathBuf::from("/b")]), "Archive.zip");
        assert_eq!(default_zip_name(&[]), "Archive.zip");
    }

    #[test]
    fn a_default_output_never_replaces_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let wanted = dir.path().join("notes.zip");
        assert_eq!(unique_path(wanted.clone()), wanted, "free name is used as-is");
        std::fs::write(&wanted, b"x").unwrap();
        assert_eq!(unique_path(wanted.clone()), dir.path().join("notes 2.zip"));
        std::fs::write(dir.path().join("notes 2.zip"), b"x").unwrap();
        assert_eq!(unique_path(wanted), dir.path().join("notes 3.zip"));
    }

    #[test]
    fn adding_items_picks_a_default_output_next_to_them_until_the_user_chooses_one() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("proj");
        std::fs::create_dir_all(&folder).unwrap();
        let mut tab = ArchiveTab::default();
        assert!(!tab.can_start());
        tab.add_sources(vec![folder.clone()]);
        assert_eq!(tab.output, Some(dir.path().join("proj.zip")));
        assert!(tab.can_start());

        tab.output = Some(PathBuf::from("/chosen/mine.zip"));
        tab.output_is_custom = true;
        tab.add_sources(vec![dir.path().join("other")]);
        assert_eq!(tab.output, Some(PathBuf::from("/chosen/mine.zip")), "a chosen path is never overwritten");
    }

    #[test]
    fn choosing_a_zip_suggests_a_folder_beside_it_and_flags_unreadable_files() {
        let dir = tempfile::tempdir().unwrap();
        let bogus = dir.path().join("broken.zip");
        std::fs::write(&bogus, b"not a zip").unwrap();
        let mut tab = ArchiveTab { mode: Mode::Extract, ..ArchiveTab::default() };
        tab.set_zip(bogus);
        assert!(matches!(tab.zip_info, Some(Err(_))));
        assert!(!tab.can_start(), "an unreadable zip can't be extracted");
        assert_eq!(tab.dest, Some(dir.path().join("broken")));
    }

    #[test]
    fn every_state_renders_without_panicking() {
        let ctx = egui::Context::default();
        let tabs = vec![
            ArchiveTab::default(),
            ArchiveTab { sources: vec![PathBuf::from("/tmp/a")], output: Some("/tmp/a.zip".into()), ..ArchiveTab::default() },
            ArchiveTab { mode: Mode::Extract, ..ArchiveTab::default() },
            ArchiveTab {
                mode: Mode::Extract,
                zip: Some("/tmp/x.zip".into()),
                zip_info: Some(Ok(ZipInfo { entries: 3, files: 2, uncompressed: 1000, compressed: 400 })),
                dest: Some("/tmp/x".into()),
                outcome: Some(Outcome::Done {
                    summary: ArchiveSummary { files: 2, bytes: 1000, archive_bytes: 400, ..ArchiveSummary::default() },
                    reveal: "/tmp/x".into(),
                }),
                ..ArchiveTab::default()
            },
            ArchiveTab { outcome: Some(Outcome::Failed("boom".into())), ..ArchiveTab::default() },
            ArchiveTab { outcome: Some(Outcome::Cancelled), ..ArchiveTab::default() },
        ];
        for mut tab in tabs {
            for _ in 0..3 {
                let mut out = ctx.run_ui(egui::RawInput::default(), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| tab.ui(ui, vec![]));
                });
                out.textures_delta.clear();
            }
        }
    }
}
