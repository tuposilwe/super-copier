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
    // The bootable-USB feature re-launches this binary with administrator
    // rights to do the actual disk writes (see engine::bootable::helper);
    // that copy must never open a window or join the single-instance IPC.
    let mut args = std::env::args_os().skip(1);
    if args.next().is_some_and(|a| a == engine::bootable::helper::HELPER_FLAG) {
        let Some(job) = args.next() else { std::process::exit(3) };
        std::process::exit(engine::bootable::helper::run_helper(std::path::Path::new(&job)));
    }

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
    Big,
    DiskUsage,
    Boot,
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
    big_tab: ui::big_tab::BigTab,
    disk_usage_tab: ui::disk_usage_tab::DiskUsageTab,
    boot_tab: ui::boot_tab::BootTab,
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
            big_tab: ui::big_tab::BigTab::default(),
            disk_usage_tab: ui::disk_usage_tab::DiskUsageTab::default(),
            boot_tab: ui::boot_tab::BootTab::default(),
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
            || self.big_tab.is_running()
            || self.boot_tab.is_running()
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
            tab_bar(ui, &mut self.tab, &mut self.theme);
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
            Tab::Big => {
                self.big_tab.add_dropped(dropped);
                self.big_tab.ui(ui);
            }
            Tab::DiskUsage => self.disk_usage_tab.ui(ui),
            Tab::Boot => self.boot_tab.ui(ui, dropped),
            Tab::Organize => {
                self.organize_tab.add_dropped(dropped);
                self.organize_tab.ui(ui);
            }
            Tab::Sync => self.sync_tab.ui(ui, dropped),
            Tab::Share => self.share_tab.ui(ui, dropped),
        });
    }
}

/// The tab strip plus the light/dark toggle. It wraps onto extra lines when
/// the window is narrow — a single non-wrapping row pushed the theme control
/// off the edge of the default-sized window once there were enough tabs.
/// Returns the toggle button's rectangle (used by tests).
fn tab_bar(ui: &mut egui::Ui, tab: &mut Tab, theme: &mut egui::ThemePreference) -> egui::Rect {
    let mut toggle_rect = egui::Rect::NOTHING;
    ui.horizontal_wrapped(|ui| {
        for (t, label) in [
            (Tab::Copy, "📁 Copy / Move / Rename"),
            (Tab::Search, "🔎 Search"),
            (Tab::Duplicates, "🔍 Duplicates"),
            (Tab::Big, "🐘 Big Files & Folders"),
            (Tab::DiskUsage, "💽 Disk Usage"),
            (Tab::Boot, "💿 Bootable USB"),
            (Tab::Organize, "🗂 Organize"),
            (Tab::Sync, "🔄 Sync"),
            (Tab::Share, "📡 Share"),
        ] {
            ui.selectable_value(tab, t, label);
        }
        ui.separator();
        // Shows the mode you'd switch *to*, so it reads as an action.
        let dark = ui.visuals().dark_mode;
        let label = if dark { "☀ Light mode" } else { "🌙 Dark mode" };
        let response = ui.button(label).on_hover_text("Switch between light and dark mode");
        toggle_rect = response.rect;
        if response.clicked() {
            *theme = toggled_theme(dark);
        }
    });
    toggle_rect
}

/// The theme a click on the toggle should switch to, given whether the UI is
/// currently dark. Always an explicit Light/Dark (never System), so one click
/// always visibly changes something.
fn toggled_theme(currently_dark: bool) -> egui::ThemePreference {
    if currently_dark {
        egui::ThemePreference::Light
    } else {
        egui::ThemePreference::Dark
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(ctx: &egui::Context, width: f32, events: Vec<egui::Event>, tab: &mut Tab, theme: &mut egui::ThemePreference) -> egui::Rect {
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 700.0))),
            events,
            ..Default::default()
        };
        let mut rect = egui::Rect::NOTHING;
        let mut out = ctx.run_ui(raw, |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                rect = tab_bar(ui, tab, theme);
            });
        });
        out.textures_delta.clear();
        rect
    }

    #[test]
    fn the_toggle_stays_on_screen_at_every_window_width() {
        for width in [680.0, 800.0, 980.0, 1200.0, 1600.0] {
            let ctx = egui::Context::default();
            let (mut tab, mut theme) = (Tab::Copy, egui::ThemePreference::Dark);
            frame(&ctx, width, vec![], &mut tab, &mut theme);
            let rect = frame(&ctx, width, vec![], &mut tab, &mut theme);
            assert!(rect.is_positive(), "toggle wasn't laid out at width {width}");
            assert!(
                rect.left() >= 0.0 && rect.right() <= width,
                "at width {width} the toggle spans x={:.0}..{:.0}, off the window",
                rect.left(),
                rect.right()
            );
        }
    }

    #[test]
    fn clicking_the_toggle_flips_between_light_and_dark() {
        assert_eq!(toggled_theme(true), egui::ThemePreference::Light);
        assert_eq!(toggled_theme(false), egui::ThemePreference::Dark);

        let ctx = egui::Context::default();
        let (mut tab, mut theme) = (Tab::Copy, egui::ThemePreference::Dark);
        frame(&ctx, 1200.0, vec![], &mut tab, &mut theme);
        let rect = frame(&ctx, 1200.0, vec![], &mut tab, &mut theme);
        let click_at = rect.center();
        let button = |pressed| egui::Event::PointerButton {
            pos: click_at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        };
        frame(&ctx, 1200.0, vec![egui::Event::PointerMoved(click_at)], &mut tab, &mut theme);
        frame(&ctx, 1200.0, vec![button(true)], &mut tab, &mut theme);
        frame(&ctx, 1200.0, vec![button(false)], &mut tab, &mut theme);
        assert_eq!(theme, egui::ThemePreference::Light, "a click while dark should switch to light");
    }
}
