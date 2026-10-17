//! Real helper-process coverage for the guarded application snapshot.
//!
//! The helper is not invoked through the service: this exercises the stdin
//! activation gate, the native scan boundary and the JSON shape a CI runner
//! with or without an interactive desktop must still produce.
#![cfg(windows)]

use std::{
    io::Write,
    process::{Command, Stdio},
};

#[test]
fn guarded_snapshot_helper_requires_the_activation_byte_and_returns_bounded_json() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_vibdock"))
        .arg("_resident_snapshot")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    {
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(&[0x56]).unwrap();
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{:?}", output);
    let text = String::from_utf8(output.stdout).unwrap();
    let value: serde_json::Value = serde_json::from_str(text.trim()).unwrap();
    assert!(
        value["available"].is_boolean(),
        "available must be explicit"
    );
    assert!(
        value["applications"].is_array(),
        "applications must always be an array"
    );
    assert!(
        value["applications_complete"].is_boolean(),
        "application completeness must be explicit"
    );
    assert!(
        value["sessions"].is_array(),
        "playback sessions must always be an array"
    );
    let applications = value["applications"].as_array().unwrap();
    assert!(applications.len() <= 32, "application budget was exceeded");
    for application in applications {
        assert!(application["process_id"].as_u64().is_some());
        assert!(application["process_started"]
            .as_str()
            .is_some_and(|started| !started.is_empty()));
        assert!(application["executable_path"]
            .as_str()
            .is_some_and(|path| path.to_ascii_lowercase().ends_with(".exe")));
        assert!(application["display_name"].as_str().is_some());
        assert!(application["background"].is_boolean());
        assert!(
            application["icon_b64"].is_null() || application["icon_b64"].is_string(),
            "icon must be bounded JSON"
        );
    }
    let sessions = value["sessions"].as_array().unwrap();
    assert!(sessions.len() <= 24, "session budget was exceeded");
}

#[test]
fn snapshot_helper_without_activation_returns_an_unavailable_record() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_vibdock"))
        .arg("_resident_snapshot")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{:?}", output);
    let value: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("helper output must stay JSON");
    assert_eq!(value["available"], false);
    assert_eq!(value["applications_complete"], false);
    assert!(value["applications"].as_array().unwrap().is_empty());
    assert!(value["sessions"].as_array().unwrap().is_empty());
}
