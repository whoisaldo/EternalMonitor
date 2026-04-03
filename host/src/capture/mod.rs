use std::time::{Duration, Instant};

use tracing::{debug, error, info, warn};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_UNKNOWN;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::Dxgi::{
    CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput, IDXGIOutput1,
    IDXGIOutputDuplication, DXGI_ERROR_ACCESS_LOST, DXGI_ERROR_WAIT_TIMEOUT,
    DXGI_OUTDUPL_FRAME_INFO,
};

const TARGET_FPS: u64 = 60;
const FRAME_BUDGET: Duration = Duration::from_micros(1_000_000 / TARGET_FPS);
const ACQUIRE_TIMEOUT_MS: u32 = 16; // slightly over one frame period

/// Run the DXGI Desktop Duplication capture loop on the current thread (blocking).
/// Call this from `tokio::task::spawn_blocking`.
pub fn run_capture_loop() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    // --- Create DXGI factory and enumerate adapters ---
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };

    let adapter: IDXGIAdapter1 = unsafe { factory.EnumAdapters1(0)? };
    let adapter_desc = unsafe { adapter.GetDesc1()? };
    let adapter_name = String::from_utf16_lossy(
        &adapter_desc
            .Description
            .iter()
            .copied()
            .take_while(|&c| c != 0)
            .collect::<Vec<_>>(),
    );
    info!(adapter = %adapter_name, "Selected GPU adapter");

    // --- Select primary output ---
    let output: IDXGIOutput = unsafe { adapter.EnumOutputs(0)? };
    let output_desc = unsafe { output.GetDesc()? };
    let monitor_name = String::from_utf16_lossy(
        &output_desc
            .DeviceName
            .iter()
            .copied()
            .take_while(|&c| c != 0)
            .collect::<Vec<_>>(),
    );
    info!(output = %monitor_name, "Selected display output");

    // --- Create D3D11 device ---
    let mut device: Option<ID3D11Device> = None;
    unsafe {
        D3D11CreateDevice(
            &adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            None,
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None, // default feature levels
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            None,
        )?;
    }
    let device = device.expect("D3D11CreateDevice succeeded but returned no device");
    info!("D3D11 device created");

    // --- Duplicate the output ---
    let output1: IDXGIOutput1 = output.cast()?;
    let mut duplication: IDXGIOutputDuplication = unsafe { output1.DuplicateOutput(&device)? };
    info!("Desktop duplication active");

    // --- Capture loop ---
    let mut frame_number: u64 = 0;
    let mut frames_dropped: u64 = 0;
    let loop_start = Instant::now();

    loop {
        let frame_start = Instant::now();

        let mut frame_info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut desktop_resource = None;

        let acquire_start = Instant::now();
        let hr = unsafe {
            duplication.AcquireNextFrame(ACQUIRE_TIMEOUT_MS, &mut frame_info, &mut desktop_resource)
        };
        let acquire_us = acquire_start.elapsed().as_micros();

        match hr {
            Ok(()) => {
                frame_number += 1;
                let dirty_rects = frame_info.TotalMetadataBufferSize;

                debug!(
                    frame = frame_number,
                    acquire_latency_us = acquire_us,
                    dirty_meta_bytes = dirty_rects,
                    accumulated_frames = frame_info.AccumulatedFrames,
                    "Frame acquired"
                );

                // Log summary every 60 frames (~1s)
                if frame_number % 60 == 0 {
                    let elapsed = loop_start.elapsed().as_secs_f64();
                    let avg_fps = frame_number as f64 / elapsed;
                    info!(
                        frame = frame_number,
                        avg_fps = format!("{avg_fps:.1}"),
                        dropped = frames_dropped,
                        acquire_us = acquire_us,
                        "Capture stats"
                    );
                }

                // Release the frame back to DXGI
                unsafe {
                    duplication.ReleaseFrame()?;
                }
            }
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {
                // No new frame within timeout — desktop unchanged. Not an error.
                debug!("No new frame (timeout)");
                continue;
            }
            Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                warn!("Desktop duplication access lost — re-duplicating");
                // Re-create duplication handle
                duplication = unsafe { output1.DuplicateOutput(&device)? };
                frames_dropped += 1;
                continue;
            }
            Err(e) => {
                error!(error = %e, "AcquireNextFrame failed");
                return Err(e.into());
            }
        }

        // Frame pacing — sleep remainder of budget
        let frame_elapsed = frame_start.elapsed();
        if frame_elapsed < FRAME_BUDGET {
            std::thread::sleep(FRAME_BUDGET - frame_elapsed);
        } else {
            frames_dropped += 1;
            debug!(
                over_budget_us = (frame_elapsed - FRAME_BUDGET).as_micros(),
                "Frame over budget"
            );
        }
    }
}
