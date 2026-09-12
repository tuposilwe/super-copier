#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod dialog;
mod dnd;
mod ipc;
mod notify;
mod reveal;
mod ui;
mod util;
mod worker;

use std::path::PathBuf;

use crossbeam_channel::Receiver;
use eframe::egui;

/// 256x256 RGBA8 pixels, pre-rendered from `assets/icon-1024.png` (see
/// `assets/` for the source and the regeneration steps in the README).
const ICON_RGBA: &[u8] = include_bytes!("../assets/icon_256.rgba");

fn app_icon() -> egui::IconData {
    egui::IconData {
        rgba: ICON_RGBA.to_vec(),
        width: 256,
        height: 256,
    }
}

fn main() -> eframe::Result {
    // `--move` plus a list of files/folders is how the Explorer "Copy with
    // Super Copier" / "Move with Super Copier" context menu entries invoke
    // us (see packaging/windows/installer.nsi).
    let mut move_mode = false;
    let mut initial_paths = Vec::new();
    for arg in std::env::args_os().skip(1) {
        if arg == "--move" {
            move_mode = true;
        } else {
            initial_paths.push(PathBuf::from(arg));
        }
    }

    let Some(ipc_rx) = ipc::acquire_primary(&initial_paths, move_mode) else {
        // Another instance is already running and now has our paths —
        // don't open a second window.
        return Ok(());
    };

    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([980.0, 680.0])
            .with_min_inner_size([680.0, 480.0])
            .with_icon(app_icon()),
        ..Default::default()
    };

    eframe::run_native(
        "Super Copier",
        native_options,
        Box::new(move |cc| {
            setup_style(&cc.egui_ctx);
            Ok(Box::new(SuperCopierApp::new(ipc_rx)))
        }),
    )
}

fn setup_style(ctx: &egui::Context) {
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = egui::vec2(8.0, 8.0);
    });
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Tab {
    Copy,
    Search,
    Duplicates,
    BigFiles,
    BigFolders,
    DiskUsage,
    Organize,
    Sync,
    Share,
}

struct SuperCopierApp {
    tab: Tab,
    theme: egui::ThemePreference,
    ipc_rx: Receiver<ipc::IpcMessage>,
    copy_tab: ui::copy_tab::CopyTab,
    search_tab: ui::search_tab::SearchTab,
    dup_tab: ui::dup_tab::DupTab,
    large_files_tab: ui::large_files_tab::LargeFilesTab,
    big_folders_tab: ui::big_folders_tab::BigFoldersTab,
    disk_usage_tab: ui::disk_usage_tab::DiskUsageTab,
    organize_tab: ui::organize_tab::OrganizeTab,
    sync_tab: ui::sync_tab::SyncTab,
    share_tab: ui::share_tab::ShareTab,
}

impl SuperCopierApp {
    fn new(ipc_rx: Receiver<ipc::IpcMessage>) -> Self {
        Self {
            tab: Tab::Copy,
            theme: egui::ThemePreference::Dark,
            ipc_rx,
            copy_tab: ui::copy_tab::CopyTab::default(),
            search_tab: ui::search_tab::SearchTab::default(),
            dup_tab: ui::dup_tab::DupTab::default(),
            large_files_tab: ui::large_files_tab::LargeFilesTab::default(),
            big_folders_tab: ui::big_folders_tab::BigFoldersTab::default(),
            disk_usage_tab: ui::disk_usage_tab::DiskUsageTab::default(),
            organize_tab: ui::organize_tab::OrganizeTab::default(),
            sync_tab: ui::sync_tab::SyncTab::default(),
            share_tab: ui::share_tab::ShareTab::default(),
        }
    }
}

impl eframe::App for SuperCopierApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        ctx.set_theme(self.theme);

        // Keep polling background job channels smoothly while any job runs.
        let any_running = self.copy_tab.is_running()
            || self.search_tab.is_running()
            || self.dup_tab.is_running()
            || self.large_files_tab.is_running()
            || self.big_folders_tab.is_running()
            || self.organize_tab.is_running()
            || self.sync_tab.is_running()
            || self.share_tab.is_running();
        if any_running {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }

        if dnd::hovering_files(&ctx) {
            dnd::paint_overlay(&ctx, "Drop files or folders to add them to this tab");
        }
        let mut dropped = dnd::take_dropped_paths(&ctx);

        // Paths (and "use Move mode") forwarded from another launch of
        // this app — e.g. Explorer's context menu, which invokes us once
        // per selected item.
        let mut ipc_arrived = false;
        for msg in self.ipc_rx.try_iter() {
            ipc_arrived = true;
            match msg {
                ipc::IpcMessage::Path(p) => dropped.push(p),
                ipc::IpcMessage::UseMoveMode => self.copy_tab.set_move_mode(),
            }
        }
        if ipc_arrived {
            self.tab = Tab::Copy;
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
        }
        // We only get here between frames via try_iter, so make sure we
        // keep checking even while otherwise idle.
        ctx.request_repaint_after(std::time::Duration::from_millis(200));

        egui::Panel::top("tabs").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Copy, "📁 Copy / Move / Rename");
                ui.selectable_value(&mut self.tab, Tab::Search, "🔎 Search");
                ui.selectable_value(&mut self.tab, Tab::Duplicates, "🔍 Duplicates");
                ui.selectable_value(&mut self.tab, Tab::BigFiles, "🐘 Big Files");
                ui.selectable_value(&mut self.tab, Tab::BigFolders, "🗄 Big Folders");
                ui.selectable_value(&mut self.tab, Tab::DiskUsage, "💽 Disk Usage");
                ui.selectable_value(&mut self.tab, Tab::Organize, "🗂 Organize");
                ui.selectable_value(&mut self.tab, Tab::Sync, "🔄 Sync");
                ui.selectable_value(&mut self.tab, Tab::Share, "📡 Share");

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    self.theme.radio_buttons(ui);
                });
            });
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show(ui, |ui| match self.tab {
            Tab::Copy => self.copy_tab.ui(ui, dropped),
            Tab::Search => {
                self.search_tab.add_dropped(dropped);
                self.search_tab.ui(ui);
            }
            Tab::Duplicates => {
                self.dup_tab.add_dropped(dropped);
                self.dup_tab.ui(ui);
            }
            Tab::BigFiles => {
                self.large_files_tab.add_dropped(dropped);
                self.large_files_tab.ui(ui);
            }
            Tab::BigFolders => {
                self.big_folders_tab.add_dropped(dropped);
                self.big_folders_tab.ui(ui);
            }
            Tab::DiskUsage => self.disk_usage_tab.ui(ui),
            Tab::Organize => {
                self.organize_tab.add_dropped(dropped);
                self.organize_tab.ui(ui);
            }
            Tab::Sync => self.sync_tab.ui(ui, dropped),
            Tab::Share => self.share_tab.ui(ui, dropped),
        });
    }
}
