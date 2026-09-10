use std::collections::VecDeque;
use std::path::PathBuf;

use eframe::egui;

pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{:.1} {}", size, UNITS[unit])
    }
}

pub fn human_rate(bytes_per_sec: f64) -> String {
    format!("{}/s", human_bytes(bytes_per_sec as u64))
}

/// Fixed-capacity log of recent status lines, newest first.
pub struct Log {
    lines: VecDeque<String>,
    cap: usize,
}

impl Log {
    pub fn new(cap: usize) -> Self {
        Self {
            lines: VecDeque::with_capacity(cap),
            cap,
        }
    }

    pub fn push(&mut self, line: impl Into<String>) {
        self.lines.push_front(line.into());
        while self.lines.len() > self.cap {
            self.lines.pop_back();
        }
    }

    pub fn clear(&mut self) {
        self.lines.clear();
    }

    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = &String> {
        self.lines.iter()
    }
}

impl Default for Log {
    fn default() -> Self {
        Self::new(200)
    }
}

/// A button that opens a menu of locally attached drives/volumes (drive
/// letters like `C:\`, `D:\` on Windows; mounted volumes on macOS), so the
/// user can add a whole drive as a scan root in one click instead of
/// browsing to it. Returns the drive picked this frame, if any.
pub fn drives_menu_button(ui: &mut egui::Ui) -> Option<PathBuf> {
    let mut chosen = None;
    ui.menu_button("💽 Add Drive", |ui| {
        let drives = engine::drives::list_drives();
        if drives.is_empty() {
            ui.label("(no drives found)");
        }
        for drive in drives {
            let label = drive.display().to_string();
            if ui.button(label).clicked() {
                chosen = Some(drive);
                ui.close();
            }
        }
    });
    chosen
}
