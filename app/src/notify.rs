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
//! On macOS this must run off the main thread, and it uses the modern
//! `UNUserNotificationCenter` API (`preview-macos-un` feature) rather than
//! notify-rust's default `NSUserNotificationCenter` path. Two separate
//! problems, found by actually checking why "scan finished" notifications
//! never appeared:
//!
//! 1. The legacy `NSUserNotificationCenter` bridge (`mac-notification-sys`)
//!    posts, then even for a "fire-and-forget" notification blocks up to 2s
//!    waiting for an XPC delivery confirmation — and on the main thread it
//!    does that by nesting `[[NSRunLoop currentRunLoop] runUntilDate:]`
//!    inside whatever call stack invoked it. Since notify() runs during a
//!    tab's poll() from inside winit's own main-thread event dispatch, that
//!    nested run loop could trip winit's re-entrancy guard and panic,
//!    aborting the whole process under `panic = "abort"` — confirmed
//!    against four real crash reports, each showing `usernoted.client`
//!    (Notification Center's XPC connection) activating milliseconds
//!    before the abort.
//! 2. Separately — and this is why notifications silently never appeared
//!    even after that fix — the legacy API's own bundle-identity handling
//!    is broken for this app. Without an explicit `set_application()` call,
//!    notify-rust tries to resolve `"use_default"` as if it were an actual
//!    installed app's *name* (via AppleScript), which of course finds
//!    nothing, and falls back to literally impersonating
//!    `com.apple.Finder`'s bundle identifier. Checked
//!    `~/Library/Preferences/com.apple.ncprefs.plist` directly: Super
//!    Copier had never once been registered there, even after "successful"
//!    (no error returned) notification calls — and on this macOS version,
//!    even explicitly calling `set_application` with Super Copier's *own*
//!    real, installed bundle id only worked for a single call before the
//!    OS stopped even attempting the XPC connection on a second attempt.
//!    The legacy API looks to be effectively non-functional for
//!    third-party apps here, not just misconfigured.
//!
//! `UNUserNotificationCenter` is the actually-supported replacement: it
//! requires a real bundle identifier (which we have, from the .app bundle)
//! and an explicit one-time permission grant via a genuine system dialog,
//! rather than a bundle-identity spoofing trick. `request_auth_blocking`
//! is safe to call every time — after the first grant/denial it returns
//! immediately without re-prompting — so no separate "have we asked
//! before" state is needed here.
//!
//! One more wrinkle, found after that fix still produced no visible
//! notification in real use despite System Settings showing everything
//! correctly enabled: `Notification::show()`'s blocking send
//! (`mac_usernotifications::send_blocking`) requires the *main thread's*
//! run loop to be idle (`CFRunLoop::main().is_waiting()`) at the exact
//! moment it's called — it returns `Err(MainThreadNotRunning)` otherwise,
//! silently on a background thread (only an `eprintln!`, invisible in a
//! normal GUI launch). Every real caller here fires notify() the instant a
//! job finishes, which is also the instant `main.rs` stops requesting
//! continuous 50ms repaints for that job — i.e. right when the run loop is
//! *least* likely to have settled into "waiting" yet. Reproduced directly:
//! simulating 60 frames of continuous repaint (an "active scan") and
//! firing notify() on the exact frame it stops reliably hit this same
//! `Mainthread not running` error. It's transient — the run loop settles
//! within well under a second — so retrying the send a few times with a
//! short delay, rather than giving up on the first attempt, is the fix.

pub fn notify(summary: &str, body: &str) {
    let summary = summary.to_string();
    let body = body.to_string();
    std::thread::spawn(move || {
        #[cfg(target_os = "macos")]
        {
            match notify_rust::request_auth_blocking() {
                Ok(true) => {}
                Ok(false) => {
                    eprintln!("desktop notification skipped: not authorized (see System Settings > Notifications > Super Copier)");
                    return;
                }
                Err(e) => {
                    eprintln!("desktop notification permission request failed: {e}");
                    return;
                }
            }
        }

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

        const MAX_ATTEMPTS: u32 = 8;
        for attempt in 1..=MAX_ATTEMPTS {
            match notification.show() {
                Ok(_) => return,
                Err(e) if attempt < MAX_ATTEMPTS => {
                    eprintln!("desktop notification attempt {attempt} failed, retrying: {e}");
                    std::thread::sleep(std::time::Duration::from_millis(150));
                }
                Err(e) => eprintln!("desktop notification failed after {MAX_ATTEMPTS} attempts: {e}"),
            }
        }
    });
}
