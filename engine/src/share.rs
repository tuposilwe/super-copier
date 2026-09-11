//! LAN peer discovery and direct file transfer between two machines both
//! running Super Copier — no cloud/relay/account involved. Discovery is a
//! UDP broadcast, the transfer itself a plain TCP connection. Both are
//! scoped to the local network: nothing here is meant to be reachable from
//! the internet.
//!
//! Wire format (all multi-byte integers big-endian; all strings
//! length-prefixed UTF-8: a u32 byte length followed by that many bytes):
//!
//! Discovery (one UDP packet per announcement):
//! `b"SCDISC1"` ++ `session_id` (16 bytes) ++ `name` (string) ++ `tcp_port` (u16)
//!
//! Transfer (TCP):
//! - Sender -> Receiver: `b"SCXF1"` ++ `sender_name` (string) ++ `file_count` (u32),
//!   then for each file: `relative_path` (string) ++ `size` (u64)
//! - Receiver -> Sender: one byte, `1` = accept, `0` = reject
//! - If accepted, Sender -> Receiver: every file's raw bytes, back to back,
//!   in manifest order (sizes are already known from the manifest, so no
//!   further framing is needed)

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;

use crate::{io_err, CancelToken, EngineError, EngineResult};

pub const DISCOVERY_PORT: u16 = 47824;
pub const TRANSFER_PORT: u16 = 47825;
const DISCOVERY_MAGIC: &[u8] = b"SCDISC1";
const TRANSFER_MAGIC: &[u8] = b"SCXF1";
const PEER_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_STRING_LEN: usize = 10_000;

fn net_err(e: std::io::Error) -> EngineError {
    EngineError::Other(e.to_string())
}

fn write_string(buf: &mut Vec<u8>, s: &str) {
    let bytes = s.as_bytes();
    buf.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    buf.extend_from_slice(bytes);
}

fn read_string(stream: &mut impl Read) -> std::io::Result<String> {
    let mut len_buf = [0u8; 4];
    stream.read_exact(&mut len_buf)?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > MAX_STRING_LEN {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "string too long"));
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf)?;
    String::from_utf8(buf).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Drops `..`/`.`/empty components and (on Windows) anything containing a
/// drive-letter colon, so a received path can never escape the destination
/// folder no matter what the sender claims it is.
fn sanitize_relative_path(raw: &str) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for part in raw.split(['/', '\\']) {
        if part.is_empty() || part == ".." || part == "." {
            continue;
        }
        #[cfg(windows)]
        if part.contains(':') {
            return None;
        }
        out.push(part);
    }
    if out.as_os_str().is_empty() {
        None
    } else {
        Some(out)
    }
}

// ---------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PeerInfo {
    pub name: String,
    /// Address to connect to for a transfer (peer's IP + its TRANSFER_PORT).
    pub addr: SocketAddr,
}

#[derive(Debug, Clone)]
pub enum DiscoveryEvent {
    PeerFound(PeerInfo),
    PeerLost(SocketAddr),
}

/// A fresh random ID to identify this app run in discovery announcements,
/// so a running instance can recognize (and ignore) its own broadcasts.
pub fn random_session_id() -> [u8; 16] {
    *uuid::Uuid::new_v4().as_bytes()
}

/// Binds the discovery UDP port with `SO_REUSEADDR`/`SO_REUSEPORT` set, so
/// more than one process on the same machine can listen for the same
/// broadcast traffic. `std::net::UdpSocket` doesn't expose these options
/// before binding, hence going through `socket2` and converting at the end.
/// This matters for two real cases, not just convenience: a stale/zombie
/// process still holding the port shouldn't be able to permanently block a
/// fresh instance from discovering peers, and — the only way this is
/// actually verified, short of two physical machines — it's what lets a
/// test simulate two peers on one machine at all.
fn bind_discovery_socket() -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Socket, Type};
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, None)?;
    socket.set_reuse_address(true)?;
    #[cfg(unix)]
    socket.set_reuse_port(true)?;
    socket.set_broadcast(true)?;
    let addr: SocketAddr = ([0, 0, 0, 0], DISCOVERY_PORT).into();
    socket.bind(&addr.into())?;
    Ok(socket.into())
}

/// Broadcasts our presence on the LAN every couple of seconds and listens
/// for other instances doing the same, until `cancel` is set. `session_id`
/// should be a random value generated once per app run, so we can
/// recognize (and ignore) our own announcements.
pub fn run_discovery(name: String, session_id: [u8; 16], cancel: CancelToken, tx: Sender<DiscoveryEvent>) {
    let Ok(socket) = bind_discovery_socket() else {
        return; // couldn't bind at all — nothing more we can do
    };
    let _ = socket.set_read_timeout(Some(Duration::from_millis(500)));

    let mut announce = Vec::new();
    announce.extend_from_slice(DISCOVERY_MAGIC);
    announce.extend_from_slice(&session_id);
    write_string(&mut announce, &name);
    announce.extend_from_slice(&TRANSFER_PORT.to_be_bytes());

    let mut last_announce = Instant::now() - Duration::from_secs(10);
    let mut last_seen: HashMap<SocketAddr, Instant> = HashMap::new();
    let mut buf = [0u8; 1024];

    while !cancel.is_cancelled() {
        if last_announce.elapsed() >= Duration::from_secs(2) {
            let _ = socket.send_to(&announce, ("255.255.255.255", DISCOVERY_PORT));
            last_announce = Instant::now();
        }

        if let Ok((n, src)) = socket.recv_from(&mut buf) {
            if let Some(peer) = parse_announcement(&buf[..n], &session_id, src) {
                let is_new = !last_seen.contains_key(&peer.addr);
                last_seen.insert(peer.addr, Instant::now());
                if is_new {
                    let _ = tx.send(DiscoveryEvent::PeerFound(peer));
                }
            }
        }

        let now = Instant::now();
        let stale: Vec<SocketAddr> =
            last_seen.iter().filter(|(_, seen)| now.duration_since(**seen) > PEER_TIMEOUT).map(|(addr, _)| *addr).collect();
        for addr in stale {
            last_seen.remove(&addr);
            let _ = tx.send(DiscoveryEvent::PeerLost(addr));
        }
    }
}

fn parse_announcement(data: &[u8], our_session_id: &[u8; 16], src: SocketAddr) -> Option<PeerInfo> {
    if data.len() < DISCOVERY_MAGIC.len() + 16 || &data[..DISCOVERY_MAGIC.len()] != DISCOVERY_MAGIC {
        return None;
    }
    let rest = &data[DISCOVERY_MAGIC.len()..];
    let (session_id, rest) = rest.split_at(16);
    if session_id == our_session_id {
        return None; // that's us
    }
    if rest.len() < 4 {
        return None;
    }
    let name_len = u32::from_be_bytes(rest[..4].try_into().ok()?) as usize;
    let rest = &rest[4..];
    if rest.len() < name_len + 2 || name_len > MAX_STRING_LEN {
        return None;
    }
    let name = String::from_utf8(rest[..name_len].to_vec()).ok()?;
    let port = u16::from_be_bytes(rest[name_len..name_len + 2].try_into().ok()?);
    Some(PeerInfo { name, addr: SocketAddr::new(src.ip(), port) })
}

// ---------------------------------------------------------------------
// Sending
// ---------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct FileToSend {
    pub abs_path: PathBuf,
    pub rel_path: String,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub enum SendEvent {
    Connecting,
    WaitingForAccept,
    Rejected,
    Progress { file: String, bytes_done: u64, file_size: u64, files_done: usize, total_files: usize },
    Finished { files_sent: usize, bytes_sent: u64 },
}

pub fn send_files(
    peer_addr: SocketAddr,
    sender_name: &str,
    files: Vec<FileToSend>,
    cancel: CancelToken,
    tx: Sender<SendEvent>,
) -> EngineResult<()> {
    let _ = tx.send(SendEvent::Connecting);
    let mut stream = TcpStream::connect(peer_addr).map_err(net_err)?;
    let _ = stream.set_nodelay(true);

    let mut manifest = Vec::new();
    manifest.extend_from_slice(TRANSFER_MAGIC);
    write_string(&mut manifest, sender_name);
    manifest.extend_from_slice(&(files.len() as u32).to_be_bytes());
    for f in &files {
        write_string(&mut manifest, &f.rel_path);
        manifest.extend_from_slice(&f.size.to_be_bytes());
    }
    stream.write_all(&manifest).map_err(net_err)?;

    let _ = tx.send(SendEvent::WaitingForAccept);
    let mut resp = [0u8; 1];
    stream.read_exact(&mut resp).map_err(net_err)?;
    if resp[0] != 1 {
        let _ = tx.send(SendEvent::Rejected);
        return Ok(());
    }

    let total_files = files.len();
    let mut bytes_sent_total = 0u64;
    let mut buf = vec![0u8; 1024 * 1024];
    for (i, f) in files.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        let mut file = std::fs::File::open(&f.abs_path).map_err(|e| io_err(&f.abs_path, e))?;
        let mut sent = 0u64;
        loop {
            if cancel.is_cancelled() {
                return Err(EngineError::Cancelled);
            }
            let n = file.read(&mut buf).map_err(|e| io_err(&f.abs_path, e))?;
            if n == 0 {
                break;
            }
            stream.write_all(&buf[..n]).map_err(net_err)?;
            sent += n as u64;
            bytes_sent_total += n as u64;
            let _ = tx.send(SendEvent::Progress {
                file: f.rel_path.clone(),
                bytes_done: sent,
                file_size: f.size,
                files_done: i,
                total_files,
            });
        }
    }

    let _ = tx.send(SendEvent::Finished { files_sent: total_files, bytes_sent: bytes_sent_total });
    Ok(())
}

// ---------------------------------------------------------------------
// Receiving
// ---------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct IncomingManifest {
    pub sender_name: String,
    pub files: Vec<(String, u64)>,
    pub total_bytes: u64,
}

#[derive(Debug, Clone)]
pub enum ReceiveEvent {
    /// The UI must respond on `respond` (true = accept) within the
    /// timeout, or the transfer is treated as rejected.
    IncomingRequest { manifest: IncomingManifest, respond: Sender<bool> },
    Progress { file: String, bytes_done: u64, file_size: u64, files_done: usize, total_files: usize },
    Finished { files_received: usize, bytes_received: u64, dest: PathBuf },
    Failed(String),
}

/// Listens for incoming transfers until `cancel` is set. Handles one
/// connection at a time — a second sender arriving while a request is
/// awaiting the user's accept/reject just waits at the TCP level.
pub fn run_receiver(dest_dir: PathBuf, cancel: CancelToken, tx: Sender<ReceiveEvent>) {
    let Ok(listener) = TcpListener::bind(("0.0.0.0", TRANSFER_PORT)) else {
        return; // port already taken locally (e.g. a second instance) — just skip
    };
    let _ = listener.set_nonblocking(true);

    while !cancel.is_cancelled() {
        match listener.accept() {
            Ok((stream, _addr)) => {
                if let Err(e) = handle_incoming(stream, &dest_dir, &cancel, &tx) {
                    if !matches!(e, EngineError::Cancelled) {
                        let _ = tx.send(ReceiveEvent::Failed(e.to_string()));
                    }
                }
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(200)),
        }
    }
}

fn handle_incoming(mut stream: TcpStream, dest_dir: &Path, cancel: &CancelToken, tx: &Sender<ReceiveEvent>) -> EngineResult<()> {
    let _ = stream.set_nonblocking(false);

    let mut magic = [0u8; TRANSFER_MAGIC.len()];
    stream.read_exact(&mut magic).map_err(net_err)?;
    if magic != *TRANSFER_MAGIC {
        return Err(EngineError::Other("bad transfer handshake".into()));
    }
    let sender_name = read_string(&mut stream).map_err(net_err)?;
    let mut count_buf = [0u8; 4];
    stream.read_exact(&mut count_buf).map_err(net_err)?;
    let count = u32::from_be_bytes(count_buf) as usize;

    let mut files = Vec::with_capacity(count);
    let mut total_bytes = 0u64;
    for _ in 0..count {
        let rel = read_string(&mut stream).map_err(net_err)?;
        let mut size_buf = [0u8; 8];
        stream.read_exact(&mut size_buf).map_err(net_err)?;
        let size = u64::from_be_bytes(size_buf);
        total_bytes += size;
        files.push((rel, size));
    }

    let manifest = IncomingManifest { sender_name, files: files.clone(), total_bytes };
    let (resp_tx, resp_rx) = crossbeam_channel::bounded(1);
    let _ = tx.send(ReceiveEvent::IncomingRequest { manifest, respond: resp_tx });

    let accepted = resp_rx.recv_timeout(Duration::from_secs(120)).unwrap_or(false);
    stream.write_all(&[u8::from(accepted)]).map_err(net_err)?;
    if !accepted {
        return Ok(());
    }

    std::fs::create_dir_all(dest_dir).map_err(|e| io_err(dest_dir, e))?;

    let total_files = files.len();
    let mut bytes_received_total = 0u64;
    let mut buf = vec![0u8; 1024 * 1024];
    for (i, (rel, size)) in files.iter().enumerate() {
        if cancel.is_cancelled() {
            return Err(EngineError::Cancelled);
        }
        let Some(safe_rel) = sanitize_relative_path(rel) else { continue };
        let dest_path = dest_dir.join(&safe_rel);
        if let Some(parent) = dest_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| io_err(parent, e))?;
        }
        let mut out = std::fs::File::create(&dest_path).map_err(|e| io_err(&dest_path, e))?;
        let mut remaining = *size;
        let mut done = 0u64;
        while remaining > 0 {
            if cancel.is_cancelled() {
                return Err(EngineError::Cancelled);
            }
            let want = remaining.min(buf.len() as u64) as usize;
            stream.read_exact(&mut buf[..want]).map_err(net_err)?;
            out.write_all(&buf[..want]).map_err(|e| io_err(&dest_path, e))?;
            remaining -= want as u64;
            done += want as u64;
            bytes_received_total += want as u64;
            let _ = tx.send(ReceiveEvent::Progress {
                file: rel.clone(),
                bytes_done: done,
                file_size: *size,
                files_done: i,
                total_files,
            });
        }
    }

    let _ = tx.send(ReceiveEvent::Finished {
        files_received: total_files,
        bytes_received: bytes_received_total,
        dest: dest_dir.to_path_buf(),
    });
    Ok(())
}
