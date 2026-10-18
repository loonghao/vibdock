//! CLI projection of the authenticated per-application shortcut API.
//!
//! The browser UI uses opaque application references; this CLI does the same.
//! It never accepts an executable path, never dispatches a shortcut, and never
//! writes `shortcuts.json` directly.

use crate::{service::ResidentControlClient, Action, Result};
use serde_json::{json, Value};

fn reference(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn value(args: &[String], index: usize, flag: &str) -> std::result::Result<String, String> {
    args.get(index + 1)
        .filter(|value| !value.is_empty() && !value.contains('\0'))
        .cloned()
        .ok_or_else(|| format!("{flag} requires a value."))
}

fn client() -> Result<ResidentControlClient> {
    ResidentControlClient::discover().map_err(|_| {
        "No verified VibDock service is running. Start it with `vibdock.exe service-start`.".into()
    })
}

fn print_view(view: &Value) {
    let entries = view["entries"].as_array().cloned().unwrap_or_default();
    println!("Application shortcuts (revision {}):", view["revision"]);
    if entries.is_empty() {
        println!("  none");
        return;
    }
    for entry in entries {
        let state = if entry["armed"] == true {
            "registered".to_string()
        } else {
            entry["error"]
                .as_str()
                .map(|error| format!("not registered: {error}"))
                .unwrap_or_else(|| "not registered".to_string())
        };
        println!(
            "  {}  [{}]  {} -> {}  {}",
            entry["label"].as_str().unwrap_or("?"),
            entry["scope"].as_str().unwrap_or("?"),
            entry["trigger"].as_str().unwrap_or("?"),
            entry["dispatch"].as_str().unwrap_or("?"),
            state
        );
    }
}

fn list(args: &[String]) -> Result<()> {
    let json_output = args.len() == 1 && args[0] == "--json";
    if !args.is_empty() && !json_output {
        return Err("Usage: vibdock shortcut-list [--json]".into());
    }
    let view = client()?.shortcuts()?;
    if json_output {
        println!("{view}");
    } else {
        print_view(&view);
    }
    Ok(())
}

fn applications(args: &[String]) -> Result<()> {
    let json_output = args.len() == 1 && args[0] == "--json";
    if !args.is_empty() && !json_output {
        return Err("Usage: vibdock shortcut-apps [--json]".into());
    }
    let view = client()?.applications()?;
    if json_output {
        println!("{view}");
        return Ok(());
    }
    let rows = view["applications"].as_array().cloned().unwrap_or_default();
    println!("Detected applications:");
    if rows.is_empty() {
        println!("  none; open the target application and retry");
        return Ok(());
    }
    for row in rows {
        println!(
            "  {}  [{}]  {}{}  reference={}",
            row["name"].as_str().unwrap_or("?"),
            row["integration"].as_str().unwrap_or("unknown"),
            if row["background"] == true {
                "background"
            } else {
                "window"
            },
            if view["applications_complete"] == true {
                ""
            } else {
                " (partial scan)"
            },
            row["selection_id"].as_str().unwrap_or("?")
        );
    }
    Ok(())
}

struct AddOptions {
    application: Option<String>,
    name: Option<String>,
    trigger: String,
    dispatch: String,
    scope: Option<String>,
    label: Option<String>,
}

fn parse_add(args: &[String]) -> std::result::Result<AddOptions, String> {
    let mut application = None;
    let mut name = None;
    let mut trigger = None;
    let mut dispatch = None;
    let mut scope = None;
    let mut label = None;
    let mut index = 0;
    while index < args.len() {
        match args[index].as_str() {
            "--application" => {
                let value = value(args, index, "--application")?;
                if !reference(&value) {
                    return Err(
                        "--application must be a 64-character reference from shortcut-apps.".into(),
                    );
                }
                application = Some(value);
                index += 2;
            }
            "--app" => {
                name = Some(value(args, index, "--app")?);
                index += 2;
            }
            "--trigger" => {
                trigger = Some(value(args, index, "--trigger")?);
                index += 2;
            }
            "--dispatch" => {
                dispatch = Some(value(args, index, "--dispatch")?);
                index += 2;
            }
            "--scope" => {
                let value = value(args, index, "--scope")?;
                if value != "foreground" && value != "running" {
                    return Err("--scope must be foreground or running.".into());
                }
                scope = Some(value);
                index += 2;
            }
            "--label" => {
                label = Some(value(args, index, "--label")?);
                index += 2;
            }
            other => return Err(format!("Unknown shortcut-add argument: {other}")),
        }
    }
    let (trigger, dispatch) = match (trigger, dispatch) {
        (Some(trigger), Some(dispatch)) => (trigger, dispatch),
        _ => return Err("--trigger and --dispatch are required.".into()),
    };
    if application.is_some() == name.is_some() {
        return Err("Specify exactly one of --application or --app.".into());
    }
    Ok(AddOptions {
        application,
        name,
        trigger,
        dispatch,
        scope,
        label,
    })
}

fn add(args: &[String]) -> Result<()> {
    let options = parse_add(args)?;
    let control = client()?;
    let apps = control.applications()?;
    let application = match (options.application, options.name) {
        (Some(reference), None) => reference,
        (None, Some(name)) => {
            let matches = apps["applications"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|row| {
                    row["name"]
                        .as_str()
                        .is_some_and(|candidate| candidate.eq_ignore_ascii_case(&name))
                })
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [row] => row["selection_id"].as_str().unwrap_or_default().to_string(),
                [] => {
                    return Err(format!(
                        "No detected application is named {name}; run shortcut-apps."
                    )
                    .into())
                }
                _ => {
                    return Err(format!(
                        "More than one application is named {name}; use --application."
                    )
                    .into())
                }
            }
        }
        _ => return Err("Specify exactly one of --application or --app.".into()),
    };
    let row = apps["applications"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row["selection_id"].as_str() == Some(application.as_str()))
        .ok_or("That application reference is no longer current; run shortcut-apps and retry.")?;
    let background = row["background"] == true;
    let scope = options.scope.unwrap_or_else(|| {
        if background {
            "running".to_string()
        } else {
            "foreground".to_string()
        }
    });
    if background && scope != "running" {
        return Err("A background application requires --scope running.".into());
    }
    let view = control.shortcuts()?;
    let mut body = json!({
        "revision": view["revision"],
        "application_reference": application,
        "trigger": options.trigger,
        "dispatch": options.dispatch,
        "scope": scope,
    });
    if let Some(label) = options.label {
        body["label"] = json!(label);
    }
    let saved = control.mutate_shortcut("add", body)?;
    let entry = saved["entries"]
        .as_array()
        .and_then(|entries| entries.last())
        .cloned()
        .unwrap_or(Value::Null);
    println!(
        "Saved {}  [{}]  {} -> {}",
        entry["label"].as_str().unwrap_or("?"),
        entry["scope"].as_str().unwrap_or("?"),
        entry["trigger"].as_str().unwrap_or("?"),
        entry["dispatch"].as_str().unwrap_or("?"),
    );
    if entry["armed"] != true {
        println!(
            "Registration is not live: {}",
            entry["error"].as_str().unwrap_or("unknown error")
        );
    }
    Ok(())
}

fn remove(args: &[String]) -> Result<()> {
    if args.len() != 2 || args[0] != "--id" || !reference(&args[1]) {
        return Err("Usage: vibdock shortcut-remove --id <saved-id>".into());
    }
    let control = client()?;
    let view = control.shortcuts()?;
    let body = json!({"revision": view["revision"], "id": args[1]});
    control.mutate_shortcut("remove", body)?;
    println!("Removed {}", args[1]);
    Ok(())
}

pub(crate) fn run(action: Action, args: &[String]) -> Result<()> {
    let rest = &args[1..];
    match action {
        Action::ShortcutList => list(rest),
        Action::ShortcutApps => applications(rest),
        Action::ShortcutAdd => add(rest),
        Action::ShortcutRemove => remove(rest),
        _ => Err("Not a shortcut CLI command.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }

    #[test]
    fn reference_is_exactly_one_hundred_twenty_eight_hex_characters() {
        assert!(reference(&"a".repeat(64)));
        assert!(!reference(&"a".repeat(63)));
        assert!(!reference(&"g".repeat(64)));
    }

    #[test]
    fn add_requires_one_application_selector_and_both_chords() {
        let reference = "a".repeat(64);
        let ok = parse_add(&args(&[
            "--application",
            &reference,
            "--trigger",
            "Ctrl+Alt+F21",
            "--dispatch",
            "Ctrl+Shift+F21",
        ]));
        assert!(ok.is_ok());
        assert!(parse_add(&args(&["--app", "Notepad"])).is_err());
        assert!(parse_add(&args(&[
            "--application",
            &reference,
            "--app",
            "Notepad",
            "--trigger",
            "Ctrl+Alt+F21",
            "--dispatch",
            "Ctrl+Shift+F21",
        ]))
        .is_err());
        assert!(parse_add(&args(&[
            "--app",
            "Notepad",
            "--trigger",
            "Ctrl+Alt+F21",
            "--dispatch",
            "Ctrl+Shift+F21",
            "--scope",
            "elsewhere",
        ]))
        .is_err());
    }
}
