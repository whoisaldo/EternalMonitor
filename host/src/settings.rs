use std::fs;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

const SETTINGS_FILE_NAME: &str = "settings.json";
const APP_FOLDER: &str = "EternalMonitor";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SettingsFile {
    pub bitrate_mbps: f32,
    pub target_fps: u32,
    #[serde(default)]
    pub target_ip: Option<String>,
    #[serde(default)]
    pub encoder_override: Option<String>,
    /// DXGI `DeviceName` of the capture display (e.g. `\\.\DISPLAY3`).
    /// `None` means auto (primary monitor).
    #[serde(default)]
    pub capture_display: Option<String>,
    pub start_on_boot: bool,
}

impl Default for SettingsFile {
    fn default() -> Self {
        Self {
            bitrate_mbps: 15.0,
            target_fps: 60,
            target_ip: None,
            encoder_override: None,
            capture_display: None,
            start_on_boot: false,
        }
    }
}

impl SettingsFile {
    /// Load settings from `%APPDATA%/EternalMonitor/settings.json`. On any failure
    /// (missing file, parse error, permission denied) returns `SettingsFile::default()`.
    pub fn load() -> Self {
        let Some(path) = settings_path() else {
            return Self::default();
        };
        match fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<SettingsFile>(&text) {
                Ok(settings) => {
                    info!(path = %path.display(), "Loaded settings from disk");
                    settings
                }
                Err(error) => {
                    warn!(
                        path = %path.display(),
                        error = %error,
                        "Failed to parse settings.json — using defaults"
                    );
                    Self::default()
                }
            },
            Err(_) => Self::default(),
        }
    }

    pub fn save(&self) {
        let Some(path) = settings_path() else {
            return;
        };
        if let Some(parent) = path.parent() {
            if let Err(error) = fs::create_dir_all(parent) {
                warn!(
                    path = %parent.display(),
                    error = %error,
                    "Failed to create settings directory"
                );
                return;
            }
        }
        match serde_json::to_string_pretty(self) {
            Ok(text) => {
                if let Err(error) = fs::write(&path, text) {
                    warn!(
                        path = %path.display(),
                        error = %error,
                        "Failed to write settings.json"
                    );
                }
            }
            Err(error) => {
                warn!(error = %error, "Failed to serialize settings");
            }
        }
    }
}

/// The per-user writable data directory for EternalMonitor (`%APPDATA%/EternalMonitor` on
/// Windows, `~/Library/Application Support/EternalMonitor` elsewhere). Used for settings, the
/// session log, and AMF diagnostics so they keep working when the app is installed read-only
/// under `Program Files`. Returns `None` only if neither `APPDATA` nor `HOME` is set.
pub fn app_data_dir() -> Option<PathBuf> {
    let base = std::env::var("APPDATA").ok().map(PathBuf::from).or_else(|| {
        std::env::var("HOME").ok().map(|home| {
            PathBuf::from(home)
                .join("Library")
                .join("Application Support")
        })
    })?;
    Some(base.join(APP_FOLDER))
}

fn settings_path() -> Option<PathBuf> {
    app_data_dir().map(|dir| dir.join(SETTINGS_FILE_NAME))
}
