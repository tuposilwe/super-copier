//! Defers opening native file/folder pickers to the frame *after* the
//! click that requested them.
//!
//! On macOS, `rfd`'s dialogs pump a nested run loop via `NSPanel
//! .runModal()`. Calling that synchronously from inside the same
//! winit-dispatched event that's handling the button click can trip
//! winit's re-entrancy guard ("tried to handle event while another event
//! is currently being handled"), which panics — and since release builds
//! use `panic = "abort"`, that takes the whole app down instead of just
//! failing the click. Waiting until a *later* frame's `poll()` call (a
//! separate, already-returned event dispatch) avoids the nesting.

use std::path::PathBuf;

#[derive(Clone, PartialEq)]
enum Kind {
    Folder,
    Files,
    /// Choose where to save a new file, suggesting this name.
    Save(String),
}

#[derive(Default)]
pub struct DeferredPicker {
    requested: Option<Kind>,
}

impl DeferredPicker {
    pub fn request_folder(&mut self) {
        self.requested = Some(Kind::Folder);
    }

    pub fn request_files(&mut self) {
        self.requested = Some(Kind::Files);
    }

    /// Asks for a location to save a new `.zip`, suggesting `file_name`.
    pub fn request_save_zip(&mut self, file_name: impl Into<String>) {
        self.requested = Some(Kind::Save(file_name.into()));
    }

    /// Call once at the very top of `ui()`, before any widgets are drawn.
    /// Returns the picked paths once a dialog requested on a *previous*
    /// frame has actually been shown and closed (empty if the user
    /// cancelled).
    pub fn poll(&mut self) -> Option<Vec<PathBuf>> {
        let kind = self.requested.take()?;
        let picked = match kind {
            Kind::Folder => rfd::FileDialog::new().pick_folder().map(|p| vec![p]),
            Kind::Files => rfd::FileDialog::new().pick_files(),
            Kind::Save(name) => rfd::FileDialog::new()
                .set_file_name(name)
                .add_filter("Zip archive", &["zip"])
                .save_file()
                .map(|p| vec![p]),
        };
        Some(picked.unwrap_or_default())
    }
}
