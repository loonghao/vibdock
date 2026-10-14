use std::{env, error::Error};
mod capabilities;
mod integrations;
mod web;
#[cfg(windows)]
mod windows;
type Result<T> = std::result::Result<T, Box<dyn Error>>;
#[derive(Debug, PartialEq)]
enum Action {
    Serve,
    Devices,
    Status,
    Mute,
    Unmute,
    Toggle,
    Listen,
    Capabilities,
}

fn parse(args: &[String]) -> Result<(Action, Option<String>)> {
    let action = match args.first().map(String::as_str) {
        Some("serve") => Action::Serve,
        Some("devices") => Action::Devices,
        Some("status") => Action::Status,
        Some("mute") => Action::Mute,
        Some("unmute") => Action::Unmute,
        Some("toggle") => Action::Toggle,
        Some("listen") => Action::Listen,
        Some("capabilities") => Action::Capabilities,
        _ => return Err(
            "Usage: vibdock serve | devices | capabilities | <status|mute|unmute|toggle|listen> --device <endpoint-id>"
                .into(),
        ),
    };
    if matches!(
        action,
        Action::Devices | Action::Serve | Action::Capabilities
    ) {
        if args.len() != 1 {
            return Err("This command does not accept device arguments.".into());
        }
        return Ok((action, None));
    }
    if args.len() != 3 || args[1] != "--device" || (args[2].is_empty() || args[2].contains('\0')) {
        return Err(
            "Specify a microphone endpoint with --device <endpoint-id>; run devices to list IDs."
                .into(),
        );
    }
    Ok((action, Some(args[2].clone())))
}

fn run() -> Result<()> {
    let (action, id) = parse(&env::args().skip(1).collect::<Vec<_>>())?;
    if action == Action::Serve {
        return web::serve();
    }
    if action == Action::Capabilities {
        return capabilities::report_all();
    }
    #[cfg(windows)]
    {
        windows::run(action, id)
    }
    #[cfg(not(windows))]
    {
        let _ = (action, id);
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
    }
    #[test]
    fn accepts_commands() {
        assert_eq!(parse(&args(&["devices"])).unwrap(), (Action::Devices, None));
        assert_eq!(
            parse(&args(&["listen", "--device", "id"])).unwrap(),
            (Action::Listen, Some("id".into()))
        );
        assert!(parse(&args(&["devices", "extra"])).is_err());
        assert!(parse(&args(&["serve", "--device", "id"])).is_err());
        assert_eq!(parse(&args(&["serve"])).unwrap(), (Action::Serve, None));
    }
}
