#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ui;
mod util;
mod worker;

use eframe::egui;

fn main() -> eframe::Result {
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([980.0, 680.0])
            .with_min_inner_size([680.0, 480.0]),
        ..Default::default()
    };

    eframe::run_native(
        "Super Copier",
        native_options,
        Box::new(|cc| {
            setup_style(&cc.egui_ctx);
            Ok(Box::new(SuperCopierApp::default()))
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
    Duplicates,
    Organize,
    Sync,
}

struct SuperCopierApp {
    tab: Tab,
    copy_tab: ui::copy_tab::CopyTab,
    dup_tab: ui::dup_tab::DupTab,
    organize_tab: ui::organize_tab::OrganizeTab,
    sync_tab: ui::sync_tab::SyncTab,
}

impl Default for SuperCopierApp {
    fn default() -> Self {
        Self {
            tab: Tab::Copy,
            copy_tab: ui::copy_tab::CopyTab::default(),
            dup_tab: ui::dup_tab::DupTab::default(),
            organize_tab: ui::organize_tab::OrganizeTab::default(),
            sync_tab: ui::sync_tab::SyncTab::default(),
        }
    }
}

impl eframe::App for SuperCopierApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // Keep polling background job channels smoothly while any job runs.
        let any_running = self.copy_tab.is_running()
            || self.dup_tab.is_running()
            || self.organize_tab.is_running()
            || self.sync_tab.is_running();
        if any_running {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
        }

        egui::Panel::top("tabs").show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.selectable_value(&mut self.tab, Tab::Copy, "📁 Copy / Move / Rename");
                ui.selectable_value(&mut self.tab, Tab::Duplicates, "🔍 Duplicates");
                ui.selectable_value(&mut self.tab, Tab::Organize, "🗂 Organize");
                ui.selectable_value(&mut self.tab, Tab::Sync, "🔄 Sync");
            });
            ui.add_space(4.0);
        });

        egui::CentralPanel::default().show(ui, |ui| match self.tab {
            Tab::Copy => self.copy_tab.ui(ui),
            Tab::Duplicates => self.dup_tab.ui(ui),
            Tab::Organize => self.organize_tab.ui(ui),
            Tab::Sync => self.sync_tab.ui(ui),
        });
    }
}
