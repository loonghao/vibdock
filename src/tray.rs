//! Thin Windows tray entry: a notification icon whose menu controls the
//! resident service that the CLI and this entry share.
//!
//! This module owns no business logic. Every menu item delegates to the same
//! service functions the console CLI uses, so the two entries cannot drift:
//! "Open" opens the service's own bound URL, "Status" reads the live instance,
//! and "Exit" performs the authenticated shutdown and then leaves the loop.

use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{
            CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, HINSTANCE, HWND, LPARAM,
            LRESULT, POINT, WPARAM,
        },
        System::{LibraryLoader::GetModuleHandleW, Threading::CreateMutexW},
        UI::{
            Shell::{
                ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD,
                NIM_DELETE, NOTIFYICONDATAW,
            },
            WindowsAndMessaging::*,
        },
    },
};

use crate::service;

/// Message the tray icon sends back to our hidden window.
const TRAY_MESSAGE: u32 = WM_USER + 1;
/// Menu item ids.
const MENU_OPEN: u32 = 1;
const MENU_STATUS: u32 = 2;
const MENU_EXIT: u32 = 3;

/// Name of the mutex that keeps a single tray instance alive.
const TRAY_MUTEX: &str = "VibDockTraySingleton";

/// Claims exclusive ownership of the tray icon for this process.
///
/// `RegisterClassW` only fails for a class already registered *in this
/// process*, so it cannot tell us another tray is running. A named mutex is
/// visible across processes and is the correct single-instance check here.
struct TraySingleton(HANDLE);

impl TraySingleton {
    /// Takes the claim, or returns `None` if another tray already holds it.
    ///
    /// `CreateMutexW` succeeds whether or not it created the mutex; the
    /// distinguishing signal is `GetLastError` reporting
    /// `ERROR_ALREADY_EXISTS`. That is the standard Win32 single-instance
    /// pattern, and the only reliable way to see another process's claim.
    fn acquire() -> Option<TraySingleton> {
        let name: Vec<u16> = TRAY_MUTEX.encode_utf16().chain(Some(0)).collect();
        unsafe {
            let handle = CreateMutexW(None, false, PCWSTR(name.as_ptr())).ok()?;
            let already_running = GetLastError() == ERROR_ALREADY_EXISTS;
            if already_running {
                let _ = CloseHandle(handle);
                return None;
            }
            Some(TraySingleton(handle))
        }
    }
}

impl Drop for TraySingleton {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Runs the tray entry until the user chooses Exit.
///
/// Shows a notification icon with Open / Status / Exit. Exit performs the real
/// authenticated shutdown of the resident service, so the tray is what stops
/// the service on Windows rather than a process terminated by name.
///
/// Only one tray runs at a time. `RegisterClassW` is process-local, so it
/// cannot detect another instance; a named mutex can, and it is what stops a
/// second double-click from stacking a second icon over the service this tray
/// already controls.
pub fn run() -> crate::Result<()> {
    // Held for the lifetime of the tray; dropping it on return releases the
    // claim so a later launch can take it again.
    let _singleton = match TraySingleton::acquire() {
        Some(guard) => guard,
        None => {
            // Another tray is already running. It owns the icon and the menu,
            // so the useful action is to open its page and leave it in control.
            open_configuration();
            return Ok(());
        }
    };
    unsafe {
        let instance = GetModuleHandleW(None)?;
        // A message-only window: the tray icon needs one to receive events.
        let class: Vec<u16> = "VibDockTray\0".encode_utf16().collect();
        let window_class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: HINSTANCE(instance.0),
            lpszClassName: PCWSTR(class.as_ptr()),
            ..Default::default()
        };
        if RegisterClassW(&window_class) == 0 {
            // Another instance already registered the class; that is fine, the
            // single-instance check below hands control back to it.
            let error = windows::core::Error::from_win32();
            if error.code() != windows::core::HRESULT(0x8007_0582u32 as i32) {
                return Err(error.into());
            }
        }
        let window = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class.as_ptr()),
            PCWSTR("VibDock\0".encode_utf16().collect::<Vec<u16>>().as_ptr()),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            None,
            None,
            Some(HINSTANCE(instance.0)),
            None,
        )?;

        add_icon(window)?;
        let result = message_loop();
        let _ = Shell_NotifyIconW(NIM_DELETE, &icon(window));
        result
    }
}

/// Builds the notification icon data for our window.
fn icon(window: HWND) -> NOTIFYICONDATAW {
    let tip: Vec<u16> = "VibDock\0".encode_utf16().collect();
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: 1,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: TRAY_MESSAGE,
        // The standard application icon: enough to identify the entry without
        // shipping a resource, and no extra build step.
        hIcon: unsafe { LoadIconW(None, IDI_APPLICATION).unwrap_or_default() },
        ..Default::default()
    };
    let copy = tip.len().min(data.szTip.len());
    data.szTip[..copy].clone_from_slice(&tip[..copy]);
    data
}

/// Adds the tray icon, reporting failure rather than running invisibly.
fn add_icon(window: HWND) -> crate::Result<()> {
    unsafe {
        if !Shell_NotifyIconW(NIM_ADD, &icon(window)).as_bool() {
            return Err("Could not add the VibDock tray icon.".into());
        }
    }
    Ok(())
}

/// Shows the context menu at the cursor and returns the chosen item id.
fn show_menu(window: HWND) -> Option<u32> {
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        let open: Vec<u16> = "Open configuration\0".encode_utf16().collect();
        let status: Vec<u16> = "Status\0".encode_utf16().collect();
        let exit: Vec<u16> = "Exit\0".encode_utf16().collect();
        let _ = AppendMenuW(menu, MF_STRING, MENU_OPEN as usize, PCWSTR(open.as_ptr()));
        let _ = AppendMenuW(
            menu,
            MF_STRING,
            MENU_STATUS as usize,
            PCWSTR(status.as_ptr()),
        );
        let _ = AppendMenuW(menu, MF_STRING, MENU_EXIT as usize, PCWSTR(exit.as_ptr()));

        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        // Required so a click outside the menu dismisses it.
        let _ = SetForegroundWindow(window);
        let chosen = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_NONOTIFY,
            point.x,
            point.y,
            None,
            window,
            None,
        );
        let _ = DestroyMenu(menu);
        if chosen.as_bool() {
            Some(chosen.0 as u32)
        } else {
            None
        }
    }
}

/// Opens the running service's actual bound URL in the default browser.
///
/// Uses the live instance's recorded port, not this process's own address: a
/// launcher and the resident service can be started with different
/// `VIBDOCK_PORT` values, and opening the wrong one would show a page that
/// does not control the running service.
fn open_configuration() {
    let url = match crate::configuration_url() {
        Ok(url) => url,
        Err(error) => {
            notify("VibDock", &error.to_string(), MB_ICONERROR);
            return;
        }
    };
    let wide: Vec<u16> = url.encode_utf16().chain(Some(0)).collect();
    unsafe {
        // SW_SHOWNORMAL; the result is advisory and the service stays up either
        // way, so a failure here must not disturb the tray.
        let _ = ShellExecuteW(
            None,
            PCWSTR("open\0".encode_utf16().collect::<Vec<u16>>().as_ptr()),
            PCWSTR(wide.as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        );
    }
}

/// Reports whether a resident service is currently answering.
///
/// Uses a message box rather than stdout: this entry point is a GUI-subsystem
/// executable with no console, so printed output would be invisible.
fn report_status() {
    let (title, body) = match service::live_instance() {
        Some(instance) => (
            "VibDock — Running",
            format!(
                "The VibDock service is running on http://127.0.0.1:{}.\n\n\
                 The token is available in the service state file.",
                instance.port
            ),
        ),
        None => (
            "VibDock — Not running",
            "No VibDock service is currently running.".to_string(),
        ),
    };
    notify(title, &body, MB_ICONINFORMATION);
}

/// Shows a small dialog. The only visible channel in a GUI-subsystem binary.
fn notify(title: &str, body: &str, icon: MESSAGEBOX_STYLE) {
    let title: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
    let body: Vec<u16> = body.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let _ = MessageBoxW(
            None,
            PCWSTR(body.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | icon,
        );
    }
}

/// Processes window messages until Exit is chosen or the loop ends.
fn message_loop() -> crate::Result<()> {
    unsafe {
        let mut message = MSG::default();
        loop {
            let result = GetMessageW(&mut message, None, 0, 0).0;
            if result == -1 {
                return Err(windows::core::Error::from_win32().into());
            }
            if result == 0 {
                return Ok(());
            }
            if message.message == TRAY_MESSAGE
                && (message.lParam.0 as u32 == WM_RBUTTONUP
                    || message.lParam.0 as u32 == WM_LBUTTONUP)
            {
                match show_menu(message.hwnd) {
                    Some(MENU_OPEN) => open_configuration(),
                    Some(MENU_STATUS) => report_status(),
                    Some(MENU_EXIT) => {
                        // Real shutdown through the shared contract: the running
                        // instance is asked to stop with its own token, so this
                        // never terminates an unrelated process.
                        match service::request_shutdown() {
                            Ok(true) => return Ok(()),
                            Ok(false) => {
                                // Nothing was running, so there is nothing left
                                // to control. Leaving is correct here.
                                return Ok(());
                            }
                            Err(error) => {
                                // A failed shutdown must NOT close the tray.
                                // Doing so would abandon a live service with no
                                // way to reach it, and the user would have to
                                // find and kill the process by hand.
                                notify(
                                    "VibDock — Could not stop the service",
                                    &format!(
                                        "{error}\n\nThe tray stays open so the service remains \
                                         reachable. Close it again to retry."
                                    ),
                                    MB_ICONWARNING,
                                );
                            }
                        }
                    }
                    _ => {}
                }
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

/// Minimal window procedure: tray clicks and destroy are all we handle.
unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(window, message, wparam, lparam),
    }
}
