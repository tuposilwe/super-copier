use std::path::PathBuf;
use std::time::Instant;

use crossbeam_channel::Receiver;
use eframe::egui;
use engine::bootable::image::{analyze_image, ImageInfo, ImageKind};
use engine::bootable::{Device, Event, Mode};

use crate::dialog::DeferredPicker;
use crate::util::{eta, human_bytes, human_duration, human_rate, Log};
use crate::worker::{self, Job};

enum Outcome {
    Success,
    Failed(String),
    Cancelled,
}

pub struct BootTab {
    image: Option<PathBuf>,
    info: Option<ImageInfo>,
    image_error: Option<String>,
    /// For images with no boot signature and no ISO structure: the user can
    /// still choose to write them raw (e.g. a headerless disk dump).
    force_raw: bool,
    verify: bool,

    devices: Vec<Device>,
    scan: Option<Receiver<Result<Vec<Device>, String>>>,
    scan_error: Option<String>,
    scanned_once: bool,
    selected: Option<String>,
    confirm_text: String,
    picker: DeferredPicker,

    job: Option<Job<Event>>,
    phase: String,
    phase_started: Option<Instant>,
    done: u64,
    total: u64,
    outcome: Option<Outcome>,
    log: Log,
}

impl Default for BootTab {
    fn default() -> Self {
        Self {
            image: None,
            info: None,
            image_error: None,
            force_raw: false,
            verify: true,
            devices: Vec::new(),
            scan: None,
            scan_error: None,
            scanned_once: false,
            selected: None,
            confirm_text: String::new(),
            picker: DeferredPicker::default(),
            job: None,
            phase: String::new(),
            phase_started: None,
            done: 0,
            total: 0,
            outcome: None,
            log: Log::default(),
        }
    }
}

impl BootTab {
    pub fn is_running(&self) -> bool {
        self.job.is_some() || self.scan.is_some()
    }

    fn set_image(&mut self, path: PathBuf) {
        match analyze_image(&path) {
            Ok(info) => {
                self.info = Some(info);
                self.image_error = None;
            }
            Err(e) => {
                self.info = None;
                self.image_error = Some(e.to_string());
            }
        }
        self.image = Some(path);
        self.force_raw = false;
        self.outcome = None;
    }

    /// How the chosen image should be written, if it can be at all.
    fn mode(&self) -> Option<Mode> {
        match self.info.as_ref()?.kind {
            ImageKind::Hybrid => Some(Mode::Raw),
            ImageKind::OpticalOnly => Some(Mode::WindowsFiles),
            ImageKind::Unknown => self.force_raw.then_some(Mode::Raw),
        }
    }

    fn refresh_devices(&mut self) {
        if self.scan.is_none() {
            self.scan = Some(worker::spawn_device_scan());
            self.scanned_once = true;
        }
    }

    fn selected_device(&self) -> Option<&Device> {
        let id = self.selected.as_ref()?;
        self.devices.iter().find(|d| &d.id == id)
    }

    fn can_start(&self) -> bool {
        let (Some(dev), Some(info), Some(_)) = (self.selected_device(), &self.info, self.mode()) else {
            return false;
        };
        self.job.is_none()
            && info.size <= dev.size
            && self.confirm_text.trim().eq_ignore_ascii_case(&dev.id)
    }

    fn start(&mut self) {
        let (Some(image), Some(dev), Some(mode)) = (self.image.clone(), self.selected_device().cloned(), self.mode()) else {
            return;
        };
        self.log.clear();
        self.outcome = None;
        self.done = 0;
        self.total = 0;
        self.phase = "Waiting for administrator permission…".to_string();
        self.phase_started = Some(Instant::now());
        self.log.push(format!("Writing {} to {} ({})", image.display(), dev.id, dev.name));
        match worker::spawn_flash(image, dev.id.clone(), mode, self.verify && mode == Mode::Raw) {
            Ok(job) => self.job = Some(job),
            Err(e) => self.outcome = Some(Outcome::Failed(e)),
        }
    }

    fn poll(&mut self) {
        if let Some(rx) = &self.scan {
            if let Ok(result) = rx.try_recv() {
                match result {
                    Ok(list) => {
                        if let Some(id) = &self.selected {
                            if !list.iter().any(|d| &d.id == id) {
                                self.selected = None;
                            }
                        }
                        self.devices = list;
                        self.scan_error = None;
                    }
                    Err(e) => self.scan_error = Some(e),
                }
                self.scan = None;
            }
        }

        let Some(job) = &self.job else { return };
        let mut finished = false;
        for event in job.rx.try_iter() {
            match event {
                Event::Phase { name } => {
                    self.log.push(name.clone());
                    self.phase = name;
                    self.phase_started = Some(Instant::now());
                    self.done = 0;
                    self.total = 0;
                }
                Event::Progress { done, total } => {
                    self.done = done;
                    self.total = total;
                }
                Event::Log { line } => self.log.push(line),
                Event::Finished => {
                    self.log.push("Done — the drive is ready to boot from.");
                    crate::notify::notify("Bootable USB ready", "The drive is ready. Safe to unplug.");
                    self.outcome = Some(Outcome::Success);
                    finished = true;
                }
                Event::Failed { message } => {
                    self.log.push(format!("Failed: {message}"));
                    crate::notify::notify("Bootable USB failed", &message);
                    self.outcome = Some(Outcome::Failed(message));
                    finished = true;
                }
                Event::Cancelled => {
                    self.log.push("Cancelled — the drive may be left unusable until it's written again.");
                    self.outcome = Some(Outcome::Cancelled);
                    finished = true;
                }
            }
        }
        if finished {
            self.job = None;
            self.confirm_text.clear();
            self.refresh_devices();
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, dropped: Vec<PathBuf>) {
        self.poll();
        if let Some(mut picked) = self.picker.poll() {
            if let Some(p) = picked.pop() {
                self.set_image(p);
            }
        }
        if let Some(p) = dropped.into_iter().find(|p| p.is_file()) {
            if self.job.is_none() {
                self.set_image(p);
            }
        }
        if !self.scanned_once {
            self.refresh_devices();
        }
        if self.is_running() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
        let running = self.job.is_some();

        egui::ScrollArea::vertical().id_salt("boot_scroll").show(ui, |ui| {
            ui.heading("Bootable USB");
            ui.label("Turn an ISO/IMG file into a USB drive you can boot a computer from — Linux, rescue disks, and Windows installers.");
            ui.colored_label(
                egui::Color32::from_rgb(230, 170, 60),
                "Everything on the chosen drive will be erased. You'll be asked for your administrator password.",
            );
            ui.separator();

            ui.add_enabled_ui(!running, |ui| {
                ui.strong("1. Choose the image");
                ui.horizontal(|ui| {
                    if ui.button("💿 Choose ISO / IMG…").clicked() {
                        self.picker.request_files();
                    }
                    match &self.image {
                        Some(p) => ui.label(p.display().to_string()),
                        None => ui.weak("(none — click, or drop a file onto this window)"),
                    };
                });
                if let Some(e) = &self.image_error {
                    ui.colored_label(egui::Color32::from_rgb(220, 80, 80), format!("Couldn't read that file: {e}"));
                }
                if let Some(info) = &self.info {
                    ui.label(format!("Size: {}", human_bytes(info.size)));
                    match info.kind {
                        ImageKind::Hybrid => {
                            ui.label("✔ Bootable image — it will be written to the drive as-is, then verified.");
                        }
                        ImageKind::OpticalOnly => {
                            ui.label("ℹ Disc-only ISO (like a Windows installer). Its files will be copied onto a FAT32 drive, and install.wim is split if it's over 4 GB.");
                            ui.weak("This suits Windows installers. Other disc-only ISOs may not boot from USB.");
                        }
                        ImageKind::Unknown => {
                            ui.colored_label(
                                egui::Color32::from_rgb(230, 170, 60),
                                "This doesn't look like a bootable ISO or disk image.",
                            );
                            ui.checkbox(&mut self.force_raw, "Write it to the drive byte-for-byte anyway");
                        }
                    }
                    if self.mode() == Some(Mode::Raw) {
                        ui.checkbox(&mut self.verify, "Verify after writing (recommended, takes about as long again)");
                    }
                }

                ui.add_space(6.0);
                ui.strong("2. Choose the USB drive");
                ui.horizontal(|ui| {
                    if ui.add_enabled(self.scan.is_none(), egui::Button::new("🔄 Refresh")).clicked() {
                        self.refresh_devices();
                    }
                    if self.scan.is_some() {
                        ui.spinner();
                        ui.weak("Looking for drives…");
                    }
                });
                if let Some(e) = &self.scan_error {
                    ui.colored_label(egui::Color32::from_rgb(220, 80, 80), format!("Couldn't list drives: {e}"));
                }
                if self.devices.is_empty() && self.scan.is_none() && self.scan_error.is_none() {
                    ui.weak("No USB drives found. Plug one in and press Refresh. Your computer's own disks are never shown.");
                }
                let mut chosen = None;
                for d in &self.devices {
                    let text = format!("{} — {} ({}, {})", d.id, d.name, human_bytes(d.size), d.bus);
                    let too_small = self.info.as_ref().is_some_and(|i| i.size > d.size);
                    let label = if too_small { format!("{text}  — too small for this image") } else { text };
                    if ui.selectable_label(self.selected.as_ref() == Some(&d.id), label).clicked() && !too_small {
                        chosen = Some(d.id.clone());
                    }
                }
                if let Some(id) = chosen {
                    if self.selected.as_ref() != Some(&id) {
                        self.confirm_text.clear();
                    }
                    self.selected = Some(id);
                }
            });

            ui.add_space(6.0);
            ui.strong("3. Confirm and start");
            if let Some(dev) = self.selected_device().cloned() {
                ui.add_enabled_ui(!running, |ui| {
                    ui.label(format!("This will ERASE {} ({}, {}).", dev.id, dev.name, human_bytes(dev.size)));
                    ui.horizontal(|ui| {
                        ui.label(format!("Type {} to confirm:", dev.id));
                        ui.add(egui::TextEdit::singleline(&mut self.confirm_text).desired_width(120.0));
                    });
                });
            } else {
                ui.weak("Pick a drive above first.");
            }
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(self.can_start(), egui::Button::new("▶ Create bootable drive"))
                    .clicked()
                {
                    self.start();
                }
                if ui.add_enabled(running, egui::Button::new("⏹ Cancel")).clicked() {
                    if let Some(job) = &self.job {
                        job.cancel.cancel();
                    }
                }
            });

            if running {
                ui.separator();
                ui.label(&self.phase);
                if self.total > 0 {
                    let frac = self.done as f32 / self.total as f32;
                    ui.add(egui::ProgressBar::new(frac).show_percentage());
                    let elapsed = self.phase_started.map(|t| t.elapsed().as_secs_f64()).unwrap_or(0.0);
                    let mut line = format!("{} of {}", human_bytes(self.done), human_bytes(self.total));
                    if elapsed > 0.5 {
                        line.push_str(&format!(" — {}", human_rate(self.done as f64 / elapsed)));
                    }
                    if let Some(left) = eta(elapsed, self.done, self.total) {
                        line.push_str(&format!(" — ~{left} remaining"));
                    }
                    ui.weak(line);
                } else {
                    ui.spinner();
                }
                let total_elapsed = self.phase_started.map(|t| human_duration(t.elapsed().as_secs_f64()));
                if let Some(t) = total_elapsed {
                    ui.weak(format!("This step: {t}"));
                }
            }

            match &self.outcome {
                Some(Outcome::Success) => {
                    ui.colored_label(egui::Color32::from_rgb(90, 200, 120), "✔ Done. You can unplug the drive and boot from it.");
                }
                Some(Outcome::Failed(msg)) => {
                    ui.colored_label(egui::Color32::from_rgb(220, 80, 80), format!("✗ Failed: {msg}"));
                }
                Some(Outcome::Cancelled) => {
                    ui.colored_label(egui::Color32::from_rgb(230, 170, 60), "Cancelled. The drive may not boot until it's written again.");
                }
                None => {}
            }

            ui.separator();
            ui.label("Log:");
            egui::ScrollArea::vertical()
                .id_salt("boot_log_scroll")
                .max_height(120.0)
                .stick_to_bottom(true)
                .show(ui, |ui| {
                    for line in self.log.iter() {
                        ui.label(line);
                    }
                });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: &str, size: u64) -> Device {
        Device {
            id: id.into(),
            node: format!("/dev/r{id}"),
            name: "Test Stick".into(),
            size,
            bus: "USB".into(),
            mounted: vec![],
            is_system: false,
            is_virtual: false,
        }
    }

    fn ready_tab(kind: ImageKind, image_size: u64, stick_size: u64) -> BootTab {
        BootTab {
            scanned_once: true, // don't touch the real machine's drives
            image: Some(PathBuf::from("/tmp/x.iso")),
            info: Some(ImageInfo { size: image_size, kind }),
            devices: vec![device("disk4", stick_size)],
            selected: Some("disk4".into()),
            ..BootTab::default()
        }
    }

    #[test]
    fn start_is_blocked_until_the_drive_id_is_typed_exactly() {
        let mut tab = ready_tab(ImageKind::Hybrid, 1000, 8000);
        assert!(!tab.can_start(), "nothing typed yet");
        tab.confirm_text = "disk5".into();
        assert!(!tab.can_start(), "the wrong drive's name must not unlock it");
        tab.confirm_text = " DISK4 ".into();
        assert!(tab.can_start(), "case and surrounding spaces are forgiven");
    }

    #[test]
    fn start_is_blocked_when_the_image_does_not_fit_or_cant_be_written() {
        let mut too_big = ready_tab(ImageKind::Hybrid, 9000, 8000);
        too_big.confirm_text = "disk4".into();
        assert!(!too_big.can_start());

        let mut unknown = ready_tab(ImageKind::Unknown, 1000, 8000);
        unknown.confirm_text = "disk4".into();
        assert!(!unknown.can_start(), "an unrecognised image needs the explicit raw-write opt-in");
        unknown.force_raw = true;
        assert!(unknown.can_start());

        let mut no_drive = ready_tab(ImageKind::Hybrid, 1000, 8000);
        no_drive.selected = None;
        no_drive.confirm_text = "disk4".into();
        assert!(!no_drive.can_start());
    }

    #[test]
    fn image_kind_decides_the_write_mode() {
        assert_eq!(ready_tab(ImageKind::Hybrid, 1, 2).mode(), Some(Mode::Raw));
        assert_eq!(ready_tab(ImageKind::OpticalOnly, 1, 2).mode(), Some(Mode::WindowsFiles));
        assert_eq!(ready_tab(ImageKind::Unknown, 1, 2).mode(), None);
    }

    #[test]
    fn every_state_renders_without_panicking() {
        let ctx = egui::Context::default();
        let states: Vec<BootTab> = vec![
            BootTab { scanned_once: true, ..BootTab::default() },
            ready_tab(ImageKind::Hybrid, 1000, 8000),
            ready_tab(ImageKind::OpticalOnly, 1000, 8000),
            ready_tab(ImageKind::Unknown, 1000, 500),
            {
                let mut t = ready_tab(ImageKind::Hybrid, 1000, 8000);
                t.outcome = Some(Outcome::Failed("boom".into()));
                t.scan_error = Some("no diskutil".into());
                t
            },
        ];
        for mut tab in states {
            for _ in 0..3 {
                let mut out = ctx.run_ui(egui::RawInput::default(), |ctx| {
                    egui::CentralPanel::default().show(ctx, |ui| tab.ui(ui, vec![]));
                });
                out.textures_delta.clear();
            }
        }
    }
}
