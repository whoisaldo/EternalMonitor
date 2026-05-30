use tracing::{info, warn};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1};

const VENDOR_NVIDIA: u32 = 0x10DE;
const VENDOR_AMD: u32 = 0x1002;
const VENDOR_INTEL: u32 = 0x8086;

/// Software adapter flag (DXGI_ADAPTER_FLAG_SOFTWARE = 2).
const FLAG_SOFTWARE: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    Unknown,
}

impl GpuVendor {
    fn from_vendor_id(id: u32) -> Self {
        match id {
            VENDOR_NVIDIA => Self::Nvidia,
            VENDOR_AMD => Self::Amd,
            VENDOR_INTEL => Self::Intel,
            _ => Self::Unknown,
        }
    }
}

impl std::fmt::Display for GpuVendor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Nvidia => write!(f, "NVIDIA"),
            Self::Amd => write!(f, "AMD"),
            Self::Intel => write!(f, "Intel"),
            Self::Unknown => write!(f, "Unknown"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct GpuInfo {
    pub vendor: GpuVendor,
    pub name: String,
    pub adapter_index: u32,
    pub dedicated_vram_mb: u64,
    pub encoder_name: String,
    pub codec_display_name: String,
}

impl GpuInfo {
    /// Detect the best GPU via DXGI and resolve the preferred encoder.
    /// Falls back to software encoding if detection fails.
    pub fn detect() -> Self {
        match detect_from_dxgi() {
            Ok(info) => info,
            Err(e) => {
                warn!(error = %e, "DXGI GPU detection failed, falling back to software encoding");
                Self::software_fallback()
            }
        }
    }

    pub fn software_fallback() -> Self {
        Self {
            vendor: GpuVendor::Unknown,
            name: "Software".to_string(),
            adapter_index: 0,
            dedicated_vram_mb: 0,
            encoder_name: "libx264".to_string(),
            codec_display_name: "H.264 (x264)".to_string(),
        }
    }
}

fn detect_from_dxgi() -> Result<GpuInfo, Box<dyn std::error::Error>> {
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };

    let mut best: Option<(u32, String, GpuVendor, u64)> = None;
    let mut index = 0u32;

    loop {
        let adapter: IDXGIAdapter1 = match unsafe { factory.EnumAdapters1(index) } {
            Ok(a) => a,
            Err(_) => break,
        };

        let desc = unsafe { adapter.GetDesc1()? };

        // Skip software adapters (e.g. Microsoft Basic Render Driver)
        if desc.Flags & FLAG_SOFTWARE != 0 {
            index += 1;
            continue;
        }

        let name = String::from_utf16_lossy(
            &desc
                .Description
                .iter()
                .copied()
                .take_while(|&c| c != 0)
                .collect::<Vec<_>>(),
        );
        let vendor = GpuVendor::from_vendor_id(desc.VendorId);
        let vram_mb = desc.DedicatedVideoMemory as u64 / (1024 * 1024);

        info!(
            adapter_index = index,
            name = %name,
            vendor = %vendor,
            vram_mb,
            "Found GPU adapter"
        );

        let is_better = best
            .as_ref()
            .is_none_or(|(_, _, _, best_vram)| vram_mb > *best_vram);
        if is_better {
            best = Some((index, name, vendor, vram_mb));
        }

        index += 1;
    }

    let (adapter_index, name, vendor, vram_mb) =
        best.ok_or("No non-software GPU adapters found via DXGI")?;

    let (encoder_name, codec_display_name) = resolve_encoder(vendor);

    Ok(GpuInfo {
        vendor,
        name,
        adapter_index,
        dedicated_vram_mb: vram_mb,
        encoder_name,
        codec_display_name,
    })
}

/// Walk a vendor-preferred fallback chain and return the first encoder
/// that is compiled into the linked FFmpeg build.
fn resolve_encoder(vendor: GpuVendor) -> (String, String) {
    let chain: &[(&str, &str)] = match vendor {
        GpuVendor::Nvidia => &[
            ("h264_nvenc", "H.264 (NVENC)"),
            ("h264_amf", "H.264 (AMF)"),
            ("h264_qsv", "H.264 (QSV)"),
            ("libx264", "H.264 (x264)"),
        ],
        GpuVendor::Amd => &[
            ("h264_amf", "H.264 (AMF)"),
            ("h264_nvenc", "H.264 (NVENC)"),
            ("h264_qsv", "H.264 (QSV)"),
            ("libx264", "H.264 (x264)"),
        ],
        GpuVendor::Intel => &[
            ("h264_qsv", "H.264 (QSV)"),
            ("h264_nvenc", "H.264 (NVENC)"),
            ("h264_amf", "H.264 (AMF)"),
            ("libx264", "H.264 (x264)"),
        ],
        GpuVendor::Unknown => &[
            ("h264_nvenc", "H.264 (NVENC)"),
            ("h264_amf", "H.264 (AMF)"),
            ("h264_qsv", "H.264 (QSV)"),
            ("libx264", "H.264 (x264)"),
        ],
    };

    for (name, display_name) in chain {
        if ffmpeg_next::encoder::find_by_name(name).is_some() {
            info!(encoder = name, codec = display_name, "Selected encoder");
            return (name.to_string(), display_name.to_string());
        }
        info!(encoder = name, "Encoder not available, trying next");
    }

    warn!("No hardware or software encoder found in FFmpeg — defaulting to libx264");
    ("libx264".to_string(), "H.264 (x264)".to_string())
}
