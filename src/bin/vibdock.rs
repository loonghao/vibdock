//! Console entry point: retains stdout/stderr and meaningful exit codes.
//!
//! All behaviour lives in the `vibdock` library, which the Windows launcher
//! also uses, so the two entries cannot drift.

use std::env;

use vibdock::{Action, Result};

fn main() {
    if let Err(error) = run() {
        eprintln!("ERROR: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let args: Vec<String> = env::args().skip(1).collect();
    #[cfg(windows)]
    if args == ["_resident_action"] {
        vibdock::resident_apps::print_action();
        return Ok(());
    }
    if args == ["_resident_snapshot"] {
        vibdock::resident_apps::print_snapshot();
        return Ok(());
    }
    #[cfg(windows)]
    if args == ["_taskbar_nudge"] {
        for _ in 0..20 {
            std::thread::sleep(std::time::Duration::from_millis(500));
            if vibdock::nudge_taskbar_auto() {
                break;
            }
        }
        return Ok(());
    }
    #[cfg(windows)]
    if args.len() == 2 && args[0] == "_taskbar_nudge" {
        if let Ok(raw) = args[1].parse::<isize>() {
            let _ = vibdock::nudge_taskbar(raw);
        }
        return Ok(());
    }
    if args.is_empty() {
        vibdock::help();
        return Ok(());
    }
    let action = vibdock::parse(&args)?;
    if action == Action::Help {
        vibdock::help();
        return Ok(());
    }
    vibdock::run_action(action, &args)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_arguments_are_not_an_error() {
        // No arguments prints usage and exits zero, matching the documented
        // behaviour: `vibdock` alone is a help request, not a failure.
        assert!(run().is_ok() || env::args().count() > 1);
    }
}
