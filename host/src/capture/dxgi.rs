//! Windows capture backend: DXGI Desktop Duplication, cursor compositing,
//! and display-output enumeration. Everything Win32 in the capture stage
//! lives here; the portable loop policy (pacing, target resolution, virtual
//! display reconciliation) stays in the parent module.

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::{debug, error, info, warn};
use windows::core::Interface;

use super::{
    client_connected, frame_budget_for, heartbeat, reconcile_virtual_display, resolve_target,
    FrameSlot, OutputInfo, RawFrame, ACQUIRE_TIMEOUT_MS, FPS_WINDOW, IDLE_KEEPALIVE,
};
use crate::control::SharedControl;
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
    CreateDXGIFactory1, IDXGIFactory1, IDXGIOutput, IDXGIOutput1, IDXGIOutputDuplication,
    IDXGIResource, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT, DXGI_OUTDUPL_FRAME_INFO,
    DXGI_OUTDUPL_POINTER_SHAPE_INFO,
};

/// Parse a null-terminated UTF-16 fixed array (DXGI `DeviceName` / `Description`) to a String.
fn utf16_to_string(buf: &[u16]) -> String {
    String::from_utf16_lossy(
        &buf.iter()
            .copied()
            .take_while(|&c| c != 0)
            .collect::<Vec<_>>(),
    )
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

/// Enumerate adapters, select the output to duplicate, create duplication,
/// and pull frames at the target fps. Runs on a blocking thread — never call from async context.
pub fn run_capture_loop(
    slot: &FrameSlot,
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
    // managed virtual output only exists while we're capturing it (and only while an iPad
    // is connected).
    let target = reconcile_virtual_display(&requested_target, &shared);
    let outputs = super::enumerate_outputs();
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
            let label = format!(
                "{} ({}x{})",
                chosen.device_name, chosen.width, chosen.height
            );
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
    // These are `mut` because a mid-stream resolution change surfaces as ACCESS_LOST; the error
    // handler re-reads the geometry and rebuilds the staging texture / CPU buffer below.
    let dupl_desc = unsafe { duplication.GetDesc() };
    let mut tex_width = dupl_desc.ModeDesc.Width;
    let mut tex_height = dupl_desc.ModeDesc.Height;
    info!(
        width = tex_width,
        height = tex_height,
        "Desktop duplication dimensions"
    );
    if tex_width == 0 || tex_height == 0 {
        return Err(format!(
            "Desktop duplication reported invalid dimensions {tex_width}x{tex_height}"
        )
        .into());
    }

    // --- Create staging texture for CPU readback (reused every frame) ---
    let mut staging_texture: ID3D11Texture2D = unsafe {
        let mut tex = None;
        device.CreateTexture2D(
            &make_staging_desc(tex_width, tex_height),
            None,
            Some(&mut tex),
        )?;
        tex.expect("CreateTexture2D returned None")
    };
    info!("Staging texture created for CPU readback");

    let mut row_bytes = (tex_width * 4) as usize;
    let mut frame_bytes = row_bytes * tex_height as usize;
    // Buffer recycling (see synthetic.rs): steady state re-uses one buffer.
    let mut spare: Option<Arc<Vec<u8>>> = None;
    // The last published frame, shared for keepalive resends without a copy.
    // Starts as a black frame so a static screen still yields a startup IDR.
    let mut last_frame: Arc<Vec<u8>> = Arc::new(vec![0u8; frame_bytes]);

    // --- Capture loop state ---
    let mut frame_number: u64 = 0;
    let mut frame_timestamps: VecDeque<Instant> = VecDeque::with_capacity(FPS_WINDOW + 1);
    let mut dirty_rect_buf: Vec<RECT> = Vec::with_capacity(64);
    // Time of the last frame handed downstream — drives the idle keepalive (T1d).
    let mut last_send = Instant::now();

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
        heartbeat(&shared.hb_capture_loop_ms);

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

                debug!(
                    frame = frame_number,
                    dirty_rects = dirty_count,
                    acquire_us = acquire_us,
                    fps = format!("{rolling_fps:.1}"),
                    "Frame acquired"
                );

                // --- Copy desktop texture to staging and read pixels into a
                // recycled buffer (the readback is the ONLY full-frame copy) ---
                let frame_data: Option<Arc<Vec<u8>>> = if let Some(ref resource) = desktop_resource
                {
                    match resource.cast::<ID3D11Texture2D>() {
                        Ok(desktop_texture) => {
                            unsafe {
                                device_context.CopyResource(&staging_texture, &desktop_texture);
                            }

                            // Release acquired frame — staging texture has its own copy
                            unsafe {
                                duplication.ReleaseFrame()?;
                            }

                            let mut pixel_buf =
                                match spare.take().and_then(|arc| Arc::try_unwrap(arc).ok()) {
                                    Some(buf) if buf.len() == frame_bytes => buf,
                                    _ => vec![0u8; frame_bytes],
                                };

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

                            Some(Arc::new(pixel_buf))
                        }
                        Err(e) => {
                            warn!(error = %e, "Failed to cast IDXGIResource to ID3D11Texture2D");
                            unsafe {
                                duplication.ReleaseFrame()?;
                            }
                            None
                        }
                    }
                } else {
                    unsafe {
                        duplication.ReleaseFrame()?;
                    }
                    None
                };

                if let Some(data) = frame_data {
                    // Record stats
                    PIPELINE_STATS.lock().record_capture(tex_width, tex_height);
                    heartbeat(&shared.hb_capture_frame_ms);

                    slot.publish(RawFrame {
                        frame_number,
                        timestamp: now,
                        data: Arc::clone(&data),
                        width: tex_width,
                        height: tex_height,
                    });
                    // The displaced spare (if any) plus last_frame keep at most
                    // two buffers alive; steady state recycles them.
                    spare = Some(Arc::clone(&last_frame));
                    last_frame = data;
                    last_send = now;
                }
            }
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                // Desktop unchanged — not an error.
                if !shared.running.load(Ordering::SeqCst) {
                    info!("Capture loop stopping after timeout on running=false");
                    break;
                }
                // Idle keepalive: a static or blank (freshly-enabled) display produces no new
                // frames, so the encoder would never see a frame to turn into the startup IDR and
                // the iPad would sit on a black screen until it times out. While a client is
                // connected, resend the last captured image so the encoder emits the pending IDR
                // and the stream stays alive. pixel_buf holds the last composited frame (zeroed
                // black before the first real frame, which is still a valid keyframe).
                if client_connected(&shared) && last_send.elapsed() >= IDLE_KEEPALIVE {
                    frame_number += 1;
                    let now = Instant::now();
                    PIPELINE_STATS.lock().record_capture(tex_width, tex_height);
                    heartbeat(&shared.hb_capture_frame_ms);
                    slot.publish(RawFrame {
                        frame_number,
                        timestamp: now,
                        data: Arc::clone(&last_frame),
                        width: tex_width,
                        height: tex_height,
                    });
                    last_send = now;
                }
                continue;
            }
            Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                warn!("Desktop duplication access lost — reinitializing");
                // Transient E_ACCESSDENIED is normal during UAC prompts,
                // Ctrl+Alt+Del, fullscreen transitions, and driver resets:
                // retry with backoff instead of dying on the first failure.
                let mut reinit = None;
                let delays_ms = [100u64, 250, 500, 1000, 2000];
                for attempt in 0..15 {
                    if !shared.running.load(Ordering::SeqCst) {
                        break;
                    }
                    // Keep the watchdog quiet: this wait IS the recovery.
                    heartbeat(&shared.hb_capture_loop_ms);
                    match unsafe { output1.DuplicateOutput(&device) } {
                        Ok(d) => {
                            reinit = Some(d);
                            break;
                        }
                        Err(retry_error) => {
                            let delay = delays_ms[attempt.min(delays_ms.len() - 1)];
                            debug!(
                                attempt,
                                error = %retry_error,
                                delay_ms = delay,
                                "DuplicateOutput retry failed"
                            );
                            std::thread::sleep(Duration::from_millis(delay));
                        }
                    }
                }
                let Some(new_duplication) = reinit else {
                    return Err(
                        "desktop duplication could not be reacquired after ~20s of retries".into(),
                    );
                };
                duplication = new_duplication;
                // A resolution/topology change also surfaces as ACCESS_LOST. Re-read the
                // duplication geometry and rebuild the staging texture + CPU buffer if it changed,
                // otherwise CopyResource silently no-ops on a size mismatch and the iPad freezes.
                let new_desc = unsafe { duplication.GetDesc() };
                let (new_w, new_h) = (new_desc.ModeDesc.Width, new_desc.ModeDesc.Height);
                if new_w != 0 && new_h != 0 && (new_w != tex_width || new_h != tex_height) {
                    info!(
                        old_width = tex_width,
                        old_height = tex_height,
                        new_width = new_w,
                        new_height = new_h,
                        "Capture resolution changed — rebuilding staging resources"
                    );
                    tex_width = new_w;
                    tex_height = new_h;
                    row_bytes = (tex_width * 4) as usize;
                    frame_bytes = row_bytes * tex_height as usize;
                    spare = None;
                    last_frame = Arc::new(vec![0u8; frame_bytes]);
                    staging_texture = unsafe {
                        let mut tex = None;
                        device.CreateTexture2D(
                            &make_staging_desc(tex_width, tex_height),
                            None,
                            Some(&mut tex),
                        )?;
                        tex.expect("CreateTexture2D returned None")
                    };
                }
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

/// Build the BGRA staging-texture descriptor for CPU readback at the given dimensions. Factored
/// out so the capture loop can recreate it after a mid-stream resolution change.
fn make_staging_desc(width: u32, height: u32) -> D3D11_TEXTURE2D_DESC {
    D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
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
    }
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
