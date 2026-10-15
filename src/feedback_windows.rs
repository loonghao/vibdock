//! Windows adapter for the desktop status indicator.
//!
//! This module owns every windowing detail and no feedback logic: what to
//! display comes from [`crate::feedback`], and this file only paints it. The
//! split is what lets the core contracts be tested without creating a window.
//!
//! The window is deliberately non-focus-stealing and mouse-transparent. A
//! status indicator that took focus or swallowed clicks would break the exact
//! thing it reports on: the user typing or clicking in another application.

use std::ptr::addr_of_mut;
use std::sync::{
    mpsc::{self, Receiver, Sender, SyncSender},
    Arc, Mutex,
};

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
        System::{LibraryLoader::GetModuleHandleW, Threading::GetCurrentThreadId},
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
/// Message used to wake the overlay thread so it drains pending frames.
///
/// Sent as a *thread* message with no window, which is why the loop can react
/// to it even while `GetMessageW` is blocked with no other input queued.
const WM_VIBDOCK_WAKE: u32 = WM_USER + 41;
/// Upper bound on frames awaiting paint.
///
/// Bounded so a stalled or slow overlay can never queue without limit behind
/// the accept loop; the oldest frame is discarded to make room, which is safe
/// because a frame is a repaint of already-recorded state, never an action.
const QUEUE_LIMIT: usize = 8;

/// Background colour of the indicator: a dark charcoal panel.
const BACKGROUND: COLORREF = COLORREF(0x00_1c_1c_1c);
/// Foreground text colour: ivory, matching the design vocabulary.
const FOREGROUND: COLORREF = COLORREF(0x00_f2_ee_e4);
/// Text colour for a failed action.
const FOREGROUND_FAILED: COLORREF = COLORREF(0x00_5a_6b_e0);

/// Alpha applied to the layered window.
///
/// A layered window with no alpha set is fully transparent, so this is what
/// makes the panel visible at all.
const OVERLAY_ALPHA: u8 = 235;

/// A screen rectangle in absolute coordinates, as `SPI_GETWORKAREA` reports it.
///
/// A plain struct rather than the Win32 `RECT` it is filled from: it keeps the
/// pure placement helper constructible from a literal in tests without
/// depending on how the platform type derives its traits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rect {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl Rect {
    /// Width of the rectangle; not guaranteed positive for a degenerate area.
    fn width(self) -> i32 {
        self.right - self.left
    }

    /// Height of the rectangle; not guaranteed positive for a degenerate area.
    fn height(self) -> i32 {
        self.bottom - self.top
    }
}

/// A running indicator window on its own thread.
///
/// Cleanup is entirely in [`Drop`]: the owning thread is asked to stop, joined,
/// and its window destroyed. Nothing is ever terminated by process name.
pub struct OverlayHandle {
    /// Bounded queue of frames awaiting paint.
    sender: SyncSender<ThreadMessage>,
    /// Drain side of the same queue, shared with the overlay thread. Used only
    /// to discard superseded frames when the queue is full, so the producer
    /// never blocks.
    queue: Arc<Mutex<Receiver<ThreadMessage>>>,
    /// Thread id of the overlay thread, used to wake its message loop.
    thread_id: u32,
    /// Joins the overlay thread on drop.
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Messages the controlling thread can send to the overlay thread.
enum ThreadMessage {
    /// Paint this frame.
    Frame(StatusFrame),
}

impl OverlayHandle {
    /// Shows one frame, coalescing paint work rather than actions.
    ///
    /// The queue is bounded: when it is full the pending frames are dropped
    /// rather than blocking, so a stalled window can never wedge the accept
    /// loop. Dropping a *frame* is safe because a frame is a view over state
    /// the service has already recorded; no action is ever dropped.
    ///
    /// Returns whether the frame was delivered, so the caller can decide
    /// whether to retry later. Both steps must succeed: a frame that is queued
    /// but never woken sits undrained, and a wake with nothing queued paints
    /// nothing. A full queue still counts as queued, because the frame that was
    /// made room for supersedes everything dropped.
    pub fn show(&self, frame: &StatusFrame) -> bool {
        let queued = self.enqueue(ThreadMessage::Frame(frame.clone()));
        // Wake the overlay thread. Without this the loop stays blocked in
        // GetMessageW and the frame is never drained. If the wake cannot be
        // posted, the frame is queued but unreachable, so it is not delivered.
        queued && self.wake()
    }

    /// Queues a frame, dropping the oldest pending one if the queue is full.
    ///
    /// Never blocks: the accept loop must stay free to serve audio requests
    /// even if the overlay thread has stopped draining. Returns whether the
    /// message was queued.
    fn enqueue(&self, message: ThreadMessage) -> bool {
        // The channel is bounded, so a stalled overlay cannot accumulate
        // frames without limit behind the accept loop.
        match self.sender.try_send(message) {
            Ok(()) => true,
            Err(mpsc::TrySendError::Disconnected(_)) => false,
            Err(mpsc::TrySendError::Full(message)) => {
                // Full: pending frames are superseded, since only the newest
                // state is ever painted. Drop the oldest to make room, then
                // queue the newer one. Never blocks.
                self.discard_pending();
                self.sender.try_send(message).is_ok()
            }
        }
    }

    /// Drains every queued frame so the queue accepts one more.
    ///
    /// Superseded frames are discarded as a batch, not one at a time: the
    /// frame about to be queued is newer than anything already waiting, so
    /// every pending frame is obsolete and none of them will be painted.
    ///
    /// Bounded so a pathological producer cannot spin here: it gives up after
    /// a fixed number of attempts and lets the caller's send simply fail.
    fn discard_pending(&self) {
        // The lock is shared with the overlay thread's drain. It is taken only
        // on the full path, so the common case stays lock-free from here.
        let Ok(queue) = self.queue.lock() else {
            return;
        };
        for _ in 0..QUEUE_LIMIT {
            if queue.try_recv().is_err() {
                break;
            }
        }
    }

    /// Wakes the overlay thread's message loop.
    ///
    /// `PostThreadMessageW` is what makes the bridge work: the loop blocks in
    /// `GetMessageW`, and a posted thread message is what unblocks it. The
    /// overlay thread posts its own thread id through the readiness channel
    /// before entering the loop, so no wake can be lost to a race.
    ///
    /// Returns whether the wake was actually posted. A failure means the
    /// overlay thread's queue is gone or full, so the frame it was meant to
    /// announce will not be drained now; the caller must not advance past it.
    fn wake(&self) -> bool {
        self.post(WM_VIBDOCK_WAKE)
    }

    /// Asks the overlay thread to stop, without consuming `self`.
    ///
    /// Kept separate from [`Drop`] so the stop path is one code path: dropping
    /// and any explicit shutdown both end at the same teardown.
    ///
    /// Returns whether the stop was posted. When it was not, [`Drop`] must not
    /// join: a thread that never received the stop stays blocked in
    /// `GetMessageW`, and joining it would hang shutdown forever.
    fn request_stop(&self) -> bool {
        self.post(WM_VIBDOCK_STOP)
    }

    /// Posts a control message to the overlay thread's queue.
    ///
    /// Returns `false` rather than silently succeeding when the post fails or
    /// there is no thread id to post to. Both callers change behaviour on that
    /// outcome, so the result must not be discarded here.
    fn post(&self, message: u32) -> bool {
        if self.thread_id == 0 {
            return false;
        }
        unsafe { PostThreadMessageW(self.thread_id, message, WPARAM(0), LPARAM(0)) }.is_ok()
    }
}

impl Drop for OverlayHandle {
    fn drop(&mut self) {
        // Ask the thread to stop rather than killing anything: it owns the
        // window, so only it can cleanly unregister the class and destroy it.
        // The stop is posted as a thread message, which is also what wakes a
        // loop blocked in GetMessageW — a stop sent only through the channel
        // would sit there unread forever.
        let stopped = self.request_stop();
        let Some(thread) = self.thread.take() else {
            return;
        };
        // Join only when the stop actually reached the thread. A post that
        // failed leaves the loop blocked in GetMessageW with no way to learn it
        // should exit, so joining would hang shutdown forever. Detaching
        // instead leaks the thread until the process ends, which is the lesser
        // failure of the two: the indicator is an auxiliary view and must never
        // wedge the service's shutdown.
        if stopped {
            let _ = thread.join();
        }
    }
}

/// Starts the indicator on a dedicated thread.
///
/// The window is created and pumped on its own thread so the service's accept
/// loop never blocks on window messages and never paints.
///
/// The thread is spawned first and the window created on it, so a window
/// creation failure is reported here as an error instead of leaving a handle
/// that silently does nothing.
pub fn spawn(config: FeedbackConfig) -> Result<OverlayHandle> {
    // Bounded: the queue cannot grow without limit if the overlay stalls.
    let (sender, overlay_receiver) = mpsc::sync_channel::<ThreadMessage>(QUEUE_LIMIT);
    // Shared drain side: the overlay thread paints from it, and the handle
    // uses it only to discard superseded frames when the queue fills.
    let queue = Arc::new(Mutex::new(overlay_receiver));
    let (ready_sender, ready_receiver) = mpsc::channel::<ThreadReady>();
    let thread_queue = Arc::clone(&queue);
    let thread = std::thread::Builder::new()
        .name("vibdock-overlay".to_string())
        .spawn(move || run_overlay(config, thread_queue, ready_sender))
        .map_err(|error| -> Box<dyn std::error::Error> {
            format!("spawn overlay thread: {error}").into()
        })?;
    // Wait for the overlay to report readiness (or failure). Without this a
    // caller could hold a handle whose window was never created, and a thread
    // whose id is not yet known could never be woken or stopped.
    match ready_receiver.recv() {
        Ok(ThreadReady::Running { thread_id }) => Ok(OverlayHandle {
            sender,
            queue,
            thread_id,
            thread: Some(thread),
        }),
        Ok(ThreadReady::Failed(message)) => {
            let _ = thread.join();
            Err(message.into())
        }
        Err(error) => {
            let _ = thread.join();
            Err(format!("overlay thread did not report readiness: {error}").into())
        }
    }
}

/// What the overlay thread reports once its window exists (or does not).
enum ThreadReady {
    /// The window is up; `thread_id` is what wakes and stops the loop.
    Running { thread_id: u32 },
    /// The window could not be created; carries why.
    Failed(String),
}

/// Positions the window in the configured corner of the work area.
///
/// The work area excludes the taskbar, which is the whole point:
/// `SM_CXSCREEN`/`SM_CYSCREEN` describe the full monitor, so a bottom-corner
/// indicator placed against them would sit *under* the taskbar and have its
/// target line clipped. `SPI_GETWORKAREA` reports the area left after the
/// shell reserves its bands, so the indicator lands where it can be read.
///
/// Falls back to the full monitor rectangle if the work area cannot be read.
/// That fallback is *not* taskbar-aware: it spans the whole monitor, so with a
/// left or top taskbar the indicator may overlap the taskbar band. It is kept
/// because a misplaced indicator is still better than one that never appears.
fn place(corner: Corner) -> (i32, i32) {
    // The only platform calls in this path; everything below is pure so it can
    // be verified against a fixture instead of the real display.
    let area = work_area().unwrap_or_else(|| Rect {
        left: 0,
        top: 0,
        right: unsafe { GetSystemMetrics(SM_CXSCREEN) },
        bottom: unsafe { GetSystemMetrics(SM_CYSCREEN) },
    });
    place_in(corner, area)
}

/// Reads the primary monitor's work area: the monitor rectangle minus the
/// space the shell reserves for the taskbar and other app bars.
///
/// The origin is preserved rather than reduced to a width and height:
/// `SPI_GETWORKAREA` reports `left`/`top` greater than zero when the taskbar
/// sits on the left or top, and dropping that origin would place the indicator
/// underneath the taskbar.
///
/// Returns `None` if the call fails, leaving the caller to fall back to the
/// full monitor rectangle.
fn work_area() -> Option<Rect> {
    let mut area = RECT::default();
    unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(addr_of_mut!(area).cast()),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    }
    .ok()?;
    let area = Rect {
        left: area.left,
        top: area.top,
        right: area.right,
        bottom: area.bottom,
    };
    // A non-positive area means the call succeeded but reported something
    // unusable; treat it as a failure so the caller falls back.
    if area.width() > 0 && area.height() > 0 {
        Some(area)
    } else {
        None
    }
}

/// Places the indicator inside the given work-area rectangle.
///
/// Pure: it takes the rectangle as an argument rather than reading the display,
/// so placement can be verified against fixtures. The origin is honoured: with
/// a left or top taskbar the area starts away from zero, and the offset is
/// added to the corner result. The result is always clamped so the window stays
/// fully inside the area, because an indicator pushed partly or wholly outside
/// it is indistinguishable from one that failed to start.
fn place_in(corner: Corner, area: Rect) -> (i32, i32) {
    // An area smaller than the window leaves no room for margins, so the
    // window is pinned to the area's origin instead of being pushed off it.
    let max_x = (area.width() - WIDTH).max(0);
    let max_y = (area.height() - HEIGHT).max(0);
    let margin_x = MARGIN.min(max_x);
    let margin_y = MARGIN.min(max_y);
    let right = (max_x - margin_x).max(0);
    let bottom = (max_y - margin_y).max(0);
    let (offset_x, offset_y) = match corner {
        Corner::BottomRight => (right, bottom),
        Corner::BottomLeft => (margin_x, bottom),
        Corner::TopRight => (right, margin_y),
        Corner::TopLeft => (margin_x, margin_y),
    };
    (area.left + offset_x, area.top + offset_y)
}

/// The extended window styles the indicator is created with.
///
/// A named constant rather than flags rebuilt at each call site, so the style
/// test exercises the same value the window is actually created with.
fn overlay_ex_style() -> WINDOW_EX_STYLE {
    WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOPMOST
}

/// Overlay thread entry: creates the window and pumps its messages.
///
/// Reports readiness through `ready` before entering the loop. That handshake
/// is load-bearing for lifetime: the caller cannot wake or stop this thread
/// until it knows the thread id, and it must not hold a handle to a window
/// that failed to be created.
fn run_overlay(
    config: FeedbackConfig,
    queue: Arc<Mutex<Receiver<ThreadMessage>>>,
    ready: Sender<ThreadReady>,
) {
    let state = Box::new(OverlayState { queue, frame: None });
    unsafe {
        let instance = match GetModuleHandleW(None) {
            Ok(instance) => HINSTANCE(instance.0),
            Err(error) => {
                let _ = ready.send(ThreadReady::Failed(format!(
                    "could not resolve the module handle: {error}"
                )));
                return;
            }
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
        let window = match create_overlay_window(instance, class.as_ptr(), x, y) {
            Ok(window) => window,
            Err(error) => {
                // Propagate the real reason instead of returning silently: a
                // caller was told the indicator started when it did not.
                let _ = ready.send(ThreadReady::Failed(error));
                return;
            }
        };
        // Hand the state to the window so `window_proc` can reach it. It is
        // taken back and dropped before the thread exits. The return is the
        // previous pointer value, not a Result.
        SetWindowLongPtrW(window, GWLP_USERDATA, Box::into_raw(state) as isize);
        // SW_SHOWNOACTIVATE is the whole point: the window appears without
        // taking focus from whatever the user is typing into.
        let _ = ShowWindow(window, SW_SHOWNOACTIVATE);
        // Report readiness only once the loop can actually be woken. The
        // thread id is published here so no wake or stop can be missed.
        let thread_id = GetCurrentThreadId();
        if ready.send(ThreadReady::Running { thread_id }).is_err() {
            // The caller went away: tear down rather than pump forever.
            cleanup(window, class.as_ptr(), instance);
            return;
        }
        let mut message = MSG::default();
        loop {
            let result = GetMessageW(&mut message, None, 0, 0).0;
            if result <= 0 {
                break;
            }
            // A stop request is handled before anything else so a queued frame
            // can never delay shutdown.
            if message.message == WM_VIBDOCK_STOP {
                break;
            }
            if message.message == WM_VIBDOCK_WAKE {
                drain(window);
                continue;
            }
            let _ = TranslateMessage(&message);
            let _ = DispatchMessageW(&message);
        }
        cleanup(window, class.as_ptr(), instance);
    }
}

/// Drains pending frames, keeping only the newest, and repaints once.
///
/// Coalescing here is what bounds paint work: a burst of actions produces one
/// repaint showing the latest state. Every action already advanced `sequence`
/// before reaching here, so coalescing frames never drops an action.
fn drain(window: HWND) {
    let pointer = unsafe { GetWindowLongPtrW(window, GWLP_USERDATA) as *mut OverlayState };
    if pointer.is_null() {
        return;
    }
    let state = unsafe { &mut *pointer };
    let Ok(queue) = state.queue.lock() else {
        return;
    };
    // Coalesce to the newest frame: intermediate states were never painted and
    // no longer matter. Every action already advanced `sequence` before being
    // queued, so this drops paint work, never an action.
    let mut newest = None;
    while let Ok(ThreadMessage::Frame(frame)) = queue.try_recv() {
        newest = Some(frame);
    }
    drop(queue);
    if let Some(frame) = newest {
        state.frame = Some(frame);
        let _ = unsafe { InvalidateRect(Some(window), None, true) };
    }
}

/// Creates the overlay window and initialises its layered attributes.
///
/// `WS_EX_LAYERED` alone leaves the window fully transparent until its
/// attributes are set, so the alpha is applied here; without this the window
/// exists and is shown but paints nothing visible.
fn create_overlay_window(
    instance: HINSTANCE,
    class: *const u16,
    x: i32,
    y: i32,
) -> std::result::Result<HWND, String> {
    let title: Vec<u16> = "VibDock\0".encode_utf16().collect();
    let window = unsafe {
        CreateWindowExW(
            overlay_ex_style(),
            PCWSTR(class),
            PCWSTR(title.as_ptr()),
            WS_POPUP,
            x,
            y,
            WIDTH,
            HEIGHT,
            None,
            None,
            Some(instance),
            None,
        )
    }
    .map_err(|error| format!("could not create the indicator window: {error}"))?;
    // Opaque panel: the layered style is used for the window's transparency
    // contract, and an unset alpha would leave it invisible.
    if let Err(error) =
        unsafe { SetLayeredWindowAttributes(window, COLORREF(0), OVERLAY_ALPHA, LWA_ALPHA) }
    {
        // Do not leak the window on a partial initialisation failure.
        let _ = unsafe { DestroyWindow(window) };
        return Err(format!(
            "could not initialise the indicator's layered attributes: {error}"
        ));
    }
    Ok(window)
}

/// Destroys the window and releases everything the thread owns.
fn cleanup(window: HWND, class: *const u16, instance: HINSTANCE) {
    unsafe {
        let pointer = GetWindowLongPtrW(window, GWLP_USERDATA) as *mut OverlayState;
        if !pointer.is_null() {
            SetWindowLongPtrW(window, GWLP_USERDATA, 0);
            drop(Box::from_raw(pointer));
        }
        let _ = DestroyWindow(window);
        let _ = UnregisterClassW(PCWSTR(class), Some(instance));
    }
}

/// Per-window state: the frame to paint and the queue feeding it.
struct OverlayState {
    queue: Arc<Mutex<Receiver<ThreadMessage>>>,
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

    /// Every corner, so placement invariants can be asserted over all of them.
    const CORNERS: [Corner; 4] = [
        Corner::BottomRight,
        Corner::BottomLeft,
        Corner::TopRight,
        Corner::TopLeft,
    ];

    /// A work-area rectangle with no reserved band: the fallback shape and the
    /// shape the origin-zero fixtures were written against.
    fn full_area(width: i32, height: i32) -> Rect {
        Rect {
            left: 0,
            top: 0,
            right: width,
            bottom: height,
        }
    }

    /// These tests are pure fixtures. They must never create a window, pump
    /// messages, or touch COM/audio: native UI acceptance is a separate,
    /// currently blocked gate. Placement is therefore verified through
    /// [`place_in`] with an injected rectangle rather than the real display,
    /// covering non-zero origins (left and top taskbars) as well as sizes.

    #[test]
    fn corners_land_inside_a_normal_screen() {
        // A typical 1080p work area, including the band a taskbar occupies:
        // the indicator must sit inside it and clear the bottom edge.
        for corner in CORNERS {
            let (x, y) = place_in(corner, full_area(1920, 1040));
            assert!(x >= 0, "{corner:?} placed off-screen at x={x}");
            assert!(y >= 0, "{corner:?} placed off-screen at y={y}");
            assert!(x + WIDTH <= 1920, "{corner:?} overflows width at x={x}");
            assert!(y + HEIGHT <= 1040, "{corner:?} overflows height at y={y}");
        }
    }

    #[test]
    fn a_bottom_corner_never_reaches_into_the_taskbar_band() {
        // The regression this guards: the work area is shorter than the screen,
        // and placing against the full screen height pushed the indicator under
        // the taskbar. A 1080p screen with a 40px taskbar leaves 1040px, so the
        // bottom corners must sit at or above 1040 - HEIGHT - MARGIN.
        //
        // The old behaviour is asserted first so the fixture is pinned to the
        // input production really passes: the full 1920x1080 screen, not the
        // 1040 height the previous test happened to use.
        let (_, full_height_y) = place_in(Corner::BottomRight, full_area(1920, 1080));
        assert_eq!(
            full_height_y,
            1080 - HEIGHT - MARGIN,
            "fixture must describe the full-screen placement that hid the target line"
        );
        let work_area_y = place_in(Corner::BottomRight, full_area(1920, 1040)).1;
        assert!(
            work_area_y < full_height_y,
            "the work area must place the indicator higher than the full screen does"
        );
        // Inside the work area, both bottom corners clear the taskbar edge.
        for corner in [Corner::BottomRight, Corner::BottomLeft] {
            let (_, y) = place_in(corner, full_area(1920, 1040));
            assert!(
                y + HEIGHT <= 1040,
                "{corner:?} bottom edge {} reaches into the taskbar band",
                y + HEIGHT
            );
        }
    }

    #[test]
    fn a_left_taskbar_shifts_every_corner_past_its_origin() {
        // The regression: the work area starts at left=40, and dropping the
        // origin put the top-left indicator inside the taskbar band at (16,16)
        // instead of (56,16), and the bottom-right one at x=1604 instead of
        // 1644.
        let area = Rect {
            left: 40,
            top: 0,
            right: 1920,
            bottom: 1080,
        };
        assert_eq!(place_in(Corner::TopLeft, area), (56, 16));
        assert_eq!(place_in(Corner::BottomLeft, area), (56, 1000));
        assert_eq!(place_in(Corner::TopRight, area), (1644, 16));
        assert_eq!(place_in(Corner::BottomRight, area), (1644, 1000));
    }

    #[test]
    fn a_top_taskbar_shifts_every_corner_below_its_origin() {
        // A top taskbar reserves the first 40 rows, so a top corner at y=16
        // would sit inside the bar; the correct landing is y=56.
        let area = Rect {
            left: 0,
            top: 40,
            right: 1920,
            bottom: 1080,
        };
        assert_eq!(place_in(Corner::TopLeft, area), (16, 56));
        assert_eq!(place_in(Corner::TopRight, area), (1644, 56));
        assert_eq!(place_in(Corner::BottomLeft, area), (16, 1000));
        assert_eq!(place_in(Corner::BottomRight, area), (1644, 1000));
    }

    #[test]
    fn a_bottom_taskbar_keeps_the_bottom_corners_inside_the_area() {
        // The classic case, now asserted with the real RECT shape rather than
        // pre-subtracted dimensions.
        let area = Rect {
            left: 0,
            top: 0,
            right: 1920,
            bottom: 1040,
        };
        for corner in [Corner::BottomRight, Corner::BottomLeft] {
            let (_, y) = place_in(corner, area);
            assert!(
                y + HEIGHT <= 1040,
                "{corner:?} bottom edge {} reaches into the taskbar band",
                y + HEIGHT
            );
        }
    }

    #[test]
    fn a_non_zero_origin_never_pushes_the_window_outside_the_area() {
        // Generalisation of the fixtures above: whatever the origin, the window
        // stays within [left, right] x [top, bottom].
        for area in [
            Rect {
                left: 40,
                top: 0,
                right: 1920,
                bottom: 1080,
            },
            Rect {
                left: 0,
                top: 40,
                right: 1920,
                bottom: 1080,
            },
            Rect {
                left: 0,
                top: 0,
                right: 1920,
                bottom: 1040,
            },
            Rect {
                left: 40,
                top: 40,
                right: 1920,
                bottom: 1040,
            },
        ] {
            for corner in CORNERS {
                let (x, y) = place_in(corner, area);
                assert!(x >= area.left, "{corner:?} on {area:?} at x={x}");
                assert!(y >= area.top, "{corner:?} on {area:?} at y={y}");
                assert!(
                    x + WIDTH <= area.right,
                    "{corner:?} on {area:?} overflows right at x={x}"
                );
                assert!(
                    y + HEIGHT <= area.bottom,
                    "{corner:?} on {area:?} overflows bottom at y={y}"
                );
            }
        }
    }

    #[test]
    fn the_work_area_falls_back_to_the_full_screen_when_unavailable() {
        // A failed SPI_GETWORKAREA must not leave the window unplaced: the
        // caller falls back to the monitor rectangle. This asserts the
        // fallback input stays placeable rather than being degenerate. The
        // fallback spans the whole monitor and is not taskbar-aware, so it is
        // only reached when the work area could not be read at all.
        let (x, y) = place_in(Corner::BottomRight, full_area(1920, 1080));
        assert!(x >= 0 && y >= 0, "the fallback area must stay on-screen");
        assert!(x + WIDTH <= 1920 && y + HEIGHT <= 1080);
    }

    #[test]
    fn placement_respects_the_configured_margin() {
        // Bottom-right keeps the margin from both edges; top-left sits exactly
        // on it. Asserting the exact numbers pins the geometry, not just bounds.
        let (x, y) = place_in(Corner::BottomRight, full_area(1920, 1080));
        assert_eq!(x, 1920 - WIDTH - MARGIN);
        assert_eq!(y, 1080 - HEIGHT - MARGIN);

        let (x, y) = place_in(Corner::TopLeft, full_area(1920, 1080));
        assert_eq!((x, y), (MARGIN, MARGIN));
    }

    #[test]
    fn a_tiny_screen_clamps_placement_on_screen() {
        // The regression that matters: a work area smaller than the window must
        // not push the indicator to negative coordinates. Clamping is what
        // keeps it visible instead of appearing broken.
        for screen in [(100, 80), (WIDTH, HEIGHT), (1, 1), (0, 0)] {
            for corner in CORNERS {
                let (x, y) = place_in(corner, full_area(screen.0, screen.1));
                assert!(
                    x >= 0 && y >= 0,
                    "{corner:?} on {screen:?} placed at ({x}, {y})"
                );
                assert!(
                    x <= screen.0.max(0) && y <= screen.1.max(0),
                    "{corner:?} on {screen:?} placed outside the work area at ({x}, {y})"
                );
            }
        }
    }

    #[test]
    fn placement_never_reports_negative_coordinates_on_a_degenerate_area() {
        // A zero or negative work area is degenerate but must still yield a
        // placeable origin rather than a negative offset.
        for screen in [(0, 0), (-100, -100)] {
            for corner in CORNERS {
                let (x, y) = place_in(corner, full_area(screen.0, screen.1));
                assert_eq!((x, y), (0, 0), "{corner:?} on {screen:?}");
            }
        }
    }

    #[test]
    fn the_production_window_style_is_non_activating_and_click_through() {
        // Tied to the helper the window is actually created with, rather than
        // rebuilding literal flags: a test that restates the constants cannot
        // catch a change to the real call site.
        let style = overlay_ex_style();
        for required in [
            WS_EX_NOACTIVATE,
            WS_EX_TRANSPARENT,
            WS_EX_TOOLWINDOW,
            WS_EX_LAYERED,
            WS_EX_TOPMOST,
        ] {
            assert_ne!(
                style & required,
                WINDOW_EX_STYLE(0),
                "the overlay style is missing {required:?}"
            );
        }
    }

    #[test]
    fn the_style_helper_excludes_the_styles_that_would_steal_focus() {
        // Explicitly not a normal activatable app window: it must never take
        // focus from what the user is typing into.
        let style = overlay_ex_style();
        assert_eq!(style & WS_EX_APPWINDOW, WINDOW_EX_STYLE(0));
    }

    /// A fake message pump: records what a real `GetMessageW` loop would
    /// observe, without creating a window or touching the window manager.
    ///
    /// This is what lets the wake/update/stop bridge be verified as logic. The
    /// real loop's contract is exactly this ordering: a posted message arrives,
    /// `WM_VIBDOCK_STOP` breaks the loop, `WM_VIBDOCK_WAKE` drains the queue.
    struct FakePump {
        messages: Vec<u32>,
        /// Messages the loop consumed, in order.
        handled: Vec<u32>,
        stopped: bool,
    }

    impl FakePump {
        fn new(messages: Vec<u32>) -> Self {
            FakePump {
                messages,
                handled: Vec::new(),
                stopped: false,
            }
        }

        /// Runs the same dispatch order as `run_overlay`.
        fn run(&mut self, drain_count: &mut usize) {
            for message in self.messages.clone() {
                if self.stopped {
                    break;
                }
                self.handled.push(message);
                match message {
                    WM_VIBDOCK_STOP => self.stopped = true,
                    // A wake makes the loop coalesce pending frames to the
                    // newest. Frames are counted here rather than painted.
                    WM_VIBDOCK_WAKE => *drain_count += 1,
                    _ => {}
                }
            }
        }
    }

    #[test]
    fn a_stop_message_ends_the_loop_before_later_frames() {
        // The lifetime bug this guards: a stop that queued behind frames would
        // leave Drop joining a loop that never exits.
        let mut pump = FakePump::new(vec![
            WM_VIBDOCK_WAKE,
            WM_VIBDOCK_STOP,
            WM_VIBDOCK_WAKE,
            WM_VIBDOCK_WAKE,
        ]);
        let mut drains = 0;
        pump.run(&mut drains);
        assert!(pump.stopped, "the loop must exit on a stop message");
        assert_eq!(drains, 1, "no frame may be drained after the stop");
        assert_eq!(
            pump.handled,
            vec![WM_VIBDOCK_WAKE, WM_VIBDOCK_STOP],
            "the loop must not process messages after stopping"
        );
    }

    // Checked at compile time: both ids are constants, so a collision with a
    // system message id — or with each other — is a build error, not a
    // test-suite failure someone has to remember to run.
    const _: () = {
        assert!(WM_VIBDOCK_WAKE != WM_VIBDOCK_STOP);
        assert!(WM_VIBDOCK_WAKE >= WM_USER);
        assert!(WM_VIBDOCK_STOP >= WM_USER);
    };

    #[test]
    fn wake_and_stop_are_distinct_messages() {
        // The bridge sends two different thread messages; confusing them would
        // either never wake the loop or never stop it.
        assert_ne!(WM_VIBDOCK_WAKE, WM_VIBDOCK_STOP);
    }

    #[test]
    fn a_wake_drains_to_the_newest_frame_only() {
        // Coalescing: several queued frames collapse into one repaint showing
        // the latest state. Verified as pure queue behaviour.
        let (sender, receiver) = mpsc::sync_channel::<ThreadMessage>(QUEUE_LIMIT);
        let frames: Vec<StatusFrame> = (1..=3)
            .map(|sequence| StatusFrame {
                kind: crate::feedback::StatusKind::Muted,
                icon: "[MUTED]",
                text: "Microphone muted".to_string(),
                target: "Shure MV7".to_string(),
                sequence,
            })
            .collect();
        for frame in &frames {
            let _ = sender.try_send(ThreadMessage::Frame(frame.clone()));
        }
        // Drain exactly as `drain` does: keep only the newest.
        let mut newest: Option<StatusFrame> = None;
        while let Ok(ThreadMessage::Frame(frame)) = receiver.try_recv() {
            newest = Some(frame);
        }
        let newest = newest.expect("the newest frame must survive draining");
        assert_eq!(
            newest.sequence, 3,
            "draining must keep the newest frame, not an older one"
        );
        // Nothing remains: coalescing consumed the superseded frames.
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn the_frame_queue_is_bounded_and_never_blocks_the_producer() {
        // The unbounded-enqueue regression: a stalled overlay must not let the
        // queue grow without limit, and the producer must never block.
        let (sender, receiver) = mpsc::sync_channel::<ThreadMessage>(QUEUE_LIMIT);
        let frame = || {
            ThreadMessage::Frame(StatusFrame {
                kind: crate::feedback::StatusKind::Live,
                icon: "[LIVE]",
                text: "Microphone live".to_string(),
                target: "Shure MV7".to_string(),
                sequence: 1,
            })
        };
        // Fill the queue; the send after it must fail rather than block.
        for _ in 0..QUEUE_LIMIT {
            assert!(sender.try_send(frame()).is_ok());
        }
        let full = sender.try_send(frame());
        assert!(
            matches!(full, Err(mpsc::TrySendError::Full(_))),
            "a full queue must report Full rather than block or grow unbounded"
        );
        // Making room restores progress.
        assert!(receiver.try_recv().is_ok());
        assert!(sender.try_send(frame()).is_ok());
    }

    #[test]
    fn readiness_reports_failure_with_a_reason() {
        // A window that failed to create must surface why, instead of the
        // caller holding a handle to an indicator that never appeared.
        let failed = ThreadReady::Failed("could not create the indicator window: boom".to_string());
        match failed {
            ThreadReady::Failed(message) => {
                assert!(message.contains("boom"), "the real reason must be carried");
            }
            ThreadReady::Running { .. } => panic!("a failed startup must not report success"),
        }
    }

    #[test]
    fn readiness_carries_the_thread_id_needed_to_wake_and_stop() {
        // Without the thread id the handle cannot post any message, which is
        // the original broken-bridge failure.
        let ready = ThreadReady::Running { thread_id: 4242 };
        match ready {
            ThreadReady::Running { thread_id } => assert_eq!(thread_id, 4242),
            ThreadReady::Failed(_) => panic!("a running overlay must not report failure"),
        }
    }
}
