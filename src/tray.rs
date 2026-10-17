//! Windows resident entry: notification icons and authenticated Web Settings.
//!
//! The hidden owner keeps the tray and compact software strip alive; only Exit stops the service.
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
    core::{w, PCWSTR, PWSTR},
    Win32::{
        Foundation::{
            CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE, HINSTANCE, HWND, LPARAM,
            LRESULT, POINT, RECT, WPARAM,
        },
        Graphics::Gdi::{
            GetStockObject, GetSysColorBrush, InvalidateRect, COLOR_WINDOW, DEFAULT_GUI_FONT,
        },
        System::{
            LibraryLoader::GetModuleHandleW,
            Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD},
            Threading::CreateMutexW,
        },
        UI::{
            Controls::{
                InitCommonControlsEx, ICC_WIN95_CLASSES, INITCOMMONCONTROLSEX, TTF_IDISHWND,
                TTF_SUBCLASS, TTM_ADDTOOLW, TTM_UPDATETIPTEXTW, TTS_ALWAYSTIP, TTS_NOPREFIX,
                TTTOOLINFOW,
            },
            Shell::{
                ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP,
                NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW,
            },
            WindowsAndMessaging::*,
        },
    },
};

const TRAY_MESSAGE: u32 = WM_APP + 1;
const OPEN_SETTINGS: u32 = WM_APP + 2;
const UI_COMMAND: u32 = WM_APP + 3;
const UI_MENU: u32 = WM_APP + 4;
const FIRST_SESSION_COMMAND: u32 = 1000;
const MENU_OPEN: u32 = 101;
const MENU_STATUS: u32 = 102;
const MENU_EXIT: u32 = 103;
const TIMER: usize = 1;
const CLASS: &str = "VibDockTray";
const TRAY_MUTEX: &str = "VibDockTraySingleton";
const STRIP_CELL: i32 = 44;
const STRIP_CLASS: PCWSTR = w!("VibDockSoftwareStrip");
// SDK TTTOOLINFOW_V2_SIZE also works without a common-controls v6 manifest.
const TOOLINFO_SIZE: u32 = std::mem::offset_of!(TTTOOLINFOW, lpReserved) as u32;

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
    microphone_checked: Option<Instant>,
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
            microphone_checked: None,
            generation,
        }
    }
    fn microphone_fresh(&self) -> bool {
        self.port.is_some()
            && self
                .microphone_checked
                .is_some_and(|checked| checked.elapsed() < Duration::from_secs(4))
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

fn receipt_is_terminal(state: &service::PlaybackReceiptState) -> bool {
    // A delayed execute can still be queued behind this receipt GET. Prepared
    // is not proof that an attempted request will never execute.
    !matches!(
        state,
        service::PlaybackReceiptState::Prepared | service::PlaybackReceiptState::Pending
    )
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
        "{message}\n{} · PID {}",
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
                        if receipt_is_terminal(&receipt.state) {
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
                    microphone_checked: observation_time(started, &apps["microphone"]),
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

/// Reuses the existing named mutex. A second launch opens authenticated Settings
/// through the original tray, without adding another icon or service.
pub fn run() -> crate::Result<()> {
    let _singleton = match TraySingleton::acquire()? {
        Some(guard) => guard,
        None => {
            let class = wide(CLASS);
            // The first launcher may still be between its mutex and window.
            for _ in 0..40 {
                if let Ok(window) = unsafe { FindWindowW(PCWSTR(class.as_ptr()), None) } {
                    unsafe {
                        PostMessageW(Some(window), OPEN_SETTINGS, WPARAM(0), LPARAM(0))?;
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
            ..Default::default()
        };
        if RegisterClassW(&wc) == 0 {
            return Err(windows::core::Error::from_win32().into());
        }
        let strip_class = WNDCLASSW {
            lpfnWndProc: Some(strip_proc),
            lpszClassName: STRIP_CLASS,
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            ..wc
        };
        if RegisterClassW(&strip_class) == 0 {
            let _ = UnregisterClassW(PCWSTR(class.as_ptr()), Some(instance));
            return Err(windows::core::Error::from_win32().into());
        }
        let title = wide("VibDock");
        // A hidden top-level owner receives Explorer's TaskbarCreated broadcast.
        let window = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            PCWSTR(class.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(instance),
            None,
        )?;
        let result = (|| -> crate::Result<()> {
            add_icon(window)?;
            if SetTimer(Some(window), TIMER, 250, None) == 0 {
                return Err(windows::core::Error::from_win32().into());
            }
            let (poller, receiver) = Poller::start()?;
            let _ = poller.request(WorkerCommand::Settings);
            message_loop(window, receiver, &poller)
        })();
        let _ = KillTimer(Some(window), TIMER);
        let _ = Shell_NotifyIconW(NIM_DELETE, &icon(window));
        let _ = DestroyWindow(window);
        let _ = UnregisterClassW(STRIP_CLASS, Some(instance));
        let _ = UnregisterClassW(PCWSTR(class.as_ptr()), Some(instance));
        result
    }
}

fn icon(window: HWND) -> NOTIFYICONDATAW {
    let tip = wide("VibDock — Settings / right-click for controls");
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
                (MENU_OPEN, "Settings…"),
                (MENU_STATUS, "Service status"),
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
fn notification_text<const N: usize>(value: &str) -> [u16; N] {
    let mut result = [0; N];
    let text = wide(value);
    let mut count = (text.len() - 1).min(N.saturating_sub(1));
    if count > 0 && (0xd800..=0xdbff).contains(&text[count - 1]) {
        count -= 1;
    }
    result[..count].copy_from_slice(&text[..count]);
    result
}
fn notify_playback(window: HWND, result: &str) {
    let mut data = icon(window);
    data.uFlags = NIF_INFO;
    data.szInfoTitle = notification_text("VibDock playback result");
    data.szInfo = notification_text(result);
    data.dwInfoFlags = NIIF_INFO;
    if !unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) }.as_bool() {
        notify("VibDock notification unavailable", result, MB_ICONWARNING);
    }
}
fn command(id: u32, poller: &Poller, observation: Option<&Observation>) {
    let requested = match id {
        MENU_OPEN => Some(poller.request(WorkerCommand::Settings)),
        MENU_EXIT => Some(poller.request(WorkerCommand::Shutdown)),
        MENU_STATUS => {
            report_status(observation);
            None
        }
        // Session commands are accepted only from this menu's frozen returned
        // binding. A posted numeric WM_COMMAND is not a playback selection.
        _ => None,
    };
    if requested == Some(false) {
        notify(
            "VibDock",
            "A request is already queued. Please wait and try again.",
            MB_ICONINFORMATION,
        );
    }
}
fn message_loop(
    window: HWND,
    receiver: mpsc::Receiver<WorkerEvent>,
    poller: &Poller,
) -> crate::Result<()> {
    let mut observation: Option<Observation> = None;
    let mut busy = false;
    let mut apps = AppIcons::new(window)?;
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
                            notify_playback(window, &result);
                        }
                        WorkerEvent::Notice(message) => {
                            notify("VibDock settings", &message, MB_ICONERROR)
                        }
                        WorkerEvent::Shutdown(Ok(_)) => {
                            PostQuitMessage(0);
                        }
                        WorkerEvent::Shutdown(Err(error)) => {
                            busy = false;
                            notify("VibDock could not exit", &format!("{error}\n\nPlayback outcome may be unconfirmed after cancelling its receipt check. The tray remains available. Retry Exit to stop the verified service."), MB_ICONWARNING);
                        }
                    }
                }
                if let Some(current) = observation.as_ref() {
                    apps.update(&current.apps, current.fresh(), current.microphone_fresh());
                }
            } else if message.message == UI_COMMAND {
                command(message.wParam.0 as u32, poller, observation.as_ref());
                continue;
            } else if message.message == UI_MENU {
                // Revoke stale icon data before opening, then retain this exact
                // mapping until modal tracking returns. Recheck at the click.
                if let Some(current) = observation.as_ref() {
                    apps.update(&current.apps, current.fresh(), current.microphone_fresh());
                }
                match show_menu(window, message.wParam.0, &apps, observation.as_ref(), busy) {
                    Some(MenuChoice::Command(id)) => command(id, poller, observation.as_ref()),
                    Some(MenuChoice::Playback(selection)) => {
                        if busy
                            || !observation
                                .as_ref()
                                .is_some_and(|view| reference_is_current(view, &selection))
                        {
                            notify("VibDock playback", "Selection expired while the menu was open. Refresh and select the exact session again.", MB_ICONWARNING);
                        } else if poller.request(WorkerCommand::Playback(selection)) {
                            busy = true;
                        } else {
                            notify(
                                "VibDock playback",
                                "A request is already queued. Please wait and select again.",
                                MB_ICONINFORMATION,
                            );
                        }
                    }
                    None => {}
                }
                continue;
            }
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

unsafe extern "system" fn window_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // Explorer restarts remove notification icons. Restore ours without
    // restarting or changing the service.
    let taskbar_created = RegisterWindowMessageW(PCWSTR(wide("TaskbarCreated").as_ptr()));
    if taskbar_created != 0 && message == taskbar_created {
        if add_icon(window).is_err() {
            notify("VibDock tray unavailable", "Windows could not restore the notification icon. Reopen VibDock to access Settings.", MB_ICONWARNING);
        }
        return LRESULT(0);
    }
    match message {
        OPEN_SETTINGS => {
            let _ = PostMessageW(
                Some(window),
                UI_COMMAND,
                WPARAM(MENU_OPEN as usize),
                LPARAM(0),
            );
            LRESULT(0)
        }
        WM_SETTINGCHANGE => {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &icon(window));
            DefWindowProcW(window, message, wparam, lparam)
        }
        WM_CLOSE => LRESULT(0),
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
                WM_LBUTTONUP if wparam.0 == 1 => {
                    let _ = PostMessageW(Some(window), OPEN_SETTINGS, WPARAM(0), LPARAM(0));
                }
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

fn playback_badge(row: &serde_json::Value) -> (&'static str, &'static str) {
    let unknown = ("?", "Playback unknown");
    if row["complete"] != true {
        return unknown;
    }
    if row["state"] == "not_connected" {
        return ("–", "No playback session");
    }
    let Some(sessions) = row["sessions"]
        .as_array()
        .filter(|sessions| !sessions.is_empty())
    else {
        return unknown;
    };
    if row["state"] != "connected"
        || sessions.iter().any(|session| {
            session["muted"].as_bool().is_none() || session["active"].as_bool().is_none()
        })
    {
        return unknown;
    }
    if sessions.iter().all(|session| session["muted"] == true) {
        ("×", "Playback all muted")
    } else if sessions.iter().any(|session| session["active"] == true) {
        ("▶", "Playback session active")
    } else {
        ("II", "Playback session idle")
    }
}

fn microphone_presentation(value: &serde_json::Value, fresh: bool) -> (&'static str, String) {
    let device = value["device_id"]
        .as_str()
        .filter(|id| !id.is_empty() && id.len() <= 512 && !id.chars().any(char::is_control));
    let read_failed = value.get("error").is_some_and(|error| !error.is_null());
    let (badge, state) = match value["state"].as_str() {
        Some("unconfigured")
            if fresh
                && value
                    .get("device_id")
                    .is_some_and(serde_json::Value::is_null)
                && !read_failed =>
        {
            ("?", "not configured")
        }
        Some("muted") if fresh && device.is_some() && !read_failed => ("×", "muted"),
        Some("unmuted") if fresh && device.is_some() && !read_failed => ("○", "unmuted"),
        Some("disconnected") if fresh && device.is_some() => ("–", "disconnected or disabled"),
        _ => ("?", "state unknown"),
    };
    let target = if fresh {
        device.map(|id| {
            visible_text(
                value["name"]
                    .as_str()
                    .filter(|name| !name.is_empty())
                    .unwrap_or(id),
            )
        })
    } else {
        None
    };
    let target = target.map(|name| format!(" {name}")).unwrap_or_default();
    (badge, format!("Microphone{target}: {state}. Endpoint mute affects every application using this device. App voice and meeting state unknown. Opens Settings."))
}

fn strip_style() -> WINDOW_EX_STYLE {
    WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST
}

fn strip_layout(work: RECT, count: usize) -> Option<RECT> {
    let width = (count.min(8) + 1) as i32 * STRIP_CELL;
    if work.right - work.left < width + 16 || work.bottom - work.top < STRIP_CELL + 16 {
        return None;
    }
    Some(RECT {
        left: work.left + 8,
        top: work.bottom - STRIP_CELL - 8,
        right: work.left + 8 + width,
        bottom: work.bottom - 8,
    })
}

unsafe extern "system" fn strip_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_COMMAND if lparam.0 != 0 => {
            if let Ok(owner) = GetWindow(window, GW_OWNER) {
                let _ = PostMessageW(Some(owner), OPEN_SETTINGS, WPARAM(0), LPARAM(0));
            }
            LRESULT(0)
        }
        WM_CONTEXTMENU => {
            let uid = GetPropW(HWND(wparam.0 as *mut _), w!("VibDockUID")).0 as usize;
            if uid >= 1 {
                if let Ok(owner) = GetWindow(window, GW_OWNER) {
                    let _ = PostMessageW(Some(owner), UI_MENU, WPARAM(uid), LPARAM(0));
                }
            }
            LRESULT(0)
        }
        WM_CLOSE => LRESULT(0),
        _ => DefWindowProcW(window, message, wparam, lparam),
    }
}

// Each pinned entry has its own notification-area icon and exact uID-to-row
// mapping. Its menu captures one explicit session, never the first or all.
struct AppIcons {
    window: HWND,
    strip: HWND,
    tooltip: HWND,
    tips: Vec<Vec<u16>>,
    key: String,
    microphone_key: String,
    icons: Vec<(HICON, bool)>,
    data: Vec<NOTIFYICONDATAW>,
    rows: Vec<serde_json::Value>,
    uids: Vec<u32>,
    next_uid: u32,
}
impl AppIcons {
    fn new(window: HWND) -> crate::Result<Self> {
        let mut apps = Self {
            window,
            strip: HWND::default(),
            tooltip: HWND::default(),
            tips: (0..9).map(|_| wide("")).collect(),
            key: String::new(),
            microphone_key: String::new(),
            icons: vec![],
            data: vec![],
            rows: vec![],
            uids: vec![],
            next_uid: 2,
        };
        unsafe {
            let common = INITCOMMONCONTROLSEX {
                dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_WIN95_CLASSES,
            };
            if !InitCommonControlsEx(&common).as_bool() {
                return Err(windows::core::Error::from_win32().into());
            }
            let instance = HINSTANCE(GetModuleHandleW(None)?.0);
            apps.strip = CreateWindowExW(
                strip_style(),
                STRIP_CLASS,
                w!("VibDock microphone and playback indicators; voice and meeting state unknown"),
                WS_POPUP,
                0,
                0,
                0,
                0,
                Some(window),
                None,
                Some(instance),
                None,
            )?;
            apps.tooltip = CreateWindowExW(
                strip_style(),
                w!("tooltips_class32"),
                None,
                WS_POPUP | WINDOW_STYLE(TTS_ALWAYSTIP | TTS_NOPREFIX),
                0,
                0,
                0,
                0,
                Some(apps.strip),
                None,
                Some(instance),
                None,
            )?;
            for index in 0..9 {
                let microphone = index == 8;
                let button = CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("BUTTON"),
                    None,
                    WS_CHILD | WS_CLIPCHILDREN | WINDOW_STYLE((BS_ICON | BS_FLAT) as u32),
                    if microphone {
                        0
                    } else {
                        (index + 1) * STRIP_CELL
                    },
                    0,
                    STRIP_CELL,
                    STRIP_CELL,
                    Some(apps.strip),
                    Some(HMENU(if microphone {
                        199usize
                    } else {
                        (200 + index) as usize
                    } as *mut _)),
                    Some(instance),
                    None,
                )?;
                let badge = CreateWindowExW(
                    WINDOW_EX_STYLE(0),
                    w!("STATIC"),
                    w!("?"),
                    WS_CHILD | WS_VISIBLE | WS_DISABLED | WINDOW_STYLE(1), // SS_CENTER
                    27,
                    27,
                    17,
                    17,
                    Some(button),
                    Some(HMENU(std::ptr::without_provenance_mut(1))),
                    Some(instance),
                    None,
                )?;
                SendMessageW(
                    badge,
                    WM_SETFONT,
                    Some(WPARAM(GetStockObject(DEFAULT_GUI_FONT).0 as usize)),
                    Some(LPARAM(1)),
                );
                if microphone {
                    let label = CreateWindowExW(
                        WINDOW_EX_STYLE(0),
                        w!("STATIC"),
                        w!("MIC"),
                        WS_CHILD | WS_VISIBLE | WS_DISABLED | WINDOW_STYLE(1),
                        0,
                        8,
                        STRIP_CELL,
                        19,
                        Some(button),
                        Some(HMENU(std::ptr::without_provenance_mut(2))),
                        Some(instance),
                        None,
                    )?;
                    SendMessageW(
                        label,
                        WM_SETFONT,
                        Some(WPARAM(GetStockObject(DEFAULT_GUI_FONT).0 as usize)),
                        Some(LPARAM(1)),
                    );
                }
                let tool = TTTOOLINFOW {
                    cbSize: TOOLINFO_SIZE,
                    uFlags: TTF_IDISHWND | TTF_SUBCLASS,
                    hwnd: apps.strip,
                    uId: button.0 as usize,
                    lpszText: PWSTR(apps.tips[index as usize].as_mut_ptr()),
                    ..Default::default()
                };
                if SendMessageW(
                    apps.tooltip,
                    TTM_ADDTOOLW,
                    None,
                    Some(LPARAM(&tool as *const _ as isize)),
                )
                .0 == 0
                {
                    return Err("Could not register a software status tooltip.".into());
                }
            }
        }
        apps.update_microphone(&serde_json::Value::Null, false);
        apps.place_strip();
        Ok(apps)
    }
    fn clear(&mut self) {
        unsafe {
            for data in &self.data {
                let _ = Shell_NotifyIconW(NIM_DELETE, data);
            }
            self.data.clear();
            if !self.strip.is_invalid() {
                for index in 0..8 {
                    if let Ok(button) = GetDlgItem(Some(self.strip), 200 + index) {
                        SendMessageW(
                            button,
                            BM_SETIMAGE,
                            Some(WPARAM(IMAGE_ICON.0 as usize)),
                            Some(LPARAM(0)),
                        );
                        let _ = RemovePropW(button, w!("VibDockUID"));
                        let _ = ShowWindow(button, SW_HIDE);
                    }
                }
            }
            for (icon, owned) in self.icons.drain(..) {
                if owned {
                    let _ = DestroyIcon(icon);
                }
            }
        }
    }
    fn update(&mut self, value: &serde_json::Value, fresh: bool, microphone_fresh: bool) {
        self.update_microphone(&value["microphone"], microphone_fresh);
        unsafe {
            let rows = displayed_rows(value, &self.rows, fresh);
            self.uids = stable_tray_ids(&self.rows, &self.uids, &rows, &mut self.next_uid);
            self.rows = rows.clone();
            let complete = fresh && value["complete"] == true;
            let key = serde_json::to_string(&(&rows, complete)).unwrap_or_default();
            if self.key == key {
                // Explorer can restart while the owner is hidden. Failed modify
                // re-adds only our icons, without spawning another resident entry.
                for data in &self.data {
                    if !Shell_NotifyIconW(NIM_MODIFY, data).as_bool() {
                        let _ = Shell_NotifyIconW(NIM_ADD, data);
                    }
                }
                self.place_strip();
                return;
            }
            self.clear();
            self.key = key;
            for (index, row) in rows.iter().enumerate() {
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
                let mut data = NOTIFYICONDATAW {
                    cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                    hWnd: self.window,
                    uID: self.uids[index],
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
                let (badge, playback) = if complete {
                    playback_badge(row)
                } else {
                    ("?", "Playback unknown")
                };
                let name = format!("{label}: {playback}. Voice activity unknown. Opens Settings.");
                if self.configure_button(index, icon, badge, &name).is_err() {
                    if let Ok(button) = GetDlgItem(Some(self.strip), 200 + index as i32) {
                        let _ = ShowWindow(button, SW_HIDE);
                    }
                }
            }
            self.place_strip();
        }
    }
    fn configure_button(
        &mut self,
        index: usize,
        icon: HICON,
        badge: &str,
        name: &str,
    ) -> windows::core::Result<HWND> {
        unsafe {
            let microphone = index == 8;
            let button = GetDlgItem(
                Some(self.strip),
                if microphone { 199 } else { 200 + index as i32 },
            )?;
            let badge_control = GetDlgItem(Some(button), 1)?;
            let next_tip = wide(name);
            SetPropW(
                button,
                w!("VibDockUID"),
                Some(HANDLE(if microphone {
                    1usize
                } else {
                    self.uids[index] as usize
                } as *mut _)),
            )?;
            SetWindowTextW(button, PCWSTR(next_tip.as_ptr()))?;
            SetWindowTextW(badge_control, PCWSTR(wide(badge).as_ptr()))?;
            SendMessageW(
                button,
                BM_SETIMAGE,
                Some(WPARAM(IMAGE_ICON.0 as usize)),
                Some(LPARAM(icon.0 as isize)),
            );
            let previous_tip = std::mem::replace(&mut self.tips[index], next_tip);
            let tool = TTTOOLINFOW {
                cbSize: TOOLINFO_SIZE,
                uFlags: TTF_IDISHWND | TTF_SUBCLASS,
                hwnd: self.strip,
                uId: button.0 as usize,
                lpszText: PWSTR(self.tips[index].as_mut_ptr()),
                ..Default::default()
            };
            SendMessageW(
                self.tooltip,
                TTM_UPDATETIPTEXTW,
                None,
                Some(LPARAM(&tool as *const _ as isize)),
            );
            drop(previous_tip);
            let _ = ShowWindow(button, SW_SHOWNOACTIVATE);
            let _ = InvalidateRect(Some(button), None, true);
            Ok(button)
        }
    }
    fn update_microphone(&mut self, value: &serde_json::Value, fresh: bool) {
        let (badge, name) = microphone_presentation(value, fresh);
        if self.microphone_key == name {
            return;
        }
        if self
            .configure_button(8, HICON::default(), badge, &name)
            .is_ok()
        {
            self.microphone_key = name;
        } else if let Ok(button) = unsafe { GetDlgItem(Some(self.strip), 199) } {
            unsafe {
                let _ = ShowWindow(button, SW_HIDE);
            }
            self.microphone_key.clear();
        }
    }
    fn place_strip(&self) {
        unsafe {
            // shortcut: primary work area only; add per-monitor placement when requested.
            let mut work = RECT::default();
            let area = SystemParametersInfoW(
                SPI_GETWORKAREA,
                0,
                Some((&mut work as *mut RECT).cast()),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
            .ok()
            .and_then(|_| strip_layout(work, self.rows.len()));
            if let Some(area) = area {
                if SetWindowPos(
                    self.strip,
                    Some(HWND_TOPMOST),
                    area.left,
                    area.top,
                    area.right - area.left,
                    STRIP_CELL,
                    SWP_NOACTIVATE | SWP_SHOWWINDOW,
                )
                .is_ok()
                {
                    return;
                }
            }
            let _ = ShowWindow(self.strip, SW_HIDE);
        }
    }
}
impl Drop for AppIcons {
    fn drop(&mut self) {
        self.clear();
        unsafe {
            if !self.tooltip.is_invalid() {
                let _ = DestroyWindow(self.tooltip);
            }
            if !self.strip.is_invalid() {
                let _ = DestroyWindow(self.strip);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    #[ignore = "Bounded native fixture view for project-owned dcc-cua inspection; no live audio"]
    fn native_strip_render_probe() -> crate::Result<()> {
        unsafe extern "system" fn probe_owner(
            window: HWND,
            message: u32,
            wparam: WPARAM,
            lparam: LPARAM,
        ) -> LRESULT {
            match message {
                OPEN_SETTINGS | UI_COMMAND | UI_MENU | WM_CLOSE => LRESULT(0),
                _ => DefWindowProcW(window, message, wparam, lparam),
            }
        }
        struct ProbeWindows {
            instance: HINSTANCE,
            owner: HWND,
            owner_class: bool,
            strip_class: bool,
        }
        impl Drop for ProbeWindows {
            fn drop(&mut self) {
                unsafe {
                    if !self.owner.is_invalid() {
                        let _ = DestroyWindow(self.owner);
                    }
                    if self.strip_class {
                        let _ = UnregisterClassW(STRIP_CLASS, Some(self.instance));
                    }
                    if self.owner_class {
                        let _ = UnregisterClassW(w!("VibDockStripProbeOwner"), Some(self.instance));
                    }
                }
            }
        }
        unsafe {
            let instance = HINSTANCE(GetModuleHandleW(None)?.0);
            let mut windows = ProbeWindows {
                instance,
                owner: HWND::default(),
                owner_class: false,
                strip_class: false,
            };
            let owner_class = WNDCLASSW {
                lpfnWndProc: Some(probe_owner),
                hInstance: instance,
                lpszClassName: w!("VibDockStripProbeOwner"),
                ..Default::default()
            };
            if RegisterClassW(&owner_class) == 0 {
                return Err(windows::core::Error::from_win32().into());
            }
            windows.owner_class = true;
            let strip_class = WNDCLASSW {
                lpfnWndProc: Some(strip_proc),
                lpszClassName: STRIP_CLASS,
                hbrBackground: GetSysColorBrush(COLOR_WINDOW),
                ..owner_class
            };
            if RegisterClassW(&strip_class) == 0 {
                return Err(windows::core::Error::from_win32().into());
            }
            windows.strip_class = true;
            windows.owner = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                owner_class.lpszClassName,
                w!("VibDock synthetic strip probe; no live app/audio"),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance),
                None,
            )?;
            let foreground = GetForegroundWindow();
            let mut apps = AppIcons::new(windows.owner)?;
            assert!(
                IsWindowVisible(apps.strip).as_bool(),
                "Microphone cell must be visible with no software pins"
            );
            apps.update_microphone(&json!({"device_id":"synthetic-microphone","name":"Synthetic microphone fixture; no live hardware","state":"muted"}), true);
            apps.rows = vec![
                json!({"complete":true,"state":"connected","sessions":[{"active":true,"muted":false}]}),
                json!({"complete":true,"state":"connected","sessions":[{"active":true,"muted":true}]}),
                json!({"complete":true,"state":"connected","sessions":[{"active":false,"muted":false}]}),
                json!({"complete":false,"state":"unknown","sessions":[]}),
            ];
            apps.uids = vec![2, 3, 4, 5];
            let microphone = GetDlgItem(Some(apps.strip), 199)?;
            let mut microphone_name = [0u16; 512];
            let microphone_len = GetWindowTextW(microphone, &mut microphone_name) as usize;
            let mut buttons = vec![
                json!({"hwnd":microphone.0 as usize,"name":String::from_utf16_lossy(&microphone_name[..microphone_len])}),
            ];
            for (index, resource) in [IDI_INFORMATION, IDI_WARNING, IDI_APPLICATION, IDI_ERROR]
                .into_iter()
                .enumerate()
            {
                let icon = CopyIcon(LoadIconW(None, resource)?)?;
                apps.icons.push((icon, true));
                let (badge, playback) = playback_badge(&apps.rows[index]);
                let name = format!("Synthetic fixture {}: {playback}. No live app/audio; voice unknown. Probe ignores Settings actions.", index + 1);
                let button = apps.configure_button(index, icon, badge, &name)?;
                let style = GetWindowLongPtrW(button, GWL_STYLE) as u32;
                assert_eq!(
                    style & 0x0f,
                    0,
                    "Native push-button semantics must remain enabled"
                );
                assert_ne!(style & BS_ICON as u32, 0);
                let badge_control = GetDlgItem(Some(button), 1)?;
                assert_ne!(
                    GetWindowLongPtrW(badge_control, GWL_STYLE) as u32 & WS_DISABLED.0,
                    0
                );
                assert_eq!(
                    SendMessageW(
                        button,
                        BM_GETIMAGE,
                        Some(WPARAM(IMAGE_ICON.0 as usize)),
                        None
                    )
                    .0,
                    icon.0 as isize
                );
                let mut text = [0u16; 256];
                let len = GetWindowTextW(button, &mut text) as usize;
                assert_eq!(String::from_utf16_lossy(&text[..len]), name);
                buttons.push(json!({"hwnd":button.0 as usize,"name":name}));
            }
            assert!(
                apps.data.is_empty(),
                "Probe must not add notification-area icons"
            );
            apps.place_strip();
            assert_eq!(GetForegroundWindow(), foreground);
            assert!(IsWindowVisible(apps.strip).as_bool());
            assert!(!IsWindowVisible(windows.owner).as_bool());
            let ex_style = GetWindowLongPtrW(apps.strip, GWL_EXSTYLE) as u32;
            for flag in [WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST] {
                assert_ne!(ex_style & flag.0, 0);
            }
            let mut work = RECT::default();
            SystemParametersInfoW(
                SPI_GETWORKAREA,
                0,
                Some((&mut work as *mut RECT).cast()),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )?;
            let expected = strip_layout(work, apps.rows.len()).unwrap();
            let mut actual = RECT::default();
            GetWindowRect(apps.strip, &mut actual)?;
            assert_eq!(
                (actual.left, actual.top, actual.right, actual.bottom),
                (expected.left, expected.top, expected.right, expected.bottom)
            );
            for index in 0..apps.rows.len() {
                let button = GetDlgItem(Some(apps.strip), 200 + index as i32)?;
                let mut rect = RECT::default();
                GetWindowRect(button, &mut rect)?;
                assert!(IsWindowVisible(button).as_bool());
                assert_eq!(rect.left, actual.left + (index + 1) as i32 * STRIP_CELL);
                assert_eq!(
                    (rect.right - rect.left, rect.bottom - rect.top),
                    (STRIP_CELL, STRIP_CELL)
                );
                assert!(rect.right <= work.right && rect.bottom <= work.bottom);
            }
            eprintln!(
                "VIBDOCK_NATIVE_STRIP_PROBE {}",
                json!({"pid":std::process::id(),"strip_hwnd":apps.strip.0 as usize,"owner_hwnd":windows.owner.0 as usize,"fixture_only":true,"duration_ms":30_000,"buttons":buttons})
            );
            let deadline = Instant::now() + Duration::from_secs(30);
            let mut message = MSG::default();
            while Instant::now() < deadline {
                while PeekMessageW(&mut message, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            let strip = apps.strip;
            drop(apps);
            assert!(!IsWindow(Some(strip)).as_bool());
        }
        Ok(())
    }

    #[test]
    fn strip_badges_require_complete_known_playback_and_never_infer_voice() {
        let mut row =
            json!({"complete":true,"state":"connected","sessions":[{"active":true,"muted":false}]});
        assert_eq!(playback_badge(&row), ("▶", "Playback session active"));
        row["sessions"][0]["muted"] = json!(true);
        assert_eq!(playback_badge(&row), ("×", "Playback all muted"));
        row["sessions"][0] = json!({"active":false,"muted":false});
        assert_eq!(playback_badge(&row), ("II", "Playback session idle"));
        row["complete"] = json!(false);
        assert_eq!(playback_badge(&row).0, "?");
        row["complete"] = json!(true);
        row["sessions"][0]["active"] = serde_json::Value::Null;
        assert_eq!(playback_badge(&row).0, "?");
        row["state"] = json!("not_connected");
        row["sessions"] = json!([]);
        assert_eq!(playback_badge(&row), ("–", "No playback session"));
        let stale = displayed_rows(&json!({"entries":[row]}), &[], false);
        assert_eq!(playback_badge(&stale[0]).0, "?");
    }

    #[test]
    fn microphone_indication_is_readback_only_and_endpoint_wide() {
        let mut value = json!({"device_id":"fixture-mic","name":"Synthetic microphone","state":"muted","error":null});
        for (state, badge) in [
            ("muted", "×"),
            ("unmuted", "○"),
            ("disconnected", "–"),
            ("unknown", "?"),
        ] {
            value["state"] = json!(state);
            let (actual, name) = microphone_presentation(&value, true);
            assert_eq!(actual, badge);
            assert!(name.contains("Synthetic microphone"));
            assert!(name.contains("every application"));
            assert!(name.contains("voice and meeting state unknown"));
        }
        value["state"] = json!("unmuted");
        value["error"] = json!("helper_timed_out");
        assert_eq!(microphone_presentation(&value, true).0, "?");
        value["error"] = serde_json::Value::Null;
        assert_eq!(microphone_presentation(&value, false).0, "?");
        value["device_id"] = json!("");
        assert_eq!(microphone_presentation(&value, true).0, "?");
        let (_, unconfigured) =
            microphone_presentation(&json!({"device_id":null,"state":"unconfigured"}), true);
        assert!(unconfigured.contains("not configured"));
        assert_eq!(
            microphone_presentation(&json!({"state":"unconfigured"}), true).0,
            "?"
        );
        assert_eq!(
            microphone_presentation(&serde_json::Value::Null, true).0,
            "?"
        );
    }

    #[test]
    fn microphone_freshness_counts_source_and_transport_age_independently() {
        let mut view = fixture_observation();
        assert!(view.microphone_fresh());
        view.checked = None;
        assert!(
            view.microphone_fresh(),
            "Unavailable playback is not a failed microphone read"
        );
        let started = Instant::now() - Duration::from_secs(3);
        view.microphone_checked = observation_time(started, &json!({"age_ms":1500}));
        assert!(!view.microphone_fresh());
        view.microphone_checked = observation_time(Instant::now(), &json!({"age_ms":0}));
        view.port = None;
        assert!(
            !view.microphone_fresh(),
            "A stale service identity cannot retain microphone authority"
        );
        assert!(!Observation::unavailable(9).microphone_fresh());
    }

    #[test]
    fn strip_layout_stays_inside_work_area_and_never_takes_focus() {
        let work = RECT {
            left: -1920,
            top: 40,
            right: 0,
            bottom: 1080,
        };
        let area = strip_layout(work, 12).unwrap();
        assert_eq!(area.right - area.left, 9 * STRIP_CELL);
        assert_eq!(area.bottom - area.top, STRIP_CELL);
        assert!(area.left >= work.left && area.right <= work.right);
        assert!(area.top >= work.top && area.bottom < work.bottom);
        let microphone_only = strip_layout(work, 0).unwrap();
        assert_eq!(microphone_only.right - microphone_only.left, STRIP_CELL);
        assert!(strip_layout(
            RECT {
                right: 40,
                bottom: 40,
                ..Default::default()
            },
            1
        )
        .is_none());
        for style in [WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW] {
            assert_ne!(strip_style().0 & style.0, 0);
        }
        assert_eq!(strip_style().0 & WS_EX_APPWINDOW.0, 0);
    }

    #[test]
    fn notification_text_keeps_null_termination_and_complete_utf16() {
        assert_eq!(notification_text::<4>("abcdef"), [97, 98, 99, 0]);
        assert_eq!(notification_text::<3>("a😀"), [97, 0, 0]);
        assert_eq!(notification_text::<4>("a😀b"), [97, 0xd83d, 0xde00, 0]);
        assert_eq!(notification_text::<1>("result"), [0]);
    }

    #[test]
    fn tooltip_contract_uses_the_sdk_v2_prefix() {
        assert_eq!(
            TOOLINFO_SIZE as usize,
            std::mem::offset_of!(TTTOOLINFOW, lParam) + std::mem::size_of::<LPARAM>()
        );
        assert!((TOOLINFO_SIZE as usize) < std::mem::size_of::<TTTOOLINFOW>());
    }

    fn fixture_observation() -> Observation {
        Observation {
            port: Some(43128),
            checked: Some(Instant::now()),
            microphone_checked: Some(Instant::now()),
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
    fn a_prepared_receipt_after_lost_execute_cannot_claim_no_write() {
        use service::PlaybackReceiptState::*;
        assert!(!receipt_is_terminal(&Prepared));
        assert!(!receipt_is_terminal(&Pending));
        for state in [
            Verified,
            VerifiedMismatch,
            RejectedBeforeWrite,
            OutcomeUnconfirmed,
        ] {
            assert!(receipt_is_terminal(&state));
        }
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
