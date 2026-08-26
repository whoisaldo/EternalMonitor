//! Windows `SendInput` backend for the input relay.

use tracing::debug;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL,
    MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK, MOUSEEVENTF_WHEEL, MOUSEINPUT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetSystemMetrics, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use super::{Injection, VirtualScreen};

/// The Windows virtual screen (all monitors' bounding box) for absolute
/// pointer mapping. Queried per batch — display topology can change under us.
pub fn virtual_screen() -> VirtualScreen {
    unsafe {
        VirtualScreen {
            left: GetSystemMetrics(SM_XVIRTUALSCREEN),
            top: GetSystemMetrics(SM_YVIRTUALSCREEN),
            width: GetSystemMetrics(SM_CXVIRTUALSCREEN).max(1) as u32,
            height: GetSystemMetrics(SM_CYVIRTUALSCREEN).max(1) as u32,
        }
    }
}

fn mouse_input(
    flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS,
    dx: i32,
    dy: i32,
    data: i32,
) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx,
                dy,
                mouseData: data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    }
}

/// Execute a resolved injection batch.
pub fn inject(injections: &[Injection]) {
    let mut inputs: Vec<INPUT> = Vec::with_capacity(injections.len());
    for injection in injections {
        let input = match *injection {
            Injection::MoveAbs { x, y } => mouse_input(
                MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                i32::from(x),
                i32::from(y),
                0,
            ),
            Injection::LeftDown { x, y } => mouse_input(
                MOUSEEVENTF_LEFTDOWN | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                i32::from(x),
                i32::from(y),
                0,
            ),
            Injection::LeftUp { x, y } => mouse_input(
                MOUSEEVENTF_LEFTUP | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                i32::from(x),
                i32::from(y),
                0,
            ),
            Injection::RightDown { x, y } => mouse_input(
                MOUSEEVENTF_RIGHTDOWN | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                i32::from(x),
                i32::from(y),
                0,
            ),
            Injection::RightUp { x, y } => mouse_input(
                MOUSEEVENTF_RIGHTUP | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                i32::from(x),
                i32::from(y),
                0,
            ),
            Injection::Wheel { delta } => mouse_input(MOUSEEVENTF_WHEEL, 0, 0, delta),
            Injection::HWheel { delta } => mouse_input(MOUSEEVENTF_HWHEEL, 0, 0, delta),
        };
        inputs.push(input);
    }
    if inputs.is_empty() {
        return;
    }
    let sent = unsafe { SendInput(&inputs, std::mem::size_of::<INPUT>() as i32) };
    if sent != inputs.len() as u32 {
        debug!(
            sent,
            requested = inputs.len(),
            "SendInput injected fewer events than requested"
        );
    }
}
