use std::{env, error::Error};

mod service;
mod web;
#[cfg(windows)]
mod windows;
type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Debug, PartialEq)]
enum Action {
    Serve,
    ServiceStart,
    ServiceStop,
    ServiceStatus,
    Help,
    Devices,
    Status,
    Mute,
    Unmute,
    Toggle,
    Listen,
}

const USAGE: &str = "Usage: vibdock <serve|service-start|service-stop|service-status|help>
       vibdock devices
       vibdock <status|mute|unmute|toggle|listen> --device <endpoint-id>";

fn parse(args: &[String]) -> Result<Action> {
    let action = match args.first().map(String::as_str) {
        Some("serve") => Action::Serve,
        Some("service-start") => Action::ServiceStart,
        Some("service-stop") => Action::ServiceStop,
        Some("service-status") => Action::ServiceStatus,
        Some("help") | Some("--help") | Some("-h") => Action::Help,
        Some("devices") => Action::Devices,
        Some("status") => Action::Status,
        Some("mute") => Action::Mute,
        Some("unmute") => Action::Unmute,
        Some("toggle") => Action::Toggle,
        Some("listen") => Action::Listen,
        _ => return Err(format!("{USAGE}\nRun `vibdock help` for details.").into()),
    };
    let rest = &args[1..];
    if matches!(
        action,
        Action::Serve
            | Action::Devices
            | Action::ServiceStart
            | Action::ServiceStop
            | Action::ServiceStatus
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
fn help() {
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
    println!("  service-stop     Ask the resident service to shut down");
    println!("  devices          List active microphone endpoints and their IDs");
    println!("  status|mute|unmute|toggle   Read or change one microphone's mute state");
    println!("  listen           Print state and toggle it on Ctrl+Alt+Shift+F9");
    println!("  help             Show this message");
    println!();
    println!("Exit codes are zero on success and nonzero on error. `service-start`");
    println!("reuses a running instance instead of binding a second listener.");
}

fn run() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() {
        help();
        return Ok(());
    }
    let action = parse(&args)?;
    match action {
        Action::Help => {
            help();
            return Ok(());
        }
        Action::ServiceStatus => {
            match service::live_instance() {
                Some(instance) => println!(
                    "Service running on http://127.0.0.1:{} (token available in the service state file)",
                    instance.port
                ),
                None => println!("Service not running."),
            }
            return Ok(());
        }
        Action::ServiceStop => {
            // Real, authenticated shutdown: only the instance holding the
            // recorded token is told to stop, and we wait for it to release
            // the port so a relaunch can bind it again.
            match service::request_shutdown() {
                Ok(true) => {
                    println!("Service stopped.");
                    return Ok(());
                }
                Ok(false) => {
                    eprintln!("Service not running.");
                    return Err("No running service to stop.".into());
                }
                Err(message) => return Err(message.into()),
            }
        }
        _ => {}
    }
    if action == Action::Serve {
        return web::serve(None);
    }
    if action == Action::ServiceStart {
        if let Some(instance) = service::live_instance() {
            println!(
                "Reusing running service on http://127.0.0.1:{}",
                instance.port
            );
            return Ok(());
        }
        // Nothing proved itself the owner, but a stale marker may still be
        // occupying the port in the sense that it misdescribes reality. Recover
        // it only after proving no valid owner answers, and only when the
        // marker is the one this task would have written.
        if let Err(message) = service::recover_stale_marker() {
            // Returned, not also printed: main already reports errors once.
            return Err(message.into());
        }
        let instance = service::Instance {
            port: service::port(),
            token: service::generate_token()?,
        };
        return web::serve(Some(instance));
    }
    #[cfg(windows)]
    {
        let device = args.get(2).cloned();
        windows::run(action, device)
    }
    #[cfg(not(windows))]
    {
        let _ = action;
        Err("Audio and global hotkeys currently require Windows. Use `vibdock serve` for the design preview.".into())
    }
}

fn main() {
    if let Err(error) = run() {
        eprintln!("ERROR: {error}");
        std::process::exit(1);
    }
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
    fn empty_invocation_prints_usage_not_error() {
        assert!(parse(&args(&[])).is_err());
    }
}
