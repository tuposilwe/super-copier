//! Splits flashing across two processes. The GUI (unprivileged) writes a
//! [`FlashJob`] file and launches a copy of itself as an elevated helper;
//! the helper does the dangerous work and reports back by appending JSON
//! [`Event`] lines to a progress file the GUI tails. That keeps the whole
//! GUI out of administrator mode, and lets the helper re-verify the target
//! itself instead of trusting whatever the GUI sent.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use crossbeam_channel::Receiver;

use super::devices::list_devices;
use super::{winmedia, write, Event, FlashJob, Mode};
use crate::{CancelToken, EngineError, EngineResult};

/// Command-line flag that turns the app binary into the helper.
pub const HELPER_FLAG: &str = "--flash-helper";

// -------------------------------------------------------- helper process

/// Entry point of the elevated helper. Returns the process exit code.
pub fn run_helper(job_path: &Path) -> i32 {
    let job: FlashJob = match std::fs::read_to_string(job_path)
        .map_err(|e| e.to_string())
        .and_then(|s| serde_json::from_str(&s).map_err(|e| e.to_string()))
    {
        Ok(j) => j,
        Err(e) => {
            eprintln!("bad flash job: {e}");
            return 3;
        }
    };
    let mut log = match OpenOptions::new().create(true).append(true).open(&job.progress_file) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("can't open progress file: {e}");
            return 3;
        }
    };
    let mut emit = move |ev: Event| {
        if let Ok(line) = serde_json::to_string(&ev) {
            let _ = writeln!(log, "{line}");
            let _ = log.flush();
        }
    };

    let cancel = CancelToken::new();
    watch_cancel_file(job.cancel_file.clone(), cancel.clone());

    match execute(&job, &cancel, &mut emit) {
        Ok(()) => {
            emit(Event::Finished);
            0
        }
        Err(EngineError::Cancelled) => {
            emit(Event::Cancelled);
            2
        }
        Err(e) => {
            emit(Event::Failed { message: e.to_string() });
            1
        }
    }
}

fn watch_cancel_file(path: PathBuf, token: CancelToken) {
    std::thread::spawn(move || loop {
        if path.exists() {
            token.cancel();
            return;
        }
        if token.is_cancelled() {
            return;
        }
        std::thread::sleep(Duration::from_millis(150));
    });
}

fn execute(job: &FlashJob, cancel: &CancelToken, emit: &mut dyn FnMut(Event)) -> EngineResult<()> {
    // Never trust the GUI's choice: look the drive up again, from scratch.
    let device = list_devices()?
        .into_iter()
        .find(|d| d.id == job.device_id)
        .ok_or_else(|| EngineError::Other(format!("{} is no longer connected", job.device_id)))?;
    if !device.is_safe_target() {
        return Err(EngineError::Other(format!(
            "refusing to write to {} — it isn't a removable drive, or it's the disk this computer runs from",
            device.id
        )));
    }
    match job.mode {
        Mode::Raw => write::flash_raw(&job.image, &device, job.verify, cancel, emit),
        Mode::WindowsFiles => winmedia::flash_windows_files(&job.image, &device, cancel, emit),
    }
}

// ------------------------------------------------------------ GUI side

/// Reads new lines from a progress file as the helper appends them.
pub struct ProgressReader {
    path: PathBuf,
    offset: u64,
    partial: String,
}

impl ProgressReader {
    pub fn new(path: PathBuf) -> Self {
        Self { path, offset: 0, partial: String::new() }
    }

    pub fn poll(&mut self) -> Vec<Event> {
        let Ok(mut f) = File::open(&self.path) else { return Vec::new() };
        if f.seek(SeekFrom::Start(self.offset)).is_err() {
            return Vec::new();
        }
        let mut chunk = String::new();
        if f.read_to_string(&mut chunk).is_err() {
            return Vec::new();
        }
        self.offset += chunk.len() as u64;
        self.partial.push_str(&chunk);
        let mut events = Vec::new();
        while let Some(nl) = self.partial.find('\n') {
            let line: String = self.partial.drain(..=nl).collect();
            if let Ok(ev) = serde_json::from_str::<Event>(line.trim()) {
                events.push(ev);
            }
        }
        events
    }
}

/// Starts a flash and returns a token to cancel it plus a stream of events.
/// With `elevate`, the helper is launched with administrator rights (which
/// prompts the user); without it the helper just runs as the current user.
pub fn spawn_flash(
    image: PathBuf,
    device_id: String,
    mode: Mode,
    verify: bool,
    elevate: bool,
) -> EngineResult<(CancelToken, Receiver<Event>)> {
    let exe = std::env::current_exe().map_err(|e| EngineError::Other(format!("can't locate the app: {e}")))?;
    spawn_flash_with(&exe, image, device_id, mode, verify, elevate)
}

/// [`spawn_flash`] with the helper executable given explicitly, so tests can
/// substitute a stand-in for the real app binary.
pub(crate) fn spawn_flash_with(
    exe: &Path,
    image: PathBuf,
    device_id: String,
    mode: Mode,
    verify: bool,
    elevate: bool,
) -> EngineResult<(CancelToken, Receiver<Event>)> {
    let exe = exe.to_path_buf();
    let dir = std::env::temp_dir().join(format!("supercopier-flash-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).map_err(|e| crate::io_err(&dir, e))?;
    let job = FlashJob {
        image,
        device_id,
        mode,
        verify,
        progress_file: dir.join("progress.jsonl"),
        cancel_file: dir.join("cancel"),
    };
    let job_path = dir.join("job.json");
    std::fs::write(&job_path, serde_json::to_string(&job).unwrap()).map_err(|e| crate::io_err(&job_path, e))?;
    File::create(&job.progress_file).map_err(|e| crate::io_err(&job.progress_file, e))?;

    let cancel = CancelToken::new();
    let (tx, rx) = crossbeam_channel::unbounded();
    let cancel2 = cancel.clone();
    std::thread::spawn(move || {
        let (done_tx, done_rx) = mpsc::channel();
        let (exe2, job_path2) = (exe.clone(), job_path.clone());
        std::thread::spawn(move || {
            let _ = done_tx.send(launch_helper(&exe2, &job_path2, elevate));
        });

        let mut reader = ProgressReader::new(job.progress_file.clone());
        let mut saw_terminal = false;
        let mut cancel_written = false;
        let helper_result = loop {
            for ev in reader.poll() {
                saw_terminal |= ev.is_terminal();
                let _ = tx.send(ev);
            }
            if cancel2.is_cancelled() && !cancel_written {
                cancel_written = std::fs::write(&job.cancel_file, b"").is_ok();
            }
            match done_rx.recv_timeout(Duration::from_millis(100)) {
                Ok(r) => break r,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break Err(EngineError::Other("helper thread died".into())),
            }
        };
        for ev in reader.poll() {
            saw_terminal |= ev.is_terminal();
            let _ = tx.send(ev);
        }
        if !saw_terminal {
            let message = match helper_result {
                Ok(code) => format!("the flashing helper stopped unexpectedly (exit code {code})"),
                Err(e) => e.to_string(),
            };
            let _ = tx.send(Event::Failed { message });
        }
        let _ = std::fs::remove_dir_all(&dir);
    });
    Ok((cancel, rx))
}

fn launch_helper(exe: &Path, job: &Path, elevate: bool) -> EngineResult<i32> {
    let args = vec![HELPER_FLAG.to_string(), job.display().to_string()];
    if elevate {
        run_elevated(exe, &args)
    } else {
        let status = std::process::Command::new(exe)
            .args(&args)
            .status()
            .map_err(|e| EngineError::Other(format!("couldn't start the helper: {e}")))?;
        Ok(status.code().unwrap_or(-1))
    }
}

/// Single-quotes a string for `sh`.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Escapes a string for use inside an AppleScript double-quoted literal.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn applescript_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(target_os = "macos")]
fn run_elevated(exe: &Path, args: &[String]) -> EngineResult<i32> {
    let mut cmd = shell_quote(&exe.display().to_string());
    for a in args {
        cmd.push(' ');
        cmd.push_str(&shell_quote(a));
    }
    let script = format!(r#"do shell script "{}" with administrator privileges"#, applescript_escape(&cmd));
    let out = std::process::Command::new("osascript")
        .args(["-e", &script])
        .output()
        .map_err(|e| EngineError::Other(format!("couldn't ask for permission: {e}")))?;
    if !out.status.success() && String::from_utf8_lossy(&out.stderr).contains("User canceled") {
        return Err(EngineError::Other("Administrator permission was not granted.".into()));
    }
    Ok(out.status.code().unwrap_or(-1))
}

#[cfg(target_os = "linux")]
fn run_elevated(exe: &Path, args: &[String]) -> EngineResult<i32> {
    let already_root = unsafe { libc::geteuid() } == 0;
    let mut cmd = if already_root {
        std::process::Command::new(exe)
    } else {
        let mut c = std::process::Command::new("pkexec");
        c.arg(exe);
        c
    };
    let status = cmd
        .args(args)
        .status()
        .map_err(|e| EngineError::Other(format!("couldn't ask for permission (is polkit's pkexec installed?): {e}")))?;
    match status.code() {
        Some(126) | Some(127) => Err(EngineError::Other("Administrator permission was not granted.".into())),
        Some(c) => Ok(c),
        None => Ok(-1),
    }
}

#[cfg(windows)]
fn run_elevated(exe: &Path, args: &[String]) -> EngineResult<i32> {
    use std::os::windows::ffi::OsStrExt;

    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
    use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};

    fn wide(s: &std::ffi::OsStr) -> Vec<u16> {
        s.encode_wide().chain(std::iter::once(0)).collect()
    }
    let verb = wide("runas".as_ref());
    let file = wide(exe.as_os_str());
    let params = wide(args.iter().map(|a| format!("\"{a}\"")).collect::<Vec<_>>().join(" ").as_ref());

    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.lpParameters = params.as_ptr();
    info.nShow = 0; // SW_HIDE
    if unsafe { ShellExecuteExW(&mut info) } == 0 {
        return Err(EngineError::Other("Administrator permission was not granted.".into()));
    }
    let mut code = 0u32;
    unsafe {
        WaitForSingleObject(info.hProcess, INFINITE);
        GetExitCodeProcess(info.hProcess, &mut code);
        CloseHandle(info.hProcess);
    }
    Ok(code as i32)
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn run_elevated(_: &Path, _: &[String]) -> EngineResult<i32> {
    Err(EngineError::Other("unsupported platform".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_reader_returns_only_complete_new_lines() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("p.jsonl");
        std::fs::write(&path, "").unwrap();
        let mut reader = ProgressReader::new(path.clone());
        assert!(reader.poll().is_empty());

        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, r#"{{"kind":"Phase","name":"Writing image"}}"#).unwrap();
        write!(f, r#"{{"kind":"Progress","done":5,"#).unwrap(); // half a line
        f.flush().unwrap();
        assert_eq!(reader.poll(), vec![Event::Phase { name: "Writing image".into() }]);

        writeln!(f, r#""total":10}}"#).unwrap();
        assert_eq!(reader.poll(), vec![Event::Progress { done: 5, total: 10 }]);
        assert!(reader.poll().is_empty(), "nothing new the second time");
    }

    #[test]
    fn quoting_survives_spaces_and_quotes() {
        assert_eq!(shell_quote("/Applications/Super Copier.app/x"), "'/Applications/Super Copier.app/x'");
        assert_eq!(shell_quote("it's"), r"'it'\''s'");
        assert_eq!(applescript_escape(r#"a "b" \c"#), r#"a \"b\" \\c"#);
    }

    #[test]
    fn the_helper_refuses_a_target_that_isnt_in_the_safe_list() {
        let dir = tempfile::tempdir().unwrap();
        let img = dir.path().join("x.img");
        std::fs::write(&img, vec![0u8; 4096]).unwrap();
        let job = FlashJob {
            image: img,
            device_id: "definitely-not-a-real-disk".into(),
            mode: Mode::Raw,
            verify: true,
            progress_file: dir.path().join("p.jsonl"),
            cancel_file: dir.path().join("cancel"),
        };
        let job_path = dir.path().join("job.json");
        std::fs::write(&job_path, serde_json::to_string(&job).unwrap()).unwrap();

        assert_eq!(run_helper(&job_path), 1);
        let events = ProgressReader::new(job.progress_file).poll();
        assert!(matches!(events.last(), Some(Event::Failed { .. })), "got {events:?}");
    }

    #[cfg(unix)]
    mod glue {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        /// Writes an executable shell script standing in for the app binary.
        /// It's invoked as `<script> --flash-helper <dir>/job.json`, so its
        /// progress file is `<dir>/progress.jsonl` and cancel file `<dir>/cancel`.
        fn fake_helper(dir: &Path, body: &str) -> PathBuf {
            let path = dir.join("fake-helper.sh");
            std::fs::write(&path, format!("#!/bin/sh\nd=$(dirname \"$2\")\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        }

        fn collect(rx: Receiver<Event>) -> Vec<Event> {
            let mut all = Vec::new();
            while let Ok(ev) = rx.recv_timeout(Duration::from_secs(10)) {
                let done = ev.is_terminal();
                all.push(ev);
                if done {
                    break;
                }
            }
            all
        }

        #[test]
        fn events_from_the_helper_reach_the_gui_in_order() {
            let dir = tempfile::tempdir().unwrap();
            let exe = fake_helper(
                dir.path(),
                r#"echo '{"kind":"Phase","name":"Writing image"}' >> "$d/progress.jsonl"
echo '{"kind":"Progress","done":5,"total":10}' >> "$d/progress.jsonl"
echo '{"kind":"Finished"}' >> "$d/progress.jsonl""#,
            );
            let (_cancel, rx) =
                spawn_flash_with(&exe, "x.iso".into(), "disk9".into(), Mode::Raw, true, false).unwrap();
            assert_eq!(
                collect(rx),
                vec![
                    Event::Phase { name: "Writing image".into() },
                    Event::Progress { done: 5, total: 10 },
                    Event::Finished
                ]
            );
        }

        #[test]
        fn cancelling_creates_the_cancel_file_and_the_helper_answers() {
            let dir = tempfile::tempdir().unwrap();
            // Waits (up to ~10s) for the cancel file, then reports Cancelled.
            let exe = fake_helper(
                dir.path(),
                r#"echo '{"kind":"Phase","name":"Writing image"}' >> "$d/progress.jsonl"
i=0; while [ ! -e "$d/cancel" ] && [ $i -lt 100 ]; do sleep 0.1; i=$((i+1)); done
if [ -e "$d/cancel" ]; then echo '{"kind":"Cancelled"}' >> "$d/progress.jsonl"; fi"#,
            );
            let (cancel, rx) =
                spawn_flash_with(&exe, "x.iso".into(), "disk9".into(), Mode::Raw, true, false).unwrap();
            assert!(matches!(rx.recv_timeout(Duration::from_secs(10)), Ok(Event::Phase { .. })));
            cancel.cancel();
            assert_eq!(collect(rx), vec![Event::Cancelled]);
        }

        #[test]
        fn a_helper_that_dies_without_a_final_event_is_reported_as_a_failure() {
            let dir = tempfile::tempdir().unwrap();
            let exe = fake_helper(
                dir.path(),
                r#"echo '{"kind":"Phase","name":"Writing image"}' >> "$d/progress.jsonl"
exit 7"#,
            );
            let (_cancel, rx) =
                spawn_flash_with(&exe, "x.iso".into(), "disk9".into(), Mode::Raw, true, false).unwrap();
            let events = collect(rx);
            match events.last() {
                Some(Event::Failed { message }) => assert!(message.contains("exit code 7"), "{message}"),
                other => panic!("expected a Failed event, got {other:?}"),
            }
        }

        #[test]
        fn the_temp_job_directory_is_cleaned_up_afterwards() {
            let dir = tempfile::tempdir().unwrap();
            let exe = fake_helper(
                dir.path(),
                r#"echo "$d" > "$(dirname "$0")/jobdir.txt"
echo '{"kind":"Finished"}' >> "$d/progress.jsonl""#,
            );
            let (_cancel, rx) =
                spawn_flash_with(&exe, "x.iso".into(), "disk9".into(), Mode::Raw, true, false).unwrap();
            collect(rx);
            std::thread::sleep(Duration::from_millis(500));
            let job_dir = std::fs::read_to_string(dir.path().join("jobdir.txt")).unwrap();
            assert!(!Path::new(job_dir.trim()).exists(), "job dir {job_dir} should be removed");
        }
    }
}
