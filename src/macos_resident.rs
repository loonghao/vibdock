//! Main-thread AppKit resident shell. No macOS audio backend is implied.
//!
//! Native widgets never perform network I/O. One joined worker serializes the
//! existing bounded proof, browser-code and shutdown operations; its two queues
//! are bounded. Windows software identities are not loaded by this adapter.

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
    NSEventModifierFlags, NSEventType, NSFloatingWindowLevel, NSFont, NSImage, NSMenu, NSMenuItem,
    NSModalPanelRunLoopMode, NSPanel, NSStatusBar, NSStatusItem, NSTextField, NSWindow,
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
    "Audio, global shortcuts and per-app voice controls are unavailable on macOS.";
const MENU_SHOW: isize = 2;
const MENU_HIDE: isize = 3;
const MENU_SETTINGS: isize = 4;
const MENU_QUIT: isize = 6;
const ACCEPTANCE_CHECKS: &[&str] = &[
    "service_ownership_proved",
    "main_thread",
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
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Command {
    Settings,
    Quit,
    ReopenProbe,
    Stop,
}

enum Reply {
    Observation { port: Option<u16>, checked: Instant },
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
    fn start() -> crate::Result<Self> {
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
                        // Only liveness/ownership reaches the UI, never the token or
                        // the Windows-specific persisted application inventory.
                        let port = service::live_instance().map(|instance| instance.port);
                        let _ = outgoing.try_send(Reply::Observation {
                            port,
                            checked: Instant::now(),
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

struct Ui {
    panel: Retained<NSPanel>,
    status: Retained<NSStatusItem>,
    status_menu: Retained<NSMenu>,
    dock_menu: Retained<NSMenu>,
    summaries: Vec<Retained<NSMenuItem>>,
    service_label: Retained<NSTextField>,
    notice_label: Retained<NSTextField>,
    menu_image: Retained<NSImage>,
}

struct DelegateState {
    ui: OnceCell<Ui>,
    worker: Worker,
    pending: Cell<Option<Command>>,
    observation: Cell<Option<(u16, Instant)>>,
    acceptance: bool,
    system_quit_acceptance: bool,
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
    }
);

impl ResidentDelegate {
    fn new(
        mtm: MainThreadMarker,
        acceptance: bool,
        reopen: Receiver<()>,
    ) -> crate::Result<Retained<Self>> {
        let this = Self::alloc(mtm).set_ivars(DelegateState {
            ui: OnceCell::new(),
            worker: Worker::start()?,
            pending: Cell::new(None),
            observation: Cell::new(None),
            acceptance,
            system_quit_acceptance: acceptance
                && std::env::var("VIBDOCK_NATIVE_ACCEPTANCE_SYSTEM_QUIT").as_deref() == Ok("1"),
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
                Reply::Observation { port, checked } => {
                    self.ivars()
                        .observation
                        .set(port.map(|port| (port, checked)));
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
        }
        if self.ivars().acceptance && !self.ivars().stopped.get() {
            self.acceptance_step();
        }
    }

    fn acceptance_step(&self) {
        if self.ivars().started.elapsed() > ACCEPTANCE_BUDGET {
            self.fail_acceptance("Native acceptance exceeded its 20-second deadline.");
            return;
        }
        match self.ivars().stage.get() {
            0 if self.ivars().observation.get().is_some() => {
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
                    // Exercise AppKit's genuine TerminateLater/modal callback
                    // path. Its successful true reply exits rather than returns.
                    NSApplication::sharedApplication(self.mtm()).terminate(None);
                } else if let Some(ui) = self.ivars().ui.get() {
                    ui.dock_menu.performActionForItemAtIndex(MENU_QUIT);
                }
            }
            _ => {}
        }
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
        self.check("status_menu", ui.status_menu.numberOfItems() == 7);
        // Send the actual delegate selectors, including the Dock's request for
        // its own menu. This is in-process lifecycle evidence, not mouse input.
        let dock: Option<Retained<NSMenu>> = unsafe { msg_send![self, applicationDockMenu: &*app] };
        self.check(
            "dock_menu",
            dock.as_ref().is_some_and(|menu| menu.numberOfItems() == 7),
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
        NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(460.0, 245.0)),
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
    // SAFETY: The views are main-thread native controls; the content view retains them.
    unsafe {
        for view in [&title, &service_label, &capabilities, &notice_label] {
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
    Ok(Ui {
        panel,
        status,
        status_menu,
        dock_menu,
        summaries: vec![status_summary, dock_summary, app_summary],
        service_label,
        notice_label,
        menu_image,
    })
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
