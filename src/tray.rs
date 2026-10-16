//! Windows resident entry: one interactive floating panel and one tray icon.
//!
//! Closing the panel hides it; only Exit asks the authenticated service to stop.
//! The independent, mouse-transparent action feedback overlay is unchanged.

use crate::service;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::{Duration, Instant},
};
use windows::{
    core::PCWSTR,
    Win32::{
        Foundation::{
            CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, HINSTANCE, HWND, LPARAM,
            LRESULT, POINT, WPARAM,
        },
        Graphics::Gdi::{GetStockObject, GetSysColorBrush, COLOR_WINDOW, DEFAULT_GUI_FONT},
        System::{
            LibraryLoader::GetModuleHandleW,
            Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD},
            Threading::CreateMutexW,
        },
        UI::{
            Shell::{
                ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD,
                NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
            },
            WindowsAndMessaging::*,
        },
    },
};

const TRAY_MESSAGE: u32 = WM_APP + 1;
const SHOW_PANEL: u32 = WM_APP + 2;
const MENU_OPEN: u32 = 1;
const MENU_STATUS: u32 = 2;
const MENU_EXIT: u32 = 3;
const MENU_SHOW: u32 = 4;
const MENU_HIDE: u32 = 5;
const STATUS_LABEL: i32 = 10;
const TIMER: usize = 1;
const CLASS: &str = "VibDockTray";
const TRAY_MUTEX: &str = "VibDockTraySingleton";

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(Some(0)).collect()
}

struct TraySingleton(HANDLE);
impl TraySingleton {
    fn acquire() -> crate::Result<Option<Self>> {
        let name = wide(TRAY_MUTEX);
        unsafe {
            let handle = CreateMutexW(None, false, PCWSTR(name.as_ptr()))?;
            if GetLastError() == ERROR_ALREADY_EXISTS {
                let _ = CloseHandle(handle);
                return Ok(None);
            }
            Ok(Some(Self(handle)))
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

/// Only the proof result and port reach the UI. No token is copied into window
/// text, messages, screenshots, or notification tooltips.
struct Observation {
    port: Option<u16>,
    checked: Instant,
}
struct Poller {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Poller {
    fn start() -> crate::Result<(Self, mpsc::Receiver<Observation>)> {
        let (sender, receiver) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("desktop-status".into())
            .spawn(move || {
                while !worker_stop.load(Ordering::Relaxed) {
                    let port = service::live_instance().map(|instance| instance.port);
                    if matches!(
                        sender.try_send(Observation {
                            port,
                            checked: Instant::now()
                        }),
                        Err(mpsc::TrySendError::Disconnected(_))
                    ) {
                        break;
                    }
                    for _ in 0..10 {
                        if worker_stop.load(Ordering::Relaxed) {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            })?;
        Ok((
            Self {
                stop,
                thread: Some(thread),
            },
            receiver,
        ))
    }
}
impl Drop for Poller {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // Discovery has a 700 ms total deadline; sleeping checks stop every 100 ms.
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Reuses the existing named mutex. A second launch reveals the original panel
/// without opening another browser, adding another icon, or stopping the service.
pub fn run() -> crate::Result<()> {
    let _singleton = match TraySingleton::acquire()? {
        Some(guard) => guard,
        None => {
            let class = wide(CLASS);
            // The first launcher may still be between its mutex and window.
            for _ in 0..40 {
                if let Ok(window) = unsafe { FindWindowW(PCWSTR(class.as_ptr()), None) } {
                    unsafe {
                        PostMessageW(Some(window), SHOW_PANEL, WPARAM(0), LPARAM(0))?;
                    }
                    return Ok(());
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            return Err("VibDock is already starting. Try opening it again in a moment.".into());
        }
    };
    unsafe {
        let instance = HINSTANCE(GetModuleHandleW(None)?.0);
        let class = wide(CLASS);
        let wc = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: PCWSTR(class.as_ptr()),
            hIcon: LoadIconW(Some(instance), PCWSTR(1usize as *const u16))?,
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return Err(windows::core::Error::from_win32().into());
        }
        let title = wide("VibDock");
        // A normal, movable small window with a taskbar entry. Windows chooses
        // the initial position; no fixed corner or monitor is assumed.
        let window = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            440,
            262,
            None,
            None,
            Some(instance),
            None,
        )?;
        let result = (|| -> crate::Result<()> {
            child(
                window,
                instance,
                "STATIC",
                "VibDock · resident controls",
                20,
                16,
                390,
                24,
                11,
                false,
            )?;
            child(
                window,
                instance,
                "STATIC",
                "Checking service identity…",
                20,
                48,
                390,
                44,
                STATUS_LABEL,
                false,
            )?;
            child(window, instance, "STATIC", "App controls: unavailable (not verified).\nRight-click for settings. Closing this panel keeps VibDock running.", 20, 94, 390, 50, 12, false)?;
            child(
                window,
                instance,
                "BUTTON",
                "&Settings…",
                20,
                164,
                120,
                32,
                MENU_OPEN as i32,
                true,
            )?;
            child(
                window,
                instance,
                "BUTTON",
                "&Hide panel",
                152,
                164,
                120,
                32,
                MENU_HIDE as i32,
                true,
            )?;
            add_icon(window)?;
            if SetTimer(Some(window), TIMER, 250, None) == 0 {
                return Err(windows::core::Error::from_win32().into());
            }
            let (_poller, receiver) = Poller::start()?;
            let _ = ShowWindow(window, SW_SHOWNOACTIVATE);
            message_loop(window, receiver)
        })();
        let _ = KillTimer(Some(window), TIMER);
        let _ = Shell_NotifyIconW(NIM_DELETE, &icon(window));
        let _ = DestroyWindow(window);
        let _ = UnregisterClassW(PCWSTR(class.as_ptr()), Some(instance));
        result
    }
}

#[allow(clippy::too_many_arguments)]
unsafe fn child(
    parent: HWND,
    instance: HINSTANCE,
    class: &str,
    text: &str,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    id: i32,
    tab: bool,
) -> crate::Result<()> {
    let class = wide(class);
    let text = wide(text);
    let window = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        PCWSTR(class.as_ptr()),
        PCWSTR(text.as_ptr()),
        WS_CHILD | WS_VISIBLE | if tab { WS_TABSTOP } else { WINDOW_STYLE(0) },
        x,
        y,
        width,
        height,
        Some(parent),
        Some(HMENU(id as usize as *mut _)),
        Some(instance),
        None,
    )?;
    SendMessageW(
        window,
        WM_SETFONT,
        Some(WPARAM(GetStockObject(DEFAULT_GUI_FONT).0 as usize)),
        Some(LPARAM(1)),
    );
    Ok(())
}

fn icon(window: HWND) -> NOTIFYICONDATAW {
    let tip = wide("VibDock — show controls / right-click for settings");
    let mut data = NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: window,
        uID: 1,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: TRAY_MESSAGE,
        hIcon: tray_icon(),
        ..Default::default()
    };
    let count = tip.len().min(data.szTip.len());
    data.szTip[..count].copy_from_slice(&tip[..count]);
    data
}
/// Use the pack's original monochrome variants for the system tray theme.
fn tray_icon() -> HICON {
    unsafe {
        let key = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize");
        let value = wide("SystemUsesLightTheme");
        let mut light = 0u32;
        let mut size = 4u32;
        let _ = RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key.as_ptr()),
            PCWSTR(value.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some((&mut light as *mut u32).cast()),
            Some(&mut size),
        );
        let id = if light == 1 { 3usize } else { 2usize };
        GetModuleHandleW(None)
            .ok()
            .and_then(|module| LoadIconW(Some(HINSTANCE(module.0)), PCWSTR(id as *const u16)).ok())
            .unwrap_or_default()
    }
}

fn add_icon(window: HWND) -> crate::Result<()> {
    if !unsafe { Shell_NotifyIconW(NIM_ADD, &icon(window)) }.as_bool() {
        return Err("Could not add the VibDock tray icon.".into());
    }
    Ok(())
}

fn show_menu(window: HWND) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        let items = [
            (MENU_SHOW, "Show controls"),
            (MENU_OPEN, "Settings…"),
            (MENU_STATUS, "Service status"),
            (MENU_HIDE, "Hide panel (keep running)"),
            (MENU_EXIT, "Exit VibDock"),
        ];
        for (id, label) in items {
            let label = wide(label);
            if AppendMenuW(menu, MF_STRING, id as usize, PCWSTR(label.as_ptr())).is_err() {
                let _ = DestroyMenu(menu);
                return;
            }
        }
        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
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
        // Required to reliably dismiss a notification-area menu on outside click.
        let _ = PostMessageW(Some(window), WM_NULL, WPARAM(0), LPARAM(0));
        if chosen.as_bool() {
            command(window, chosen.0 as u32);
        }
    }
}

fn open_configuration() {
    let result = crate::configuration_url().and_then(|url| {
        let url = wide(&url); let verb = wide("open");
        let result = unsafe { ShellExecuteW(None, PCWSTR(verb.as_ptr()), PCWSTR(url.as_ptr()), None, None, SW_SHOWNORMAL) };
        if result.0 as isize <= 32 { Err("Windows could not open the settings page. Check your default browser and try again.".into()) } else { Ok(()) }
    });
    if let Err(error) = result {
        notify("VibDock settings", &error.to_string(), MB_ICONERROR);
    }
}

fn report_status() {
    let body = match service::live_instance() {
        Some(instance) => format!("Connected to the verified VibDock service on port {}.\n\nConnection is not proof of an audio action or an application voice session.", instance.port),
        None => "Service unavailable: no current instance proof. Open VibDock again to restart the service.".into(),
    };
    notify("VibDock service status", &body, MB_ICONINFORMATION);
}
fn notify(title: &str, body: &str, style: MESSAGEBOX_STYLE) {
    let title = wide(title);
    let body = wide(body);
    unsafe {
        let _ = MessageBoxW(
            None,
            PCWSTR(body.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | style,
        );
    }
}
fn show_panel(window: HWND) {
    unsafe {
        let _ = ShowWindow(window, SW_RESTORE);
        let _ = SetForegroundWindow(window);
    }
}
fn command(window: HWND, id: u32) {
    match id {
        MENU_OPEN => open_configuration(), MENU_STATUS => report_status(), MENU_SHOW => show_panel(window),
        MENU_HIDE => unsafe { let _ = ShowWindow(window, SW_HIDE); },
        MENU_EXIT => match service::request_shutdown() {
            Ok(_) => unsafe { PostQuitMessage(0); },
            Err(error) => notify("VibDock could not exit", &format!("{error}\n\nThe panel and tray remain available. Retry Exit to stop the verified service."), MB_ICONWARNING),
        },
        _ => {}
    }
}
fn message_loop(window: HWND, receiver: mpsc::Receiver<Observation>) -> crate::Result<()> {
    let mut observation: Option<Observation> = None;
    let mut shown = String::new();
    unsafe {
        let mut message = MSG::default();
        loop {
            match GetMessageW(&mut message, None, 0, 0).0 {
                -1 => return Err(windows::core::Error::from_win32().into()),
                0 => return Ok(()),
                _ => {}
            }
            if message.message == WM_TIMER && message.wParam.0 == TIMER {
                if let Ok(next) = receiver.try_recv() {
                    observation = Some(next);
                }
                let port = observation
                    .as_ref()
                    .filter(|value| value.checked.elapsed() < Duration::from_secs(4))
                    .and_then(|value| value.port);
                let text = match port { Some(port) => format!("Service: connected (verified, port {port})\nAudio action / app voice state: not verified here"), None => "Service: unavailable / unknown\nNo current instance proof; audio state is unknown".into() };
                if text != shown {
                    if let Ok(label) = GetDlgItem(Some(window), STATUS_LABEL) {
                        let value = wide(&text);
                        let _ = SetWindowTextW(label, PCWSTR(value.as_ptr()));
                    }
                    shown = text;
                }
            }
            if !IsDialogMessageW(window, &message).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
}
unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // Explorer restarts remove notification icons. Restore ours, including while
    // the panel is hidden, without restarting or changing the service.
    let taskbar_created = RegisterWindowMessageW(PCWSTR(wide("TaskbarCreated").as_ptr()));
    if taskbar_created != 0 && message == taskbar_created {
        if add_icon(window).is_err() {
            show_panel(window);
        }
        return LRESULT(0);
    }
    match message {
        SHOW_PANEL => {
            show_panel(window);
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &icon(window));
            DefWindowProcW(window, message, wparam, lparam)
        }
        WM_CLOSE => {
            let _ = ShowWindow(window, SW_HIDE);
            LRESULT(0)
        }
        WM_COMMAND => {
            command(window, (wparam.0 & 0xffff) as u32);
            LRESULT(0)
        }
        WM_CONTEXTMENU => {
            show_menu(window);
            LRESULT(0)
        }
        TRAY_MESSAGE => {
            match lparam.0 as u32 {
                WM_LBUTTONUP => show_panel(window),
                WM_RBUTTONUP => show_menu(window),
                _ => {}
            };
            LRESULT(0)
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            LRESULT(0)
        }
        _ => DefWindowProcW(window, message, wparam, lparam),
    }
}
