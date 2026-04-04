use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use once_cell::sync::Lazy;
use parking_lot::Mutex;

const LOG_BUFFER_LIMIT: usize = 200;
const SESSION_LOG_FILE_NAME: &str = "eternal-host-session.log";

static LOG_BUFFER: Lazy<Arc<Mutex<VecDeque<String>>>> =
    Lazy::new(|| Arc::new(Mutex::new(VecDeque::with_capacity(LOG_BUFFER_LIMIT))));
static SESSION_LOG_PATH: Lazy<PathBuf> = Lazy::new(resolve_session_log_path);
static SESSION_LOG_FILE: Lazy<Option<Arc<Mutex<File>>>> = Lazy::new(|| {
    let path = SESSION_LOG_PATH.clone();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    match OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&path)
    {
        Ok(file) => Some(Arc::new(Mutex::new(file))),
        Err(_) => None,
    }
});

#[derive(Clone)]
pub struct MemoryLogWriter {
    shared: Arc<Mutex<VecDeque<String>>>,
    session_file: Option<Arc<Mutex<File>>>,
    current_line: Vec<u8>,
}

impl MemoryLogWriter {
    pub fn new() -> Self {
        Self {
            shared: LOG_BUFFER.clone(),
            session_file: SESSION_LOG_FILE.clone(),
            current_line: Vec::new(),
        }
    }

    fn push_line(&mut self) {
        if self.current_line.is_empty() {
            return;
        }

        let line = String::from_utf8_lossy(&self.current_line)
            .trim_end_matches(['\r', '\n'])
            .to_string();
        self.current_line.clear();

        if line.is_empty() {
            return;
        }

        let mut shared = self.shared.lock();
        if shared.len() >= LOG_BUFFER_LIMIT {
            shared.pop_front();
        }
        shared.push_back(line);

        if let Some(file) = &self.session_file {
            let mut file = file.lock();
            let _ = writeln!(file, "{}", shared.back().expect("log line just inserted"));
            let _ = file.flush();
        }
    }
}

impl io::Write for MemoryLogWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        for &byte in buf {
            self.current_line.push(byte);
            if byte == b'\n' {
                self.push_line();
            }
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.push_line();
        Ok(())
    }
}

pub fn recent_log_text(limit: usize) -> Option<String> {
    let shared = LOG_BUFFER.lock();
    if shared.is_empty() {
        return None;
    }

    let count = limit.max(1);
    let start = shared.len().saturating_sub(count);
    Some(
        shared
            .iter()
            .skip(start)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n"),
    )
}

pub fn session_log_text() -> Option<String> {
    match std::fs::read_to_string(session_log_path()) {
        Ok(text) if !text.trim().is_empty() => Some(text),
        _ => recent_log_text(LOG_BUFFER_LIMIT),
    }
}

pub fn session_log_path() -> PathBuf {
    SESSION_LOG_PATH.clone()
}

fn resolve_session_log_path() -> PathBuf {
    let base_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(PathBuf::from))
        .unwrap_or_else(|| PathBuf::from("."));
    base_dir.join("logs").join(SESSION_LOG_FILE_NAME)
}
