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

/// Formats a duration as e.g. "42s", "3m 07s", "1h 05m". Sub-second
/// durations round to "0s" rather than "<1s" — good enough at the
/// granularity these progress displays update.
pub fn human_duration(secs: f64) -> String {
    let total = secs.max(0.0).round() as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{h}h {m:02}m")
    } else if m > 0 {
        format!("{m}m {s:02}s")
    } else {
        format!("{s}s")
    }
}

/// Estimated time remaining, given how long `done` out of `total` (of
/// whatever unit — bytes, files, ...) took. `None` when there's not
/// enough progress yet to extrapolate from, or nothing left to do.
pub fn eta(elapsed_secs: f64, done: u64, total: u64) -> Option<String> {
    if done == 0 || total == 0 || done >= total || elapsed_secs < 0.5 {
        return None;
    }
    let rate = done as f64 / elapsed_secs;
    let remaining = (total - done) as f64 / rate;
    Some(human_duration(remaining))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_duration_formats_seconds_minutes_hours() {
        assert_eq!(human_duration(0.0), "0s");
        assert_eq!(human_duration(9.4), "9s");
        assert_eq!(human_duration(65.0), "1m 05s");
        assert_eq!(human_duration(3661.0), "1h 01m");
    }

    #[test]
    fn eta_extrapolates_linearly_from_progress() {
        // 50 of 100 done after 10s -> 10s more at the same rate.
        assert_eq!(eta(10.0, 50, 100), Some("10s".to_string()));
    }

    #[test]
    fn eta_is_none_without_enough_signal() {
        assert_eq!(eta(10.0, 0, 100), None); // nothing done yet
        assert_eq!(eta(10.0, 100, 100), None); // already done
        assert_eq!(eta(10.0, 5, 0), None); // no total to compare against
        assert_eq!(eta(0.1, 5, 100), None); // too little elapsed to extrapolate
    }
}
