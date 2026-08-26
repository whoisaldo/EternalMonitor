use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex as StdMutex};

use once_cell::sync::Lazy;
use parking_lot::Mutex;
use tracing::field::{Field, Visit};
use tracing::{Event, Metadata, Subscriber};
use tracing_subscriber::layer::{Context, Filter};
use tracing_subscriber::registry::LookupSpan;

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

impl Default for MemoryLogWriter {
    fn default() -> Self {
        Self::new()
    }
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
    // Prefer %APPDATA%/EternalMonitor/logs so the log is writable even when the app is installed
    // read-only under Program Files; fall back to the exe directory only if APPDATA is unavailable.
    let logs_dir = crate::settings::app_data_dir()
        .or_else(|| {
            std::env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().map(PathBuf::from))
        })
        .unwrap_or_else(|| PathBuf::from("."))
        .join("logs");
    logs_dir.join(SESSION_LOG_FILE_NAME)
}

// --- mDNS interface-failure deduper -----------------------------------------
//
// The `mdns-sd` crate emits one WARN per send attempt per interface when an
// interface (often Tailscale or other tunnels) refuses the multicast send.
// At ~10s probe cadence on multiple interfaces this floods the log. We keep a
// shared HashSet of interface markers seen so far and let only the FIRST
// failure per interface through.

static SUPPRESSED_MDNS_INTERFACES: Lazy<Arc<StdMutex<HashSet<String>>>> =
    Lazy::new(|| Arc::new(StdMutex::new(HashSet::new())));

#[derive(Clone)]
pub struct MdnsDedupFilter;

impl MdnsDedupFilter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MdnsDedupFilter {
    fn default() -> Self {
        Self::new()
    }
}

impl<S> Filter<S> for MdnsDedupFilter
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn enabled(&self, _metadata: &Metadata<'_>, _ctx: &Context<'_, S>) -> bool {
        true
    }

    fn event_enabled(&self, event: &Event<'_>, _ctx: &Context<'_, S>) -> bool {
        if event.metadata().target() != "mdns_sd" {
            return true;
        }
        let mut visitor = MessageVisitor::new();
        event.record(&mut visitor);
        let Some(message) = visitor.message else {
            return true;
        };
        // mdns-sd's send failure messages look like:
        //   "Failed to send to ... interface ..."
        // or in raw socket form contain a zone id like "%5". Bucket on either.
        if !message.contains("Failed to send") {
            return true;
        }
        let bucket = interface_bucket(&message);
        let mut set = match SUPPRESSED_MDNS_INTERFACES.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        // First failure per interface: insert returns true → emit.
        // Subsequent failures on the same interface: insert returns false → drop.
        set.insert(bucket)
    }
}

fn interface_bucket(message: &str) -> String {
    if let Some(idx) = message.find("interface") {
        let rest = &message[idx + "interface".len()..];
        let trimmed = rest.trim_start_matches([':', ' ']);
        let end = trimmed.find([',', '"', ')']).unwrap_or(trimmed.len());
        return trimmed[..end].trim().to_string();
    }
    if let Some(pct) = message.find('%') {
        let rest = &message[pct + 1..];
        let end = rest
            .find(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
            .unwrap_or(rest.len());
        if end > 0 {
            return format!("%{}", &rest[..end]);
        }
    }
    // Fallback: dedupe by the verb prefix so all "Failed to send" messages
    // collapse to a single line if we can't extract an interface name.
    "generic".to_string()
}

struct MessageVisitor {
    message: Option<String>,
}

impl MessageVisitor {
    fn new() -> Self {
        Self { message: None }
    }
}

impl Visit for MessageVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" && self.message.is_none() {
            self.message = Some(format!("{:?}", value));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" && self.message.is_none() {
            self.message = Some(value.to_string());
        }
    }
}
