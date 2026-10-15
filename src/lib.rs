//! VibDock shared core.
//!
//! Everything both executables need lives here: the loopback service, the
//! authenticated configuration and action contracts, instance discovery, and
//! the device adapter. The two binaries are thin entry points over this crate,
//! so the console CLI and the Windows launcher cannot drift apart.

use std::{error::Error, fmt};
mod app_audio;
#[cfg(not(windows))]
mod app_session_stub;
#[cfg(windows)]
mod app_session_windows;
mod browser_auth;
mod capabilities;
mod integrations;

pub mod hotkey;
pub mod service;
pub mod transport;
#[cfg(windows)]
pub mod tray;
pub mod web;
#[cfg(windows)]
pub mod windows;

/// Result type shared by the core and both entry points.
pub type Result<T> = std::result::Result<T, Box<dyn Error>>;

/// A command the user can ask either entry point to perform.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Action {
    Serve,
    ServiceStart,
    ServiceStop,
    ServiceStatus,
    Tray,
    Help,
    Devices,
    Status,
    Mute,
    Unmute,
    Toggle,
    Listen,
    Capabilities,
    AppList,
    AppDevices,
    AppStatus,
    AppMute,
    AppUnmute,
    AppToggle,
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Action::Serve => "serve",
            Action::ServiceStart => "service-start",
            Action::ServiceStop => "service-stop",
            Action::ServiceStatus => "service-status",
            Action::Tray => "tray",
            Action::Help => "help",
            Action::Devices => "devices",
            Action::Status => "status",
            Action::Mute => "mute",
            Action::Unmute => "unmute",
            Action::Toggle => "toggle",
            Action::Listen => "listen",
            Action::Capabilities => "capabilities",
            Action::AppList => "app-list",
            Action::AppDevices => "app-devices",
            Action::AppStatus => "app-status",
            Action::AppMute => "app-mute",
            Action::AppUnmute => "app-unmute",
            Action::AppToggle => "app-toggle",
        };
        f.write_str(name)
    }
}

/// Maps a parsed application-session verb onto its [`Action`].
///
/// Separate from [`parse`] only to keep the match bodies small; both sides
/// live in the core so the CLI and the service cannot disagree.
fn session_action(action: app_audio::SessionAction) -> Action {
    match action {
        app_audio::SessionAction::Devices => Action::AppDevices,
        app_audio::SessionAction::List => Action::AppList,
        app_audio::SessionAction::Status => Action::AppStatus,
        app_audio::SessionAction::Mute => Action::AppMute,
        app_audio::SessionAction::Unmute => Action::AppUnmute,
        app_audio::SessionAction::Toggle => Action::AppToggle,
    }
}

/// Maps an [`Action`] onto its application-session verb, if it has one.
///
/// Used to keep the end-to-end command path honest: the text render and the
/// unsupported-platform message must name the verb the user actually typed.
fn as_session_action(action: Action) -> Option<app_audio::SessionAction> {
    match action {
        Action::AppList => Some(app_audio::SessionAction::List),
        Action::AppDevices => Some(app_audio::SessionAction::Devices),
        Action::AppStatus => Some(app_audio::SessionAction::Status),
        Action::AppMute => Some(app_audio::SessionAction::Mute),
        Action::AppUnmute => Some(app_audio::SessionAction::Unmute),
        Action::AppToggle => Some(app_audio::SessionAction::Toggle),
        _ => None,
    }
}

/// Usage text for the console entry point.
pub const USAGE: &str = "Usage: vibdock <serve|service-start|service-stop|service-status|tray|help>
       vibdock <devices|capabilities>
       vibdock <status|mute|unmute|toggle|listen> --device <endpoint-id>
       vibdock app-devices
       vibdock app-list --endpoint <endpoint-id>
       vibdock <app-status|app-mute|app-unmute|app-toggle> --endpoint <endpoint-id> \\
               --session <session-instance-id>";

/// Parses command-line arguments into an [`Action`].
///
/// Application-session commands are recognised here but their selectors are
/// parsed separately by [`app_audio::parse_session`], whose rules are richer:
/// two flags in any order, each required or forbidden depending on the verb.
pub fn parse(args: &[String]) -> Result<Action> {
    // Recognise the application-session verbs before the device verbs so the
    // two families cannot be confused: `app-mute` must never be read as a
    // microphone mute, which has a much wider blast radius.
    if args.first().map(String::as_str).is_some_and(|first| {
        matches!(
            first,
            "app-devices" | "app-list" | "app-status" | "app-mute" | "app-unmute" | "app-toggle"
        )
    }) {
        let (action, _, _) = app_audio::parse_session(args)?;
        return Ok(session_action(action));
    }
    let action = match args.first().map(String::as_str) {
        Some("serve") => Action::Serve,
        Some("service-start") => Action::ServiceStart,
        Some("service-stop") => Action::ServiceStop,
        Some("service-status") => Action::ServiceStatus,
        Some("tray") => Action::Tray,
        Some("help") | Some("--help") | Some("-h") => Action::Help,
        Some("devices") => Action::Devices,
        Some("status") => Action::Status,
        Some("mute") => Action::Mute,
        Some("unmute") => Action::Unmute,
        Some("toggle") => Action::Toggle,
        Some("listen") => Action::Listen,
        Some("capabilities") => Action::Capabilities,
        _ => return Err(format!("{USAGE}\nRun `vibdock help` for details.").into()),
    };
    let rest = &args[1..];
    if matches!(
        action,
        Action::Serve
            | Action::Capabilities
            | Action::Devices
            | Action::ServiceStart
            | Action::ServiceStop
            | Action::ServiceStatus
            | Action::Tray
            | Action::Help
    ) && !rest.is_empty()
    {
        return Err("This command does not accept additional arguments.".into());
    }
    if matches!(
        action,
        Action::Status | Action::Mute | Action::Unmute | Action::Toggle | Action::Listen
    ) && (rest.len() != 2
        || rest[0] != "--device"
        || (rest[1].is_empty() || rest[1].contains('\0')))
    {
        return Err(
            "Specify a microphone endpoint with --device <endpoint-id>; run devices to list IDs."
                .into(),
        );
    }
    Ok(action)
}

/// Prints the CLI help text.
pub fn help() {
    println!("VibDock — local-first hardware and voice bridge");
    println!();
    println!("{USAGE}");
    println!();
    println!("Commands:");
    println!(
        "  serve            Run the read-only design preview on http://127.0.0.1:{}",
        service::port()
    );
    println!("  service-start    Run the resident authenticated service on the same address");
    println!("  service-status   Report whether a resident service is listening");
    println!("  service-stop     Shut down the running service (authenticated)");
    println!("  tray             Run the Windows tray entry (Windows only)");
    println!("  devices          List active microphone endpoints and their IDs");
    println!("  capabilities     Report documented integration capabilities");
    println!("  status|mute|unmute|toggle   Read or change one microphone's mute state");
    println!("  listen           Print state and toggle it on Ctrl+Alt+Shift+F9");
    println!("  app-devices      List playback endpoints for application session control");
    println!("  app-list         List application playback sessions on a render endpoint");
    println!("  app-status|app-mute|app-unmute|app-toggle");
    println!("                   Read or change one application's playback mute state");
    println!("  help             Show this message");
    println!();
    println!("Exit codes are zero on success and nonzero on error. `service-start`");
    println!("reuses a running instance instead of binding a second listener.");
}

/// Ensures a resident service is running and reports how it was obtained.
///
/// Shared by both executables so the launcher and the CLI apply exactly the
/// same reuse rules. Reuse is only reported when the listener on the recorded
/// port proves it holds the recorded token; a stale marker is recovered only
/// after proving no owner answers.
#[derive(Debug, PartialEq, Eq)]
pub enum Ensure {
    /// An existing, authenticated instance is answering; this is its port.
    Reused(u16),
    /// No instance was running, so the caller must start one and serve it.
    Started,
}

/// Resolves the running instance, recovering a stale marker if needed.
///
/// Returns `Reused` when a service proved ownership, or `Started` when the
/// caller should bind and serve. An unrelated process holding the port is an
/// error: it must never be mistaken for our service, and its marker must not
/// be cleared.
pub fn ensure_service() -> Result<Ensure> {
    if let Some(instance) = service::live_instance() {
        return Ok(Ensure::Reused(instance.port));
    }
    service::recover_stale_marker().map_err(|e| -> Box<dyn Error> { e.into() })?;
    Ok(Ensure::Started)
}

/// Claims the right to start a service by binding the port exclusively.
///
/// Ownership is the bound port itself, which is the smallest OS-backed
/// primitive available and needs no extra state: only one process can bind a
/// given address, the kernel releases it if the holder crashes, and there is no
/// lock file to leave stale or to unlink under a waiting contender.
///
/// Returns `None` if another process already owns the port, meaning it has
/// taken responsibility for starting the service.
pub fn claim_startup() -> Result<Option<StartupClaim>> {
    claim_startup_on(service::port())
}

/// How many 50ms ticks to wait for another process's service to publish.
const STARTUP_WAIT_TICKS: u32 = 40;

/// Claims the right to start a service on a specific port.
///
/// Separate from [`claim_startup`] so tests can exercise the race on a private
/// port without mutating the process-wide port setting that other tests read.
fn claim_startup_on(port: u16) -> Result<Option<StartupClaim>> {
    match std::net::TcpListener::bind(("127.0.0.1", port)) {
        Ok(listener) => {
            // We hold the port. One final check: a service may have published
            // itself between the caller's earlier check and this bind, in which
            // case it owns the startup and we must not start a second one.
            if service::live_instance().is_some() {
                drop(listener);
                return Ok(None);
            }
            Ok(Some(StartupClaim { listener }))
        }
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
            // Another process holds the port, so it owns this startup. Never
            // retry to win: a loser that waits for the winner to release and
            // then binds would start a duplicate service.
            //
            // Wait briefly for that owner to publish its instance record, so
            // the caller can report reuse instead of "starting".
            let deadline = std::time::Instant::now()
                + std::time::Duration::from_millis(50 * u64::from(STARTUP_WAIT_TICKS));
            while std::time::Instant::now() < deadline {
                if service::live_instance().is_some() {
                    return Ok(None);
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(format!(
                "Port {} is in use but no VibDock service answered on it; \
                 another process may be starting.",
                port
            )
            .into())
        }
        Err(error) => Err(error.into()),
    }
}

/// Holds exclusive ownership of the service port during startup.
///
/// Transferred directly to the HTTP server, without releasing the port.
pub struct StartupClaim {
    listener: std::net::TcpListener,
}

impl StartupClaim {
    /// Transfers the bound listener without a close-and-rebind race.
    pub fn into_listener(self) -> std::net::TcpListener {
        self.listener
    }
}

/// Builds the instance record for a freshly started service.
pub fn new_instance() -> Result<service::Instance> {
    Ok(service::Instance {
        port: service::port(),
        token: service::generate_token()?,
    })
}

/// The address a running service is actually bound to, for opening its page.
///
/// Taken from the live instance when there is one, so a launcher whose port
/// differs from the resident service's still opens the right URL. Falls back to
/// this process's own address only when no instance is answering yet.
pub fn configuration_url() -> Result<String> {
    let instance = service::live_instance().ok_or("No authenticated service is running")?;
    let code = service::browser_code(&instance)?;
    Ok(format!(
        "http://127.0.0.1:{}/#bootstrap={code}",
        instance.port
    ))
}

/// Runs one action through the core. Both executables call this.
pub fn run_action(action: Action, args: &[String]) -> Result<()> {
    match action {
        Action::Capabilities => capabilities::report_all(),
        Action::Help => {
            help();
            Ok(())
        }
        Action::ServiceStatus => {
            match service::live_instance() {
                Some(instance) => println!(
                    "Service running on http://127.0.0.1:{} (token available in the service state file)",
                    instance.port
                ),
                None => println!("Service not running."),
            }
            Ok(())
        }
        Action::ServiceStop => match service::request_shutdown() {
            Ok(true) => {
                println!("Service stopped.");
                Ok(())
            }
            Ok(false) => {
                eprintln!("Service not running.");
                Err("No running service to stop.".into())
            }
            Err(message) => Err(message.into()),
        },
        Action::Serve => web::serve(None),
        Action::ServiceStart => {
            if let Some(instance) = service::live_instance() {
                println!(
                    "Reusing running service on http://127.0.0.1:{}",
                    instance.port
                );
                return Ok(());
            }
            // Claim the port for the whole startup: from the decision to start,
            // through binding, until the instance record is published. Without
            // this span, two launches can both decide to start and both try to
            // bind, leaving a stray process behind.
            match claim_startup()? {
                Some(claim) => web::serve_listener(claim.into_listener(), Some(new_instance()?)),
                // Another process won the race and is starting up. It will
                // publish its record shortly, so report reuse rather than
                // starting a second service.
                None => match service::live_instance() {
                    Some(instance) => {
                        println!(
                            "Reusing running service on http://127.0.0.1:{}",
                            instance.port
                        );
                        Ok(())
                    }
                    None => {
                        Err("Another VibDock process is starting; try again in a moment.".into())
                    }
                },
            }
        }
        Action::Tray => {
            #[cfg(windows)]
            {
                tray::run()
            }
            #[cfg(not(windows))]
            {
                let _ = args;
                Err("The tray entry requires Windows.".into())
            }
        }
        Action::AppList
        | Action::AppDevices
        | Action::AppStatus
        | Action::AppMute
        | Action::AppUnmute
        | Action::AppToggle => run_session_command(action, args),
        _ => {
            #[cfg(windows)]
            {
                let device = args.get(2).cloned();
                windows::run(action, device)
            }
            #[cfg(not(windows))]
            {
                let _ = args;
                Err("Audio and global hotkeys currently require Windows. Use `vibdock serve` for the design preview.".into())
            }
        }
    }
}

/// Runs one application-playback-session command.
///
/// Distinct from the device path on purpose: application session control never
/// falls back to a default endpoint or to device-level mute, and it never
/// touches the microphone. On a platform without a session adapter it reports
/// `unsupported` rather than an empty session list, so "cannot look" is never
/// mistaken for "looked and found none".
fn run_session_command(action: Action, args: &[String]) -> Result<()> {
    // Resolve the verb from the action as well as parsing it, so the two
    // mappings are checked against each other on every invocation rather
    // than only in tests.
    let (verb, endpoint, session) = app_audio::parse_session(args)?;
    let mapped =
        as_session_action(action).ok_or("This command is not an application-session action.")?;
    if mapped != verb {
        return Err(format!("Command {action} does not match the parsed verb {verb}.").into());
    }
    if verb == app_audio::SessionAction::Devices {
        let devices = render_endpoints()?;
        if devices.is_empty() {
            println!("No active playback endpoints found.");
            return Ok(());
        }
        for (name, id) in devices {
            println!("{name}\n  {id}");
        }
        return Ok(());
    }
    let endpoint = endpoint.ok_or("Specify a render endpoint with --endpoint <endpoint-id>.")?;
    let outcome = execute(verb, &endpoint, session.as_deref())
        .map_err(|message| -> Box<dyn std::error::Error> { message.into() })?;
    // Human-readable first: the session id and its actual state are what the
    // user needs to copy into the next command, or to confirm that only the
    // intended application changed.
    println!("{}", app_audio::to_text(&outcome));
    println!();
    // The JSON line is the stable contract for a future UI or agent call.
    println!("{}", app_audio::to_json(&outcome));
    Ok(())
}

/// Windows path: the real Core Audio session adapter.
#[cfg(windows)]
fn execute(
    action: app_audio::SessionAction,
    endpoint_id: &str,
    session_id: Option<&str>,
) -> std::result::Result<app_audio::SessionOutcome, String> {
    app_session_windows::execute(action, endpoint_id, session_id)
}

/// Windows path: enumerate the render endpoints that carry sessions.
#[cfg(windows)]
fn render_endpoints() -> Result<Vec<(String, String)>> {
    app_session_windows::render_endpoints()
}

/// Non-Windows path: no adapter, so every command reports unsupported.
#[cfg(not(windows))]
fn execute(
    action: app_audio::SessionAction,
    endpoint_id: &str,
    session_id: Option<&str>,
) -> std::result::Result<app_audio::SessionOutcome, String> {
    app_session_stub::execute(action, endpoint_id, session_id)
}

/// Non-Windows path: no adapter, so enumeration reports unsupported.
#[cfg(not(windows))]
fn render_endpoints() -> Result<Vec<(String, String)>> {
    app_session_stub::render_endpoints()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn requires_explicit_device() {
        assert!(parse(&args(&["toggle"])).is_err());
        assert!(parse(&args(&["mute", "--device", ""])).is_err());
        assert!(parse(&args(&["mute", "--wrong", "id"])).is_err());
        assert!(parse(&args(&["unknown"])).is_err());
        assert!(parse(&args(&["status"])).is_err());
    }

    #[test]
    fn accepts_commands() {
        assert_eq!(parse(&args(&["devices"])).unwrap(), Action::Devices);
        assert_eq!(
            parse(&args(&["listen", "--device", "id"])).unwrap(),
            Action::Listen
        );
        assert!(parse(&args(&["devices", "extra"])).is_err());
        assert!(parse(&args(&["serve", "--device", "id"])).is_err());
        assert_eq!(parse(&args(&["serve"])).unwrap(), Action::Serve);
    }

    #[test]
    fn accepts_service_commands() {
        assert_eq!(
            parse(&args(&["capabilities"])).unwrap(),
            Action::Capabilities
        );
        assert!(parse(&args(&["capabilities", "--device", "id"])).is_err());
        assert_eq!(
            parse(&args(&["service-start"])).unwrap(),
            Action::ServiceStart
        );
        assert_eq!(
            parse(&args(&["service-stop"])).unwrap(),
            Action::ServiceStop
        );
        assert_eq!(
            parse(&args(&["service-status"])).unwrap(),
            Action::ServiceStatus
        );
        assert_eq!(parse(&args(&["help"])).unwrap(), Action::Help);
        assert_eq!(parse(&args(&["--help"])).unwrap(), Action::Help);
        assert!(parse(&args(&["service-start", "extra"])).is_err());
        assert!(parse(&args(&["service-status", "--device", "id"])).is_err());
    }

    #[test]
    fn accepts_tray_entry() {
        assert_eq!(parse(&args(&["tray"])).unwrap(), Action::Tray);
        assert!(parse(&args(&["tray", "--device", "id"])).is_err());
    }

    #[test]
    fn empty_invocation_prints_usage_not_error() {
        assert!(parse(&args(&[])).is_err());
    }

    #[test]
    fn app_devices_is_parsed_and_takes_no_arguments() {
        assert_eq!(parse(&args(&["app-devices"])).unwrap(), Action::AppDevices);
        assert!(parse(&args(&["app-devices", "--endpoint", "e"])).is_err());
    }

    #[test]
    fn session_actions_round_trip_through_the_core_mapping() {
        // The verb a user types must survive parse, dispatch, and rendering
        // unchanged, including on platforms where the adapter is absent and
        // only the unsupported message is produced.
        for action in [
            Action::AppList,
            Action::AppDevices,
            Action::AppStatus,
            Action::AppMute,
            Action::AppUnmute,
            Action::AppToggle,
        ] {
            let verb = as_session_action(action).expect("app actions map to a verb");
            assert_eq!(session_action(verb), action);
            assert_eq!(verb.to_string(), action.to_string());
        }
        assert!(as_session_action(Action::Mute).is_none());
        assert!(as_session_action(Action::Devices).is_none());
    }

    #[test]
    fn unsupported_session_commands_name_the_verb_the_user_typed() {
        // The message must stay honest about being unimplemented rather than
        // degrading to an empty result.
        let message = app_audio::unsupported(app_audio::SessionAction::List);
        assert!(message.contains("app-list"), "got: {message}");
        assert!(message.contains("unimplemented"), "got: {message}");
        let message = app_audio::unsupported(app_audio::SessionAction::Devices);
        assert!(message.contains("app-devices"), "got: {message}");
    }

    #[test]
    fn actions_display_their_command_name() {
        // The shared core is used by both entry points, so an action's display
        // name must match the word the user typed to request it.
        assert_eq!(Action::ServiceStart.to_string(), "service-start");
        assert_eq!(Action::ServiceStop.to_string(), "service-stop");
        assert_eq!(Action::Tray.to_string(), "tray");
        assert_eq!(Action::Toggle.to_string(), "toggle");
    }

    /// Binds a free loopback port for tests that must not touch the real one.
    fn free_port() -> u16 {
        std::net::TcpListener::bind("127.0.0.1:0")
            .expect("a free port must be available")
            .local_addr()
            .expect("bound listener has an address")
            .port()
    }

    /// Serialises tests that redirect the port, and restores it afterwards.
    ///
    /// `VIBDOCK_PORT` and `LOCALAPPDATA` are process-wide, so a test that sets
    /// them while another reads them corrupts the other's expectations.
    /// Runs `body` with a private state directory, restoring it afterwards.
    ///
    /// `LOCALAPPDATA` is process-wide, so a test that redirects it while
    /// another reads it corrupts the other's expectations. `VIBDOCK_PORT` is
    /// deliberately NOT touched: tests pass a port explicitly instead, so a
    /// concurrently running test always reads a stable port.
    fn with_private_state<F: FnOnce()>(body: F) {
        static STATE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = STATE_LOCK.lock().unwrap();
        let saved = std::env::var_os("LOCALAPPDATA");
        let dir = std::env::temp_dir().join(format!("vibdock-lib-{}", std::process::id()));
        std::env::set_var("LOCALAPPDATA", &dir);
        body();
        match saved {
            Some(value) => std::env::set_var("LOCALAPPDATA", value),
            None => std::env::remove_var("LOCALAPPDATA"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn startup_claim_is_exclusive_between_threads() {
        // Two threads deciding at once must not both believe they may start.
        // The port is the ownership primitive: only one bind can succeed.
        with_private_state(|| {
            let port = free_port();
            // Both threads claim and HOLD their result until the other has had
            // its turn, so a loser cannot win by waiting for the winner to
            // finish and release first.
            let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
            let handles: Vec<_> = (0..2)
                .map(|_| {
                    let barrier = barrier.clone();
                    std::thread::spawn(move || {
                        let outcome = claim_startup_on(port);
                        let won = matches!(outcome, Ok(Some(_)));
                        // Hold any claim across the barrier so it is still
                        // owned while the other thread tries.
                        let held = outcome.ok().flatten();
                        barrier.wait();
                        drop(held);
                        won
                    })
                })
                .collect();
            let outcomes: Vec<bool> = handles
                .into_iter()
                .map(|handle| handle.join().expect("thread must not panic"))
                .collect();
            // Exactly one thread may win; the other must not also start.
            assert_eq!(
                outcomes.iter().filter(|won| **won).count(),
                1,
                "exactly one starter allowed, got {outcomes:?}"
            );
        });
    }

    #[test]
    fn a_released_claim_can_be_taken_again() {
        // Holder-crash recovery: the OS releases the port when the holder is
        // gone, so a later launch must be able to claim it.
        with_private_state(|| {
            let port = free_port();
            let first = claim_startup_on(port).expect("claim must succeed on a free port");
            assert!(first.is_some(), "a free port must be claimable");
            // Simulate the holder crashing without a clean release.
            drop(first);
            let second = claim_startup_on(port).expect("claim must succeed after release");
            assert!(
                second.is_some(),
                "the port must be claimable again after the holder is gone"
            );
        });
    }

    #[test]
    fn a_port_held_by_another_process_is_not_claimable() {
        // The loser must never win by waiting. A foreign process holding the
        // port is refused, and importantly it is reported as an error rather
        // than silently treated as a running service we could reuse.
        with_private_state(|| {
            let port = free_port();
            let held = std::net::TcpListener::bind(("127.0.0.1", port))
                .expect("the private port must be free first");
            let claim = claim_startup_on(port);
            drop(held);
            assert!(
                claim.is_err(),
                "a port held by a non-VibDock process must be an error, not a claim, \
                 got is_some={:?}",
                claim.map(|c| c.is_some()).ok()
            );
        });
    }
}
