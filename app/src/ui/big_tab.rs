use std::path::PathBuf;

use eframe::egui;

use super::big_folders_tab::BigFoldersTab;
use super::large_files_tab::LargeFilesTab;

#[derive(PartialEq, Clone, Copy)]
enum Mode {
    Files,
    Folders,
}

/// Big Files and Big Folders share one tab: both answer "what's using my
/// space", just at different granularity. Each keeps its own results and
/// settings; only the chosen scan folders are carried across a switch so
/// the user doesn't have to re-add them.
pub struct BigTab {
    mode: Mode,
    files: LargeFilesTab,
    folders: BigFoldersTab,
}

impl Default for BigTab {
    fn default() -> Self {
        Self {
            mode: Mode::Files,
            files: LargeFilesTab::default(),
            folders: BigFoldersTab::default(),
        }
    }
}

impl BigTab {
    pub fn is_running(&self) -> bool {
        self.files.is_running() || self.folders.is_running()
    }

    pub fn add_dropped(&mut self, paths: Vec<PathBuf>) {
        match self.mode {
            Mode::Files => self.files.add_dropped(paths),
            Mode::Folders => self.folders.add_dropped(paths),
        }
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        let previous = self.mode;
        // Only the visible mode polls its scan's results, so switching
        // mid-scan would leave the other one stalled until switched back.
        let locked = self.is_running();
        ui.add_enabled_ui(!locked, |ui| {
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.mode, Mode::Files, "🐘 Big Files");
                ui.selectable_value(&mut self.mode, Mode::Folders, "🗄 Big Folders");
            });
        });
        if self.mode != previous {
            match self.mode {
                Mode::Folders => self.folders.set_roots(self.files.roots().to_vec()),
                Mode::Files => self.files.set_roots(self.folders.roots().to_vec()),
            }
        }
        ui.separator();

        match self.mode {
            Mode::Files => self.files.ui(ui),
            Mode::Folders => self.folders.ui(ui),
        }
    }
}
