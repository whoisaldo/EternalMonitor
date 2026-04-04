use std::collections::VecDeque;
use std::io;
use std::sync::Arc;

use once_cell::sync::Lazy;
use parking_lot::Mutex;

const LOG_BUFFER_LIMIT: usize = 200;

static LOG_BUFFER: Lazy<Arc<Mutex<VecDeque<String>>>> =
    Lazy::new(|| Arc::new(Mutex::new(VecDeque::with_capacity(LOG_BUFFER_LIMIT))));

#[derive(Clone)]
pub struct MemoryLogWriter {
    shared: Arc<Mutex<VecDeque<String>>>,
    current_line: Vec<u8>,
}

impl MemoryLogWriter {
    pub fn new() -> Self {
        Self {
            shared: LOG_BUFFER.clone(),
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
