use std::path::PathBuf;

use crossbeam_channel::Sender;
use eframe::egui;
use engine::share::{self, DiscoveryEvent, FileToSend, IncomingManifest, PeerInfo, ReceiveEvent, SendEvent};

use crate::dnd;
use crate::util::{human_bytes, Log};
use crate::worker::{self, Job};

pub struct ShareTab {
    device_name: String,
    discoverable: bool,
    session_id: [u8; 16],

    discovery: Option<Job<DiscoveryEvent>>,
    peers: Vec<PeerInfo>,

    receiver: Option<Job<ReceiveEvent>>,
    dest_dir: PathBuf,
    pending_request: Option<(IncomingManifest, Sender<bool>)>,
    receive_progress: Option<Progress>,

    sources: Vec<PathBuf>,
    send_job: Option<Job<SendEvent>>,
    send_target: Option<PeerInfo>,
    send_progress: Option<Progress>,

    log: Log,
}

struct Progress {
    file: String,
    bytes_done: u64,
    file_size: u64,
    files_done: usize,
    total_files: usize,
}

impl Default for ShareTab {
    fn default() -> Self {
        let device_name = gethostname::gethostname().to_string_lossy().into_owned();
        let dest_dir = dirs_downloads().unwrap_or_else(std::env::temp_dir).join("SuperCopierReceived");
        Self {
            device_name,
            discoverable: false,
            session_id: share::random_session_id(),
            discovery: None,
            peers: Vec::new(),
            receiver: None,
            dest_dir,
            pending_request: None,
            receive_progress: None,
            sources: Vec::new(),
            send_job: None,
            send_target: None,
            send_progress: None,
            log: Log::default(),
        }
    }
}

/// Best-effort `~/Downloads`, falling back to the temp dir if it can't be
/// determined — good enough for a sensible default the user can change.
fn dirs_downloads() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join("Downloads"))
}

impl ShareTab {
    pub fn is_running(&self) -> bool {
        self.discoverable || self.send_job.is_some()
    }

    fn set_discoverable(&mut self, on: bool) {
        if on == self.discoverable {
            return;
        }
        self.discoverable = on;
        if on {
            self.discovery = Some(worker::spawn_discovery(self.device_name.clone(), self.session_id));
            self.receiver = Some(worker::spawn_share_receiver(self.dest_dir.clone()));
            self.peers.clear();
            self.log.push("Now discoverable on the local network.".to_string());
        } else {
            if let Some(job) = self.discovery.take() {
                job.cancel.cancel();
            }
            if let Some(job) = self.receiver.take() {
                job.cancel.cancel();
            }
            self.peers.clear();
            self.log.push("No longer discoverable.".to_string());
        }
    }

    fn poll(&mut self) {
        if let Some(job) = &self.discovery {
            for event in job.rx.try_iter() {
                match event {
                    DiscoveryEvent::PeerFound(p) => {
                        if !self.peers.iter().any(|existing| existing.addr == p.addr) {
                            self.peers.push(p);
                        }
                    }
                    DiscoveryEvent::PeerLost(addr) => self.peers.retain(|p| p.addr != addr),
                }
            }
        }

        if let Some(job) = &self.receiver {
            for event in job.rx.try_iter() {
                match event {
                    ReceiveEvent::IncomingRequest { manifest, respond } => {
                        self.pending_request = Some((manifest, respond));
                    }
                    ReceiveEvent::Progress { file, bytes_done, file_size, files_done, total_files } => {
                        self.receive_progress = Some(Progress { file, bytes_done, file_size, files_done, total_files });
                    }
                    ReceiveEvent::Finished { files_received, bytes_received, dest } => {
                        self.receive_progress = None;
                        self.log.push(format!(
                            "Received {files_received} file(s), {} → {}",
                            human_bytes(bytes_received),
                            dest.display()
                        ));
                        crate::notify::notify("Files received", &format!("{files_received} file(s) from a peer"));
                    }
                    ReceiveEvent::Failed(msg) => {
                        self.receive_progress = None;
                        self.log.push(format!("Receive failed: {msg}"));
                    }
                }
            }
        }

        if let Some(job) = &self.send_job {
            let mut done = false;
            for event in job.rx.try_iter() {
                match event {
                    SendEvent::Connecting => self.log.push("Connecting…".to_string()),
                    SendEvent::WaitingForAccept => self.log.push("Waiting for the other side to accept…".to_string()),
                    SendEvent::Rejected => {
                        self.log.push("The other side declined the transfer.".to_string());
                        done = true;
                    }
                    SendEvent::Progress { file, bytes_done, file_size, files_done, total_files } => {
                        self.send_progress = Some(Progress { file, bytes_done, file_size, files_done, total_files });
                    }
                    SendEvent::Finished { files_sent, bytes_sent } => {
                        self.send_progress = None;
                        self.log.push(format!("Sent {files_sent} file(s), {}", human_bytes(bytes_sent)));
                        crate::notify::notify("Files sent", &format!("{files_sent} file(s) sent"));
                        done = true;
                    }
                }
            }
            if done {
                self.send_job = None;
            }
        }
    }

    fn send_to(&mut self, peer: PeerInfo) {
        if self.sources.is_empty() || self.send_job.is_some() {
            return;
        }
        let mut files = Vec::new();
        for src in &self.sources {
            collect_files(src, src.parent().unwrap_or(src), &mut files);
        }
        if files.is_empty() {
            self.log.push("Nothing to send — the selected item(s) contain no files.".to_string());
            return;
        }
        self.log.clear();
        self.log.push(format!("Sending {} file(s) to {}…", files.len(), peer.name));
        self.send_target = Some(peer.clone());
        self.send_job = Some(worker::spawn_share_send(peer.addr, self.device_name.clone(), files));
    }

    pub fn ui(&mut self, ui: &mut egui::Ui, dropped: Vec<PathBuf>) {
        self.poll();
        if self.is_running() || self.receive_progress.is_some() {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(200));
        }
        self.sources.extend(dropped);

        ui.heading("Share");
        ui.label("Send files directly to another computer running Super Copier on the same network — no cloud, no account.");
        ui.separator();

        ui.horizontal(|ui| {
            ui.label("Device name:");
            ui.add_enabled(!self.discoverable, egui::TextEdit::singleline(&mut self.device_name).desired_width(160.0));
            let mut discoverable = self.discoverable;
            if ui.checkbox(&mut discoverable, "🟢 Discoverable").changed() {
                self.set_discoverable(discoverable);
            }
        });
        ui.horizontal(|ui| {
            ui.label("Save received files to:");
            ui.label(self.dest_dir.display().to_string());
            if ui.small_button("Change…").clicked() {
                if let Some(path) = rfd::FileDialog::new().pick_folder() {
                    self.dest_dir = path;
                }
            }
        });
        if !self.discoverable {
            ui.weak("Turn on \"Discoverable\" to find other computers and let them send you files.");
        }

        if let Some((manifest, respond)) = self.pending_request.take() {
            let resp = ui
                .group(|ui| {
                    ui.colored_label(egui::Color32::from_rgb(240, 180, 60), format!("📥 {} wants to send you:", manifest.sender_name));
                    for (name, size) in manifest.files.iter().take(8) {
                        ui.label(format!("  {name} ({})", human_bytes(*size)));
                    }
                    if manifest.files.len() > 8 {
                        ui.weak(format!("  …and {} more", manifest.files.len() - 8));
                    }
                    ui.label(format!("Total: {} file(s), {}", manifest.files.len(), human_bytes(manifest.total_bytes)));
                    ui.horizontal(|ui| {
                        let accept = ui.button("✅ Accept").clicked();
                        let reject = ui.button("❌ Reject").clicked();
                        (accept, reject)
                    })
                    .inner
                })
                .inner;
            match resp {
                (true, _) => {
                    let _ = respond.send(true);
                }
                (_, true) => {
                    let _ = respond.send(false);
                }
                _ => self.pending_request = Some((manifest, respond)),
            }
        }

        if let Some(p) = &self.receive_progress {
            ui.separator();
            ui.label(format!("Receiving {} ({}/{})…", p.file, p.files_done + 1, p.total_files));
            let frac = if p.file_size > 0 { p.bytes_done as f32 / p.file_size as f32 } else { 1.0 };
            ui.add(egui::ProgressBar::new(frac.clamp(0.0, 1.0)));
        }

        ui.separator();
        ui.label(format!("{} nearby device(s):", self.peers.len()));
        if self.discoverable && self.peers.is_empty() {
            ui.weak("Looking for other computers on the network…");
        }
        egui::ScrollArea::vertical().id_salt("peers_scroll").max_height(140.0).show(ui, |ui| {
            for peer in self.peers.clone() {
                ui.horizontal(|ui| {
                    ui.label(format!("💻 {}", peer.name));
                    ui.weak(peer.addr.ip().to_string());
                    if ui
                        .add_enabled(!self.sources.is_empty() && self.send_job.is_none(), egui::Button::new("Send files"))
                        .clicked()
                    {
                        self.send_to(peer);
                    }
                });
            }
        });

        ui.separator();
        let source_resp = self.ui_source_drop_zone(ui);
        if source_resp.clicked() {
            if let Some(paths) = rfd::FileDialog::new().pick_files() {
                self.sources.extend(paths);
            }
        }
        if !self.sources.is_empty() {
            ui.horizontal(|ui| {
                ui.label(format!("{} item(s) staged to send:", self.sources.len()));
                if ui.small_button("Clear").clicked() {
                    self.sources.clear();
                }
            });
            egui::ScrollArea::vertical().id_salt("share_sources_scroll").max_height(80.0).show(ui, |ui| {
                let mut remove_idx = None;
                for (i, p) in self.sources.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(p.display().to_string());
                        if ui.small_button("✕").clicked() {
                            remove_idx = Some(i);
                        }
                    });
                }
                if let Some(i) = remove_idx {
                    self.sources.remove(i);
                }
            });
        }

        if let Some(p) = &self.send_progress {
            ui.separator();
            let target = self.send_target.as_ref().map(|t| t.name.as_str()).unwrap_or("peer");
            ui.label(format!("Sending {} to {target} ({}/{})…", p.file, p.files_done + 1, p.total_files));
            let frac = if p.file_size > 0 { p.bytes_done as f32 / p.file_size as f32 } else { 1.0 };
            ui.add(egui::ProgressBar::new(frac.clamp(0.0, 1.0)));
            if ui.button("⏹ Cancel").clicked() {
                if let Some(job) = &self.send_job {
                    job.cancel.cancel();
                }
            }
        }

        if !self.log.is_empty() {
            ui.add_space(4.0);
            egui::CollapsingHeader::new("Log").default_open(false).show(ui, |ui| {
                egui::ScrollArea::vertical().id_salt("share_log_scroll").max_height(150.0).stick_to_bottom(true).show(ui, |ui| {
                    for line in self.log.iter() {
                        ui.label(line);
                    }
                });
            });
        }
    }

    fn ui_source_drop_zone(&self, ui: &mut egui::Ui) -> egui::Response {
        let hovering = dnd::hovering_files(ui.ctx());
        let fill = if hovering {
            ui.visuals().selection.bg_fill.linear_multiply(0.3)
        } else {
            ui.visuals().faint_bg_color
        };
        let inner = egui::Frame::group(ui.style()).fill(fill).show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.set_min_height(50.0);
            ui.vertical_centered(|ui| {
                ui.add_space(6.0);
                ui.label("Drag & drop files or folders here to stage them for sending");
                ui.weak("or click to browse");
                ui.add_space(6.0);
            });
        });
        ui.interact(inner.response.rect, ui.id().with("share_drop_zone"), egui::Sense::click())
    }
}

/// Walks `path` (file or folder) collecting every file under it, with
/// paths relative to `base` — that relative structure is what's preserved
/// on the receiving end.
fn collect_files(path: &std::path::Path, base: &std::path::Path, out: &mut Vec<FileToSend>) {
    let Ok(meta) = std::fs::metadata(path) else { return };
    if meta.is_file() {
        let rel = path.strip_prefix(base).unwrap_or(path).to_string_lossy().replace('\\', "/");
        out.push(FileToSend { abs_path: path.to_path_buf(), rel_path: rel, size: meta.len() });
    } else if meta.is_dir() {
        if let Ok(entries) = std::fs::read_dir(path) {
            for entry in entries.flatten() {
                collect_files(&entry.path(), base, out);
            }
        }
    }
}
