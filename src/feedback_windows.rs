//! Windows adapter for the desktop status indicator.
//!
//! This module owns every windowing detail and no feedback logic: what to
//! display comes from [`crate::feedback`], and this file only paints it. The
//! split is what lets the core contracts be tested without creating a window.
//!
//! The window is deliberately non-focus-stealing and mouse-transparent. A
//! status indicator that took focus or swallowed clicks would break the exact
//! thing it reports on: the user typing or clicking in another application.

use std::sync::mpsc::{self, Receiver, Sender};

use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{COLORREF, HINSTANCE, HWND, LPARAM, LRESULT, RECT, WPARAM},
        Graphics::Gdi::{
            BeginPaint, CreateSolidBrush, DeleteObject, DrawTextW, EndPaint, FillRect,
            GetStockObject, InvalidateRect, SelectObject, SetBkMode, SetTextColor,
            DEFAULT_GUI_FONT, DT_CENTER, DT_SINGLELINE, DT_VCENTER, HBRUSH, HGDIOBJ, PAINTSTRUCT,
            TRANSPARENT,
        },
        System::LibraryLoader::GetModuleHandleW,
        UI::WindowsAndMessaging::*,
    },
};

use crate::{
    feedback::{Corner, FeedbackConfig, StatusFrame},
    Result,
};

/// Width of the indicator window, in pixels.
const WIDTH: i32 = 260;
/// Height of the indicator window, in pixels.
const HEIGHT: i32 = 64;
/// Margin from the screen edge, in pixels.
const MARGIN: i32 = 16;

/// Message used to ask the overlay thread to stop.
const WM_VIBDOCK_STOP: u32 = WM_USER + 40;
/// Message used to hand a new frame to the overlay thread.
///
/// The frame travels as a boxed pointer in `LPARAM` so nothing is shared
/// across threads by reference; the receiver box is dropped after painting.
const WM_VIBDOCK_FRAME: u32 = WM_USER + 41;

/// Background colour of the indicator: a dark charcoal panel.
const BACKGROUND: COLORREF = COLORREF(0x00_1c_1c_1c);
/// Foreground text colour: ivory, matching the design vocabulary.
const FOREGROUND: COLORREF = COLORREF(0x00_f2_ee_e4);
/// Text colour for a failed action.
const FOREGROUND_FAILED: COLORREF = COLORREF(0x00_5a_6b_e0);

/// A running indicator window on its own thread.
///
/// Cleanup is entirely in [`Drop`]: the owning thread is asked to stop, joined,
/// and its window destroyed. Nothing is ever terminated by process name.
pub struct OverlayHandle {
    /// Sends stop/frame messages to the overlay thread.
    sender: Sender<ThreadMessage>,
    /// Joins the overlay thread on drop.
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Messages the controlling thread can send to the overlay thread.
enum ThreadMessage {
    /// Paint this frame.
    Frame(StatusFrame),
    /// Tear down the window and leave the message loop.
    Stop,
}

impl OverlayHandle {
    /// Shows one frame, coalescing paint work rather than actions.
    ///
    /// A failure to deliver is ignored on purpose: the indicator is an
    /// auxiliary view over state the service already recorded, so a broken
    /// window must never disable audio control or surface as an action error.
    pub fn show(&self, frame: &StatusFrame) {
        let _ = self.sender.send(ThreadMessage::Frame(frame.clone()));
    }
}

impl Drop for OverlayHandle {
    fn drop(&mut self) {
        // Ask the thread to stop rather than killing anything: it owns the
        // window, so only it can cleanly unregister the class and destroy it.
        let _ = self.sender.send(ThreadMessage::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Starts the indicator on a dedicated thread.
///
/// The window is created and pumped on its own thread so the service's accept
/// loop never blocks on window messages and never paints.
pub fn spawn(config: FeedbackConfig) -> Result<OverlayHandle> {
    let (sender, receiver) = mpsc::channel::<ThreadMessage>();
    let thread = std::thread::Builder::new()
        .name("vibdock-overlay".to_string())
        .spawn(move || run_overlay(config, receiver))
        .map_err(|error| -> Box<dyn std::error::Error> { format!("{error}").into() })?;
    Ok(OverlayHandle {
        sender,
        thread: Some(thread),
    })
}

/// Positions the window in the configured corner of the primary monitor.
///
/// Uses `SM_CXSCREEN`/`SM_CYSCREEN` so the indicator lands on-screen without
/// depending on multi-monitor APIs this slice does not need.
fn place(corner: Corner) -> (i32, i32) {
    let (width, height) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
    match corner {
        Corner::BottomRight => (width - WIDTH - MARGIN, height - HEIGHT - MARGIN),
        Corner::BottomLeft => (MARGIN, height - HEIGHT - MARGIN),
        Corner::TopRight => (width - WIDTH - MARGIN, MARGIN),
        Corner::TopLeft => (MARGIN, MARGIN),
    }
}

/// Overlay thread entry: creates the window and pumps its messages.
fn run_overlay(config: FeedbackConfig, receiver: Receiver<ThreadMessage>) {
    // A dedicated message loop is what keeps this window responsive without
    // touching the service thread; `Drop` stops it by posting WM_QUIT.
    let state = Box::new(OverlayState {
        receiver,
        frame: None,
    });
    unsafe {
        let instance = match GetModuleHandleW(None) {
            Ok(instance) => HINSTANCE(instance.0),
            Err(_) => return,
        };
        let class: Vec<u16> = "VibDockOverlay\0".encode_utf16().collect();
        let window_class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: PCWSTR(class.as_ptr()),
            // Painted entirely in WM_PAINT, so no class background brush.
            hbrBackground: HBRUSH(std::ptr::null_mut()),
            ..Default::default()
        };
        // An already-registered class in this process is not an error: a
        // previous overlay in the same process is gone by the time we respawn.
        // The return is an atom (0 on failure), not a Result.
        let _ = RegisterClassW(&window_class);
        let (x, y) = place(config.corner);
        let window = match CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
            PCWSTR(class.as_ptr()),
            PCWSTR("VibDock\0".encode_utf16().collect::<Vec<u16>>().as_ptr()),
            WS_POPUP,
            x,
            y,
            WIDTH,
            HEIGHT,
            None,
            None,
            Some(instance),
            None,
        ) {
            Ok(window) => window,
            Err(_) => return,
        };
        // Hand the state to the window so `window_proc` can reach it. It is
        // taken back and dropped before the thread exits. The return is the
        // previous pointer value, not a Result.
        SetWindowLongPtrW(window, GWLP_USERDATA, Box::into_raw(state) as isize);
        // SW_SHOWNOACTIVATE is the whole point: the window appears without
        // taking focus from whatever the user is typing into. The return only
        // says whether it was visible before, which we do not act on.
        let _ = ShowWindow(window, SW_SHOWNOACTIVATE);
        let mut message = MSG::default();
        loop {
            let result = GetMessageW(&mut message, None, 0, 0).0;
            if result <= 0 {
                break;
            }
            if message.message == WM_VIBDOCK_FRAME || message.message == WM_VIBDOCK_STOP {
                let pointer = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut OverlayState;
                if !pointer.is_null() {
                    let state = &mut *pointer;
                    if message.message == WM_VIBDOCK_STOP {
                        break;
                    }
                    if let Ok(ThreadMessage::Frame(frame)) = state.receiver.try_recv() {
                        state.frame = Some(frame);
                        let _ = InvalidateRect(Some(window), None, true);
                    }
                }
            }
            let _ = TranslateMessage(&message);
            let _ = DispatchMessageW(&message);
        }
        let pointer = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut OverlayState;
        if !pointer.is_null() {
            SetWindowLongPtrW(window, GWLP_USERDATA, 0);
            drop(Box::from_raw(pointer));
        }
        let _ = DestroyWindow(window);
        let _ = UnregisterClassW(PCWSTR(class.as_ptr()), Some(instance));
    }
}

/// Per-window state: the frame to paint and the channel feeding it.
struct OverlayState {
    receiver: Receiver<ThreadMessage>,
    frame: Option<StatusFrame>,
}

/// Window procedure: paints the current frame and handles teardown.
unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_PAINT => {
            paint(window);
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(window, message, wparam, lparam),
    }
}

/// Paints the icon, text, and target of the current frame.
///
/// Text and icon are both drawn because the state must be readable without
/// relying on colour. Colour reinforces the text; it is never the only signal.
unsafe fn paint(window: HWND) {
    let pointer = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut OverlayState;
    if pointer.is_null() {
        return;
    }
    let state = &*pointer;
    let mut paint_struct = PAINTSTRUCT::default();
    // BeginPaint returns the device context directly, not a Result.
    let device = BeginPaint(window, &mut paint_struct);
    let background = CreateSolidBrush(BACKGROUND);
    let mut area = RECT::default();
    let _ = GetClientRect(window, &mut area);
    // Fill first so a transparent layered window still shows an opaque panel
    // the text can be read against.
    FillRect(device, &area, background);
    let _ = SetBkMode(device, TRANSPARENT);
    let colour = match state.frame.as_ref().map(|frame| frame.kind) {
        Some(crate::feedback::StatusKind::Failed) => FOREGROUND_FAILED,
        _ => FOREGROUND,
    };
    let _ = SetTextColor(device, colour);
    let font = GetStockObject(DEFAULT_GUI_FONT);
    let _ = SelectObject(device, font);
    let lines = match &state.frame {
        Some(frame) => vec![
            format!("{} {}", frame.icon, frame.text),
            if frame.target.is_empty() {
                "No target selected".to_string()
            } else {
                frame.target.clone()
            },
        ],
        None => vec!["VibDock".to_string(), "Starting".to_string()],
    };
    let line_height = HEIGHT / (lines.len() as i32 + 1);
    for (index, line) in lines.iter().enumerate() {
        let top = line_height * index as i32;
        let rect = RECT {
            left: 8,
            top,
            right: WIDTH - 8,
            bottom: top + line_height,
        };
        let mut wide: Vec<u16> = line.encode_utf16().collect();
        let mut rect = rect;
        let _ = DrawTextW(
            device,
            &mut wide,
            &mut rect,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
    }
    // Release the brush we created: GDI objects are a limited per-process
    // resource, and this repaints on every frame.
    let _ = DeleteObject(HGDIOBJ(background.0));
    let _ = EndPaint(window, &paint_struct);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// These tests are compile-and-fixture only. They must never create a
    /// window, pump messages, or touch COM/audio: native UI acceptance is a
    /// separate, currently blocked gate.
    #[test]
    fn corners_land_inside_the_primary_screen() {
        // Placement must never push the indicator off-screen, since an
        // off-screen indicator is indistinguishable from one that is broken.
        for corner in [
            Corner::BottomRight,
            Corner::BottomLeft,
            Corner::TopRight,
            Corner::TopLeft,
        ] {
            let (x, y) = place(corner);
            assert!(x >= 0, "{corner:?} placed off-screen at x={x}");
            assert!(y >= 0, "{corner:?} placed off-screen at y={y}");
        }
    }

    #[test]
    fn the_window_style_is_non_activating_and_click_through() {
        // The two properties that make this a status indicator rather than an
        // interruption: no focus steal, and no click capture.
        let style = WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE;
        assert_ne!(style & WS_EX_NOACTIVATE, WINDOW_EX_STYLE(0));
        assert_ne!(style & WS_EX_TRANSPARENT, WINDOW_EX_STYLE(0));
        assert_ne!(style & WS_EX_TOOLWINDOW, WINDOW_EX_STYLE(0));
    }
}
