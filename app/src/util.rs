use std::collections::VecDeque;

pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{:.1} {}", size, UNITS[unit])
    }
}

pub fn human_rate(bytes_per_sec: f64) -> String {
    format!("{}/s", human_bytes(bytes_per_sec as u64))
}

/// Fixed-capacity log of recent status lines, newest first.
pub struct Log {
    lines: VecDeque<String>,
    cap: usize,
}

impl Log {
    pub fn new(cap: usize) -> Self {
        Self {
            lines: VecDeque::with_capacity(cap),
            cap,
        }
    }

    pub fn push(&mut self, line: impl Into<String>) {
        self.lines.push_front(line.into());
        while self.lines.len() > self.cap {
            self.lines.pop_back();
        }
    }

    pub fn clear(&mut self) {
        self.lines.clear();
    }

    pub fn iter(&self) -> impl Iterator<Item = &String> {
        self.lines.iter()
    }
}

impl Default for Log {
    fn default() -> Self {
        Self::new(200)
    }
}
