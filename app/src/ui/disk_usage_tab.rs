use std::time::Instant;

use eframe::egui;
use engine::diskusage::DriveUsage;

use crate::util::{human_bytes, human_duration};

pub struct DiskUsageTab {
    drives: Vec<DriveUsage>,
    refreshed_at: Option<Instant>,
}

impl Default for DiskUsageTab {
    fn default() -> Self {
        let mut tab = Self {
            drives: Vec::new(),
            refreshed_at: None,
        };
        tab.refresh();
        tab
    }
}

impl DiskUsageTab {
    fn refresh(&mut self) {
        self.drives = engine::diskusage::drive_usage();
        self.refreshed_at = Some(Instant::now());
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        ui.heading("Disk Usage");
        ui.label("Capacity and free space for every attached drive/volume — like Disk Utility, not a folder-by-folder breakdown.");
        ui.separator();

        ui.horizontal(|ui| {
            if ui.button("🔄 Refresh").clicked() {
                self.refresh();
            }
            if let Some(t) = self.refreshed_at {
                ui.weak(format!("Updated {} ago", human_duration(t.elapsed().as_secs_f64())));
            }
        });

        ui.separator();

        if self.drives.is_empty() {
            ui.weak("No drives found.");
            return;
        }

        egui::ScrollArea::vertical().show(ui, |ui| {
            for drive in &self.drives {
                ui.group(|ui| {
                    ui.set_width(ui.available_width());
                    ui.strong(drive.path.display().to_string());
                    match drive.space {
                        Some(space) => {
                            let used_frac = space.used_fraction();
                            let color = if used_frac > 0.9 {
                                egui::Color32::from_rgb(220, 80, 80)
                            } else if used_frac > 0.75 {
                                egui::Color32::from_rgb(230, 180, 60)
                            } else {
                                egui::Color32::from_rgb(90, 170, 90)
                            };
                            ui.add(
                                egui::ProgressBar::new(used_frac).fill(color).text(format!(
                                    "{} used of {} ({:.0}%)",
                                    human_bytes(space.used()),
                                    human_bytes(space.total),
                                    used_frac * 100.0
                                )),
                            );
                            ui.weak(format!("{} free", human_bytes(space.free)));
                        }
                        None => {
                            ui.weak("Couldn't read usage for this volume.");
                        }
                    }
                });
            }
        });
    }
}
