//! Desktop notifications for background jobs that finish while the app
//! isn't the focused window — copies, scans, syncs, etc. can all take a
//! while, so this is how the user finds out without staring at the tab.

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
