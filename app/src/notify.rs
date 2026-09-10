//! Desktop notifications for background jobs that finish while the app
//! isn't the focused window — copies, scans, syncs, etc. can all take a
//! while, so this is how the user finds out without staring at the tab.
//!
//! On Windows, notify-rust posts under `Toast::POWERSHELL_APP_ID` unless
//! given our own AppUserModelID — which is why notifications show up
//! attributed to "Windows PowerShell" rather than "Super Copier". A
//! previous attempt to fix that (setting our own AUMID via
//! `SetCurrentProcessExplicitAppUserModelID` + `Notification::app_id`)
//! made notifications stop appearing *at all*: Windows only accepts toasts
//! under a custom AUMID if that AUMID is also registered — normally via a
//! Start Menu shortcut whose `System.AppUserModel.ID` property matches it
//! exactly (this is documented as a hard requirement for unpackaged Win32
//! apps, not an optional nicety). Implementing that shortcut registration
//! needs COM scripting in the NSIS installer, which can't be verified
//! without a real Windows machine to test on — and getting it wrong is
//! exactly what broke notifications last time. So for now this
//! deliberately borrows PowerShell's identity again: a visible
//! notification with the wrong name beats a correctly-named one that
//! never appears.

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
    notification.sound_name("Default");

    if let Err(e) = notification.show() {
        eprintln!("desktop notification failed: {e}");
    }
}
