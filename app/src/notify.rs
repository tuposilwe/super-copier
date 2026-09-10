//! Desktop notifications for background jobs that finish while the app
//! isn't the focused window — copies, scans, syncs, etc. can all take a
//! while, so this is how the user finds out without staring at the tab.

/// Our own AppUserModelID. Windows toast notifications are attributed to
/// whatever AUMID they're posted under; without one, notify-rust's Windows
/// backend falls back to `Toast::POWERSHELL_APP_ID` — which is exactly why
/// notifications were showing up as "Windows PowerShell" instead of "Super
/// Copier". [`register_app_id`] (called once at startup) and this constant
/// (passed to every notification below) fix that together — see
/// `windows::register_app_id` for why both are needed.
#[allow(dead_code, reason = "only referenced from #[cfg(windows)] blocks")]
const APP_USER_MODEL_ID: &str = "dev.tuposilwe.supercopier";

pub fn notify(summary: &str, body: &str) {
    let mut notification = notify_rust::Notification::new();
    notification.summary(summary).body(body).appname("Super Copier");

    // notify-rust passes this straight through rather than mapping a
    // shared "default" concept per platform, so the right value differs:
    // macOS wants an actual system sound file name (in /System/Library/Sounds),
    // while Windows' toast backend recognizes the literal name "Default".
    #[cfg(target_os = "macos")]
    notification.sound_name("Ping");
    #[cfg(windows)]
    {
        notification.sound_name("Default");
        notification.app_id(APP_USER_MODEL_ID);
    }

    if let Err(e) = notification.show() {
        eprintln!("desktop notification failed: {e}");
    }
}

#[cfg(windows)]
pub mod windows {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;

    /// Declares this process's AppUserModelID to Windows. Must be called
    /// once, early in `main`, before any window is created.
    ///
    /// This alone gets toast notifications out from under PowerShell's
    /// identity (they'll show generic branding instead). Getting our own
    /// name/icon on them too needs a further step this app doesn't do yet:
    /// a Start Menu shortcut whose `System.AppUserModel.ID` property matches
    /// this same string — Windows uses that pairing to resolve the display
    /// name and icon for an unpackaged app's notifications.
    pub fn register_app_id() {
        let wide: Vec<u16> = std::ffi::OsStr::new(super::APP_USER_MODEL_ID)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            let _ = SetCurrentProcessExplicitAppUserModelID(wide.as_ptr());
        }
    }
}
