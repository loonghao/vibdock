//! Main-thread AppKit resident shell. No macOS audio backend is implied.
//!
//! Native widgets never perform network I/O. One joined worker serializes the
//! existing bounded proof, software summary, browser-code and shutdown
//! operations; its two queues are bounded. Windows software identities are not
//! loaded by this adapter. Application presence never implies audio activity.

use std::{
    cell::{Cell, OnceCell, RefCell},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc,
    },
    time::{Duration, Instant},
};

use objc2::{
    define_class, msg_send,
    rc::{autoreleasepool, Retained},
    runtime::{AnyObject, ProtocolObject, Sel},
    sel, AnyThread, DefinedClass, MainThreadOnly,
};
use objc2_app_kit::{
    NSAlert, NSApplication, NSApplicationActivationPolicy, NSApplicationDelegate,
    NSApplicationTerminateReply, NSBackingStoreType, NSBitmapImageFileType, NSButton, NSEvent,
    NSEventModifierFlags, NSEventType, NSFloatingWindowLevel, NSFont, NSImage, NSImageView, NSMenu,
    NSMenuItem, NSModalPanelRunLoopMode, NSPanel, NSStatusBar, NSStatusItem, NSTextField, NSWindow,
    NSWindowDelegate, NSWindowStyleMask, NSWorkspace,
};
use objc2_foundation::{
    ns_string, MainThreadMarker, NSData, NSDictionary, NSNotification, NSObject, NSObjectProtocol,
    NSPoint, NSProcessInfo, NSRect, NSRunLoop, NSRunLoopCommonModes, NSSize, NSString, NSTimer,
    NSURL,
};
use serde_json::{json, Map, Value};

use crate::service;

const STATUS_TTL: Duration = Duration::from_secs(4);
const ACCEPTANCE_BUDGET: Duration = Duration::from_secs(20);
const CAPABILITY_TEXT: &str =
    "Running means application presence only.\nPlayback, audio, global shortcuts and voice controls are unavailable on macOS.";
const MAX_SOFTWARE_ROWS: usize = 8;
const BASE_MENU_ITEMS: isize = 7;
const SOFTWARE_MENU_START: isize = BASE_MENU_ITEMS + 2;
const ROW_HEIGHT: f64 = 40.0;
const ROW_BOTTOM: f64 = 166.0;
const PANEL_WIDTH: f64 = 460.0;
const MENU_SHOW: isize = 2;
const MENU_HIDE: isize = 3;
const MENU_SETTINGS: isize = 4;
const MENU_QUIT: isize = 6;
const ACCEPTANCE_CHECKS: &[&str] = &[
    "service_ownership_proved",
    "main_thread",
    "application_finished_launching",
    "native_panel_visible",
    "floating_panel",
    "original_template_image",
    "retained_status_item",
    "dock_entry",
    "status_menu",
    "dock_menu",
    "hide_keeps_resident",
    "dock_show_reuses_panel",
    "close_hides_only",
    "reopen_reuses_panel",
    "last_window_keeps_resident",
    "native_view_rendered",
    "second_launcher_exited",
    "owner_ipc_reopen_restores",
    "settings_handoff_issued",
    "authenticated_shutdown_and_port_released",
    "native_resources_released",
];
const SOFTWARE_ACCEPTANCE_CHECKS: &[&str] = &[
    "software_rows_match_summary",
    "software_rows_within_panel",
    "software_status_menu_rows",
    "software_dock_menu_rows",
    "software_presence_only",
    "software_ui_has_no_bundle_paths",
    "software_expiry_unknown",
    "software_expiry_clears_icons",
];
const OFFLINE_ACCEPTANCE_CHECKS: &[&str] = &[
    "software_persisted_offline_label",
    "software_offline_not_running_or_unknown",
    "software_offline_placeholder",
];
const RUNNING_ACCEPTANCE_CHECKS: &[&str] = &[
    "software_self_running_observed",
    "software_self_native_icon",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Presence {
    Running,
    NotRunning,
    Unknown,
}

impl Presence {
    fn label(self) -> &'static str {
        match self {
            Self::Running => "Running",
            Self::NotRunning => "Not running",
            Self::Unknown => "Unknown",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SoftwareEntry {
    id: String,
    label: String,
    presence: Presence,
    icon: Option<Vec<u8>>,
}

impl SoftwareEntry {
    fn unknown(&mut self) {
        self.presence = Presence::Unknown;
        self.icon = None;
    }

    fn menu_title(&self, has_icon: bool) -> String {
        let placeholder = if has_icon { "" } else { "□ " };
        format!(
            "{placeholder}{} · {}",
            self.display_label(),
            self.presence.label()
        )
    }

    fn display_label(&self) -> String {
        let mut chars = self.label.chars();
        let mut label: String = chars.by_ref().take(32).collect();
        if chars.next().is_some() {
            label.push('…');
        }
        label
    }
}

#[derive(Default)]
struct SoftwareSnapshot {
    entries: Vec<SoftwareEntry>,
    checked: Option<Instant>,
}

fn summary_string(value: &Value, key: &str, max: usize) -> Option<String> {
    let text = value.get(key)?.as_str()?;
    (!text.is_empty() && text.len() <= max && !text.chars().any(char::is_control))
        .then(|| text.to_owned())
}

impl SoftwareSnapshot {
    // This runs on the serial service worker. Only bounded labels, presence and
    // validated PNG bytes reach native widgets; no executable or bundle path is
    // ever used by the resident UI, including when a menu row is selected.
    fn parse(value: &Value, checked: Instant) -> Option<Self> {
        if value["platform"] != "macos" || value["observation_kind"] != "application_presence" {
            return None;
        }
        let values = value["entries"].as_array()?;
        if values.len() > MAX_SOFTWARE_ROWS {
            return None;
        }
        let observed = value["age_ms"]
            .as_u64()
            .filter(|age| *age < STATUS_TTL.as_millis() as u64)
            .and_then(|age| checked.checked_sub(Duration::from_millis(age)));
        let available = value["available"].as_bool() == Some(true) && observed.is_some();
        let mut entries: Vec<SoftwareEntry> = Vec::with_capacity(values.len());
        for value in values {
            let id = summary_string(value, "id", 128)?;
            if entries.iter().any(|entry| entry.id == id) {
                return None;
            }
            let label = summary_string(value, "label", 80)?;
            let read_only = value["actions_enabled"].as_bool() == Some(false)
                && value["sessions"].as_array().is_some_and(Vec::is_empty);
            let presence = if available && read_only {
                match value["state"].as_str() {
                    // A partial scan can still positively verify one app. Only
                    // absence requires a complete observation for this entry.
                    Some("running") => Presence::Running,
                    Some("not_running") if value["complete"].as_bool() == Some(true) => {
                        Presence::NotRunning
                    }
                    _ => Presence::Unknown,
                }
            } else {
                Presence::Unknown
            };
            let icon = if presence == Presence::Running && value["icon_mime"] == "image/png" {
                value["icon_b64"]
                    .as_str()
                    .and_then(crate::resident_apps::decode_icon)
            } else {
                None
            };
            entries.push(SoftwareEntry {
                id,
                label,
                presence,
                icon,
            });
        }
        Some(Self {
            entries,
            checked: observed,
        })
    }

    fn expire(&mut self) {
        self.checked = None;
        for entry in &mut self.entries {
            entry.unknown();
        }
    }

    fn display(&self, now: Instant) -> Vec<SoftwareEntry> {
        let mut entries = self.entries.clone();
        if !self
            .checked
            .is_some_and(|checked| now.saturating_duration_since(checked) < STATUS_TTL)
        {
            for entry in &mut entries {
                entry.unknown();
            }
        }
        entries
    }
}

#[derive(Clone)]
struct SoftwareAcceptance {
    offline: Option<(String, String)>,
    running: Option<String>,
}

impl SoftwareAcceptance {
    fn from_environment(acceptance: bool) -> crate::Result<Option<Self>> {
        if !acceptance {
            return Ok(None);
        }
        let id = std::env::var("VIBDOCK_NATIVE_ACCEPTANCE_SOFTWARE_ID").ok();
        let label = std::env::var("VIBDOCK_NATIVE_ACCEPTANCE_SOFTWARE_LABEL").ok();
        let offline = match (id, label) {
            (Some(id), Some(label)) => Some((id, label)),
            (None, None) => None,
            _ => {
                return Err(
                    "Software acceptance requires both the persisted fixture ID and label.".into(),
                )
            }
        };
        let running = std::env::var("VIBDOCK_NATIVE_ACCEPTANCE_RUNNING_ID").ok();
        Ok((offline.is_some() || running.is_some()).then_some(Self { offline, running }))
    }
}

fn public_observation_error(value: &Value) -> Option<&'static str> {
    match value.as_str() {
        None if value.is_null() => None,
        Some("Waiting for native application observation") => Some("waiting_for_observation"),
        Some("Application observation worker could not start") => Some("worker_start_failed"),
        Some("Application observation contained invalid or incomplete data") => Some("invalid_observation"),
        Some("Native application observation is incomplete or unavailable") => Some("native_observation_unavailable"),
        Some("Native application observation is unavailable on this platform") => Some("platform_observation_unavailable"),
        Some("Some applications could not be observed completely") => Some("partial_observation"),
        Some("Application storage is unavailable; existing data was not changed.") => Some("storage_unavailable"),
        Some("apps-macos.json is unreadable, invalid or unsupported; original data is preserved and application edits are disabled.") => Some("storage_invalid"),
        _ => Some("other_error_redacted"),
    }
}

fn public_presence(value: &Value) -> &'static str {
    match value.as_str() {
        Some("running") => "running",
        Some("not_running") => "not_running",
        Some("unknown") => "unknown",
        _ => "invalid",
    }
}

// Acceptance-only diagnostics intentionally omit ports, IDs, labels, paths,
// tokens, image bytes and arbitrary error strings, even on malformed payloads.
fn software_observation_diagnostic(
    value: Option<&Value>,
    parsed: Option<&SoftwareSnapshot>,
    expected: Option<&SoftwareAcceptance>,
    error: Option<&(dyn std::error::Error + 'static)>,
    request_duration: Duration,
) -> Value {
    let (proved, failure) = if value.is_some() {
        (Some(true), None)
    } else if let Some(error) = error {
        if error.is::<serde_json::Error>() {
            (Some(true), Some("summary_json_invalid"))
        } else {
            match error.to_string().as_str() {
                "No service instance" => (Some(false), Some("no_service_instance")),
                "Listener failed instance proof" => (Some(false), Some("instance_proof_rejected")),
                "Software status is unavailable" => {
                    (Some(true), Some("summary_status_unavailable"))
                }
                _ => (None, Some("proof_or_summary_exchange_failed")),
            }
        }
    } else {
        (None, Some("no_observation_result"))
    };
    let empty = Value::Null;
    let value = value.unwrap_or(&empty);
    let rows = value["entries"].as_array();
    let contains =
        |id: &str| rows.is_some_and(|rows| rows.iter().any(|row| row["id"].as_str() == Some(id)));
    let offline = expected.and_then(|expected| expected.offline.as_ref());
    let running = expected.and_then(|expected| expected.running.as_ref());
    json!({
        "service_proof_succeeded": proved,
        "request_failure": failure,
        "request_duration_ms": request_duration.as_millis().min(u64::MAX as u128) as u64,
        "platform_macos": value["platform"] == "macos",
        "application_presence_kind": value["observation_kind"] == "application_presence",
        "available": value["available"].as_bool(),
        "complete": value["complete"].as_bool(),
        "skipped": value["skipped"].as_u64(),
        "age_ms": value["age_ms"].as_u64(),
        "observation_error": public_observation_error(&value["error"]),
        "parser_accepted": parsed.is_some(),
        "raw_row_count": rows.map(Vec::len),
        "parsed_row_count": parsed.map(|snapshot| snapshot.entries.len()),
        "expected_offline_id_present": offline.map(|(id, _)| contains(id)),
        "expected_offline_label_matches": offline.map(|(id, label)| rows.is_some_and(|rows| rows.iter()
            .any(|row| row["id"].as_str() == Some(id.as_str()) && row["label"].as_str() == Some(label.as_str())))),
        "expected_live_id_present": running.map(|id| contains(id)),
        "rows": rows.into_iter().flatten().take(MAX_SOFTWARE_ROWS).enumerate().map(|(index, row)| json!({
            "state": public_presence(&row["state"]),
            "complete": row["complete"].as_bool(),
            "id_valid": summary_string(row, "id", 128).is_some(),
            "label_valid": summary_string(row, "label", 80).is_some(),
            "actions_disabled": row["actions_enabled"] == false,
            "sessions_empty": row["sessions"].as_array().is_some_and(Vec::is_empty),
            "icon_provided": row["icon_b64"].as_str().is_some_and(|icon| !icon.is_empty()),
            "icon_mime_png": row["icon_mime"] == "image/png",
            "parsed_state": parsed.and_then(|snapshot| snapshot.entries.get(index)).map(|entry| entry.presence.label()),
            "decoded_icon_present": parsed.and_then(|snapshot| snapshot.entries.get(index)).is_some_and(|entry| entry.icon.is_some()),
        })).collect::<Vec<_>>(),
    })
}

fn service_text(observation: Option<(u16, Instant)>, now: Instant) -> String {
    match observation {
        Some((port, checked)) if now.saturating_duration_since(checked) < STATUS_TTL => {
            format!("Service verified on 127.0.0.1:{port}")
        }
        _ => "Service unknown / unavailable".into(),
    }
}

fn acceptance_passed(report: &Map<String, Value>) -> bool {
    !report.contains_key("error")
        && ACCEPTANCE_CHECKS
            .iter()
            .all(|name| report.get(*name).and_then(Value::as_bool) == Some(true))
        && [
            ("software_acceptance_requested", SOFTWARE_ACCEPTANCE_CHECKS),
            (
                "software_offline_fixture_requested",
                OFFLINE_ACCEPTANCE_CHECKS,
            ),
            (
                "software_self_observation_requested",
                RUNNING_ACCEPTANCE_CHECKS,
            ),
        ]
        .iter()
        .all(|(requested, checks)| {
            report.get(*requested).and_then(Value::as_bool) != Some(true)
                || checks
                    .iter()
                    .all(|name| report.get(*name).and_then(Value::as_bool) == Some(true))
        })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Command {
    Settings,
    Quit,
    ReopenProbe,
    Stop,
}

enum Reply {
    Observation {
        port: Option<u16>,
        software: Option<SoftwareSnapshot>,
        diagnostic: Option<Value>,
        checked: Instant,
    },
    Settings(std::result::Result<String, String>),
    Quit(std::result::Result<bool, String>),
    ReopenProbe(std::result::Result<(), String>),
}

struct Worker {
    commands: SyncSender<Command>,
    replies: RefCell<Option<Receiver<Reply>>>,
    stop: Arc<AtomicBool>,
    thread: RefCell<Option<std::thread::JoinHandle<()>>>,
}

impl Worker {
    fn start(acceptance: bool, expected: Option<SoftwareAcceptance>) -> crate::Result<Self> {
        let (commands, incoming) = mpsc::sync_channel(1);
        let (outgoing, replies) = mpsc::sync_channel(4);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("macos-resident-service".into())
            .spawn(move || {
                let mut next_observation = Instant::now();
                while !stopping.load(Ordering::Relaxed) {
                    match incoming.recv_timeout(Duration::from_millis(100)) {
                        Ok(Command::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        Ok(Command::Settings) => {
                            let reply =
                                crate::configuration_url().map_err(|error| error.to_string());
                            if outgoing.send(Reply::Settings(reply)).is_err() {
                                break;
                            }
                        }
                        Ok(Command::Quit) => {
                            if outgoing
                                .send(Reply::Quit(service::request_shutdown()))
                                .is_err()
                            {
                                break;
                            }
                        }
                        Ok(Command::ReopenProbe) => {
                            if outgoing
                                .send(Reply::ReopenProbe(probe_second_launch()))
                                .is_err()
                            {
                                break;
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                    }
                    if Instant::now() >= next_observation && !stopping.load(Ordering::Relaxed) {
                        // The summary follows the ownership proof on the same
                        // socket. There is no main-thread networking or second
                        // connection carrying a bearer to an unproved listener.
                        let requested = Instant::now();
                        let observation = service::resident_app_view();
                        let checked = Instant::now();
                        let (port, software, diagnostic) = match observation {
                            Ok((port, value)) => {
                                let software = SoftwareSnapshot::parse(&value, requested);
                                let diagnostic = acceptance.then(|| {
                                    software_observation_diagnostic(
                                        Some(&value),
                                        software.as_ref(),
                                        expected.as_ref(),
                                        None,
                                        checked.saturating_duration_since(requested),
                                    )
                                });
                                (Some(port), software, diagnostic)
                            }
                            Err(error) => {
                                let diagnostic = acceptance.then(|| {
                                    software_observation_diagnostic(
                                        None,
                                        None,
                                        expected.as_ref(),
                                        Some(error.as_ref()),
                                        checked.saturating_duration_since(requested),
                                    )
                                });
                                (None, None, diagnostic)
                            }
                        };
                        let _ = outgoing.try_send(Reply::Observation {
                            port,
                            software,
                            diagnostic,
                            checked,
                        });
                        next_observation = Instant::now() + Duration::from_secs(1);
                    }
                }
            })?;
        Ok(Self {
            commands,
            replies: RefCell::new(Some(replies)),
            stop,
            thread: RefCell::new(Some(thread)),
        })
    }

    fn drain(&self) -> Vec<Reply> {
        self.replies
            .borrow()
            .as_ref()
            .map(|receiver| receiver.try_iter().take(4).collect())
            .unwrap_or_default()
    }

    fn shutdown(&self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.commands.try_send(Command::Stop);
        // Release any blocked result send before joining. Existing network calls
        // already have deadlines; shutdown additionally waits at most 2 seconds.
        self.replies.borrow_mut().take();
        if let Some(thread) = self.thread.borrow_mut().take() {
            let _ = thread.join();
        }
    }
}

/// This acceptance-only child takes the existing launcher's lock-contention
/// path. No shell, process-name lookup, permissions, or arbitrary executable.
fn probe_second_launch() -> std::result::Result<(), String> {
    use std::process::{Command as ProcessCommand, Stdio};
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    let mut child = ProcessCommand::new(executable)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| error.to_string())?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            return if status.success() {
                Ok(())
            } else {
                Err("The second launcher failed.".into())
            };
        }
        if Instant::now() >= deadline {
            // Only our short-lived probe child is stopped, never the service or
            // original resident. The owner's Show-only IPC has its own bound.
            let _ = child.kill();
            let reap_deadline = Instant::now() + Duration::from_millis(500);
            while matches!(child.try_wait(), Ok(None)) && Instant::now() < reap_deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            return Err("The second launcher did not finish within five seconds.".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

struct SoftwareRow {
    image: Retained<NSImageView>,
    placeholder: Retained<NSTextField>,
    label: Retained<NSTextField>,
    state: Retained<NSTextField>,
}

struct Ui {
    panel: Retained<NSPanel>,
    status: Retained<NSStatusItem>,
    status_menu: Retained<NSMenu>,
    dock_menu: Retained<NSMenu>,
    software_menus: Vec<Retained<NSMenu>>,
    summaries: Vec<Retained<NSMenuItem>>,
    title: Retained<NSTextField>,
    service_label: Retained<NSTextField>,
    software_heading: Retained<NSTextField>,
    software_empty: Retained<NSTextField>,
    software_rows: Vec<SoftwareRow>,
    rendered_software: RefCell<Option<Vec<SoftwareEntry>>>,
    notice_label: Retained<NSTextField>,
    menu_image: Retained<NSImage>,
}

struct DelegateState {
    ui: OnceCell<Ui>,
    worker: Worker,
    pending: Cell<Option<Command>>,
    observation: Cell<Option<(u16, Instant)>>,
    software: RefCell<SoftwareSnapshot>,
    software_acceptance: Option<SoftwareAcceptance>,
    acceptance: bool,
    system_quit_acceptance: bool,
    finished_launching: Cell<bool>,
    stage: Cell<u8>,
    started: Instant,
    report: RefCell<Map<String, Value>>,
    stopped: Cell<bool>,
    reopens: Cell<u32>,
    reopen_baseline: Cell<u32>,
    acceptance_window: Cell<isize>,
    second_launcher_exited: Cell<bool>,
    reopen_requests: Receiver<()>,
    owner_reopens: Cell<u32>,
    system_termination: Cell<bool>,
    cleaned: Cell<bool>,
    timer: OnceCell<Retained<NSTimer>>,
    system_quit_timer: OnceCell<Retained<NSTimer>>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements. All AppKit state and
    // callbacks are main-thread-only; no native object reaches the worker.
    #[unsafe(super = NSObject)]
    #[thread_kind = MainThreadOnly]
    #[ivars = DelegateState]
    struct ResidentDelegate;

    unsafe impl NSObjectProtocol for ResidentDelegate {}

    unsafe impl NSApplicationDelegate for ResidentDelegate {
        #[unsafe(method(applicationDidFinishLaunching:))]
        fn did_finish_launching(&self, _notification: &NSNotification) {
            self.show_panel();
            self.ivars().finished_launching.set(true);
        }

        #[unsafe(method(applicationShouldTerminateAfterLastWindowClosed:))]
        fn keep_resident(&self, _application: &NSApplication) -> bool {
            false
        }

        #[unsafe(method(applicationShouldHandleReopen:hasVisibleWindows:))]
        fn reopen(&self, _application: &NSApplication, _visible: bool) -> bool {
            self.ivars().reopens.set(self.ivars().reopens.get().saturating_add(1));
            self.show_panel();
            true
        }

        // method_id applies objc2's owned-return ABI conversion, including
        // autorelease for this non-owning Objective-C method family.
        #[unsafe(method_id(applicationDockMenu:))]
        #[unsafe(method_family = none)]
        fn dock_menu(&self, _application: &NSApplication) -> Option<Retained<NSMenu>> {
            self.ivars().ui.get().map(|ui| ui.dock_menu.clone())
        }

        #[unsafe(method(applicationShouldTerminate:))]
        fn should_terminate(&self, _application: &NSApplication) -> NSApplicationTerminateReply {
            // Dock/system Quit must defer, rather than cancel, logout/restart.
            // The modal-mode timer completes the bounded authenticated request.
            self.ivars().system_termination.set(true);
            if self.ivars().pending.get().is_none() {
                self.queue(Command::Quit);
            }
            if self.ivars().pending.get().is_some() {
                NSApplicationTerminateReply::TerminateLater
            } else {
                self.ivars().system_termination.set(false);
                NSApplicationTerminateReply::TerminateCancel
            }
        }
    }

    unsafe impl NSWindowDelegate for ResidentDelegate {
        #[unsafe(method(windowShouldClose:))]
        fn should_close(&self, _window: &NSWindow) -> bool {
            self.hide_panel();
            false
        }
    }

    impl ResidentDelegate {
        #[unsafe(method(showPanel:))]
        fn show_action(&self, _sender: Option<&AnyObject>) {
            self.show_panel();
        }

        #[unsafe(method(hidePanel:))]
        fn hide_action(&self, _sender: Option<&AnyObject>) {
            self.hide_panel();
        }

        #[unsafe(method(openSettings:))]
        fn settings_action(&self, _sender: Option<&AnyObject>) {
            self.queue(Command::Settings);
        }

        #[unsafe(method(quitVibDock:))]
        fn quit_action(&self, _sender: Option<&AnyObject>) {
            self.queue(Command::Quit);
        }

        #[unsafe(method(pollService:))]
        fn timer_fired(&self, _timer: &NSTimer) {
            self.tick();
        }

        #[unsafe(method(acceptanceSystemQuit:))]
        fn acceptance_system_quit(&self, _timer: &NSTimer) {
            if self.ivars().system_quit_acceptance && !self.ivars().stopped.get() {
                self.check("system_termination_trigger_deferred", true);
                // This is a separate callback: the repeating poll timer must
                // be free to drain replies in TerminateLater's modal loop.
                NSApplication::sharedApplication(self.mtm()).terminate(None);
            }
        }
    }
);

impl ResidentDelegate {
    fn new(
        mtm: MainThreadMarker,
        acceptance: bool,
        reopen: Receiver<()>,
    ) -> crate::Result<Retained<Self>> {
        let software_acceptance = SoftwareAcceptance::from_environment(acceptance)?;
        let this = Self::alloc(mtm).set_ivars(DelegateState {
            ui: OnceCell::new(),
            worker: Worker::start(acceptance, software_acceptance.clone())?,
            pending: Cell::new(None),
            observation: Cell::new(None),
            software: RefCell::new(SoftwareSnapshot::default()),
            software_acceptance,
            acceptance,
            system_quit_acceptance: acceptance
                && std::env::var("VIBDOCK_NATIVE_ACCEPTANCE_SYSTEM_QUIT").as_deref() == Ok("1"),
            finished_launching: Cell::new(false),
            stage: Cell::new(0),
            started: Instant::now(),
            report: RefCell::new(Map::new()),
            stopped: Cell::new(false),
            reopens: Cell::new(0),
            reopen_baseline: Cell::new(0),
            acceptance_window: Cell::new(0),
            second_launcher_exited: Cell::new(false),
            reopen_requests: reopen,
            owner_reopens: Cell::new(0),
            system_termination: Cell::new(false),
            cleaned: Cell::new(false),
            timer: OnceCell::new(),
            system_quit_timer: OnceCell::new(),
        });
        // SAFETY: NSObject's designated initializer has this signature.
        Ok(unsafe { msg_send![super(this), init] })
    }

    fn show_panel(&self) {
        if let Some(ui) = self.ivars().ui.get() {
            ui.panel.makeKeyAndOrderFront(None);
            // This older API supports the minimum deployment target; activation
            // is only requested at launch or an explicit user Show/reopen action.
            #[allow(deprecated)]
            NSApplication::sharedApplication(self.mtm()).activateIgnoringOtherApps(true);
        }
    }

    fn hide_panel(&self) {
        if let Some(ui) = self.ivars().ui.get() {
            ui.panel.orderOut(None);
        }
    }

    fn notice(&self, text: &str) {
        if let Some(ui) = self.ivars().ui.get() {
            let bounded: String = text.chars().take(240).collect();
            ui.notice_label
                .setStringValue(&NSString::from_str(&bounded));
        }
    }

    fn queue(&self, command: Command) {
        if self.ivars().pending.get().is_some() {
            self.notice("A service request is in progress. Please try again shortly.");
            return;
        }
        if self.ivars().worker.commands.try_send(command).is_err() {
            self.notice("The service worker is unavailable. Please try again.");
            return;
        }
        self.ivars().pending.set(Some(command));
        self.notice(match command {
            Command::Settings => "Preparing a one-use Settings handoff…",
            Command::Quit => "Verifying and stopping the service…",
            Command::ReopenProbe => "Checking a second application launch…",
            Command::Stop => "Stopping…",
        });
    }

    fn check(&self, name: &str, passed: bool) {
        self.ivars()
            .report
            .borrow_mut()
            .insert(name.into(), json!(passed));
    }

    fn fail_acceptance(&self, error: &str) {
        self.ivars()
            .report
            .borrow_mut()
            .insert("error".into(), json!(error));
        self.stop_loop();
    }

    fn tick(&self) {
        // The launcher authenticates and bounds its own Show-only local IPC.
        // Native UI work remains on this main-thread timer, never that listener.
        let reopens = self.ivars().reopen_requests.try_iter().take(8).count();
        if reopens > 0 {
            self.ivars().owner_reopens.set(
                self.ivars()
                    .owner_reopens
                    .get()
                    .saturating_add(reopens as u32),
            );
            self.show_panel();
        }
        for reply in self.ivars().worker.drain() {
            match reply {
                Reply::Observation {
                    port,
                    software,
                    diagnostic,
                    checked,
                } => {
                    if let Some(mut diagnostic) = diagnostic {
                        let mut report = self.ivars().report.borrow_mut();
                        let attempts = report
                            .get("software_observation_attempts")
                            .and_then(Value::as_u64)
                            .unwrap_or(0)
                            .saturating_add(1);
                        report.insert("software_observation_attempts".into(), json!(attempts));
                        diagnostic["attempt"] = json!(attempts);
                        diagnostic["acceptance_elapsed_ms"] = json!(self
                            .ivars()
                            .started
                            .elapsed()
                            .as_millis()
                            .min(u64::MAX as u128)
                            as u64);
                        let history = report
                            .entry("software_observation_diagnostics")
                            .or_insert_with(|| json!([]))
                            .as_array_mut()
                            .expect("owned diagnostic array");
                        if history.len() == 8 {
                            history.remove(0);
                        }
                        history.push(diagnostic);
                    }
                    self.ivars()
                        .observation
                        .set(port.map(|port| (port, checked)));
                    if let Some(software) = software {
                        *self.ivars().software.borrow_mut() = software;
                    } else {
                        // Preserve saved locations in their stable order, but
                        // never preserve a running claim or an old native icon.
                        self.ivars().software.borrow_mut().expire();
                    }
                }
                Reply::Settings(result) => {
                    self.ivars().pending.set(None);
                    match result {
                        Ok(url) if self.ivars().system_termination.get() => drop(url),
                        Ok(url) if self.ivars().acceptance => {
                            // Acquiring this URL exercised the real same-socket
                            // proof and one-use code API. Never log its secret.
                            self.check("settings_handoff_issued", true);
                            self.ivars().stage.set(2);
                            drop(url);
                        }
                        Ok(url) => {
                            let opened = NSURL::URLWithString(&NSString::from_str(&url))
                                .is_some_and(|url| NSWorkspace::sharedWorkspace().openURL(&url));
                            self.notice(if opened {
                                "Settings handoff sent to your default browser."
                            } else {
                                "The browser could not open Settings. Please try again."
                            });
                            if !opened {
                                self.show_panel();
                            }
                        }
                        Err(error) => {
                            self.notice(&format!("Settings failed: {error}"));
                            self.show_panel();
                            if self.ivars().acceptance {
                                self.fail_acceptance("The real Settings handoff failed.");
                            }
                        }
                    }
                }
                Reply::Quit(result) => {
                    self.ivars().pending.set(None);
                    match result {
                        Ok(stopped) => {
                            if self.ivars().acceptance {
                                self.check("authenticated_shutdown_and_port_released", stopped);
                            }
                            if self.ivars().system_termination.replace(false) {
                                if self.ivars().acceptance && !stopped {
                                    NSApplication::sharedApplication(self.mtm())
                                        .replyToApplicationShouldTerminate(false);
                                    self.fail_acceptance(
                                        "No proved test service was available to stop.",
                                    );
                                    return;
                                }
                                // A true reply may exit inside AppKit without
                                // returning from run(), so clean up first.
                                self.cleanup_native();
                                self.ivars().worker.shutdown();
                                crate::macos_launcher::cleanup_reopen_before_system_exit();
                                if self.ivars().acceptance {
                                    use std::io::Write;
                                    self.check("system_termination_path", true);
                                    self.check("system_termination_cleaned", true);
                                    let mut output = std::io::stdout().lock();
                                    let _ = writeln!(output, "{}", self.final_report());
                                    let _ = output.flush();
                                }
                                NSApplication::sharedApplication(self.mtm())
                                    .replyToApplicationShouldTerminate(true);
                                return;
                            }
                            if self.ivars().acceptance && !stopped {
                                self.fail_acceptance(
                                    "No proved test service was available to stop.",
                                );
                            }
                            self.stop_loop();
                        }
                        Err(error) => {
                            if self.ivars().system_termination.replace(false) {
                                NSApplication::sharedApplication(self.mtm())
                                    .replyToApplicationShouldTerminate(false);
                            }
                            self.notice(&format!("Quit failed: {error}"));
                            self.show_panel();
                            if self.ivars().acceptance {
                                self.fail_acceptance("The real authenticated shutdown failed.");
                            }
                        }
                    }
                }
                Reply::ReopenProbe(result) => {
                    self.ivars().pending.set(None);
                    match result {
                        Ok(()) => {
                            self.ivars().second_launcher_exited.set(true);
                            self.check("second_launcher_exited", true);
                        }
                        Err(error) => self.fail_acceptance(&error),
                    }
                }
            }
        }
        if self.ivars().system_termination.get() && self.ivars().pending.get().is_none() {
            self.queue(Command::Quit);
            if self.ivars().pending.get().is_none() {
                self.ivars().system_termination.set(false);
                NSApplication::sharedApplication(self.mtm())
                    .replyToApplicationShouldTerminate(false);
            }
        }
        let text = service_text(self.ivars().observation.get(), Instant::now());
        if let Some(ui) = self.ivars().ui.get() {
            let text = NSString::from_str(&text);
            ui.service_label.setStringValue(&text);
            for summary in &ui.summaries {
                summary.setTitle(&text);
            }
            self.render_software(ui, Instant::now());
        }
        if self.ivars().acceptance && !self.ivars().stopped.get() {
            self.acceptance_step();
        }
    }

    fn render_software(&self, ui: &Ui, now: Instant) {
        let entries = self.ivars().software.borrow().display(now);
        if ui.rendered_software.borrow().as_ref() == Some(&entries) {
            return;
        }
        let row_count = entries.len().max(1);
        let height = 281.0 + row_count as f64 * ROW_HEIGHT;
        ui.panel.setContentSize(NSSize::new(PANEL_WIDTH, height));
        ui.title.setFrame(NSRect::new(
            NSPoint::new(22.0, height - 47.0),
            NSSize::new(416.0, 32.0),
        ));
        ui.service_label.setFrame(NSRect::new(
            NSPoint::new(22.0, height - 81.0),
            NSSize::new(416.0, 24.0),
        ));
        ui.software_heading.setFrame(NSRect::new(
            NSPoint::new(22.0, height - 111.0),
            NSSize::new(416.0, 22.0),
        ));
        ui.software_empty.setHidden(!entries.is_empty());
        for (index, row) in ui.software_rows.iter().enumerate() {
            let entry = entries.get(index);
            let hidden = entry.is_none();
            row.image.setHidden(hidden);
            row.placeholder.setHidden(hidden);
            row.label.setHidden(hidden);
            row.state.setHidden(hidden);
            let Some(entry) = entry else {
                row.image.setImage(None);
                row.label.setStringValue(ns_string!(""));
                row.state.setStringValue(ns_string!(""));
                continue;
            };
            let y = ROW_BOTTOM + (row_count - index - 1) as f64 * ROW_HEIGHT;
            row.image.setFrame(NSRect::new(
                NSPoint::new(22.0, y + 2.0),
                NSSize::new(32.0, 32.0),
            ));
            row.placeholder
                .setFrame(NSRect::new(NSPoint::new(24.0, y), NSSize::new(30.0, 36.0)));
            row.label
                .setFrame(NSRect::new(NSPoint::new(66.0, y), NSSize::new(244.0, 36.0)));
            row.state.setFrame(NSRect::new(
                NSPoint::new(322.0, y + 7.0),
                NSSize::new(116.0, 22.0),
            ));
            row.label
                .setStringValue(&NSString::from_str(&entry.display_label()));
            row.label
                .setToolTip(Some(&NSString::from_str(&entry.label)));
            row.state
                .setStringValue(&NSString::from_str(entry.presence.label()));
            let image = entry.icon.as_ref().and_then(|bytes| {
                let image = NSImage::initWithData(NSImage::alloc(), &NSData::with_bytes(bytes))?;
                if !image.isValid() {
                    return None;
                }
                image.setSize(NSSize::new(32.0, 32.0));
                Some(image)
            });
            row.image.setImage(image.as_deref());
            row.image.setHidden(image.is_none());
            row.placeholder.setHidden(image.is_some());
        }
        for menu in &ui.software_menus {
            // Existing command indices stay unchanged. Rebuild only when the
            // displayed snapshot changes, not on every timer/menu tracking tick.
            while menu.numberOfItems() > BASE_MENU_ITEMS {
                menu.removeItemAtIndex(BASE_MENU_ITEMS);
            }
            if !entries.is_empty() {
                menu.addItem(&NSMenuItem::separatorItem(self.mtm()));
                let heading = menu_item(self, "Saved software · presence only", None, "");
                heading.setEnabled(false);
                menu.addItem(&heading);
                for (entry, row) in entries.iter().zip(&ui.software_rows) {
                    let image = row.image.image();
                    let item = menu_item(
                        self,
                        &entry.menu_title(image.is_some()),
                        Some(sel!(showPanel:)),
                        "",
                    );
                    item.setImage(image.as_deref());
                    item.setToolTip(Some(ns_string!("Show VibDock's panel. Application presence does not establish playback or voice state.")));
                    menu.addItem(&item);
                }
            }
        }
        *ui.rendered_software.borrow_mut() = Some(entries);
    }

    fn software_acceptance_ready(&self) -> bool {
        let Some(expected) = &self.ivars().software_acceptance else {
            return true;
        };
        let entries = self.ivars().software.borrow().display(Instant::now());
        let offline_ready = expected.offline.as_ref().is_none_or(|(id, label)| {
            entries.iter().any(|entry| {
                &entry.id == id
                    && &entry.label == label
                    && entry.presence != Presence::Running
                    && entry.icon.is_none()
            })
        });
        let running_ready = expected.running.as_ref().is_none_or(|id| {
            entries
                .iter()
                .position(|entry| {
                    &entry.id == id && entry.presence == Presence::Running && entry.icon.is_some()
                })
                .is_some_and(|index| {
                    self.ivars().ui.get().is_some_and(|ui| {
                        ui.software_rows[index]
                            .image
                            .image()
                            .is_some_and(|image| image.isValid())
                    })
                })
        });
        offline_ready && running_ready
    }

    fn exercise_software(&self, ui: &Ui) -> crate::Result<()> {
        let Some(expected) = &self.ivars().software_acceptance else {
            return Ok(());
        };
        self.check("software_acceptance_requested", true);
        self.check(
            "software_offline_fixture_requested",
            expected.offline.is_some(),
        );
        self.check(
            "software_self_observation_requested",
            expected.running.is_some(),
        );
        let entries = self.ivars().software.borrow().display(Instant::now());
        let view = ui
            .panel
            .contentView()
            .ok_or("The software panel has no content view.")?;
        let bounds = view.bounds();
        self.check(
            "software_rows_match_summary",
            !entries.is_empty()
                && entries.len() <= MAX_SOFTWARE_ROWS
                && entries.iter().zip(&ui.software_rows).all(|(entry, row)| {
                    !row.label.isHidden()
                        && row.label.stringValue().to_string() == entry.display_label()
                        && row.state.stringValue().to_string() == entry.presence.label()
                })
                && ui
                    .software_rows
                    .iter()
                    .skip(entries.len())
                    .all(|row| row.label.isHidden()),
        );
        self.check(
            "software_rows_within_panel",
            ui.software_rows.iter().take(entries.len()).all(|row| {
                [
                    row.image.frame(),
                    row.placeholder.frame(),
                    row.label.frame(),
                    row.state.frame(),
                ]
                .iter()
                .all(|frame| rect_contains(bounds, *frame))
            }),
        );
        self.check(
            "software_status_menu_rows",
            software_menu_matches(&ui.status_menu, &entries, &ui.software_rows),
        );
        self.check(
            "software_dock_menu_rows",
            software_menu_matches(&ui.dock_menu, &entries, &ui.software_rows),
        );
        self.check(
            "software_presence_only",
            CAPABILITY_TEXT.contains("application presence only")
                && CAPABILITY_TEXT.contains("Playback")
                && ui.software_menus.iter().all(|menu| {
                    entries.iter().enumerate().all(|(index, _)| {
                        menu.itemAtIndex(SOFTWARE_MENU_START + index as isize)
                            .is_some_and(|item| item.action() == Some(sel!(showPanel:)))
                    })
                }),
        );
        self.check(
            "software_ui_has_no_bundle_paths",
            ui.software_rows
                .iter()
                .take(entries.len())
                .all(|row| !row.label.stringValue().to_string().contains('/'))
                && ui.software_menus.iter().all(|menu| {
                    (0..entries.len()).all(|index| {
                        menu.itemAtIndex(SOFTWARE_MENU_START + index as isize)
                            .is_some_and(|item| !item.title().to_string().contains('/'))
                    })
                }),
        );
        if let Some((id, label)) = &expected.offline {
            let offline = entries
                .iter()
                .enumerate()
                .find(|(_, entry)| &entry.id == id);
            self.check(
                "software_persisted_offline_label",
                offline.is_some_and(|(index, entry)| {
                    &entry.label == label
                        && ui.software_rows[index].label.stringValue().to_string() == *label
                }),
            );
            self.check(
                "software_offline_not_running_or_unknown",
                offline.is_some_and(|(_, entry)| {
                    matches!(entry.presence, Presence::NotRunning | Presence::Unknown)
                }),
            );
            self.check(
                "software_offline_placeholder",
                offline.is_some_and(|(index, _)| {
                    let row = &ui.software_rows[index];
                    row.image.image().is_none()
                        && !row.placeholder.isHidden()
                        && row.placeholder.stringValue().to_string() == "□"
                }),
            );
        }
        if let Some(id) = &expected.running {
            let running = entries
                .iter()
                .enumerate()
                .find(|(_, entry)| &entry.id == id);
            self.check(
                "software_self_running_observed",
                running.is_some_and(|(_, entry)| entry.presence == Presence::Running),
            );
            self.check(
                "software_self_native_icon",
                running.is_some_and(|(index, entry)| {
                    entry.icon.is_some()
                        && ui.software_rows[index].image.image().is_some_and(|image| {
                            image.isValid() && image.size() == NSSize::new(32.0, 32.0)
                        })
                        && ui.software_rows[index].placeholder.isHidden()
                }),
            );
        }
        self.ivars().report.borrow_mut().insert(
            "software_render".into(),
            json!({
                "row_count": entries.len(), "observation_kind": "application_presence",
                "offline_fixture_is_presence_evidence": false,
            "self_running_app_observed": expected.running.as_ref().is_some_and(|id| entries.iter()
                .any(|entry| &entry.id == id && entry.presence == Presence::Running && entry.icon.is_some())),
                "playback_supported": false, "voice_supported": false,
                "third_party_application_control": false,
            }),
        );
        // Advance only the display clock for the actual observed snapshot. This
        // neither changes OS process state nor injects fabricated live entries.
        self.render_software(ui, Instant::now() + STATUS_TTL);
        self.check(
            "software_expiry_unknown",
            ui.software_rows
                .iter()
                .take(entries.len())
                .all(|row| row.state.stringValue().to_string() == Presence::Unknown.label()),
        );
        self.check(
            "software_expiry_clears_icons",
            ui.software_rows
                .iter()
                .take(entries.len())
                .all(|row| row.image.image().is_none() && !row.placeholder.isHidden())
                && ui.software_menus.iter().all(|menu| {
                    (0..entries.len()).all(|index| {
                        menu.itemAtIndex(SOFTWARE_MENU_START + index as isize)
                            .is_some_and(|item| {
                                item.image().is_none()
                                    && item.title().to_string().ends_with(" · Unknown")
                            })
                    })
                }),
        );
        self.render_software(ui, Instant::now());
        Ok(())
    }

    fn acceptance_step(&self) {
        if self.ivars().started.elapsed() > ACCEPTANCE_BUDGET {
            self.fail_acceptance(if !self.ivars().finished_launching.get() {
                "AppKit did not finish launching within the 20-second acceptance deadline."
            } else if self.ivars().stage.get() == 0 && !self.software_acceptance_ready() {
                "Saved software rows or the requested live application/icon were not observed within the 20-second acceptance deadline."
            } else if self.ivars().stage.get() == 0 {
                "The initial native panel or proved service was not ready within the 20-second acceptance deadline."
            } else {
                "Native acceptance exceeded its 20-second deadline."
            });
            return;
        }
        match self.ivars().stage.get() {
            // AppKit may pump common-mode timers during finishLaunching(),
            // before the delegate's initial Show. Observe both the genuine
            // notification and its visible window before exercising lifecycle.
            // Never issue a test-only Show to conceal a broken startup path.
            0 if self.ivars().finished_launching.get()
                && self.ivars().observation.get().is_some()
                && self.software_acceptance_ready()
                && self
                    .ivars()
                    .ui
                    .get()
                    .is_some_and(|ui| ui.panel.isVisible() && ui.panel.windowNumber() > 0) =>
            {
                self.check("service_ownership_proved", true);
                if let Err(error) = self.exercise_widgets() {
                    self.fail_acceptance(&error.to_string());
                    return;
                }
                self.ivars()
                    .reopen_baseline
                    .set(self.ivars().owner_reopens.get());
                self.ivars().stage.set(4);
                self.hide_panel();
                self.queue(Command::ReopenProbe);
            }
            4 if self.ivars().second_launcher_exited.get()
                && self.ivars().owner_reopens.get() > self.ivars().reopen_baseline.get() =>
            {
                if let Some(ui) = self.ivars().ui.get() {
                    self.check(
                        "owner_ipc_reopen_restores",
                        ui.panel.isVisible()
                            && ui.panel.windowNumber() == self.ivars().acceptance_window.get(),
                    );
                    self.ivars().stage.set(1);
                    ui.status_menu.performActionForItemAtIndex(MENU_SETTINGS);
                }
            }
            2 => {
                self.ivars().stage.set(3);
                if self.ivars().system_quit_acceptance {
                    if let Err(error) = self.schedule_system_quit() {
                        self.fail_acceptance(&error.to_string());
                    }
                } else if let Some(ui) = self.ivars().ui.get() {
                    ui.dock_menu.performActionForItemAtIndex(MENU_QUIT);
                }
            }
            _ => {}
        }
    }

    fn schedule_system_quit(&self) -> crate::Result<()> {
        // Never enter terminate() from the service poll timer: a timer cannot
        // refire while its callback is inside AppKit's nested modal run loop.
        // SAFETY: The selector takes an NSTimer sender and runs on the main
        // thread. Retain/invalidate this one-shot alongside the polling timer.
        let timer = unsafe {
            NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
                0.001,
                self,
                sel!(acceptanceSystemQuit:),
                None,
                false,
            )
        };
        self.ivars()
            .system_quit_timer
            .set(timer.clone())
            .map_err(|_| "Native system Quit was already scheduled.")?;
        unsafe { NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes) };
        Ok(())
    }

    fn exercise_widgets(&self) -> crate::Result<()> {
        let ui = self
            .ivars()
            .ui
            .get()
            .ok_or("Native widgets were not created.")?;
        let app = NSApplication::sharedApplication(self.mtm());
        let number = ui.panel.windowNumber();
        self.ivars().acceptance_window.set(number);
        self.ivars()
            .report
            .borrow_mut()
            .insert("native_window_number".into(), json!(number));
        self.check("main_thread", MainThreadMarker::new().is_some());
        self.check(
            "application_finished_launching",
            self.ivars().finished_launching.get(),
        );
        self.check("native_panel_visible", ui.panel.isVisible() && number > 0);
        self.check("floating_panel", ui.panel.isFloatingPanel());
        self.check(
            "original_template_image",
            ui.menu_image.isValid() && ui.menu_image.isTemplate(),
        );
        self.check(
            "retained_status_item",
            ui.status.isVisible() && ui.status.button(self.mtm()).is_some(),
        );
        self.check(
            "dock_entry",
            app.activationPolicy() == NSApplicationActivationPolicy::Regular,
        );
        self.check("status_menu", native_menu_is_valid(&ui.status_menu));
        // Send the actual delegate selectors, including the Dock's request for
        // its own menu. This is in-process lifecycle evidence, not mouse input.
        let dock: Option<Retained<NSMenu>> = unsafe { msg_send![self, applicationDockMenu: &*app] };
        self.check(
            "dock_menu",
            dock.as_ref().is_some_and(|menu| native_menu_is_valid(menu)),
        );
        ui.status_menu.performActionForItemAtIndex(MENU_HIDE);
        self.check(
            "hide_keeps_resident",
            !ui.panel.isVisible() && !self.ivars().stopped.get(),
        );
        ui.dock_menu.performActionForItemAtIndex(MENU_SHOW);
        self.check(
            "dock_show_reuses_panel",
            ui.panel.isVisible() && ui.panel.windowNumber() == number,
        );
        ui.panel.performClose(None);
        self.check(
            "close_hides_only",
            !ui.panel.isVisible() && !self.ivars().stopped.get(),
        );
        let reopened: bool = unsafe {
            msg_send![self, applicationShouldHandleReopen: &*app, hasVisibleWindows: false]
        };
        self.check(
            "reopen_reuses_panel",
            reopened && ui.panel.isVisible() && ui.panel.windowNumber() == number,
        );
        let terminate_on_close: bool =
            unsafe { msg_send![self, applicationShouldTerminateAfterLastWindowClosed: &*app] };
        self.check("last_window_keeps_resident", !terminate_on_close);
        self.exercise_software(ui)?;
        self.capture_own_view(ui)?;
        Ok(())
    }

    fn capture_own_view(&self, ui: &Ui) -> crate::Result<()> {
        let view = ui
            .panel
            .contentView()
            .ok_or("The panel has no content view.")?;
        view.displayIfNeeded();
        let bounds = view.bounds();
        let bitmap = view
            .bitmapImageRepForCachingDisplayInRect(bounds)
            .ok_or("AppKit could not allocate an own-view bitmap.")?;
        view.cacheDisplayInRect_toBitmapImageRep(bounds, &bitmap);
        // SAFETY: PNG accepts an empty, immutable property dictionary.
        let png = unsafe {
            bitmap.representationUsingType_properties(
                NSBitmapImageFileType::PNG,
                &NSDictionary::new(),
            )
        }
        .ok_or("AppKit could not encode the own-view bitmap.")?;
        self.check(
            "native_view_rendered",
            bitmap.pixelsWide() > 0 && bitmap.pixelsHigh() > 0 && !png.is_empty(),
        );
        self.ivars().report.borrow_mut().insert(
            "render".into(),
            json!({
                "kind": "AppKit own-content-view bitmap; not a desktop screenshot",
                "width": bitmap.pixelsWide(), "height": bitmap.pixelsHigh(),
                "screen_recording_requested": false
            }),
        );
        // Only the explicit acceptance mode can write this opt-in artifact.
        if let Some(path) = std::env::var_os("VIBDOCK_NATIVE_ACCEPTANCE_IMAGE") {
            std::fs::write(path, png.to_vec())?;
            self.check("native_view_png_written", true);
        }
        Ok(())
    }

    fn stop_loop(&self) {
        if self.ivars().stopped.replace(true) {
            return;
        }
        let app = NSApplication::sharedApplication(self.mtm());
        app.stop(None);
        // stop: does not itself wake nextEventMatchingMask:. Post a harmless
        // application-local event so termination cannot require user input.
        if let Some(event) = NSEvent::otherEventWithType_location_modifierFlags_timestamp_windowNumber_context_subtype_data1_data2(
            NSEventType::ApplicationDefined, NSPoint::new(0.0, 0.0),
            NSEventModifierFlags::empty(), 0.0, 0, None, 0, 0, 0,
        ) {
            app.postEvent_atStart(&event, true);
        }
    }

    fn cleanup_native(&self) {
        if self.ivars().cleaned.replace(true) {
            return;
        }
        if let Some(timer) = self.ivars().timer.get() {
            timer.invalidate();
        }
        if let Some(timer) = self.ivars().system_quit_timer.get() {
            timer.invalidate();
        }
        if let Some(ui) = self.ivars().ui.get() {
            ui.status.setMenu(None);
            NSStatusBar::systemStatusBar().removeStatusItem(&ui.status);
            ui.panel.setDelegate(None);
            ui.panel.orderOut(None);
            ui.panel.close();
        }
        let app = NSApplication::sharedApplication(self.mtm());
        app.setDelegate(None);
        app.setMainMenu(None);
    }

    fn final_report(&self) -> Value {
        let mut report = self.ivars().report.borrow().clone();
        if self.ivars().acceptance {
            let displayed = self.ivars().software.borrow().display(Instant::now());
            let running = self
                .ivars()
                .software_acceptance
                .as_ref()
                .and_then(|expected| expected.running.as_ref());
            let running_index =
                running.and_then(|id| displayed.iter().position(|entry| &entry.id == id));
            report.insert("software_final_display".into(), json!({
                "acceptance_ready": self.software_acceptance_ready(),
                "row_count": displayed.len(),
                "rows": displayed.iter().map(|entry| json!({
                    "state": entry.presence.label(), "decoded_icon_present": entry.icon.is_some(),
                })).collect::<Vec<_>>(),
                "native_rows": self.ivars().ui.get().map(|ui| ui.software_rows.iter()
                    .filter(|row| !row.label.isHidden()).map(|row| {
                        let state = match row.state.stringValue().to_string().as_str() {
                            "Running" => "running", "Not running" => "not_running",
                            "Unknown" => "unknown", _ => "invalid",
                        };
                        json!({ "state": state, "placeholder_visible": !row.placeholder.isHidden(),
                            "native_icon_valid": row.image.image().is_some_and(|image| image.isValid()),
                        })
                    }).collect::<Vec<_>>()),
                "expected_live_row_present": running.map(|_| running_index.is_some()),
                "expected_live_row_running": running.map(|_| running_index.is_some_and(|index| displayed[index].presence == Presence::Running)),
                "expected_live_native_icon_valid": running.map(|_| running_index.is_some_and(|index| {
                    self.ivars().ui.get().is_some_and(|ui| ui.software_rows[index].image.image()
                        .is_some_and(|image| image.isValid()))
                })),
            }));
            report.insert(
                "native_resources_released".into(),
                json!(self.ivars().cleaned.get()),
            );
            let passed = acceptance_passed(&report);
            report.insert("passed".into(), json!(passed));
            report.insert("pid".into(), json!(std::process::id()));
            report.insert(
                "macos_version".into(),
                json!(NSProcessInfo::processInfo()
                    .operatingSystemVersionString()
                    .to_string()),
            );
            report.insert("evidence".into(), json!("Real in-process AppKit widgets, a second launcher, and real isolated service; no pointer interaction or full-desktop acceptance."));
            report.insert("browser_launched".into(), json!(false));
            report.insert("audio_supported".into(), json!(false));
            report
                .entry("system_termination_path")
                .or_insert(json!(false));
        }
        Value::Object(report)
    }
}

fn rect_contains(outer: NSRect, inner: NSRect) -> bool {
    inner.origin.x >= outer.origin.x
        && inner.origin.y >= outer.origin.y
        && inner.origin.x + inner.size.width <= outer.origin.x + outer.size.width
        && inner.origin.y + inner.size.height <= outer.origin.y + outer.size.height
}

fn native_menu_is_valid(menu: &NSMenu) -> bool {
    let count = menu.numberOfItems();
    (count == BASE_MENU_ITEMS
        || (SOFTWARE_MENU_START + 1..=SOFTWARE_MENU_START + MAX_SOFTWARE_ROWS as isize)
            .contains(&count))
        && [
            (MENU_SHOW, sel!(showPanel:)),
            (MENU_HIDE, sel!(hidePanel:)),
            (MENU_SETTINGS, sel!(openSettings:)),
            (MENU_QUIT, sel!(quitVibDock:)),
        ]
        .iter()
        .all(|(index, selector)| {
            menu.itemAtIndex(*index)
                .is_some_and(|item| item.action() == Some(*selector))
        })
}

fn software_menu_matches(menu: &NSMenu, entries: &[SoftwareEntry], rows: &[SoftwareRow]) -> bool {
    menu.numberOfItems() == SOFTWARE_MENU_START + entries.len() as isize
        && entries
            .iter()
            .zip(rows)
            .enumerate()
            .all(|(index, (entry, row))| {
                let image = row.image.image();
                menu.itemAtIndex(SOFTWARE_MENU_START + index as isize)
                    .is_some_and(|item| {
                        item.title().to_string() == entry.menu_title(image.is_some())
                            && item.image().is_some() == image.is_some()
                            && item.action() == Some(sel!(showPanel:))
                    })
            })
}

fn menu_item(
    delegate: &ResidentDelegate,
    title: &str,
    action: Option<Sel>,
    key: &str,
) -> Retained<NSMenuItem> {
    // SAFETY: All selectors name the delegate's matching one-sender action
    // methods. The delegate outlives every menu and button targeting it.
    unsafe {
        let item = NSMenuItem::initWithTitle_action_keyEquivalent(
            NSMenuItem::alloc(delegate.mtm()),
            &NSString::from_str(title),
            action,
            &NSString::from_str(key),
        );
        item.setTarget(Some(delegate));
        item
    }
}

fn make_menu(
    delegate: &ResidentDelegate,
    image: &NSImage,
) -> (Retained<NSMenu>, Retained<NSMenuItem>) {
    let menu = NSMenu::new(delegate.mtm());
    menu.setAutoenablesItems(false);
    let summary = menu_item(delegate, "Service unknown / unavailable", None, "");
    summary.setEnabled(false);
    summary.setImage(Some(image));
    menu.addItem(&summary);
    menu.addItem(&NSMenuItem::separatorItem(delegate.mtm()));
    menu.addItem(&menu_item(
        delegate,
        "Show panel",
        Some(sel!(showPanel:)),
        "",
    ));
    menu.addItem(&menu_item(
        delegate,
        "Hide panel",
        Some(sel!(hidePanel:)),
        "",
    ));
    menu.addItem(&menu_item(
        delegate,
        "Settings…",
        Some(sel!(openSettings:)),
        ",",
    ));
    menu.addItem(&NSMenuItem::separatorItem(delegate.mtm()));
    menu.addItem(&menu_item(
        delegate,
        "Quit VibDock",
        Some(sel!(quitVibDock:)),
        "q",
    ));
    (menu, summary)
}

fn build_ui(delegate: &ResidentDelegate, app: &NSApplication) -> crate::Result<Ui> {
    let mtm = delegate.mtm();
    let original = NSImage::initWithData(
        NSImage::alloc(),
        &NSData::with_bytes(include_bytes!("../assets/brand/vibdock.icns")),
    )
    .ok_or("AppKit could not read the original VibDock icon.")?;
    let menu_image = NSImage::initWithData(
        NSImage::alloc(),
        &NSData::with_bytes(include_bytes!("../assets/brand/vibdock-dark.ico")),
    )
    .ok_or("AppKit could not read the original monochrome VibDock icon.")?;
    menu_image.setSize(NSSize::new(18.0, 18.0));
    // The original silhouette remains unchanged; AppKit renders the template
    // in the correct system menu-bar appearance and contrast.
    menu_image.setTemplate(true);
    // SAFETY: Both images are retained for the native setters' call duration;
    // AppKit retains the application image. No arbitrary external path is read.
    unsafe { app.setApplicationIconImage(Some(&original)) };
    // A bundled app may already have its required policy from Info.plist.
    // Check the actual state, rather than requiring a redundant change to
    // succeed. The setter only reports whether its change was accepted.
    let policy_before = app.activationPolicy();
    let policy_request = (policy_before != NSApplicationActivationPolicy::Regular)
        .then(|| app.setActivationPolicy(NSApplicationActivationPolicy::Regular));
    let policy_after = app.activationPolicy();
    if delegate.ivars().acceptance {
        delegate.ivars().report.borrow_mut().insert(
            "activation_policy".into(),
            json!({
                "before": policy_before.0,
                "request_accepted": policy_request,
                "after": policy_after.0,
            }),
        );
    }
    if policy_after != NSApplicationActivationPolicy::Regular {
        return Err(format!(
            "macOS did not provide VibDock's Dock activation policy (before={}, request_accepted={policy_request:?}, after={}). A usable macOS desktop session is required.",
            policy_before.0, policy_after.0,
        ).into());
    }

    let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
        NSPanel::alloc(mtm),
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(PANEL_WIDTH, 321.0)),
        NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
        NSBackingStoreType::Buffered,
        false,
    );
    // SAFETY: Our retained owner, rather than close:, controls panel lifetime.
    unsafe { panel.setReleasedWhenClosed(false) };
    panel.setTitle(ns_string!("VibDock"));
    panel.setFloatingPanel(true);
    panel.setLevel(NSFloatingWindowLevel);
    panel.setHidesOnDeactivate(false);
    panel.setDelegate(Some(ProtocolObject::from_ref(delegate)));
    panel.center();
    let content = panel
        .contentView()
        .ok_or("AppKit created no panel content view.")?;
    let title = NSTextField::labelWithString(ns_string!("VibDock"), mtm);
    title.setFont(Some(&NSFont::boldSystemFontOfSize(24.0)));
    title.setFrame(NSRect::new(
        NSPoint::new(22.0, 198.0),
        NSSize::new(416.0, 32.0),
    ));
    let service_label =
        NSTextField::labelWithString(ns_string!("Checking service ownership…"), mtm);
    service_label.setFrame(NSRect::new(
        NSPoint::new(22.0, 164.0),
        NSSize::new(416.0, 24.0),
    ));
    let capabilities =
        NSTextField::wrappingLabelWithString(&NSString::from_str(CAPABILITY_TEXT), mtm);
    capabilities.setFont(Some(&NSFont::systemFontOfSize(12.0)));
    capabilities.setFrame(NSRect::new(
        NSPoint::new(22.0, 111.0),
        NSSize::new(416.0, 44.0),
    ));
    let notice_label = NSTextField::wrappingLabelWithString(
        ns_string!("Closing this panel keeps the service and menu-bar entry running."),
        mtm,
    );
    notice_label.setFrame(NSRect::new(
        NSPoint::new(22.0, 61.0),
        NSSize::new(416.0, 44.0),
    ));
    let software_heading =
        NSTextField::labelWithString(ns_string!("Saved software · manage in Settings"), mtm);
    software_heading.setFont(Some(&NSFont::boldSystemFontOfSize(12.0)));
    let software_empty = NSTextField::labelWithString(
        ns_string!("Add saved software from the running apps in Settings."),
        mtm,
    );
    software_empty.setFont(Some(&NSFont::systemFontOfSize(12.0)));
    software_empty.setFrame(NSRect::new(
        NSPoint::new(22.0, ROW_BOTTOM + 8.0),
        NSSize::new(416.0, 22.0),
    ));
    let mut software_rows = Vec::with_capacity(MAX_SOFTWARE_ROWS);
    for _ in 0..MAX_SOFTWARE_ROWS {
        let image = NSImageView::new(mtm);
        image.setEditable(false);
        image.setAnimates(false);
        image.setAllowsCutCopyPaste(false);
        let placeholder = NSTextField::labelWithString(ns_string!("□"), mtm);
        placeholder.setFont(Some(&NSFont::systemFontOfSize(28.0)));
        let label = NSTextField::wrappingLabelWithString(ns_string!(""), mtm);
        label.setFont(Some(&NSFont::systemFontOfSize(13.0)));
        label.setMaximumNumberOfLines(2);
        let state = NSTextField::labelWithString(ns_string!("Unknown"), mtm);
        state.setFont(Some(&NSFont::systemFontOfSize(12.0)));
        state.setToolTip(Some(ns_string!(
            "Application presence only; playback, microphone and voice state are unsupported."
        )));
        content.addSubview(&image);
        for view in [&placeholder, &label, &state] {
            content.addSubview(view);
        }
        software_rows.push(SoftwareRow {
            image,
            placeholder,
            label,
            state,
        });
    }
    // SAFETY: The views are main-thread native controls; the content view retains them.
    unsafe {
        for view in [
            &title,
            &service_label,
            &software_heading,
            &software_empty,
            &capabilities,
            &notice_label,
        ] {
            content.addSubview(view);
        }
        for (x, width, text, selector) in [
            (22.0, 124.0, "Settings…", sel!(openSettings:)),
            (158.0, 124.0, "Hide panel", sel!(hidePanel:)),
            (294.0, 144.0, "Quit VibDock", sel!(quitVibDock:)),
        ] {
            let button = NSButton::buttonWithTitle_target_action(
                &NSString::from_str(text),
                Some(delegate),
                Some(selector),
                mtm,
            );
            button.setFrame(NSRect::new(NSPoint::new(x, 16.0), NSSize::new(width, 32.0)));
            content.addSubview(&button);
        }
    }
    let (status_menu, status_summary) = make_menu(delegate, &menu_image);
    let (dock_menu, dock_summary) = make_menu(delegate, &menu_image);
    let (app_menu, app_summary) = make_menu(delegate, &menu_image);
    let main_menu = NSMenu::new(mtm);
    let application_item = menu_item(delegate, "VibDock", None, "");
    application_item.setSubmenu(Some(&app_menu));
    main_menu.addItem(&application_item);
    app.setMainMenu(Some(&main_menu));
    let status = NSStatusBar::systemStatusBar().statusItemWithLength(26.0);
    let button = status
        .button(mtm)
        .ok_or("AppKit created no menu-bar button.")?;
    button.setImage(Some(&menu_image));
    button.setToolTip(Some(ns_string!("VibDock • service controls")));
    status.setMenu(Some(&status_menu));
    let ui = Ui {
        panel,
        status,
        software_menus: vec![status_menu.clone(), dock_menu.clone(), app_menu],
        status_menu,
        dock_menu,
        summaries: vec![status_summary, dock_summary, app_summary],
        title,
        service_label,
        software_heading,
        software_empty,
        software_rows,
        rendered_software: RefCell::new(None),
        notice_label,
        menu_image,
    };
    delegate.render_software(&ui, Instant::now());
    Ok(ui)
}

fn run_native(acceptance: bool, reopen: Receiver<()>) -> crate::Result<Value> {
    autoreleasepool(|_| {
        let mtm = MainThreadMarker::new()
            .ok_or("The macOS resident UI must start on the main thread.")?;
        let app = NSApplication::sharedApplication(mtm);
        let delegate = ResidentDelegate::new(mtm, acceptance, reopen)?;
        let ui = build_ui(&delegate, &app)?;
        delegate
            .ivars()
            .ui
            .set(ui)
            .map_err(|_| "Native UI was already initialized.")?;
        app.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
        // SAFETY: pollService: has the NSTimer sender signature, and the timer
        // runs only on the main run loop. Common modes keep status expiring while
        // a menu is tracking. Invalidate before releasing the delegate below.
        let timer = unsafe {
            let timer = NSTimer::timerWithTimeInterval_target_selector_userInfo_repeats(
                0.1,
                &delegate,
                sel!(pollService:),
                None,
                true,
            );
            NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSRunLoopCommonModes);
            NSRunLoop::mainRunLoop().addTimer_forMode(&timer, NSModalPanelRunLoopMode);
            timer
        };
        delegate
            .ivars()
            .timer
            .set(timer.clone())
            .map_err(|_| "Native timer was already initialized.")?;
        app.run();
        delegate.cleanup_native();
        delegate.ivars().worker.shutdown();
        // Worker is joined when the final retained delegate goes away, after
        // the event loop has stopped and its timer no longer retains its target.
        Ok(delegate.final_report())
    })
}

/// Run on the process main thread after the launcher has acquired its singleton
/// and started/proved the service. LaunchServices reopen targets this delegate.
pub fn run(reopen: Receiver<()>) -> crate::Result<()> {
    run_native(false, reopen).map(|_| ())
}

/// Exercise real native lifecycle and authenticated service operations. The
/// launcher must enforce the isolated acceptance directory/port opt-in first.
/// This needs an available macOS WindowServer; hosted builds alone are no proof.
pub fn self_acceptance(reopen: Receiver<()>) -> crate::Result<Value> {
    run_native(true, reopen)
}

/// Visible startup errors for a double-clicked .app. Acceptance callers must
/// use stderr instead: a modal alert must never hold an unattended CI job open.
pub fn report_error(message: &str) {
    let Some(mtm) = MainThreadMarker::new() else {
        eprintln!("ERROR: {message}");
        return;
    };
    autoreleasepool(|_| {
        let alert = NSAlert::new(mtm);
        alert.setMessageText(ns_string!("VibDock could not start"));
        alert.setInformativeText(&NSString::from_str(message));
        alert.addButtonWithTitle(ns_string!("OK"));
        alert.runModal();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn software_summary() -> Value {
        json!({
            "platform": "macos", "observation_kind": "application_presence",
            "available": true, "complete": false, "age_ms": 0,
            "entries": [{ "id": "a".repeat(64), "label": "Native application",
                "state": "running", "complete": false, "sessions": [],
                "actions_enabled": false, "icon_mime": "image/png", "icon_b64": null,
            }],
        })
    }

    #[test]
    fn acceptance_diagnostics_are_bounded_and_never_expose_private_strings() {
        let mut value = software_summary();
        value["error"] = json!("Cannot read /Users/SECRET_PRIVATE/App.app token=SECRET_PRIVATE");
        value["entries"][0]["id"] = json!("SECRET_PRIVATE_ID");
        value["entries"][0]["label"] = json!("SECRET_PRIVATE_LABEL");
        value["entries"][0]["icon_b64"] = json!("SECRET_PRIVATE_ICON");
        let expected = SoftwareAcceptance {
            offline: Some(("SECRET_PRIVATE_ID".into(), "SECRET_PRIVATE_LABEL".into())),
            running: Some("SECRET_PRIVATE_ID".into()),
        };
        let snapshot = SoftwareSnapshot::parse(&value, Instant::now());
        let diagnostic = software_observation_diagnostic(
            Some(&value),
            snapshot.as_ref(),
            Some(&expected),
            None,
            Duration::from_millis(12),
        );
        assert_eq!(diagnostic["service_proof_succeeded"], true);
        assert_eq!(diagnostic["parser_accepted"], true);
        assert_eq!(diagnostic["expected_offline_id_present"], true);
        assert_eq!(diagnostic["expected_live_id_present"], true);
        assert_eq!(diagnostic["observation_error"], "other_error_redacted");
        assert!(!diagnostic.to_string().contains("SECRET_PRIVATE"));
        let row = value["entries"][0].clone();
        value["entries"] = json!(vec![row; 20]);
        let diagnostic =
            software_observation_diagnostic(Some(&value), None, None, None, Duration::ZERO);
        assert_eq!(diagnostic["raw_row_count"], 20);
        assert_eq!(
            diagnostic["rows"].as_array().unwrap().len(),
            MAX_SOFTWARE_ROWS
        );
        assert_eq!(diagnostic["parser_accepted"], false);
    }

    #[test]
    fn failed_observation_diagnostics_do_not_invent_proof_or_print_errors() {
        for (error, proof, failure) in [
            ("No service instance", json!(false), "no_service_instance"),
            (
                "Listener failed instance proof",
                json!(false),
                "instance_proof_rejected",
            ),
            (
                "Software status is unavailable",
                json!(true),
                "summary_status_unavailable",
            ),
            (
                "SECRET_PRIVATE /Users/private token",
                Value::Null,
                "proof_or_summary_exchange_failed",
            ),
        ] {
            let error: Box<dyn std::error::Error> = error.into();
            let diagnostic = software_observation_diagnostic(
                None,
                None,
                None,
                Some(error.as_ref()),
                Duration::ZERO,
            );
            assert_eq!(diagnostic["service_proof_succeeded"], proof);
            assert_eq!(diagnostic["request_failure"], failure);
            assert!(!diagnostic.to_string().contains("SECRET_PRIVATE"));
        }
    }

    #[test]
    fn partial_observation_proves_presence_but_never_absence() {
        let checked = Instant::now();
        let mut value = software_summary();
        let snapshot = SoftwareSnapshot::parse(&value, checked).unwrap();
        assert_eq!(snapshot.display(checked)[0].presence, Presence::Running);
        value["entries"][0]["state"] = json!("not_running");
        let snapshot = SoftwareSnapshot::parse(&value, checked).unwrap();
        assert_eq!(snapshot.display(checked)[0].presence, Presence::Unknown);
        value["entries"][0]["complete"] = json!(true);
        let snapshot = SoftwareSnapshot::parse(&value, checked).unwrap();
        assert_eq!(snapshot.display(checked)[0].presence, Presence::NotRunning);
        assert!(snapshot.display(checked)[0].icon.is_none());
    }

    #[test]
    fn source_age_is_not_extended_by_native_polling() {
        let checked = Instant::now();
        let mut value = software_summary();
        value["age_ms"] = json!(3999);
        let snapshot = SoftwareSnapshot::parse(&value, checked).unwrap();
        assert_eq!(snapshot.display(checked)[0].presence, Presence::Running);
        assert_eq!(
            snapshot.display(checked + Duration::from_millis(1))[0].presence,
            Presence::Unknown
        );
        for age in [json!(4000), json!(6000), json!(-1), json!(0.5), Value::Null] {
            value["age_ms"] = age;
            let snapshot = SoftwareSnapshot::parse(&value, checked).unwrap();
            assert_eq!(snapshot.display(checked)[0].presence, Presence::Unknown);
        }
    }

    #[test]
    fn expiry_or_failure_keeps_saved_rows_and_clears_live_icons() {
        let checked = Instant::now();
        let mut snapshot = SoftwareSnapshot {
            entries: vec![SoftwareEntry {
                id: "saved".into(),
                label: "Saved application".into(),
                presence: Presence::Running,
                icon: Some(vec![1, 2, 3]),
            }],
            checked: Some(checked),
        };
        // These bytes exercise only pure display-state expiry, not image decode
        // or a native evidence report; no fabricated app reaches AppKit.
        let expired = snapshot.display(checked + STATUS_TTL);
        assert_eq!(expired[0].id, "saved");
        assert_eq!(expired[0].label, "Saved application");
        assert_eq!(expired[0].presence, Presence::Unknown);
        assert!(expired[0].icon.is_none());
        snapshot.expire();
        assert_eq!(snapshot.display(checked), expired);
    }

    #[test]
    fn unexpected_summary_types_and_unsafe_shapes_fail_closed() {
        let checked = Instant::now();
        let mut value = software_summary();
        value["platform"] = json!("windows");
        assert!(SoftwareSnapshot::parse(&value, checked).is_none());
        value = software_summary();
        value["observation_kind"] = json!("playback");
        assert!(SoftwareSnapshot::parse(&value, checked).is_none());
        for (field, invalid) in [
            ("actions_enabled", json!(true)),
            ("sessions", json!([{"active": true}])),
            ("state", json!("playing")),
        ] {
            value = software_summary();
            value["entries"][0][field] = invalid;
            assert_eq!(
                SoftwareSnapshot::parse(&value, checked)
                    .unwrap()
                    .display(checked)[0]
                    .presence,
                Presence::Unknown
            );
        }
        value = software_summary();
        value["available"] = json!(false);
        assert_eq!(
            SoftwareSnapshot::parse(&value, checked)
                .unwrap()
                .display(checked)[0]
                .presence,
            Presence::Unknown
        );
        value = software_summary();
        value["entries"][0]["icon_b64"] = json!("not a bounded PNG");
        assert!(SoftwareSnapshot::parse(&value, checked)
            .unwrap()
            .display(checked)[0]
            .icon
            .is_none());
    }

    #[test]
    fn software_rows_and_labels_are_bounded_without_reordering() {
        let checked = Instant::now();
        let mut value = software_summary();
        let template = value["entries"][0].clone();
        let entries: Vec<Value> = (0..MAX_SOFTWARE_ROWS)
            .map(|index| {
                let mut entry = template.clone();
                entry["id"] = json!(format!("{index:064x}"));
                entry
            })
            .collect();
        value["entries"] = json!(entries);
        let snapshot = SoftwareSnapshot::parse(&value, checked).unwrap();
        assert_eq!(snapshot.entries.len(), 8);
        assert_eq!(snapshot.entries[7].id, format!("{:064x}", 7));
        value["entries"].as_array_mut().unwrap().push(template);
        assert!(SoftwareSnapshot::parse(&value, checked).is_none());
        value = software_summary();
        value["entries"][0]["label"] = json!("x".repeat(80));
        let entry = SoftwareSnapshot::parse(&value, checked)
            .unwrap()
            .entries
            .remove(0);
        assert_eq!(entry.display_label(), format!("{}…", "x".repeat(32)));
        for label in ["x".repeat(81), "Saved\napplication".into(), String::new()] {
            value["entries"][0]["label"] = json!(label);
            assert!(SoftwareSnapshot::parse(&value, checked).is_none());
        }
        value = software_summary();
        let duplicate = value["entries"][0].clone();
        value["entries"].as_array_mut().unwrap().push(duplicate);
        assert!(SoftwareSnapshot::parse(&value, checked).is_none());
    }

    #[test]
    fn requested_software_evidence_is_required_for_acceptance() {
        let mut report = Map::new();
        for name in ACCEPTANCE_CHECKS {
            report.insert((*name).into(), json!(true));
        }
        assert!(acceptance_passed(&report));
        for (flag, checks) in [
            ("software_acceptance_requested", SOFTWARE_ACCEPTANCE_CHECKS),
            (
                "software_offline_fixture_requested",
                OFFLINE_ACCEPTANCE_CHECKS,
            ),
            (
                "software_self_observation_requested",
                RUNNING_ACCEPTANCE_CHECKS,
            ),
        ] {
            report.insert(flag.into(), json!(true));
            assert!(!acceptance_passed(&report));
            for name in checks {
                report.insert((*name).into(), json!(true));
            }
            assert!(acceptance_passed(&report));
        }
        report.insert("software_self_native_icon".into(), json!(false));
        assert!(!acceptance_passed(&report));
    }

    #[test]
    fn service_observation_expires_and_never_implies_audio() {
        let checked = Instant::now();
        let live = service_text(Some((43129, checked)), checked);
        assert_eq!(live, "Service verified on 127.0.0.1:43129");
        assert_eq!(
            service_text(Some((43129, checked)), checked + STATUS_TTL),
            "Service unknown / unavailable"
        );
        assert_eq!(service_text(None, checked), "Service unknown / unavailable");
        assert!(CAPABILITY_TEXT.contains("unavailable on macOS"));
    }

    #[test]
    fn incomplete_or_failed_native_evidence_never_passes() {
        let mut report = Map::new();
        assert!(!acceptance_passed(&report));
        for name in ACCEPTANCE_CHECKS {
            report.insert((*name).into(), json!(true));
        }
        assert!(acceptance_passed(&report));
        report.insert("owner_ipc_reopen_restores".into(), json!(false));
        assert!(!acceptance_passed(&report));
        report.insert("owner_ipc_reopen_restores".into(), json!(true));
        report.insert("error".into(), json!("WindowServer unavailable"));
        assert!(!acceptance_passed(&report));
    }
}
