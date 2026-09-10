//! Desktop notifications for background jobs that finish while the app
//! isn't the focused window — copies, scans, syncs, etc. can all take a
//! while, so this is how the user finds out without staring at the tab.

pub fn notify(summary: &str, body: &str) {
    let result = notify_rust::Notification::new()
        .summary(summary)
        .body(body)
        .appname("Super Copier")
        .show();
    if let Err(e) = result {
        eprintln!("desktop notification failed: {e}");
    }
}
