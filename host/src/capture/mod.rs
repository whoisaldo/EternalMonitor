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
    DXGI_OUTDUPL_FRAME_INFO, DXGI_OUTDUPL_POINTER_SHAPE_INFO,
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
        // VirtualExtended is reconciled into an Output(name) before resolution; treat any
        // leftover as primary defensively.
        CaptureTarget::PrimaryAuto | CaptureTarget::VirtualExtended => Some(primary),
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

/// How long to wait for the virtual display to attach after enabling its driver.
const VDD_ATTACH_TIMEOUT: Duration = Duration::from_secs(6);
const VDD_POLL_INTERVAL: Duration = Duration::from_millis(120);

/// Reconcile the virtual display device to the requested target and return the concrete
/// target to capture. `VirtualExtended` enables the bundled driver, waits for its output to
/// attach, and returns it as an `Output(name)`; any other target disables the driver so no
/// phantom monitor lingers. Falls back to `PrimaryAuto` if the virtual display can't be
/// brought up.
fn reconcile_virtual_display(target: &CaptureTarget) -> CaptureTarget {
    match target {
        CaptureTarget::VirtualExtended => {
            let before: Vec<String> = enumerate_outputs()
                .into_iter()
                .map(|o| o.device_name)
                .collect();
            if !crate::vdd::enable() {
                warn!(
                    "Virtual display could not be enabled (installer task missing?) — \
                     capturing the primary display instead"
                );
                return CaptureTarget::PrimaryAuto;
            }
            let deadline = Instant::now() + VDD_ATTACH_TIMEOUT;
            while Instant::now() < deadline {
                std::thread::sleep(VDD_POLL_INTERVAL);
                let now = enumerate_outputs();
                let non_primary: Vec<&OutputInfo> =
                    now.iter().filter(|o| !o.is_primary).collect();
                if let Some(chosen) = non_primary
                    .iter()
                    .find(|o| !before.contains(&o.device_name))
                    .or_else(|| non_primary.first())
                {
                    info!(
                        device = %chosen.device_name,
                        width = chosen.width,
                        height = chosen.height,
                        "Virtual display attached — capturing it"
                    );
                    return CaptureTarget::Output(chosen.device_name.clone());
                }
            }
            warn!("Virtual display did not attach in time — capturing the primary display instead");
            CaptureTarget::PrimaryAuto
        }
        other => {
            // Any non-virtual target: ensure the virtual display is off.
            crate::vdd::disable();
            other.clone()
        }
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
    let requested_target = shared.capture_target.lock().clone();
    // Turn the virtual display on/off to match the request before enumerating, so the
    // managed virtual output only exists while we're capturing it.
    let target = reconcile_virtual_display(&requested_target);
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

    // Desktop Duplication never bakes the mouse cursor into the desktop image — it sends
    // the position every frame and the shape only when it changes. Cache the shape and
    // composite it into each frame so the cursor is visible on the iPad.
    let mut cursor = CursorState::default();
    let mut cursor_visible = false;
    let mut cursor_x: i32 = 0;
    let mut cursor_y: i32 = 0;

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

                // --- Track cursor position/shape (must read shape before ReleaseFrame) ---
                if frame_info.LastMouseUpdateTime != 0 {
                    cursor_visible = frame_info.PointerPosition.Visible.as_bool();
                    cursor_x = frame_info.PointerPosition.Position.x;
                    cursor_y = frame_info.PointerPosition.Position.y;
                }
                if frame_info.PointerShapeBufferSize > 0 {
                    cursor.update_shape(&duplication, frame_info.PointerShapeBufferSize);
                }

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

                            // Composite the mouse cursor into the BGRA frame.
                            if cursor_visible && cursor.has_shape {
                                draw_cursor(
                                    &mut pixel_buf,
                                    tex_width,
                                    tex_height,
                                    row_bytes,
                                    &cursor,
                                    cursor_x,
                                    cursor_y,
                                );
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

// DXGI pointer shape types (DXGI_OUTDUPL_POINTER_SHAPE_TYPE_*).
const DXGI_POINTER_SHAPE_MONOCHROME: u32 = 1;
const DXGI_POINTER_SHAPE_COLOR: u32 = 2;
const DXGI_POINTER_SHAPE_MASKED_COLOR: u32 = 4;

/// Caches the latest mouse-cursor shape from DXGI Desktop Duplication. The shape only
/// arrives when it changes, so it's kept across frames and composited at the per-frame
/// pointer position.
#[derive(Default)]
struct CursorState {
    shape: Vec<u8>,
    shape_type: u32,
    width: u32,
    height: u32,
    pitch: u32,
    has_shape: bool,
}

impl CursorState {
    /// Fetch the current pointer shape. Must be called while a frame is acquired
    /// (before `ReleaseFrame`). On failure the previous shape is kept.
    fn update_shape(&mut self, duplication: &IDXGIOutputDuplication, buffer_size: u32) {
        self.shape.resize(buffer_size as usize, 0);
        let mut required = 0u32;
        let mut info = DXGI_OUTDUPL_POINTER_SHAPE_INFO::default();
        match unsafe {
            duplication.GetFramePointerShape(
                buffer_size,
                self.shape.as_mut_ptr() as *mut core::ffi::c_void,
                &mut required,
                &mut info,
            )
        } {
            Ok(()) => {
                self.shape_type = info.Type;
                self.width = info.Width;
                self.height = info.Height;
                self.pitch = info.Pitch;
                self.has_shape = true;
            }
            Err(e) => {
                warn!(error = %e, "GetFramePointerShape failed — cursor shape not updated");
            }
        }
    }
}

/// Composite the cached cursor into a BGRA frame at top-left `(px, py)` (this output's
/// pixel space). Handles the three DXGI pointer shape types; out-of-bounds pixels are
/// clipped.
fn draw_cursor(
    buf: &mut [u8],
    frame_w: u32,
    frame_h: u32,
    row_bytes: usize,
    cursor: &CursorState,
    px: i32,
    py: i32,
) {
    match cursor.shape_type {
        DXGI_POINTER_SHAPE_COLOR => {
            draw_color_cursor(buf, frame_w, frame_h, row_bytes, cursor, px, py, false)
        }
        DXGI_POINTER_SHAPE_MASKED_COLOR => {
            draw_color_cursor(buf, frame_w, frame_h, row_bytes, cursor, px, py, true)
        }
        DXGI_POINTER_SHAPE_MONOCHROME => {
            draw_monochrome_cursor(buf, frame_w, frame_h, row_bytes, cursor, px, py)
        }
        _ => {}
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_color_cursor(
    buf: &mut [u8],
    frame_w: u32,
    frame_h: u32,
    row_bytes: usize,
    cursor: &CursorState,
    px: i32,
    py: i32,
    masked: bool,
) {
    let pitch = cursor.pitch as usize;
    for row in 0..cursor.height {
        let sy = py + row as i32;
        if sy < 0 || sy >= frame_h as i32 {
            continue;
        }
        for col in 0..cursor.width {
            let sx = px + col as i32;
            if sx < 0 || sx >= frame_w as i32 {
                continue;
            }
            let src = row as usize * pitch + col as usize * 4;
            if src + 4 > cursor.shape.len() {
                continue;
            }
            let (b, g, r, a) = (
                cursor.shape[src],
                cursor.shape[src + 1],
                cursor.shape[src + 2],
                cursor.shape[src + 3],
            );
            let dst = sy as usize * row_bytes + sx as usize * 4;
            if dst + 4 > buf.len() {
                continue;
            }
            if masked {
                // MASKED_COLOR: alpha 0 = opaque pixel; alpha 0xFF = XOR with screen.
                if a == 0 {
                    buf[dst] = b;
                    buf[dst + 1] = g;
                    buf[dst + 2] = r;
                } else {
                    buf[dst] ^= b;
                    buf[dst + 1] ^= g;
                    buf[dst + 2] ^= r;
                }
            } else {
                // COLOR: straight alpha blend over the screen.
                let af = a as u32;
                let inv = 255 - af;
                buf[dst] = ((b as u32 * af + buf[dst] as u32 * inv) / 255) as u8;
                buf[dst + 1] = ((g as u32 * af + buf[dst + 1] as u32 * inv) / 255) as u8;
                buf[dst + 2] = ((r as u32 * af + buf[dst + 2] as u32 * inv) / 255) as u8;
            }
        }
    }
}

fn draw_monochrome_cursor(
    buf: &mut [u8],
    frame_w: u32,
    frame_h: u32,
    row_bytes: usize,
    cursor: &CursorState,
    px: i32,
    py: i32,
) {
    // Monochrome shapes pack an AND mask (top half) and an XOR mask (bottom half),
    // 1 bit per pixel. The real cursor height is half the reported buffer height.
    let pitch = cursor.pitch as usize;
    let cur_h = cursor.height / 2;
    for row in 0..cur_h {
        let sy = py + row as i32;
        if sy < 0 || sy >= frame_h as i32 {
            continue;
        }
        for col in 0..cursor.width {
            let sx = px + col as i32;
            if sx < 0 || sx >= frame_w as i32 {
                continue;
            }
            let byte = col as usize / 8;
            let bit = 7u8 - (col % 8) as u8;
            let and_idx = row as usize * pitch + byte;
            let xor_idx = (row + cur_h) as usize * pitch + byte;
            if xor_idx >= cursor.shape.len() {
                continue;
            }
            let and_bit = (cursor.shape[and_idx] >> bit) & 1;
            let xor_bit = (cursor.shape[xor_idx] >> bit) & 1;
            let dst = sy as usize * row_bytes + sx as usize * 4;
            if dst + 4 > buf.len() {
                continue;
            }
            match (and_bit, xor_bit) {
                (0, 0) => {
                    // Opaque black.
                    buf[dst] = 0;
                    buf[dst + 1] = 0;
                    buf[dst + 2] = 0;
                }
                (0, 1) => {
                    // Opaque white.
                    buf[dst] = 255;
                    buf[dst + 1] = 255;
                    buf[dst + 2] = 255;
                }
                (1, 0) => {} // Transparent — leave the screen pixel.
                _ => {
                    // (1, 1): invert the screen pixel.
                    buf[dst] = !buf[dst];
                    buf[dst + 1] = !buf[dst + 1];
                    buf[dst + 2] = !buf[dst + 2];
                }
            }
        }
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
