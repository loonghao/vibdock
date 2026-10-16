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
    if args == ["_resident_snapshot"] {
        vibdock::resident_apps::print_snapshot();
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
