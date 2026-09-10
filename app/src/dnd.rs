//! OS-level drag-and-drop helpers (dragging files/folders in from Finder,
//! Explorer, etc. — distinct from egui's own in-app widget dragging).

use std::path::{Path, PathBuf};

use eframe::egui;

/// True while the user is dragging OS files over the window (but hasn't
/// dropped them yet). Use this to show a "drop here" indicator.
pub fn hovering_files(ctx: &egui::Context) -> bool {
    ctx.input(|i| !i.raw.hovered_files.is_empty())
}

/// Takes (and clears) any files dropped this frame. Empty on every frame
/// except the one where a drop actually happens.
pub fn take_dropped_paths(ctx: &egui::Context) -> Vec<PathBuf> {
    ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect())
}

/// `path` itself if it's a directory, otherwise its parent directory.
/// Lets folder-oriented drop targets (Duplicates roots, Organize folder,
/// Sync source/destination) accept a dropped *file* by resolving to the
/// folder it lives in.
pub fn as_dir(path: &Path) -> Option<PathBuf> {
    if path.is_dir() {
        Some(path.to_path_buf())
    } else {
        path.parent().map(Path::to_path_buf)
    }
}

/// A clickable box that also acts as a drop target: shows `current` (or a
/// placeholder), highlights while files are being dragged over the window,
/// and returns a response whose `.rect` the caller can hit-test against the
/// drop position and whose `.clicked()` should open a folder picker.
pub fn drop_zone(ui: &mut egui::Ui, label: &str, current: &Option<PathBuf>, hovering_files: bool) -> egui::Response {
    let fill = if hovering_files {
        ui.visuals().selection.bg_fill.linear_multiply(0.3)
    } else {
        ui.visuals().faint_bg_color
    };
    let inner = egui::Frame::group(ui.style()).fill(fill).show(ui, |ui| {
        ui.set_min_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(label);
            ui.label(
                current
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "(none — click or drop a folder here)".to_string()),
            );
        });
    });
    ui.interact(inner.response.rect, ui.id().with(label), egui::Sense::click())
}

/// Paints a full-window translucent "drop to add" overlay. Call every frame
/// while [`hovering_files`] is true.
pub fn paint_overlay(ctx: &egui::Context, text: &str) {
    let rect = ctx.input(|i| i.viewport_rect());
    egui::Area::new(egui::Id::new("dnd_overlay"))
        .order(egui::Order::Foreground)
        .fixed_pos(rect.min)
        .interactable(false)
        .show(ctx, |ui| {
            let painter = ui.painter();
            painter.rect_filled(rect, 0.0, egui::Color32::from_black_alpha(160));
            painter.text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                text,
                egui::FontId::proportional(26.0),
                egui::Color32::WHITE,
            );
        });
}
