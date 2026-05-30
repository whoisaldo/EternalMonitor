use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};
use windows::core::Interface;

use crate::control::{CaptureTarget, SharedControl};
use crate::stats::PIPELINE_STATS;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11DeviceContext, ID3D11Texture2D, D3D11_CPU_ACCESS_READ,
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_MAPPED_SUBRESOURCE, D3D11_MAP_READ, D3D11_SDK_VERSION,
    D3D11_TEXTURE2D_DESC, D3D11_USAGE_STAGING,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput, IDXGIOutput1,
    IDXGIOutputDuplication, IDXGIResource, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT,
    DXGI_OUTDUPL_FRAME_INFO,
};

const ACQUIRE_TIMEOUT_MS: u32 = 16;
const FPS_WINDOW: usize = 60;

fn frame_budget_for(target_fps: u32) -> Duration {
    let fps = target_fps.max(1) as u64;
    Duration::from_micros(1_000_000 / fps)
}

/// Frame data sent downstream for encoding/transport.
#[derive(Debug, Clone)]
pub struct RawFrame {
    pub frame_number: u64,
    pub timestamp: Instant,
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

/// A desktop-attached display output discovered via DXGI, usable as a capture source.
#[derive(Debug, Clone)]
pub struct OutputInfo {
    pub adapter_index: u32,
    pub output_index: u32,
    pub device_name: String,
    pub width: u32,
    pub height: u32,
    pub left: i32,
    pub top: i32,
    pub is_primary: bool,
    pub adapter_name: String,
}

/// Parse a null-terminated UTF-16 fixed array (DXGI `DeviceName` / `Description`) to a String.
fn utf16_to_string(buf: &[u16]) -> String {
    String::from_utf16_lossy(&buf.iter().copied().take_while(|&c| c != 0).collect::<Vec<_>>())
}

/// Enumerate every desktop-attached display output across all adapters. Read-only and
/// reusable by both the GUI picker and the capture loop. Never panics — on any DXGI error
/// it returns whatever was discovered so far (possibly empty).
pub fn enumerate_outputs() -> Vec<OutputInfo> {
    let mut outputs = Vec::new();
    let factory: IDXGIFactory1 = match unsafe { CreateDXGIFactory1() } {
        Ok(f) => f,
        Err(error) => {
            warn!(error = %error, "CreateDXGIFactory1 failed during output enumeration");
            return outputs;
        }
    };

    let mut adapter_index = 0u32;
    loop {
        let adapter = match unsafe { factory.EnumAdapters1(adapter_index) } {
            Ok(a) => a,
            Err(_) => break,
        };
        let adapter_name = match unsafe { adapter.GetDesc1() } {
            Ok(desc) => utf16_to_string(&desc.Description),
            Err(_) => String::new(),
        };

        let mut output_index = 0u32;
        loop {
            let output = match unsafe { adapter.EnumOutputs(output_index) } {
                Ok(o) => o,
                Err(_) => break,
            };
            if let Ok(desc) = unsafe { output.GetDesc() } {
                if desc.AttachedToDesktop.as_bool() {
                    let r = desc.DesktopCoordinates;
                    outputs.push(OutputInfo {
                        adapter_index,
                        output_index,
                        device_name: utf16_to_string(&desc.DeviceName),
                        width: (r.right - r.left).max(0) as u32,
                        height: (r.bottom - r.top).max(0) as u32,
                        left: r.left,
                        top: r.top,
                        is_primary: r.left == 0 && r.top == 0,
                        adapter_name: adapter_name.clone(),
                    });
                }
            }
            output_index += 1;
        }
        adapter_index += 1;
    }
    outputs
}

/// Resolve which output (index into `outputs`) the capture loop should duplicate for
/// `target`. `PrimaryAuto` picks the (0,0) output, else the first attached. `Output(name)`
/// matches the `DeviceName`, falling back to the primary (with a warning) when not found —
/// e.g. the virtual display driver was disabled. Returns `None` only when `outputs` is empty.
fn resolve_target(outputs: &[OutputInfo], target: &CaptureTarget) -> Option<usize> {
    if outputs.is_empty() {
        return None;
    }
    let primary = outputs.iter().position(|o| o.is_primary).unwrap_or(0);
    match target {
        CaptureTarget::PrimaryAuto => Some(primary),
        CaptureTarget::Output(name) => match outputs.iter().position(|o| &o.device_name == name) {
            Some(i) => Some(i),
            None => {
                warn!(
                    requested = %name,
                    "Requested capture display not found (driver disabled/removed?) — \
                     falling back to the primary output"
                );
                Some(primary)
            }
        },
    }
}

/// Starts the DXGI Desktop Duplication capture loop on a dedicated blocking thread.
/// Returns an `mpsc::Receiver<RawFrame>` that downstream pipeline stages can consume.
pub fn start_capture(shared: SharedControl, adapter_index: u32) -> mpsc::Receiver<RawFrame> {
    let (tx, rx) = mpsc::channel::<RawFrame>(4);

    tokio::task::spawn_blocking(move || {
        if let Err(e) = run_capture_loop(tx, shared, adapter_index) {
            error!(error = %e, "Capture loop exited with error");
        }
    });

    rx
}

/// Enumerate adapters, select the one driving the primary output, create duplication,
/// and pull frames at ~60fps. Runs on a blocking thread — never call from async context.
fn run_capture_loop(
    tx: mpsc::Sender<RawFrame>,
    shared: SharedControl,
    adapter_index: u32,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // --- Create DXGI factory and select the output to capture ---
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };

    // The capture adapter FOLLOWS the chosen output (which may differ from the
    // encoder-detection adapter on a multi-GPU system); the encoder stays vendor-based
    // and independent. `adapter_index` is only the last-resort fallback below.
    let target = shared.capture_target.lock().clone();
    let outputs = enumerate_outputs();
    for o in &outputs {
        info!(
            device = %o.device_name,
            adapter = %o.adapter_name,
            width = o.width,
            height = o.height,
            primary = o.is_primary,
            "Discovered display output"
        );
    }

    let (adapter, output, adapter_name, capture_label) = match resolve_target(&outputs, &target) {
        Some(idx) => {
            let chosen = &outputs[idx];
            let adapter = match unsafe { factory.EnumAdapters1(chosen.adapter_index) } {
                Ok(a) => a,
                Err(error) => {
                    warn!(
                        adapter_index = chosen.adapter_index,
                        error = %error,
                        "EnumAdapters1 failed for the chosen output's adapter — falling back to adapter 0"
                    );
                    unsafe { factory.EnumAdapters1(0)? }
                }
            };
            let adapter_name = match unsafe { adapter.GetDesc1() } {
                Ok(desc) => utf16_to_string(&desc.Description),
                Err(_) => chosen.adapter_name.clone(),
            };
            let output: IDXGIOutput = unsafe { adapter.EnumOutputs(chosen.output_index)? };
            let label = format!("{} ({}x{})", chosen.device_name, chosen.width, chosen.height);
            info!(
                device = %chosen.device_name,
                adapter = %adapter_name,
                width = chosen.width,
                height = chosen.height,
                "Capture: selected display output"
            );
            (adapter, output, adapter_name, label)
        }
        None => {
            // No outputs enumerated (unusual) — preserve the legacy path exactly.
            warn!("No display outputs enumerated — falling back to the detection adapter's first output");
            let adapter = match unsafe { factory.EnumAdapters1(adapter_index) } {
                Ok(a) => a,
                Err(error) => {
                    warn!(
                        requested = adapter_index,
                        error = %error,
                        "EnumAdapters1 failed for requested adapter index, falling back to adapter 0"
                    );
                    unsafe { factory.EnumAdapters1(0)? }
                }
            };
            let adapter_name = match unsafe { adapter.GetDesc1() } {
                Ok(desc) => utf16_to_string(&desc.Description),
                Err(_) => String::new(),
            };
            let output: IDXGIOutput = unsafe { adapter.EnumOutputs(0)? };
            (adapter, output, adapter_name, "primary (auto)".to_string())
        }
    };

    info!(adapter = %adapter_name, "Capture using adapter");
    {
        let mut stats = PIPELINE_STATS.lock();
        stats.set_gpu_name(adapter_name);
        stats.set_capture_display(capture_label);
    }

    // --- Create D3D11 device on the selected adapter ---
    let mut device: Option<ID3D11Device> = None;
    unsafe {
        D3D11CreateDevice(
            &adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            None,
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;
    }
    let device = device.expect("D3D11CreateDevice succeeded but returned no device");
    info!("D3D11 device created");

    // --- Get immediate device context ---
    let device_context: ID3D11DeviceContext = unsafe { device.GetImmediateContext()? };

    // --- Duplicate the output ---
    let output1: IDXGIOutput1 = output.cast()?;
    let mut duplication: IDXGIOutputDuplication = unsafe { output1.DuplicateOutput(&device)? };
    info!("Desktop duplication active");

    // --- Query desktop dimensions from duplication ---
    let dupl_desc = unsafe { duplication.GetDesc() };
    let tex_width = dupl_desc.ModeDesc.Width;
    let tex_height = dupl_desc.ModeDesc.Height;
    info!(
        width = tex_width,
        height = tex_height,
        "Desktop duplication dimensions"
    );

    // --- Create staging texture for CPU readback (reused every frame) ---
    let staging_desc = D3D11_TEXTURE2D_DESC {
        Width: tex_width,
        Height: tex_height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: Default::default(),
    };
    let staging_texture: ID3D11Texture2D = unsafe {
        let mut tex = None;
        device.CreateTexture2D(&staging_desc, None, Some(&mut tex))?;
        tex.expect("CreateTexture2D returned None")
    };
    info!("Staging texture created for CPU readback");

    let row_bytes = (tex_width * 4) as usize;
    let frame_size = row_bytes * tex_height as usize;
    let mut pixel_buf: Vec<u8> = vec![0u8; frame_size];

    // --- Capture loop state ---
    let mut frame_number: u64 = 0;
    let mut frame_timestamps: VecDeque<Instant> = VecDeque::with_capacity(FPS_WINDOW + 1);
    let mut dirty_rect_buf: Vec<RECT> = Vec::with_capacity(64);

    loop {
        if !shared.running.load(Ordering::SeqCst) {
            info!("Capture loop stopping on running=false");
            break;
        }

        let frame_start = Instant::now();
        let frame_budget = frame_budget_for(shared.target_fps.load(Ordering::SeqCst));

        let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut desktop_resource: Option<IDXGIResource> = None;

        let acquire_start = Instant::now();
        let hr = unsafe {
            duplication.AcquireNextFrame(ACQUIRE_TIMEOUT_MS, &mut frame_info, &mut desktop_resource)
        };
        let acquire_us = acquire_start.elapsed().as_micros();

        match hr {
            Ok(()) => {
                frame_number += 1;

                // --- Get dirty rect count ---
                let dirty_count =
                    get_dirty_rect_count(&duplication, &frame_info, &mut dirty_rect_buf);

                // --- Rolling FPS (60-frame window) ---
                let now = Instant::now();
                frame_timestamps.push_back(now);
                if frame_timestamps.len() > FPS_WINDOW {
                    frame_timestamps.pop_front();
                }
                let rolling_fps = if frame_timestamps.len() >= 2 {
                    let window = frame_timestamps
                        .back()
                        .unwrap()
                        .duration_since(*frame_timestamps.front().unwrap());
                    let secs = window.as_secs_f64();
                    if secs > 0.0 {
                        (frame_timestamps.len() - 1) as f64 / secs
                    } else {
                        0.0
                    }
                } else {
                    0.0
                };

                info!(
                    frame = frame_number,
                    dirty_rects = dirty_count,
                    acquire_us = acquire_us,
                    fps = format!("{rolling_fps:.1}"),
                    "Frame acquired"
                );

                // --- Copy desktop texture to staging and read pixels ---
                let bgra_data = if let Some(ref resource) = desktop_resource {
                    match resource.cast::<ID3D11Texture2D>() {
                        Ok(desktop_texture) => {
                            unsafe {
                                device_context.CopyResource(&staging_texture, &desktop_texture);
                            }

                            // Release acquired frame — staging texture has its own copy
                            unsafe {
                                duplication.ReleaseFrame()?;
                            }

                            // Map staging texture for CPU read
                            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                            unsafe {
                                device_context.Map(
                                    &staging_texture,
                                    0,
                                    D3D11_MAP_READ,
                                    0,
                                    Some(&mut mapped),
                                )?;
                            }

                            // Copy row-by-row (pitch may differ from width*4)
                            let src_pitch = mapped.RowPitch as usize;
                            let src_ptr = mapped.pData as *const u8;
                            for row in 0..tex_height as usize {
                                let src_offset = row * src_pitch;
                                let dst_offset = row * row_bytes;
                                unsafe {
                                    std::ptr::copy_nonoverlapping(
                                        src_ptr.add(src_offset),
                                        pixel_buf.as_mut_ptr().add(dst_offset),
                                        row_bytes,
                                    );
                                }
                            }

                            unsafe {
                                device_context.Unmap(&staging_texture, 0);
                            }

                            pixel_buf.clone()
                        }
                        Err(e) => {
                            warn!(error = %e, "Failed to cast IDXGIResource to ID3D11Texture2D");
                            unsafe {
                                duplication.ReleaseFrame()?;
                            }
                            Vec::new()
                        }
                    }
                } else {
                    unsafe {
                        duplication.ReleaseFrame()?;
                    }
                    Vec::new()
                };

                // Record stats
                PIPELINE_STATS.lock().record_capture(tex_width, tex_height);

                // Send downstream
                let raw_frame = RawFrame {
                    frame_number,
                    timestamp: now,
                    data: bgra_data,
                    width: tex_width,
                    height: tex_height,
                };
                if tx.blocking_send(raw_frame).is_err() {
                    info!("Channel closed, stopping capture");
                    break;
                }
            }
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                // Desktop unchanged — not an error.
                if !shared.running.load(Ordering::SeqCst) {
                    info!("Capture loop stopping after timeout on running=false");
                    break;
                }
                continue;
            }
            Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                warn!("Desktop duplication access lost — reinitializing");
                duplication = unsafe { output1.DuplicateOutput(&device)? };
                continue;
            }
            Err(e) => {
                error!(error = %e, "AcquireNextFrame failed");
                return Err(e.into());
            }
        }

        // Frame pacing — sleep remainder of budget
        let frame_elapsed = frame_start.elapsed();
        if frame_elapsed < frame_budget {
            std::thread::sleep(frame_budget - frame_elapsed);
        } else {
            debug!(
                over_budget_us = (frame_elapsed - frame_budget).as_micros(),
                "Frame over budget"
            );
        }
    }

    Ok(())
}

/// Query the actual number of dirty rectangles from DXGI for this frame.
fn get_dirty_rect_count(
    duplication: &IDXGIOutputDuplication,
    frame_info: &DXGI_OUTDUPL_FRAME_INFO,
    buf: &mut Vec<RECT>,
) -> usize {
    let meta_size = frame_info.TotalMetadataBufferSize as usize;
    if meta_size == 0 {
        return 0;
    }

    let rect_capacity = meta_size / std::mem::size_of::<RECT>();
    buf.resize(rect_capacity.max(1), RECT::default());

    let mut actual_size = 0u32;
    match unsafe {
        duplication.GetFrameDirtyRects(
            (buf.len() * std::mem::size_of::<RECT>()) as u32,
            buf.as_mut_ptr(),
            &mut actual_size,
        )
    } {
        Ok(()) => actual_size as usize / std::mem::size_of::<RECT>(),
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn out(name: &str, left: i32, top: i32) -> OutputInfo {
        OutputInfo {
            adapter_index: 0,
            output_index: 0,
            device_name: name.to_string(),
            width: 1920,
            height: 1080,
            left,
            top,
            is_primary: left == 0 && top == 0,
            adapter_name: "Test GPU".to_string(),
        }
    }

    #[test]
    fn primary_auto_picks_origin_output() {
        let outputs = vec![out(r"\\.\DISPLAY2", 2560, 0), out(r"\\.\DISPLAY1", 0, 0)];
        let idx = resolve_target(&outputs, &CaptureTarget::PrimaryAuto).unwrap();
        assert_eq!(outputs[idx].device_name, r"\\.\DISPLAY1");
    }

    #[test]
    fn known_output_is_selected() {
        let outputs = vec![out(r"\\.\DISPLAY1", 0, 0), out(r"\\.\DISPLAY3", 2560, 0)];
        let target = CaptureTarget::Output(r"\\.\DISPLAY3".to_string());
        let idx = resolve_target(&outputs, &target).unwrap();
        assert_eq!(outputs[idx].device_name, r"\\.\DISPLAY3");
    }

    #[test]
    fn unknown_output_falls_back_to_primary() {
        let outputs = vec![out(r"\\.\DISPLAY1", 0, 0), out(r"\\.\DISPLAY2", 2560, 0)];
        let target = CaptureTarget::Output(r"\\.\DISPLAY9".to_string());
        let idx = resolve_target(&outputs, &target).unwrap();
        assert!(outputs[idx].is_primary);
    }

    #[test]
    fn empty_outputs_returns_none() {
        assert!(resolve_target(&[], &CaptureTarget::PrimaryAuto).is_none());
    }
}
