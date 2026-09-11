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
//!
//! On macOS this must run off the main thread. `mac-notification-sys`'s
//! native bridge (`objc/notify.m`) posts via `NSUserNotificationCenter`
//! and then, even for a "fire-and-forget" notification, blocks up to 2s
//! waiting for an XPC delivery confirmation — and on the main thread it
//! does that by nesting `[[NSRunLoop currentRunLoop] runUntilDate:]`
//! inside whatever call stack invoked it. If that stack already
//! originated from winit's own event dispatch (which it does here: jobs
//! finish and call this during the UI's poll(), itself invoked from
//! within winit's main-thread event loop), the nested run loop can trip
//! winit's re-entrancy guard and panic — and with `panic = "abort"` in
//! release builds, that aborts the whole process. This is the same class
//! of bug as the rfd dialog reentrancy (see `dialog.rs`), just triggered
//! by every finished job instead of by opening a picker. Off the main
//! thread, the native bridge takes a plain Condvar-wait path instead of
//! the nested run loop, so spawning a thread sidesteps it entirely — the
//! same fix already used for trash deletion in `worker.rs`.

pub fn notify(summary: &str, body: &str) {
    let summary = summary.to_string();
    let body = body.to_string();
    std::thread::spawn(move || {
        let mut notification = notify_rust::Notification::new();
        notification.summary(&summary).body(&body).appname("Super Copier");

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
    });
}
