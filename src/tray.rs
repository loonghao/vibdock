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
const UI_COMMAND: u32 = WM_APP + 3;
const UI_MENU: u32 = WM_APP + 4;
const FIRST_SESSION_COMMAND: u32 = 1000;
const RESULT_LABEL: i32 = 13;
const ROW_TOP: i32 = 274;
const PANEL_HEIGHT: i32 = 326;
const MENU_OPEN: u32 = 101;
const MENU_STATUS: u32 = 102;
const MENU_EXIT: u32 = 103;
const MENU_SHOW: u32 = 104;
const MENU_HIDE: u32 = 105;
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

/// Only public observation data and a local generation reach the UI. Bearers
/// and prepared capabilities remain on the worker, including during retries of
/// receipt reads (execute itself is never retried).
#[derive(Clone)]
struct Observation {
    port: Option<u16>,
    apps: serde_json::Value,
    checked: Option<Instant>,
    generation: u64,
}
impl Observation {
    fn fresh(&self) -> bool {
        self.port.is_some()
            && self
                .checked
                .is_some_and(|checked| checked.elapsed() < Duration::from_secs(4))
    }
    fn unavailable(generation: u64) -> Self {
        Self {
            port: None,
            apps: serde_json::Value::Null,
            checked: None,
            generation,
        }
    }
}

fn observation_time(started: Instant, value: &serde_json::Value) -> Option<Instant> {
    started.checked_sub(Duration::from_millis(value["age_ms"].as_u64()?))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PlaybackSelection {
    generation: u64,
    entry_id: String,
    session_reference: String,
    label: String,
    process_id: u32,
    requested_muted: bool,
}

fn reference_is_current(observation: &Observation, selection: &PlaybackSelection) -> bool {
    observation.fresh()
        && observation.generation == selection.generation
        && observation.apps["entries"].as_array().is_some_and(|rows| {
            rows.iter().any(|row| {
                row["id"].as_str() == Some(&selection.entry_id)
                    && row["actions_enabled"] == true
                    && row["sessions"].as_array().is_some_and(|sessions| {
                        sessions.iter().any(|session| {
                            session["actions_enabled"] == true
                                && session["session_reference"].as_str()
                                    == Some(&selection.session_reference)
                                && session["process_id"].as_u64()
                                    == Some(u64::from(selection.process_id))
                        })
                    })
            })
        })
}

enum WorkerCommand {
    Playback(PlaybackSelection),
    Settings,
    Shutdown,
}
enum WorkerEvent {
    Observation(Observation),
    ActionResult(String),
    Notice(String),
    Shutdown(std::result::Result<bool, String>),
}
struct PendingPlayback {
    client: service::ResidentControlClient,
    prepared: service::PreparedPlayback,
    selection: PlaybackSelection,
    checked: Instant,
    execute_attempted: bool,
    started: Instant,
    next_read: Instant,
}

/// Important results cannot be displaced by a periodic observation. The queue
/// is bounded, and waiting for the UI always remains cancellable.
fn send_worker_event(
    sender: &mpsc::SyncSender<WorkerEvent>,
    stop: &AtomicBool,
    mut event: WorkerEvent,
) -> bool {
    while !stop.load(Ordering::Relaxed) {
        match sender.try_send(event) {
            Ok(()) => return true,
            Err(mpsc::TrySendError::Full(returned)) => {
                event = returned;
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(mpsc::TrySendError::Disconnected(_)) => return false,
        }
    }
    false
}

fn action_result(selection: &PlaybackSelection, message: &str) -> WorkerEvent {
    WorkerEvent::ActionResult(format!(
        "{} · PID {}\n{message}",
        selection.label, selection.process_id
    ))
}

fn poll_worker(
    sender: mpsc::SyncSender<WorkerEvent>,
    commands: mpsc::Receiver<WorkerCommand>,
    stop: Arc<AtomicBool>,
) {
    let mut client: Option<service::ResidentControlClient> = None;
    let mut latest: Option<Observation> = None;
    let mut pending: Option<PendingPlayback> = None;
    let mut generation = 0u64;
    let mut next_poll = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        match commands.recv_timeout(Duration::from_millis(50)) {
            Ok(WorkerCommand::Playback(selection)) => {
                if pending.is_some()
                    || client.is_none()
                    || !latest
                        .as_ref()
                        .is_some_and(|view| reference_is_current(view, &selection))
                {
                    if !send_worker_event(&sender, &stop, action_result(&selection, "Selection is stale or unavailable. Refresh and select the exact playback session again.")) { return; }
                    continue;
                }
                let preparation = client.as_ref().unwrap().prepare(
                    &selection.entry_id,
                    &selection.session_reference,
                    selection.process_id,
                    selection.requested_muted,
                );
                match preparation {
                    Ok(prepared) => {
                        pending = Some(PendingPlayback {
                            client: client.take().unwrap(),
                            prepared,
                            selection,
                            checked: latest.as_ref().unwrap().checked.unwrap(),
                            execute_attempted: false,
                            started: Instant::now(),
                            next_read: Instant::now(),
                        });
                    }
                    Err(_) => {
                        // Preparation never writes. Do not keep actionable data
                        // after an expired reference, failed proof, or failed read.
                        client = None;
                        latest = None;
                        let _ = send_worker_event(
                            &sender,
                            &stop,
                            WorkerEvent::Observation(Observation::unavailable(generation)),
                        );
                        if !send_worker_event(&sender, &stop, action_result(&selection, "Could not prepare this exact playback session. No execute request was sent; refresh and select again.")) { return; }
                        next_poll = Instant::now();
                    }
                }
            }
            Ok(WorkerCommand::Settings) => {
                if let Err(error) = open_configuration() {
                    if !send_worker_event(&sender, &stop, WorkerEvent::Notice(error)) {
                        return;
                    }
                }
            }
            Ok(WorkerCommand::Shutdown) => {
                // Cancel an unexecuted preparation, or stop polling an executed
                // request without implying that stopping restores its audio.
                pending = None;
                let result = service::request_shutdown();
                if !send_worker_event(&sender, &stop, WorkerEvent::Shutdown(result)) {
                    return;
                }
                client = None;
                latest = None;
                next_poll = Instant::now();
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if stop.load(Ordering::Relaxed) {
            return;
        }
        if let Some(mut operation) = pending.take() {
            let mut complete = None;
            if !operation.execute_attempted {
                if operation.checked.elapsed() >= Duration::from_secs(4) {
                    complete = Some("Selection expired before execute. No execute request was sent; refresh and select again.".to_owned());
                } else {
                    // Exactly one attempt, even if the server's response is
                    // lost. Subsequent requests only read this known receipt.
                    if operation.client.execute(&operation.prepared).is_err() {
                        latest = None;
                        let _ = send_worker_event(
                            &sender,
                            &stop,
                            WorkerEvent::Observation(Observation::unavailable(generation)),
                        );
                    }
                    operation.execute_attempted = true;
                    operation.next_read = Instant::now() + Duration::from_millis(250);
                }
            } else if operation.started.elapsed() >= Duration::from_secs(12) {
                complete = Some("Playback outcome unconfirmed. Audio may have changed; inspect the session before trying again.".to_owned());
            } else if Instant::now() >= operation.next_read {
                match operation.client.receipt(operation.prepared.operation_id()) {
                    Ok(receipt)
                        if receipt.requested_muted == operation.selection.requested_muted =>
                    {
                        if receipt.state != service::PlaybackReceiptState::Pending {
                            complete = Some(receipt.presentation().to_owned());
                        }
                    }
                    _ => {
                        latest = None;
                        let _ = send_worker_event(
                            &sender,
                            &stop,
                            WorkerEvent::Observation(Observation::unavailable(generation)),
                        );
                    }
                }
                operation.next_read = Instant::now() + Duration::from_millis(500);
            }
            if let Some(message) = complete {
                // Invalidate the pre-action observation even on verified
                // completion. A receipt is not a continuous/live observation.
                latest = None;
                let _ = send_worker_event(
                    &sender,
                    &stop,
                    WorkerEvent::Observation(Observation::unavailable(generation)),
                );
                if !send_worker_event(
                    &sender,
                    &stop,
                    action_result(&operation.selection, &message),
                ) {
                    return;
                }
                client = Some(operation.client);
                next_poll = Instant::now();
            } else {
                pending = Some(operation);
            }
            continue;
        }
        if Instant::now() < next_poll {
            continue;
        }
        // Age is measured from request start, then source age is subtracted.
        // Network/queue time must never renew a stale source observation.
        let started = Instant::now();
        let result = service::ResidentControlClient::discover().and_then(|candidate| {
            let apps = candidate.summary()?;
            Ok((candidate, apps))
        });
        let observation = match result {
            Ok((candidate, apps)) => {
                if !client
                    .as_ref()
                    .is_some_and(|previous| previous.same_instance(&candidate))
                {
                    generation = generation.wrapping_add(1);
                }
                let observation = Observation {
                    port: Some(candidate.port()),
                    checked: observation_time(started, &apps),
                    apps,
                    generation,
                };
                client = Some(candidate);
                observation
            }
            Err(_) => {
                client = None;
                Observation::unavailable(generation)
            }
        };
        latest = Some(observation.clone());
        match sender.try_send(WorkerEvent::Observation(observation)) {
            Err(mpsc::TrySendError::Disconnected(_)) => return,
            // Full status queues never block the next check or accept a stale
            // click: the worker rechecks its latest reference before prepare.
            Ok(()) | Err(mpsc::TrySendError::Full(_)) => {}
        }
        next_poll = Instant::now() + Duration::from_secs(1);
    }
}

struct Poller {
    stop: Arc<AtomicBool>,
    commands: mpsc::SyncSender<WorkerCommand>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Poller {
    fn start() -> crate::Result<(Self, mpsc::Receiver<WorkerEvent>)> {
        let (sender, receiver) = mpsc::sync_channel(4);
        let (commands, requests) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("desktop-status".into())
            .spawn(move || poll_worker(sender, requests, worker_stop))?;
        Ok((
            Self {
                stop,
                commands,
                thread: Some(thread),
            },
            receiver,
        ))
    }
    fn request(&self, command: WorkerCommand) -> bool {
        self.commands.try_send(command).is_ok()
    }
}
impl Drop for Poller {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        // No detached request threads. A playback exchange has one two-second
        // deadline, and every step/queue wait checks stop. Shutdown's existing
        // bounded release wait is also owned by this worker.
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
            hIcon: LoadIconW(Some(instance), PCWSTR(std::ptr::without_provenance(1)))?,
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
            PANEL_HEIGHT,
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
            child(window, instance, "STATIC", "Right-click a software tray icon to choose a playback session.\nPlayback controls never change microphone mute.", 20, 94, 390, 50, 12, false)?;
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
            child(
                window,
                instance,
                "STATIC",
                "No playback action requested.",
                20,
                206,
                390,
                60,
                RESULT_LABEL,
                false,
            )?;
            for index in 0..8 {
                child(
                    window,
                    instance,
                    "STATIC",
                    "",
                    20,
                    ROW_TOP + index * 40,
                    32,
                    32,
                    200 + index * 2,
                    false,
                )?;
                if let Ok(icon) = GetDlgItem(Some(window), 200 + index * 2) {
                    SetWindowLongPtrW(
                        icon,
                        GWL_STYLE,
                        (WS_CHILD | WINDOW_STYLE(0x0000_0003)/* SS_ICON */).0 as isize,
                    );
                }
                child(
                    window,
                    instance,
                    "STATIC",
                    "",
                    62,
                    ROW_TOP + index * 40,
                    345,
                    36,
                    201 + index * 2,
                    false,
                )?;
            }
            add_icon(window)?;
            if SetTimer(Some(window), TIMER, 250, None) == 0 {
                return Err(windows::core::Error::from_win32().into());
            }
            let (poller, receiver) = Poller::start()?;
            let _ = ShowWindow(window, SW_SHOWNOACTIVATE);
            message_loop(window, receiver, &poller)
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

fn visible_text(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(100)
        .collect()
}
fn tray_row<'a>(
    rows: &'a [serde_json::Value],
    uids: &[u32],
    uid: usize,
) -> Option<&'a serde_json::Value> {
    if uid < 2 {
        return None;
    }
    let index = uids.iter().position(|id| *id as usize == uid)?;
    rows.get(index)
}

fn stable_tray_ids(
    previous: &[serde_json::Value],
    ids: &[u32],
    rows: &[serde_json::Value],
    next: &mut u32,
) -> Vec<u32> {
    rows.iter()
        .map(|row| {
            previous
                .iter()
                .position(|old| old["id"] == row["id"])
                .and_then(|index| ids.get(index).copied())
                .unwrap_or_else(|| {
                    if *next < 2 {
                        return 0;
                    }
                    let id = *next;
                    // An exhausted ID space uses no actionable ID. Never wrap to
                    // another row or the main icon after a delayed tray callback.
                    *next = next.checked_add(1).unwrap_or(0);
                    id
                })
        })
        .collect()
}

struct SessionMenuEntry {
    description: String,
    mute: PlaybackSelection,
    unmute: PlaybackSelection,
}
fn session_menu_entries(row: &serde_json::Value, generation: u64) -> Vec<SessionMenuEntry> {
    if row["actions_enabled"] != true {
        return vec![];
    }
    let Some(entry_id) = row["id"].as_str() else {
        return vec![];
    };
    let Some(sessions) = row["sessions"].as_array() else {
        return vec![];
    };
    sessions
        .iter()
        .take(32)
        .enumerate()
        .filter_map(|(index, session)| {
            if session["actions_enabled"] != true {
                return None;
            }
            let reference = session["session_reference"].as_str()?;
            if reference.len() != 64 || !reference.bytes().all(|value| value.is_ascii_hexdigit()) {
                return None;
            }
            let process_id = u32::try_from(session["process_id"].as_u64()?)
                .ok()
                .filter(|pid| *pid != 0)?;
            let label = visible_text(row["label"].as_str().unwrap_or("Software"));
            let display = visible_text(
                session["display_name"]
                    .as_str()
                    .filter(|name| !name.is_empty())
                    .unwrap_or(&label),
            );
            let state = match session["muted"].as_bool() {
                Some(true) => "muted",
                Some(false) => "unmuted",
                None => "mute unknown",
            };
            let activity = if session["active"] == true {
                "active"
            } else {
                "inactive"
            };
            let mute = PlaybackSelection {
                generation,
                entry_id: entry_id.to_owned(),
                session_reference: reference.to_owned(),
                label: format!(
                    "{} / session {}: {}",
                    label.chars().take(28).collect::<String>(),
                    index + 1,
                    display.chars().take(28).collect::<String>()
                ),
                process_id,
                requested_muted: true,
            };
            let mut unmute = mute.clone();
            unmute.requested_muted = false;
            Some(SessionMenuEntry {
                description: format!(
                    "Session {}: {display} · PID {process_id} · {activity}, {state}",
                    index + 1
                ),
                mute,
                unmute,
            })
        })
        .collect()
}

enum MenuChoice {
    Command(u32),
    Playback(PlaybackSelection),
}

/// The row mapping and all command bindings are immutable for the entire
/// TrackPopupMenu call. No periodic observation can retarget an open menu.
fn show_menu(
    window: HWND,
    uid: usize,
    apps: &AppIcons,
    observation: Option<&Observation>,
    busy: bool,
) -> Option<MenuChoice> {
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        let mut actions = Vec::new();
        let built = (|| -> windows::core::Result<()> {
            if uid != 1 {
                let Some(row) = tray_row(&apps.rows, &apps.uids, uid) else {
                    return Ok(());
                };
                let label = wide(
                    &visible_text(row["label"].as_str().unwrap_or("Software")).replace('&', "&&"),
                );
                AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, PCWSTR(label.as_ptr()))?;
                let available = observation.is_some_and(Observation::fresh) && !busy;
                let sessions = if available {
                    session_menu_entries(row, observation.unwrap().generation)
                } else {
                    vec![]
                };
                if sessions.is_empty() {
                    let label = wide(if busy {
                        "Playback request pending; controls disabled"
                    } else {
                        "No fresh, controllable playback session"
                    });
                    AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, PCWSTR(label.as_ptr()))?;
                }
                for session in sessions {
                    let submenu = CreatePopupMenu()?;
                    // Attach first so DestroyMenu owns it on every later error.
                    let label = wide(&session.description.replace('&', "&&"));
                    if let Err(error) =
                        AppendMenuW(menu, MF_POPUP, submenu.0 as usize, PCWSTR(label.as_ptr()))
                    {
                        let _ = DestroyMenu(submenu);
                        return Err(error);
                    }
                    for (selection, label) in [
                        (session.mute, "Mute this playback session"),
                        (session.unmute, "Unmute this playback session"),
                    ] {
                        let id = FIRST_SESSION_COMMAND + actions.len() as u32;
                        let label = wide(label);
                        AppendMenuW(submenu, MF_STRING, id as usize, PCWSTR(label.as_ptr()))?;
                        actions.push(selection);
                    }
                }
                if row["sessions"]
                    .as_array()
                    .is_some_and(|sessions| sessions.len() > 32)
                {
                    let label =
                        wide("Showing the first 32 sessions; no other session will be changed");
                    AppendMenuW(menu, MF_STRING | MF_GRAYED, 0, PCWSTR(label.as_ptr()))?;
                }
                AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null())?;
            }
            for (id, label) in [
                (MENU_SHOW, "Show controls"),
                (MENU_OPEN, "Settings…"),
                (MENU_STATUS, "Service status"),
                (MENU_HIDE, "Hide panel (keep running)"),
                (MENU_EXIT, "Exit VibDock"),
            ] {
                let label = wide(label);
                AppendMenuW(menu, MF_STRING, id as usize, PCWSTR(label.as_ptr()))?;
            }
            Ok(())
        })();
        if built.is_err() || (uid != 1 && tray_row(&apps.rows, &apps.uids, uid).is_none()) {
            let _ = DestroyMenu(menu);
            return None;
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
        let _ = PostMessageW(Some(window), WM_NULL, WPARAM(0), LPARAM(0));
        let id = chosen.0 as u32;
        if id >= FIRST_SESSION_COMMAND {
            actions
                .get((id - FIRST_SESSION_COMMAND) as usize)
                .cloned()
                .map(MenuChoice::Playback)
        } else if chosen.as_bool() {
            Some(MenuChoice::Command(id))
        } else {
            None
        }
    }
}

/// Called exclusively on the poll worker. No network, proof, or browser
/// authorization is performed by the window procedure or modal menu callbacks.
fn open_configuration() -> std::result::Result<(), String> {
    let url = crate::configuration_url().map_err(|_| "Could not authorize Settings with the current service. Try again after its identity is verified.".to_owned())?;
    let url = wide(&url);
    let verb = wide("open");
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(url.as_ptr()),
            None,
            None,
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        Err("Windows could not open Settings. Check your default browser and try again.".into())
    } else {
        Ok(())
    }
}

fn report_status(observation: Option<&Observation>) {
    let body = match observation.filter(|view| view.fresh()).and_then(|view| view.port) {
        Some(port) => format!("Connected to the verified VibDock service on port {port}.\n\nConnection is not proof of an audio action or an application voice session."),
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
fn set_result(window: HWND, text: &str) {
    unsafe {
        if let Ok(label) = GetDlgItem(Some(window), RESULT_LABEL) {
            let text = wide(text);
            let _ = SetWindowTextW(label, PCWSTR(text.as_ptr()));
        }
    }
}
fn command(window: HWND, id: u32, poller: &Poller, observation: Option<&Observation>) {
    let requested = match id {
        MENU_OPEN => Some(poller.request(WorkerCommand::Settings)),
        MENU_EXIT => Some(poller.request(WorkerCommand::Shutdown)),
        MENU_STATUS => {
            report_status(observation);
            None
        }
        MENU_SHOW => {
            show_panel(window);
            None
        }
        MENU_HIDE | 2 => {
            unsafe {
                let _ = ShowWindow(window, SW_HIDE);
            }
            None
        }
        // Session commands are accepted only from this menu's frozen returned
        // binding. A posted numeric WM_COMMAND is not a playback selection.
        _ => None,
    };
    if requested == Some(false) {
        set_result(
            window,
            "A request is already queued. Please wait and try again.",
        );
    }
}
fn message_loop(
    window: HWND,
    receiver: mpsc::Receiver<WorkerEvent>,
    poller: &Poller,
) -> crate::Result<()> {
    let mut observation: Option<Observation> = None;
    let mut shown = String::new();
    let mut busy = false;
    let mut apps = AppIcons::new(window);
    unsafe {
        let mut message = MSG::default();
        loop {
            match GetMessageW(&mut message, None, 0, 0).0 {
                -1 => return Err(windows::core::Error::from_win32().into()),
                0 => return Ok(()),
                _ => {}
            }
            if message.message == WM_TIMER && message.wParam.0 == TIMER {
                while let Ok(event) = receiver.try_recv() {
                    match event {
                        WorkerEvent::Observation(next) => observation = Some(next),
                        WorkerEvent::ActionResult(result) => {
                            busy = false;
                            set_result(window, &result);
                        }
                        WorkerEvent::Notice(message) => {
                            notify("VibDock settings", &message, MB_ICONERROR)
                        }
                        WorkerEvent::Shutdown(Ok(_)) => {
                            PostQuitMessage(0);
                        }
                        WorkerEvent::Shutdown(Err(error)) => {
                            busy = false;
                            set_result(window, "Playback outcome may be unconfirmed after cancelling its receipt check.");
                            notify("VibDock could not exit", &format!("{error}\n\nThe panel and tray remain available. Retry Exit to stop the verified service."), MB_ICONWARNING);
                        }
                    }
                }
                let port = observation
                    .as_ref()
                    .filter(|value| value.fresh())
                    .and_then(|value| value.port);
                let text = match port {
                    Some(port) => format!("Service: connected (verified, port {port})\nPlayback actions require an explicit session selection"),
                    None => "Service: unavailable / unknown\nNo current observation; playback controls are disabled".into(),
                };
                if let Some(current) = observation.as_ref() {
                    apps.update(&current.apps, current.fresh());
                }
                if text != shown {
                    if let Ok(label) = GetDlgItem(Some(window), STATUS_LABEL) {
                        let value = wide(&text);
                        let _ = SetWindowTextW(label, PCWSTR(value.as_ptr()));
                    }
                    shown = text;
                }
            } else if message.message == UI_COMMAND {
                command(
                    window,
                    message.wParam.0 as u32,
                    poller,
                    observation.as_ref(),
                );
                continue;
            } else if message.message == UI_MENU {
                // Revoke stale icon data before opening, then retain this exact
                // mapping until modal tracking returns. Recheck at the click.
                if let Some(current) = observation.as_ref() {
                    apps.update(&current.apps, current.fresh());
                }
                match show_menu(window, message.wParam.0, &apps, observation.as_ref(), busy) {
                    Some(MenuChoice::Command(id)) => {
                        command(window, id, poller, observation.as_ref())
                    }
                    Some(MenuChoice::Playback(selection)) => {
                        let pending_text = format!(
                            "{} · PID {}\n{} request pending; result not yet verified.",
                            selection.label,
                            selection.process_id,
                            if selection.requested_muted {
                                "Mute"
                            } else {
                                "Unmute"
                            }
                        );
                        if busy
                            || !observation
                                .as_ref()
                                .is_some_and(|view| reference_is_current(view, &selection))
                        {
                            set_result(window, "Selection expired while the menu was open. Refresh and select the exact session again.");
                        } else if poller.request(WorkerCommand::Playback(selection)) {
                            busy = true;
                            set_result(window, &pending_text);
                        } else {
                            set_result(
                                window,
                                "A request is already queued. Please wait and select again.",
                            );
                        }
                    }
                    None => {}
                }
                continue;
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
            let _ = PostMessageW(
                Some(window),
                UI_COMMAND,
                WPARAM(wparam.0 & 0xffff),
                LPARAM(0),
            );
            LRESULT(0)
        }
        WM_CONTEXTMENU => {
            let _ = PostMessageW(Some(window), UI_MENU, WPARAM(1), LPARAM(0));
            LRESULT(0)
        }
        TRAY_MESSAGE => {
            match lparam.0 as u32 {
                WM_LBUTTONUP => show_panel(window),
                WM_RBUTTONUP => {
                    // With the legacy notify-icon protocol wParam is the
                    // actual uID. Do not substitute the primary icon's menu.
                    let _ = PostMessageW(Some(window), UI_MENU, wparam, LPARAM(0));
                }
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

fn displayed_rows(
    value: &serde_json::Value,
    previous: &[serde_json::Value],
    fresh: bool,
) -> Vec<serde_json::Value> {
    let mut rows: Vec<_> = value["entries"]
        .as_array()
        .map(|rows| rows.iter().take(8).cloned().collect())
        .unwrap_or_else(|| previous.to_vec());
    if !fresh {
        for row in &mut rows {
            row["state"] = serde_json::json!("unknown");
            row["playback_label"] = serde_json::json!("Playback state unknown");
            row["icon_b64"] = serde_json::Value::Null;
            row["actions_enabled"] = serde_json::json!(false);
            row["sessions"] = serde_json::json!([]);
        }
    }
    rows
}

// Each pinned entry has its own notification-area icon and exact uID-to-row
// mapping. Its menu captures one explicit session, never the first or all.
struct AppIcons {
    window: HWND,
    key: String,
    icons: Vec<(HICON, bool)>,
    data: Vec<NOTIFYICONDATAW>,
    rows: Vec<serde_json::Value>,
    uids: Vec<u32>,
    next_uid: u32,
}
impl AppIcons {
    fn new(window: HWND) -> Self {
        Self {
            window,
            key: String::new(),
            icons: vec![],
            data: vec![],
            rows: vec![],
            uids: vec![],
            next_uid: 2,
        }
    }
    fn clear(&mut self) {
        unsafe {
            for data in &self.data {
                let _ = Shell_NotifyIconW(NIM_DELETE, data);
            }
            self.data.clear();
            for index in 0..8 {
                if let Ok(child) = GetDlgItem(Some(self.window), 200 + index * 2) {
                    SendMessageW(child, STM_SETICON, Some(WPARAM(0)), Some(LPARAM(0)));
                }
            }
            for (icon, owned) in self.icons.drain(..) {
                if owned {
                    let _ = DestroyIcon(icon);
                }
            }
        }
    }
    fn update(&mut self, value: &serde_json::Value, fresh: bool) {
        unsafe {
            let rows = displayed_rows(value, &self.rows, fresh);
            self.uids = stable_tray_ids(&self.rows, &self.uids, &rows, &mut self.next_uid);
            self.rows = rows.clone();
            let key = serde_json::to_string(&rows).unwrap_or_default();
            if self.key == key {
                // Explorer can restart while the panel is hidden. Failed modify
                // re-adds only our icons, without spawning another resident entry.
                for data in &self.data {
                    if !Shell_NotifyIconW(NIM_MODIFY, data).as_bool() {
                        let _ = Shell_NotifyIconW(NIM_ADD, data);
                    }
                }
                return;
            }
            self.clear();
            self.key = key;
            for index in 0..8 {
                let Some(row) = rows.get(index as usize) else {
                    for id in [200 + index * 2, 201 + index * 2] {
                        if let Ok(child) = GetDlgItem(Some(self.window), id) {
                            let _ = ShowWindow(child, SW_HIDE);
                        }
                    }
                    continue;
                };
                let label = row["label"].as_str().unwrap_or("Software");
                let status = match row["state"].as_str() {
                    Some("not_connected") => "No playback session",
                    Some("connected") if row["complete"] != true => "Partial playback observation",
                    Some("connected") => row["playback_label"]
                        .as_str()
                        .unwrap_or("Playback observed"),
                    _ => "Playback state unknown",
                };
                let resource = row["icon_b64"]
                    .as_str()
                    .and_then(crate::resident_apps::decode_icon);
                let created = resource.and_then(|bytes| {
                    let payload = &bytes[22..];
                    // presbits must be DWORD-aligned; the ICO payload at offset
                    // 22 is not. Copy into aligned storage without padding the
                    // byte length passed to the decoder.
                    let mut aligned = vec![0u32; payload.len().div_ceil(4)];
                    let data = std::slice::from_raw_parts_mut(
                        aligned.as_mut_ptr().cast::<u8>(),
                        payload.len(),
                    );
                    data.copy_from_slice(payload);
                    CreateIconFromResourceEx(data, true, 0x00030000, 32, 32, IMAGE_FLAGS(0)).ok()
                });
                let owned = created.is_some();
                let icon =
                    created.unwrap_or_else(|| LoadIconW(None, IDI_APPLICATION).unwrap_or_default());
                if let Ok(child) = GetDlgItem(Some(self.window), 200 + index * 2) {
                    SendMessageW(
                        child,
                        STM_SETICON,
                        Some(WPARAM(icon.0 as usize)),
                        Some(LPARAM(0)),
                    );
                    let _ = ShowWindow(child, SW_SHOW);
                }
                if let Ok(child) = GetDlgItem(Some(self.window), 201 + index * 2) {
                    let text = wide(&format!("{label}\n{status}"));
                    let _ = SetWindowTextW(child, PCWSTR(text.as_ptr()));
                    let _ = ShowWindow(child, SW_SHOW);
                }
                let mut data = NOTIFYICONDATAW {
                    cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                    hWnd: self.window,
                    uID: self.uids[index as usize],
                    uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
                    uCallbackMessage: TRAY_MESSAGE,
                    hIcon: icon,
                    ..Default::default()
                };
                let tip = wide(&format!("{label}: {status}"));
                let count = (tip.len() - 1).min(data.szTip.len() - 1);
                data.szTip[..count].copy_from_slice(&tip[..count]);
                let _ = Shell_NotifyIconW(NIM_ADD, &data);
                self.data.push(data);
                self.icons.push((icon, owned));
            }
            let _ = SetWindowPos(
                self.window,
                None,
                0,
                0,
                440,
                PANEL_HEIGHT + rows.len() as i32 * 40,
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }
}
impl Drop for AppIcons {
    fn drop(&mut self) {
        self.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture_observation() -> Observation {
        Observation {
            port: Some(43128),
            checked: Some(Instant::now()),
            generation: 7,
            apps: json!({"age_ms":0,"entries":[{"id":"a".repeat(64),"label":"Synthetic fixture, not live audio","icon_b64":"old icon","actions_enabled":true,"sessions":[
                {"session_reference":"b".repeat(64),"process_id":41,"display_name":"First fixture","muted":false,"active":true,"actions_enabled":true},
                {"session_reference":"c".repeat(64),"process_id":42,"display_name":"Second fixture","muted":true,"active":false,"actions_enabled":true}
            ]}]}),
        }
    }

    #[test]
    fn tray_uid_routes_to_exact_displayed_row_and_never_falls_back() {
        let rows = vec![json!({"label":"first"}), json!({"label":"second"})];
        assert_eq!(tray_row(&rows, &[2, 3], 2).unwrap()["label"], "first");
        assert_eq!(tray_row(&rows, &[2, 3], 3).unwrap()["label"], "second");
        for uid in [0, 1, 4, usize::MAX] {
            assert!(tray_row(&rows, &[2, 3], uid).is_none());
        }
    }

    #[test]
    fn removed_tray_ids_are_never_reassigned_to_another_entry() {
        let old = vec![json!({"id":"one"}), json!({"id":"two"})];
        let rows = vec![json!({"id":"two"}), json!({"id":"three"})];
        let mut next = 4;
        let ids = stable_tray_ids(&old, &[2, 3], &rows, &mut next);
        assert_eq!(ids, vec![3, 4]);
        assert!(tray_row(&rows, &ids, 2).is_none());
        assert_eq!(tray_row(&rows, &ids, 3).unwrap()["id"], "two");
        assert_eq!(next, 5);
    }

    #[test]
    fn each_session_has_fixed_explicit_actions_and_frozen_identity() {
        let mut observation = fixture_observation();
        let menu = session_menu_entries(&observation.apps["entries"][0], observation.generation);
        assert_eq!(menu.len(), 2);
        assert!(menu[0].description.contains("PID 41"));
        assert!(menu[1].description.contains("PID 42"));
        assert_eq!(menu[1].mute.process_id, 42);
        assert_eq!(menu[1].mute.session_reference, "c".repeat(64));
        assert!(menu[1].mute.requested_muted);
        assert!(!menu[1].unmute.requested_muted);
        observation.apps["entries"][0]["sessions"][1]["session_reference"] = json!("d".repeat(64));
        // A refresh cannot turn this chosen second session into the replacement.
        assert_eq!(menu[1].mute.session_reference, "c".repeat(64));
        assert!(!reference_is_current(&observation, &menu[1].mute));
        assert!(reference_is_current(&observation, &menu[0].mute));
    }

    #[test]
    fn tray_id_exhaustion_is_sticky_and_never_reuses_primary_or_removed_ids() {
        let mut next = u32::MAX;
        assert_eq!(
            stable_tray_ids(&[], &[], &[serde_json::json!({"id":"last"})], &mut next),
            vec![u32::MAX]
        );
        for _ in 0..4 {
            assert_eq!(
                stable_tray_ids(&[], &[], &[serde_json::json!({"id":"new"})], &mut next),
                vec![0]
            );
            assert_eq!(next, 0);
        }
        assert!(tray_row(&[serde_json::json!({"id":"new"})], &[0], 0).is_none());
    }

    #[test]
    fn freshness_counts_source_age_network_time_and_service_replacement() {
        let mut observation = fixture_observation();
        let selected =
            session_menu_entries(&observation.apps["entries"][0], observation.generation)
                .remove(0)
                .mute;
        let started = Instant::now() - Duration::from_millis(2200);
        observation.checked = observation_time(started, &json!({"age_ms":2100}));
        assert!(!observation.fresh());
        assert!(!reference_is_current(&observation, &selected));
        assert!(observation_time(Instant::now(), &json!({})).is_none());
        observation.checked = Some(Instant::now());
        observation.generation += 1;
        assert!(!reference_is_current(&observation, &selected));
        observation.generation = selected.generation;
        assert!(reference_is_current(&observation, &selected));
        observation.apps["entries"][0]["sessions"][0]["actions_enabled"] = json!(false);
        assert!(!reference_is_current(&observation, &selected));
    }

    #[test]
    fn stale_or_unproved_rows_clear_icons_references_and_actions() {
        let observation = fixture_observation();
        let rows = displayed_rows(&observation.apps, &[], true);
        assert!(rows[0]["icon_b64"].is_string());
        let stale = displayed_rows(&serde_json::Value::Null, &rows, false);
        assert_eq!(stale[0]["state"], "unknown");
        assert!(stale[0]["icon_b64"].is_null());
        assert_eq!(stale[0]["actions_enabled"], false);
        assert!(stale[0]["sessions"].as_array().unwrap().is_empty());
        assert!(session_menu_entries(&stale[0], 7).is_empty());
        assert!(displayed_rows(&json!({"entries":[]}), &stale, true).is_empty());
    }
}
