//! Single-instance coordination.
//!
//! Explorer's "Copy with Super Copier" / "Move with Super Copier" context
//! menu entries invoke the exe once **per selected item** when multiple
//! files are selected (that's how classic registry shell verbs work —
//! there's no single invocation carrying the whole selection). Without
//! this module, selecting five files and right-clicking would pop open
//! five separate windows, one file in each.
//!
//! Instead, the first invocation becomes the "primary" instance (it wins
//! a bind on a local TCP port — used purely as loopback IPC, nothing is
//! reachable from the network) and keeps running normally. Every
//! following invocation notices the port is taken, ships its paths (and
//! whether it was launched as "move") to the primary over that
//! connection, and exits immediately without opening a window. The
//! primary polls for these messages once per frame, the same way it
//! polls background job channels.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;

use crossbeam_channel::{Receiver, Sender};

/// Arbitrary fixed loopback port, chosen to be unlikely to collide with
/// anything else running locally.
const PORT: u16 = 47823;

pub enum IpcMessage {
    Path(PathBuf),
    UseMoveMode,
}

/// Tries to become the primary instance.
///
/// On success, returns a receiver that yields messages forwarded from
/// later invocations (already seeded with `initial_paths`/`initial_move`
/// from *this* invocation's own command line). On failure — another
/// instance is already primary — forwards this invocation's arguments to
/// it and returns `None`; the caller should exit without opening a
/// window.
pub fn acquire_primary(initial_paths: &[PathBuf], initial_move: bool) -> Option<Receiver<IpcMessage>> {
    match TcpListener::bind(("127.0.0.1", PORT)) {
        Ok(listener) => {
            let (tx, rx) = crossbeam_channel::unbounded();
            if initial_move {
                let _ = tx.send(IpcMessage::UseMoveMode);
            }
            for p in initial_paths {
                let _ = tx.send(IpcMessage::Path(p.clone()));
            }
            std::thread::spawn(move || listen(listener, tx));
            Some(rx)
        }
        Err(_) => {
            forward(initial_paths, initial_move);
            None
        }
    }
}

fn listen(listener: TcpListener, tx: Sender<IpcMessage>) {
    for stream in listener.incoming().flatten() {
        let reader = BufReader::new(stream);
        for line in reader.lines().map_while(Result::ok) {
            if line == "--move" {
                let _ = tx.send(IpcMessage::UseMoveMode);
            } else if !line.is_empty() {
                let _ = tx.send(IpcMessage::Path(PathBuf::from(line)));
            }
        }
    }
}

fn forward(paths: &[PathBuf], move_mode: bool) {
    if let Ok(mut stream) = TcpStream::connect(("127.0.0.1", PORT)) {
        if move_mode {
            let _ = writeln!(stream, "--move");
        }
        for p in paths {
            let _ = writeln!(stream, "{}", p.display());
        }
        let _ = stream.flush();
    }
}
