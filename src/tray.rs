//! Windows resident entry: notification icons and authenticated Web Settings.
//!
//! The hidden owner keeps native notification icons alive; only Exit stops the service.
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
        Graphics::Gdi::{
            CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush, SelectObject,
            BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
        },
        System::{
            LibraryLoader::GetModuleHandleW,
            Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_DWORD},
            Threading::CreateMutexW,
        },
        UI::{
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
const UI_RESTORE_ICONS: u32 = WM_APP + 5;
const FIRST_SESSION_COMMAND: u32 = 1000;
const MENU_OPEN: u32 = 101;
const MENU_STATUS: u32 = 102;
const MENU_EXIT: u32 = 103;
const MENU_STARTUP: u32 = 104;
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
    Startup(StartupSelection),
}
enum WorkerEvent {
    Observation(Observation),
    ActionResult(String),
    Notice(String),
    Shutdown(std::result::Result<bool, String>),
    Startup(StartupObservation),
    StartupResult(StartupObservation, Option<String>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StartupState {
    Confirmed(bool),
    Conflict,
    Unknown,
    Unsupported,
}
#[derive(Clone)]
struct StartupObservation {
    state: StartupState,
    launcher_available: bool,
    checked: Option<Instant>,
    generation: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct StartupSelection {
    generation: u64,
    enabled: bool,
    expected_enabled: bool,
}
impl StartupObservation {
    fn unavailable(generation: u64) -> Self {
        Self {
            state: StartupState::Unknown,
            launcher_available: false,
            checked: None,
            generation,
        }
    }
    fn parse(value: &serde_json::Value, generation: u64, checked: Instant) -> Self {
        let state = if value["supported"] == false
            && value["platform"] == "unsupported"
            && value["state"] == "unsupported"
        {
            StartupState::Unsupported
        } else if value["platform"] == "windows"
            && value["supported"] == true
            && value["scope"] == "current_user"
        {
            match (
                value["readback_confirmed"].as_bool(),
                value["enabled"].as_bool(),
                value["state"].as_str(),
                value["error"].is_null(),
            ) {
                (Some(true), Some(true), Some("enabled"), true) => StartupState::Confirmed(true),
                (Some(true), Some(false), Some("disabled"), true) => StartupState::Confirmed(false),
                (_, None, Some("conflict"), _) => StartupState::Conflict,
                _ => StartupState::Unknown,
            }
        } else {
            StartupState::Unknown
        };
        Self {
            state,
            launcher_available: value["launcher_available"] == true,
            checked: Some(checked),
            generation,
        }
    }
    fn fresh(&self) -> bool {
        self.checked
            .is_some_and(|checked| checked.elapsed() < Duration::from_secs(4))
    }
}
fn startup_menu(
    view: &StartupObservation,
    busy: bool,
) -> (&'static str, MENU_ITEM_FLAGS, Option<StartupSelection>) {
    if !view.fresh() {
        return ("开机自启（状态未知）", MF_STRING | MF_GRAYED, None);
    }
    match view.state {
        StartupState::Confirmed(enabled) => {
            let flags = MF_STRING | if enabled { MF_CHECKED } else { MF_UNCHECKED };
            if busy {
                return ("开机自启（操作处理中）", flags | MF_GRAYED, None);
            }
            if !enabled && !view.launcher_available {
                return ("开机自启（启动器不可用）", flags | MF_GRAYED, None);
            }
            (
                "开机自启",
                flags,
                Some(StartupSelection {
                    generation: view.generation,
                    enabled: !enabled,
                    expected_enabled: enabled,
                }),
            )
        }
        StartupState::Conflict => ("开机自启（启动项冲突）", MF_STRING | MF_GRAYED, None),
        StartupState::Unsupported => ("开机自启（此平台不支持）", MF_STRING | MF_GRAYED, None),
        StartupState::Unknown => ("开机自启（状态未知）", MF_STRING | MF_GRAYED, None),
    }
}
fn startup_request(
    client: &service::ResidentControlClient,
    method: &str,
    body: Option<serde_json::Value>,
    generation: u64,
) -> StartupObservation {
    let checked = Instant::now();
    match client.request(method, "/api/startup", body, 200, 8192) {
        Ok(value) => StartupObservation::parse(&value, generation, checked),
        Err(_) => StartupObservation::unavailable(generation),
    }
}
fn update_startup(
    previous: &StartupObservation,
    mut next: StartupObservation,
    instance_changed: bool,
) -> StartupObservation {
    next.generation = previous.generation.wrapping_add(u64::from(
        instance_changed
            || previous.state != next.state
            || previous.launcher_available != next.launcher_available,
    ));
    next
}
fn apply_startup(
    selection: StartupSelection,
    latest: &StartupObservation,
    mut request: impl FnMut(Option<serde_json::Value>) -> StartupObservation,
) -> (StartupObservation, Option<String>) {
    if selection.generation != latest.generation
        || !latest.fresh()
        || latest.state != StartupState::Confirmed(selection.expected_enabled)
    {
        return (
            latest.clone(),
            Some("开机自启状态已过期，请重新打开菜单后再试。".into()),
        );
    }
    let result = request(Some(serde_json::json!({"enabled":selection.enabled})));
    if result.state == StartupState::Confirmed(selection.enabled) && result.fresh() {
        return (result, None);
    }
    (
        request(None),
        Some("开机自启设置未确认，已重新读取实际配置；请检查状态后再试。".into()),
    )
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

fn next_worker_command(
    commands: &mpsc::Receiver<WorkerCommand>,
    shutdown: &AtomicBool,
    settings: &AtomicBool,
) -> std::result::Result<WorkerCommand, mpsc::RecvTimeoutError> {
    let command = if shutdown.load(Ordering::Relaxed) || settings.load(Ordering::Relaxed) {
        commands.try_recv().map_err(|error| match error {
            mpsc::TryRecvError::Empty => mpsc::RecvTimeoutError::Timeout,
            mpsc::TryRecvError::Disconnected => mpsc::RecvTimeoutError::Disconnected,
        })
    } else {
        commands.recv_timeout(Duration::from_millis(50))
    };
    // Exit cannot be displaced by a queued Settings or startup request. Check
    // again after receiving because Exit may arrive while this worker waits.
    if shutdown.load(Ordering::Relaxed) {
        settings.store(false, Ordering::Relaxed);
        while commands.try_recv().is_ok() {}
        Ok(WorkerCommand::Shutdown)
    } else if matches!(&command, Err(mpsc::RecvTimeoutError::Timeout))
        && settings.swap(false, Ordering::Relaxed)
    {
        // Already accepted actions retain their queue order. Repeated Settings
        // requests consume no queue slot and become one later browser opening.
        Ok(WorkerCommand::Settings)
    } else {
        command
    }
}

// Recheck at execute admission after a potentially blocking preparation.
fn dispatch_prepared_playback<T>(shutdown: &AtomicBool, dispatch: impl FnOnce() -> T) -> Option<T> {
    if shutdown.load(Ordering::Relaxed) {
        None
    } else {
        Some(dispatch())
    }
}

fn poll_worker(
    sender: mpsc::SyncSender<WorkerEvent>,
    commands: mpsc::Receiver<WorkerCommand>,
    stop: Arc<AtomicBool>,
    shutdown: Arc<AtomicBool>,
    settings: Arc<AtomicBool>,
) {
    let mut client: Option<service::ResidentControlClient> = None;
    let mut latest: Option<Observation> = None;
    let mut pending: Option<PendingPlayback> = None;
    let mut generation = 0u64;
    let mut startup = StartupObservation::unavailable(generation);
    let mut startup_client: Option<service::ResidentControlClient> = None;
    let mut next_poll = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        match next_worker_command(&commands, &shutdown, &settings) {
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
            Ok(WorkerCommand::Startup(selection)) => {
                let (result, notice) = if pending.is_none() {
                    if let Some(client) = startup_client.as_ref() {
                        apply_startup(selection, &startup, |body| {
                            startup_request(
                                client,
                                if body.is_some() { "POST" } else { "GET" },
                                body,
                                startup.generation,
                            )
                        })
                    } else {
                        (
                            StartupObservation::unavailable(startup.generation),
                            Some("服务状态未知，开机自启设置未改变。".into()),
                        )
                    }
                } else {
                    (
                        startup.clone(),
                        Some("请等待当前操作完成后再更改开机自启。".into()),
                    )
                };
                startup = update_startup(&startup, result, false);
                if !send_worker_event(
                    &sender,
                    &stop,
                    WorkerEvent::StartupResult(startup.clone(), notice),
                ) {
                    return;
                }
            }
            Ok(WorkerCommand::Shutdown) => {
                // Cancel an unexecuted preparation, or stop polling an executed
                // request without implying that stopping restores its audio.
                pending = None;
                let result = service::request_shutdown();
                let finished = result.is_ok();
                if !send_worker_event(&sender, &stop, WorkerEvent::Shutdown(result)) {
                    return;
                }
                if finished {
                    return;
                }
                shutdown.store(false, Ordering::Relaxed);
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
        if shutdown.load(Ordering::Relaxed) {
            continue;
        }
        if let Some(mut operation) = pending.take() {
            let mut complete = None;
            if !operation.execute_attempted {
                if operation.checked.elapsed() >= Duration::from_secs(4) {
                    complete = Some("Selection expired before execute. No execute request was sent; refresh and select again.".to_owned());
                } else {
                    // Exactly one attempt, even if the server's response is
                    // lost. Subsequent requests only read this known receipt.
                    let Some(result) = dispatch_prepared_playback(&shutdown, || {
                        operation.execute_attempted = true;
                        operation.client.execute(&operation.prepared)
                    }) else {
                        pending = Some(operation);
                        continue;
                    };
                    if result.is_err() {
                        latest = None;
                        let _ = send_worker_event(
                            &sender,
                            &stop,
                            WorkerEvent::Observation(Observation::unavailable(generation)),
                        );
                    }
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
        // Keep startup's instance/state epoch independent of audio observations.
        // A repeated GET may renew freshness, but cannot invalidate an open menu.
        match service::ResidentControlClient::discover() {
            Ok(candidate) => {
                let changed = !startup_client
                    .as_ref()
                    .is_some_and(|previous| previous.same_instance(&candidate));
                let next = startup_request(&candidate, "GET", None, startup.generation);
                startup = update_startup(&startup, next, changed);
                startup_client = Some(candidate);
            }
            Err(_) => {
                startup = update_startup(
                    &startup,
                    StartupObservation::unavailable(startup.generation),
                    startup_client.is_some(),
                );
                startup_client = None;
            }
        }
        latest = Some(observation.clone());
        if let Err(mpsc::TrySendError::Disconnected(_)) =
            sender.try_send(WorkerEvent::Startup(startup.clone()))
        {
            return;
        }
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
    shutdown: Arc<AtomicBool>,
    settings: Arc<AtomicBool>,
    commands: mpsc::SyncSender<WorkerCommand>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Poller {
    fn start() -> crate::Result<(Self, mpsc::Receiver<WorkerEvent>)> {
        let (sender, receiver) = mpsc::sync_channel(4);
        let (commands, requests) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let shutdown = Arc::new(AtomicBool::new(false));
        let worker_shutdown = Arc::clone(&shutdown);
        let settings = Arc::new(AtomicBool::new(false));
        let worker_settings = Arc::clone(&settings);
        let thread = std::thread::Builder::new()
            .name("desktop-status".into())
            .spawn(move || {
                poll_worker(
                    sender,
                    requests,
                    worker_stop,
                    worker_shutdown,
                    worker_settings,
                )
            })?;
        Ok((
            Self {
                stop,
                shutdown,
                settings,
                commands,
                thread: Some(thread),
            },
            receiver,
        ))
    }
    fn request(&self, command: WorkerCommand) -> bool {
        match command {
            WorkerCommand::Shutdown => {
                self.shutdown.store(true, Ordering::Relaxed);
                self.settings.store(false, Ordering::Relaxed);
                true
            }
            WorkerCommand::Settings => {
                // Opening Settings is idempotent. Closing takes precedence and
                // must not produce a busy modal or leave a page queued to open.
                if !self.shutdown.load(Ordering::Relaxed) {
                    self.settings.store(true, Ordering::Relaxed);
                }
                true
            }
            _ if self.shutdown.load(Ordering::Relaxed) => false,
            command => self.commands.try_send(command).is_ok(),
        }
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
    run_with_settings(true)
}

/// A fixed startup entry keeps the same singleton without opening Web Settings.
pub fn run_with_settings(open_settings: bool) -> crate::Result<()> {
    let _singleton = match TraySingleton::acquire()? {
        Some(guard) => guard,
        None => {
            if !open_settings {
                return Ok(());
            }
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
            if SetTimer(Some(window), TIMER, 250, None) == 0 {
                return Err(windows::core::Error::from_win32().into());
            }
            let (poller, receiver) = Poller::start()?;
            if open_settings {
                let _ = poller.request(WorkerCommand::Settings);
            }
            message_loop(window, receiver, &poller)
        })();
        let _ = KillTimer(Some(window), TIMER);
        let _ = Shell_NotifyIconW(NIM_DELETE, &icon(window));
        let _ = DestroyWindow(window);
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
            .unwrap_or_else(|| LoadIconW(None, IDI_APPLICATION).unwrap_or_default())
    }
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
    Startup(StartupSelection),
}

/// The row mapping and all command bindings are immutable for the entire
/// TrackPopupMenu call. No periodic observation can retarget an open menu.
fn show_menu(
    window: HWND,
    uid: usize,
    apps: &AppIcons,
    observation: Option<&Observation>,
    startup: &StartupObservation,
    busy: bool,
) -> Option<MenuChoice> {
    unsafe {
        let menu = CreatePopupMenu().ok()?;
        let mut actions = Vec::new();
        let (startup_label, startup_flags, startup_selection) = startup_menu(startup, busy);
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
            AppendMenuW(
                menu,
                startup_flags,
                MENU_STARTUP as usize,
                PCWSTR(wide(startup_label).as_ptr()),
            )?;
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
        } else if id == MENU_STARTUP {
            startup_selection.map(MenuChoice::Startup)
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
    let mut startup = StartupObservation::unavailable(0);
    let mut startup_busy = false;
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
                        WorkerEvent::Startup(next) => startup = next,
                        WorkerEvent::StartupResult(next, notice) => {
                            startup = next;
                            startup_busy = false;
                            if let Some(message) = notice {
                                notify("VibDock 开机自启", &message, MB_ICONWARNING);
                            }
                        }
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
                            startup_busy = false;
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
                match show_menu(
                    window,
                    message.wParam.0,
                    &apps,
                    observation.as_ref(),
                    &startup,
                    busy || startup_busy,
                ) {
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
                    Some(MenuChoice::Startup(selection)) => {
                        if poller.request(WorkerCommand::Startup(selection)) {
                            startup_busy = true;
                        } else {
                            notify(
                                "VibDock 开机自启",
                                "请求已排队，请稍后再试。",
                                MB_ICONINFORMATION,
                            );
                        }
                    }
                    None => {}
                }
                continue;
            }
            if message.message == UI_RESTORE_ICONS {
                if let Some(current) = observation.as_ref() {
                    apps.update(&current.apps, current.fresh(), current.microphone_fresh());
                }
                apps.restore();
                continue;
            }
            if message.message == WM_SETTINGCHANGE {
                apps.refresh_theme()?;
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
        let _ = PostMessageW(Some(window), UI_RESTORE_ICONS, WPARAM(0), LPARAM(0));
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
                WM_LBUTTONUP => {
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
    (badge, format!("Microphone: {state}. Mute affects every application. App voice and meeting state unknown.{target}"))
}

const ICON_SIZE: usize = 32;

struct OwnedIcon(HICON);
impl Drop for OwnedIcon {
    fn drop(&mut self) {
        unsafe {
            let _ = DestroyIcon(self.0);
        }
    }
}

fn icon_pixels(icon: HICON) -> windows::core::Result<[u32; ICON_SIZE * ICON_SIZE]> {
    unsafe {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: ICON_SIZE as i32,
                biHeight: -(ICON_SIZE as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let bitmap = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0)?;
        let dc = CreateCompatibleDC(None);
        if dc.is_invalid() {
            let error = windows::core::Error::from_win32();
            let _ = DeleteObject(bitmap.into());
            return Err(error);
        }
        let previous = SelectObject(dc, bitmap.into());
        if previous.is_invalid() {
            let error = windows::core::Error::from_win32();
            let _ = DeleteDC(dc);
            let _ = DeleteObject(bitmap.into());
            return Err(error);
        }
        let result = (|| {
            let pixels = std::slice::from_raw_parts_mut(bits.cast::<u32>(), ICON_SIZE * ICON_SIZE);
            pixels.fill(0);
            DrawIconEx(
                dc,
                0,
                0,
                icon,
                ICON_SIZE as i32,
                ICON_SIZE as i32,
                0,
                None,
                DI_NORMAL,
            )?;
            if !GdiFlush().as_bool() {
                return Err(windows::core::Error::from_win32());
            }
            Ok(pixels.try_into().unwrap())
        })();
        SelectObject(dc, previous);
        let _ = DeleteDC(dc);
        let _ = DeleteObject(bitmap.into());
        result
    }
}

fn badge_pixels(pixels: &mut [u32; ICON_SIZE * ICON_SIZE], badge: &str) {
    // A contrasting 5x5 symbol remains distinct after Windows scales the icon.
    let (glyph, background) = match badge {
        "×" => ([17, 10, 4, 10, 17], 0xffb83232),
        "○" => ([14, 17, 17, 17, 14], 0xff21784c),
        "▶" => ([16, 24, 28, 24, 16], 0xff21784c),
        "II" => ([10, 10, 10, 10, 10], 0xff6b5a28),
        "–" => ([0, 0, 31, 0, 0], 0xff555555),
        _ => ([14, 17, 2, 0, 4], 0xff555555),
    };
    for y in 18..ICON_SIZE {
        for x in 18..ICON_SIZE {
            let gx = x.saturating_sub(20) / 2;
            let gy = y.saturating_sub(20) / 2;
            let mark =
                (20..30).contains(&x) && (20..30).contains(&y) && glyph[gy] & (1 << (4 - gx)) != 0;
            pixels[y * ICON_SIZE + x] = if mark { 0xffffffff } else { background };
        }
    }
}

fn badged_icon(base: HICON, badge: &str) -> windows::core::Result<OwnedIcon> {
    let mut pixels = icon_pixels(base)?;
    badge_pixels(&mut pixels, badge);
    // Reuse the existing DWORD-aligned DIB icon decoder and explicit alpha plane.
    let mut payload = vec![0u32; 10 + ICON_SIZE * ICON_SIZE + ICON_SIZE];
    payload[..10].copy_from_slice(&[40, 32, 64, 0x00200001, 0, 4096, 0, 0, 0, 0]);
    for (row, source) in pixels.as_chunks::<ICON_SIZE>().0.iter().rev().enumerate() {
        payload[10 + row * ICON_SIZE..10 + (row + 1) * ICON_SIZE].copy_from_slice(source);
    }
    unsafe {
        let bytes =
            std::slice::from_raw_parts_mut(payload.as_mut_ptr().cast::<u8>(), payload.len() * 4);
        CreateIconFromResourceEx(bytes, true, 0x00030000, 32, 32, IMAGE_FLAGS(0)).map(OwnedIcon)
    }
}

fn microphone_icons() -> windows::core::Result<Vec<(&'static str, OwnedIcon)>> {
    let base = tray_icon();
    ["?", "×", "○", "–"]
        .into_iter()
        .map(|badge| badged_icon(base, badge).map(|icon| (badge, icon)))
        .collect()
}

fn modify_or_restore(data: &NOTIFYICONDATAW) -> bool {
    unsafe {
        Shell_NotifyIconW(NIM_MODIFY, data).as_bool() || Shell_NotifyIconW(NIM_ADD, data).as_bool()
    }
}

// Each pinned entry has its own notification-area icon and exact uID-to-row
// mapping. Its menu captures one explicit session, never the first or all.
struct AppIcons {
    window: HWND,
    key: String,
    microphone_badge: &'static str,
    main: NOTIFYICONDATAW,
    microphone_icons: Vec<(&'static str, OwnedIcon)>,
    fallback: OwnedIcon,
    icons: Vec<OwnedIcon>,
    data: Vec<NOTIFYICONDATAW>,
    rows: Vec<serde_json::Value>,
    uids: Vec<u32>,
    next_uid: u32,
}
impl AppIcons {
    fn new(window: HWND) -> crate::Result<Self> {
        let microphone_icons = microphone_icons()?;
        let fallback = badged_icon(unsafe { LoadIconW(None, IDI_APPLICATION)? }, "?")?;
        let mut main = icon(window);
        let (_, unknown) = &microphone_icons[0];
        main.hIcon = unknown.0;
        main.szTip = notification_text(&microphone_presentation(&serde_json::Value::Null, false).1);
        if !unsafe { Shell_NotifyIconW(NIM_ADD, &main) }.as_bool() {
            return Err("Could not add the VibDock notification icon.".into());
        }
        Ok(Self {
            window,
            key: String::new(),
            microphone_badge: "?",
            main,
            microphone_icons,
            fallback,
            icons: vec![],
            data: vec![],
            rows: vec![],
            uids: vec![],
            next_uid: 2,
        })
    }
    fn clear(&mut self) {
        unsafe {
            for data in &self.data {
                let _ = Shell_NotifyIconW(NIM_DELETE, data);
            }
        }
        self.data.clear();
        self.icons.clear();
    }
    fn update(&mut self, value: &serde_json::Value, fresh: bool, microphone_fresh: bool) {
        self.update_microphone(&value["microphone"], microphone_fresh);
        let rows = displayed_rows(value, &self.rows, fresh);
        self.uids = stable_tray_ids(&self.rows, &self.uids, &rows, &mut self.next_uid);
        self.rows = rows;
        let complete = fresh && value["complete"] == true;
        // Capability renewal changes action data, not a notification icon's pixels.
        let key = serde_json::to_string(
            &self
                .rows
                .iter()
                .map(|row| {
                    (
                        &row["id"],
                        &row["label"],
                        &row["icon_b64"],
                        if complete {
                            playback_badge(row)
                        } else {
                            ("?", "Playback unknown")
                        },
                    )
                })
                .collect::<Vec<_>>(),
        )
        .unwrap_or_default();
        if self.key == key {
            self.restore();
            return;
        }
        self.clear();
        self.key = key;
        for (index, row) in self.rows.iter().enumerate() {
            let label = visible_text(row["label"].as_str().unwrap_or("Software"));
            let (badge, playback) = if complete {
                playback_badge(row)
            } else {
                ("?", "Playback unknown")
            };
            let created = row["icon_b64"]
                .as_str()
                .and_then(crate::resident_apps::decode_icon)
                .and_then(|bytes| {
                    let payload = &bytes[22..];
                    let mut aligned = vec![0u32; payload.len().div_ceil(4)];
                    unsafe {
                        let data = std::slice::from_raw_parts_mut(
                            aligned.as_mut_ptr().cast::<u8>(),
                            payload.len(),
                        );
                        data.copy_from_slice(payload);
                        CreateIconFromResourceEx(data, true, 0x00030000, 32, 32, IMAGE_FLAGS(0))
                            .ok()
                            .map(OwnedIcon)
                    }
                });
            let base = created
                .as_ref()
                .map(|icon| icon.0)
                .unwrap_or_else(|| unsafe { LoadIconW(None, IDI_APPLICATION).unwrap_or_default() });
            let painted = badged_icon(base, badge).ok();
            let mut data = NOTIFYICONDATAW {
                cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
                hWnd: self.window,
                uID: self.uids[index],
                uFlags: NIF_ICON | NIF_MESSAGE | NIF_TIP,
                uCallbackMessage: TRAY_MESSAGE,
                hIcon: painted.as_ref().unwrap_or(&self.fallback).0,
                ..Default::default()
            };
            let playback = if painted.is_some() {
                playback
            } else {
                "Playback unknown: icon rendering failed"
            };
            data.szTip =
                notification_text(&format!("{playback}: {label}. Voice activity unknown."));
            modify_or_restore(&data);
            self.data.push(data);
            self.icons.extend(painted);
        }
    }
    fn update_microphone(&mut self, value: &serde_json::Value, fresh: bool) {
        let (badge, name) = microphone_presentation(value, fresh);
        self.microphone_badge = badge;
        self.main.hIcon = self
            .microphone_icons
            .iter()
            .find(|(symbol, _)| *symbol == badge)
            .map(|(_, icon)| icon.0)
            .unwrap();
        self.main.szTip = notification_text(&name);
        modify_or_restore(&self.main);
    }
    fn refresh_theme(&mut self) -> crate::Result<()> {
        let icons = microphone_icons()?;
        self.main.hIcon = icons
            .iter()
            .find(|(symbol, _)| *symbol == self.microphone_badge)
            .map(|(_, icon)| icon.0)
            .unwrap();
        modify_or_restore(&self.main);
        self.microphone_icons = icons;
        Ok(())
    }
    fn restore(&self) {
        modify_or_restore(&self.main);
        for data in &self.data {
            modify_or_restore(data);
        }
    }
}
impl Drop for AppIcons {
    fn drop(&mut self) {
        self.clear();
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &self.main);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::{core::w, Win32::System::Threading::GetCurrentThreadId};
    fn command_fixture() -> (Poller, mpsc::Receiver<WorkerCommand>) {
        let (commands, requests) = mpsc::sync_channel(1);
        (
            Poller {
                stop: Arc::new(AtomicBool::new(false)),
                shutdown: Arc::new(AtomicBool::new(false)),
                settings: Arc::new(AtomicBool::new(false)),
                commands,
                thread: None,
            },
            requests,
        )
    }

    #[test]
    fn exit_during_preparation_prevents_execute_and_retains_failure_semantics() {
        let (poller, requests) = command_fixture();
        let shutdown = Arc::clone(&poller.shutdown);
        let writes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let worker_writes = Arc::clone(&writes);
        let (preparing, preparation_started) = mpsc::sync_channel(1);
        let (prepared, preparation_finished) = mpsc::sync_channel(1);
        let worker = std::thread::spawn(move || {
            preparing.send(()).unwrap();
            preparation_finished
                .recv_timeout(Duration::from_secs(2))
                .expect("The fixture preparation must be released");
            dispatch_prepared_playback(&shutdown, || {
                worker_writes.fetch_add(1, Ordering::Relaxed);
                Err::<(), _>("unconfirmed response")
            })
        });
        preparation_started
            .recv_timeout(Duration::from_secs(2))
            .expect("The fixture must enter preparation before Exit");
        command(103, &poller, None);
        prepared.send(()).unwrap();
        assert_eq!(worker.join().unwrap(), None);
        assert_eq!(writes.load(Ordering::Relaxed), 0);
        assert!(matches!(
            next_worker_command(&requests, &poller.shutdown, &poller.settings),
            Ok(WorkerCommand::Shutdown)
        ));
        // If shutdown fails, a later explicit action may execute once. Its lost
        // response remains an attempted write, never a cancelled/no-write result.
        poller.shutdown.store(false, Ordering::Relaxed);
        let result = dispatch_prepared_playback(&poller.shutdown, || {
            writes.fetch_add(1, Ordering::Relaxed);
            Err::<(), _>("unconfirmed response")
        });
        assert_eq!(result, Some(Err("unconfirmed response")));
        assert_eq!(writes.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn exit_keeps_legacy_id_and_preempts_a_full_settings_queue() {
        assert_eq!(MENU_EXIT, 103);
        assert_eq!(MENU_STARTUP, 104);
        let ids = [MENU_OPEN, MENU_STATUS, MENU_EXIT, MENU_STARTUP];
        for (index, id) in ids.iter().enumerate() {
            assert!(!ids[index + 1..].contains(id));
        }
        let (poller, requests) = command_fixture();
        // A bare numeric startup command has no frozen selection and cannot act.
        command(MENU_STARTUP, &poller, None);
        assert!(matches!(
            requests.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        command(MENU_OPEN, &poller, None);
        assert!(poller.commands.try_send(WorkerCommand::Settings).is_ok());
        command(103, &poller, None);
        assert!(poller.shutdown.load(Ordering::Relaxed));
        command(MENU_OPEN, &poller, None);
        assert!(!poller.settings.load(Ordering::Relaxed));
        assert!(matches!(
            next_worker_command(&requests, &poller.shutdown, &poller.settings),
            Ok(WorkerCommand::Shutdown)
        ));
        assert!(matches!(
            requests.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        // A failed shutdown can be retried; no stale queued command survives it.
        poller.shutdown.store(false, Ordering::Relaxed);
        assert!(poller.request(WorkerCommand::Shutdown));
        assert!(matches!(
            next_worker_command(&requests, &poller.shutdown, &poller.settings),
            Ok(WorkerCommand::Shutdown)
        ));
    }

    #[test]
    fn repeated_settings_commands_merge_into_one_browser_request() {
        let (poller, requests) = command_fixture();
        for _ in 0..5 {
            command(101, &poller, None);
        }
        assert!(matches!(
            requests.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        assert!(matches!(
            next_worker_command(&requests, &poller.shutdown, &poller.settings),
            Ok(WorkerCommand::Settings)
        ));
        assert!(matches!(
            next_worker_command(&requests, &poller.shutdown, &poller.settings),
            Err(mpsc::RecvTimeoutError::Timeout)
        ));
    }

    #[test]
    fn coalesced_settings_preserve_accepted_actions_and_yield_to_exit() {
        let startup = StartupSelection {
            generation: 9,
            enabled: true,
            expected_enabled: false,
        };
        let playback = PlaybackSelection {
            generation: 8,
            entry_id: "fixture-entry".into(),
            session_reference: "fixture-session".into(),
            label: "Fixture only".into(),
            process_id: 77,
            requested_muted: true,
        };
        for playback_action in [false, true] {
            for exit in [false, true] {
                let (poller, requests) = command_fixture();
                assert!(poller.request(if playback_action {
                    WorkerCommand::Playback(playback.clone())
                } else {
                    WorkerCommand::Startup(startup)
                }));
                for _ in 0..3 {
                    command(101, &poller, None);
                }
                if exit {
                    command(103, &poller, None);
                    assert!(matches!(
                        next_worker_command(&requests, &poller.shutdown, &poller.settings),
                        Ok(WorkerCommand::Shutdown)
                    ));
                    command(101, &poller, None);
                    assert!(!poller.settings.load(Ordering::Relaxed));
                    assert!(matches!(
                        requests.try_recv(),
                        Err(mpsc::TryRecvError::Empty)
                    ));
                } else {
                    match (
                        playback_action,
                        next_worker_command(&requests, &poller.shutdown, &poller.settings),
                    ) {
                        (false, Ok(WorkerCommand::Startup(actual))) => assert_eq!(actual, startup),
                        (true, Ok(WorkerCommand::Playback(actual))) => assert_eq!(actual, playback),
                        _ => panic!("An accepted action must precede coalesced Settings"),
                    }
                    assert!(matches!(
                        next_worker_command(&requests, &poller.shutdown, &poller.settings),
                        Ok(WorkerCommand::Settings)
                    ));
                    assert!(!poller.settings.load(Ordering::Relaxed));
                }
            }
        }
    }

    fn startup_fixture(enabled: bool, generation: u64) -> StartupObservation {
        StartupObservation::parse(
            &serde_json::json!({"platform":"windows","supported":true,"scope":"current_user","enabled":enabled,"state":if enabled{"enabled"}else{"disabled"},"readback_confirmed":true,"launcher_available":true,"error":null}),
            generation,
            Instant::now(),
        )
    }
    #[test]
    fn startup_menu_checks_only_confirmed_current_configuration() {
        for enabled in [false, true] {
            let view = startup_fixture(enabled, 7);
            let (label, flags, selection) = startup_menu(&view, false);
            assert_eq!(label, "开机自启");
            assert_eq!(flags.contains(MF_CHECKED), enabled);
            assert!(!flags.contains(MF_GRAYED));
            assert_eq!(
                selection.unwrap(),
                StartupSelection {
                    generation: 7,
                    enabled: !enabled,
                    expected_enabled: enabled,
                }
            );
        }
        let mut unavailable = startup_fixture(false, 7);
        unavailable.launcher_available = false;
        assert!(startup_menu(&unavailable, false).2.is_none());
        unavailable.state = StartupState::Confirmed(true);
        assert!(!startup_menu(&unavailable, false).2.unwrap().enabled);
        for state in [
            StartupState::Unknown,
            StartupState::Conflict,
            StartupState::Unsupported,
        ] {
            let mut view = startup_fixture(false, 7);
            view.state = state;
            let (_, flags, selection) = startup_menu(&view, false);
            assert!(flags.contains(MF_GRAYED));
            assert!(!flags.contains(MF_CHECKED));
            assert!(selection.is_none());
        }
        let mut expired = startup_fixture(true, 7);
        expired.checked = Some(Instant::now() - Duration::from_secs(5));
        assert!(startup_menu(&expired, false).2.is_none());
        assert!(!startup_menu(&expired, false).1.contains(MF_CHECKED));
        assert!(startup_menu(&startup_fixture(true, 7), true).2.is_none());
        let inconsistent = StartupObservation::parse(
            &serde_json::json!({"platform":"windows","supported":true,"scope":"current_user","enabled":true,"state":"disabled","readback_confirmed":true,"launcher_available":true,"error":null}),
            7,
            Instant::now(),
        );
        assert_eq!(inconsistent.state, StartupState::Unknown);
    }
    #[test]
    fn startup_worker_keeps_frozen_target_and_reads_back_after_failure() {
        let target = startup_menu(&startup_fixture(false, 9), false).2.unwrap();
        let changed = startup_fixture(false, 9);
        let mut calls = Vec::new();
        let (result, notice) = apply_startup(target, &changed, |body| {
            calls.push(body);
            startup_fixture(true, 9)
        });
        assert_eq!(calls, vec![Some(serde_json::json!({"enabled":true}))]);
        assert_eq!(result.state, StartupState::Confirmed(true));
        assert!(notice.is_none());
        calls.clear();
        let (result, notice) = apply_startup(target, &changed, |body| {
            let posting = body.is_some();
            calls.push(body);
            if posting {
                StartupObservation::unavailable(9)
            } else {
                startup_fixture(false, 9)
            }
        });
        assert_eq!(calls, vec![Some(serde_json::json!({"enabled":true})), None]);
        assert_eq!(result.state, StartupState::Confirmed(false));
        assert!(notice.is_some());
        calls.clear();
        let (_, notice) = apply_startup(target, &startup_fixture(false, 10), |body| {
            calls.push(body);
            startup_fixture(true, 10)
        });
        assert!(calls.is_empty());
        assert!(notice.is_some());
    }
    #[test]
    fn startup_menu_survives_same_state_polls_but_not_state_or_instance_changes() {
        let mut latest = startup_fixture(false, 31);
        let selection = startup_menu(&latest, false).2.unwrap();
        for unrelated_generation in 200..205 {
            latest = update_startup(&latest, startup_fixture(false, unrelated_generation), false);
            assert_eq!(latest.generation, 31);
        }
        let mut calls = Vec::new();
        let (_, notice) = apply_startup(selection, &latest, |body| {
            calls.push(body);
            startup_fixture(true, 31)
        });
        assert!(notice.is_none());
        assert_eq!(calls, vec![Some(serde_json::json!({"enabled":true}))]);
        for changed in [
            update_startup(&latest, startup_fixture(true, 31), false),
            update_startup(&latest, startup_fixture(false, 31), true),
        ] {
            calls.clear();
            let (_, notice) = apply_startup(selection, &changed, |body| {
                calls.push(body);
                startup_fixture(true, 31)
            });
            assert!(notice.is_some());
            assert!(calls.is_empty());
        }
    }
    use serde_json::json;

    #[test]
    #[ignore = "Hosted native notification-area fixture; no live audio or application actions"]
    fn native_notification_icons_probe() -> crate::Result<()> {
        unsafe extern "system" fn probe_owner(
            window: HWND,
            message: u32,
            wparam: WPARAM,
            lparam: LPARAM,
        ) -> LRESULT {
            DefWindowProcW(window, message, wparam, lparam)
        }
        unsafe extern "system" fn own_window_count(
            _: HWND,
            context: LPARAM,
        ) -> windows::core::BOOL {
            *(context.0 as *mut usize) += 1;
            true.into()
        }
        struct ProbeWindow(HWND);
        impl Drop for ProbeWindow {
            fn drop(&mut self) {
                unsafe {
                    let _ = DestroyWindow(self.0);
                    let _ = UnregisterClassW(w!("VibDockNotificationProbe"), None);
                }
            }
        }
        unsafe {
            let class = WNDCLASSW {
                lpfnWndProc: Some(probe_owner),
                hInstance: HINSTANCE(GetModuleHandleW(None)?.0),
                lpszClassName: w!("VibDockNotificationProbe"),
                ..Default::default()
            };
            assert_ne!(RegisterClassW(&class), 0);
            let owner = ProbeWindow(CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class.lpszClassName,
                w!("VibDock synthetic notification icons; no live audio"),
                WS_OVERLAPPED,
                0,
                0,
                0,
                0,
                None,
                None,
                Some(class.hInstance),
                None,
            )?);
            let foreground = GetForegroundWindow();
            let mut apps = AppIcons::new(owner.0)?;
            assert!(!IsWindowVisible(owner.0).as_bool());
            let mut value = json!({"complete":true,"microphone":{"device_id":"synthetic","name":"Synthetic microphone","state":"muted"},"entries":[
                {"id":"active","label":"Synthetic active","complete":true,"state":"connected","sessions":[{"active":true,"muted":false}]},
                {"id":"muted","label":"Synthetic muted","complete":true,"state":"connected","sessions":[{"active":true,"muted":true}]},
                {"id":"idle","label":"Synthetic idle","complete":true,"state":"connected","sessions":[{"active":false,"muted":false}]},
                {"id":"unknown","label":"Synthetic unknown","complete":false,"state":"unknown","sessions":[]}
            ]});
            apps.update(&value, true, true);
            assert_eq!(apps.data.len(), 4);
            for uid in [1, apps.uids[0]] {
                window_proc(
                    owner.0,
                    TRAY_MESSAGE,
                    WPARAM(uid as usize),
                    LPARAM(WM_LBUTTONUP as isize),
                );
                let mut request = MSG::default();
                assert!(PeekMessageW(
                    &mut request,
                    Some(owner.0),
                    OPEN_SETTINGS,
                    OPEN_SETTINGS,
                    PM_REMOVE
                )
                .as_bool());
                assert_eq!(request.message, OPEN_SETTINGS);
            }
            let muted = icon_pixels(apps.main.hIcon)?;
            value["microphone"]["state"] = json!("unmuted");
            apps.update(&value, true, true);
            let unmuted = icon_pixels(apps.main.hIcon)?;
            apps.update(&value, true, false);
            let unknown = icon_pixels(apps.main.hIcon)?;
            assert_ne!(muted, unmuted);
            assert_ne!(muted, unknown);
            assert_ne!(unmuted, unknown);
            assert!(Shell_NotifyIconW(NIM_MODIFY, &apps.main).as_bool());
            for (index, data) in apps.data.iter().enumerate() {
                assert_eq!(data.hWnd, owner.0);
                assert_eq!(data.uID, apps.uids[index]);
                assert!(Shell_NotifyIconW(NIM_MODIFY, data).as_bool());
                assert!(!data.hIcon.is_invalid());
            }
            let displayed: Vec<_> = apps
                .data
                .iter()
                .map(|data| icon_pixels(data.hIcon))
                .collect::<windows::core::Result<_>>()?;
            for (index, pixels) in displayed.iter().enumerate() {
                for other in &displayed[index + 1..] {
                    assert_ne!(pixels, other);
                }
            }
            let main_data = apps.main;
            let app_data = apps.data.clone();
            assert!(Shell_NotifyIconW(NIM_DELETE, &apps.main).as_bool());
            for data in &apps.data {
                assert!(Shell_NotifyIconW(NIM_DELETE, data).as_bool());
            }
            apps.restore();
            assert!(Shell_NotifyIconW(NIM_MODIFY, &apps.main).as_bool());
            for data in &apps.data {
                assert!(Shell_NotifyIconW(NIM_MODIFY, data).as_bool());
            }
            assert_eq!(GetForegroundWindow(), foreground);
            assert!(!IsWindowVisible(owner.0).as_bool());
            let mut window_count = 0usize;
            EnumThreadWindows(
                GetCurrentThreadId(),
                Some(own_window_count),
                LPARAM((&mut window_count as *mut usize) as isize),
            )
            .ok()?;
            assert_eq!(
                window_count, 1,
                "Only the hidden notification callback owner may exist"
            );
            eprintln!(
                "VIBDOCK_NATIVE_NOTIFICATION_PROBE {}",
                json!({"pid":std::process::id(),"owner_hwnd":owner.0.0 as usize,"fixture_only":true,"notification_icons":5,"floating_windows":0,"native_modify_confirmed":true,"restore_confirmed":true,"mic_states_distinct":true,"playback_states_distinct":true})
            );
            drop(apps);
            assert!(!Shell_NotifyIconW(NIM_MODIFY, &main_data).as_bool());
            for data in &app_data {
                assert!(!Shell_NotifyIconW(NIM_MODIFY, data).as_bool());
            }
        }
        Ok(())
    }

    #[test]
    fn notification_badges_are_distinct_opaque_and_preserve_the_logo() {
        let mut rendered = Vec::new();
        for badge in ["×", "○", "▶", "II", "–", "?"] {
            let mut pixels = [0xff123456; ICON_SIZE * ICON_SIZE];
            badge_pixels(&mut pixels, badge);
            assert_eq!(pixels[0], 0xff123456);
            assert_eq!(pixels[17 * ICON_SIZE + 31], 0xff123456);
            assert!(pixels.iter().all(|pixel| pixel >> 24 == 0xff));
            assert!(pixels.contains(&0xffffffff));
            for previous in &rendered {
                assert_ne!(&pixels, previous);
            }
            rendered.push(pixels);
        }
    }

    #[test]
    fn notification_badges_require_complete_known_playback_and_never_infer_voice() {
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
    fn notification_text_keeps_null_termination_and_complete_utf16() {
        assert_eq!(notification_text::<4>("abcdef"), [97, 98, 99, 0]);
        assert_eq!(notification_text::<3>("a😀"), [97, 0, 0]);
        assert_eq!(notification_text::<4>("a😀b"), [97, 0xd83d, 0xde00, 0]);
        assert_eq!(notification_text::<1>("result"), [0]);
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
