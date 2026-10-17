//! Read-only desktop application presence in the disposable native helper.
//!
//! A visible top-level window establishes presence only. It does not establish
//! a playback session, microphone use, a meeting, or any application action.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use windows::{
    core::BOOL,
    Win32::{
        Foundation::{HWND, LPARAM},
        UI::WindowsAndMessaging::{
            EnumWindows, GetWindow, GetWindowTextW, GetWindowThreadProcessId, IsWindow,
            IsWindowVisible, GW_OWNER,
        },
    },
};

use super::{
    base64, local_executable_path, process_name, process_started, Handle, LocalImage, OpenProcess,
};

const MAX_WINDOWS: usize = 64;
const MAX_APPLICATIONS: usize = 24;
const MAX_TEXT: usize = 160;
const MAX_OUTPUT: usize = 64 * 1024;
const PARTIAL: &str = "Application presence observation is partial; absence is unconfirmed.";

#[derive(Default)]
struct WindowBudget {
    visited: usize,
    truncated: bool,
}

impl WindowBudget {
    fn admit(&mut self) -> bool {
        if self.visited == MAX_WINDOWS {
            self.truncated = true;
            false
        } else {
            self.visited += 1;
            true
        }
    }
}

struct Candidate {
    window: HWND,
    process_id: u32,
    title: String,
}

struct Capture {
    own_process_id: u32,
    budget: WindowBudget,
    windows: Vec<Candidate>,
    skipped: usize,
}

struct Observation {
    candidate: Candidate,
    process: Handle,
    started: u64,
    image: LocalImage,
    display_name: String,
    icon: Option<String>,
}

/// Only call inside the owned helper; the parent provides its hard deadline.
pub(super) fn snapshot() -> Value {
    unsafe { observe() }
}

fn unavailable(error: &str) -> Value {
    json!({"available":false,"complete":false,"applications":[],"skipped":0,"error":error})
}

/// Truncate on Unicode boundaries, retaining only plain bounded display text.
fn display_text(value: &str) -> Option<String> {
    let mut result = String::new();
    for character in value.chars().filter(|character| !character.is_control()) {
        if result.len() + character.len_utf8() > MAX_TEXT {
            break;
        }
        result.push(character);
    }
    let result = result.trim();
    (!result.is_empty()).then(|| result.to_owned())
}

fn title_text(value: &[u16]) -> Option<String> {
    // A title longer than the native buffer can end on half a surrogate pair.
    // Removing only that trailing half preserves a valid bounded prefix.
    let end = value.len()
        - usize::from(
            value
                .last()
                .is_some_and(|unit| (0xd800..=0xdbff).contains(unit)),
        );
    display_text(&String::from_utf16(&value[..end]).ok()?)
}

unsafe extern "system" fn enumerate(window: HWND, parameter: LPARAM) -> BOOL {
    // EnumWindows invokes the callback synchronously with this stack-owned
    // capture. No reference or HWND leaves the contained helper.
    let capture = &mut *(parameter.0 as *mut Capture);
    if !capture.budget.admit() {
        return false.into();
    }
    if !IsWindowVisible(window).as_bool() || GetWindow(window, GW_OWNER).is_ok() {
        return true.into();
    }
    let mut process_id = 0;
    if GetWindowThreadProcessId(window, Some(&mut process_id)) == 0 {
        capture.skipped += 1;
        return true.into();
    }
    if process_id <= 4 || process_id == capture.own_process_id {
        return true.into();
    }
    // Foreign captions are read without sending WM_GETTEXT to application
    // controls. Never send application messages or inspect child controls.
    let mut text = [0u16; MAX_TEXT * 2 + 1];
    let length = GetWindowTextW(window, &mut text);
    if length <= 0 {
        return true.into(); // Untitled windows are outside this catalog's scope.
    }
    let Some(title) = title_text(&text[..length as usize]) else {
        capture.skipped += 1;
        return true.into();
    };
    capture.windows.push(Candidate {
        window,
        process_id,
        title,
    });
    true.into()
}

unsafe fn same_window(candidate: &Candidate) -> bool {
    let mut process_id = 0;
    IsWindow(Some(candidate.window)).as_bool()
        && IsWindowVisible(candidate.window).as_bool()
        && GetWindow(candidate.window, GW_OWNER).is_err()
        && GetWindowThreadProcessId(candidate.window, Some(&mut process_id)) != 0
        && process_id == candidate.process_id
}

fn window_set(candidates: &[Candidate]) -> BTreeSet<(usize, u32)> {
    candidates
        .iter()
        .map(|candidate| (candidate.window.0 as usize, candidate.process_id))
        .collect()
}

unsafe fn own_executable() -> Option<String> {
    let process_id = std::process::id();
    let raw = OpenProcess(0x1000 | 0x0010_0000, 0, process_id);
    if raw.is_null() {
        return None;
    }
    let process = Handle(raw);
    let started = process_started(&process, process_id)?;
    let image = LocalImage::from_process(&process)?;
    (process_started(&process, process_id)? == started && image.unchanged()).then_some(image.path)
}

unsafe fn observe() -> Value {
    let Some(own_executable) = own_executable() else {
        return unavailable("The resident executable identity could not be confirmed.");
    };
    let mut capture = Capture {
        own_process_id: std::process::id(),
        budget: WindowBudget::default(),
        windows: Vec::new(),
        skipped: 0,
    };
    let enumerated = EnumWindows(
        Some(enumerate),
        LPARAM((&mut capture as *mut Capture) as isize),
    );
    if enumerated.is_err() && !capture.budget.truncated {
        return unavailable("Desktop application windows could not be enumerated.");
    }
    let mut complete = !capture.budget.truncated && capture.skipped == 0;
    let mut skipped = capture.skipped + usize::from(capture.budget.truncated);
    let mut observations = Vec::new();
    let mut seen = BTreeSet::new();
    let initial_windows = window_set(&capture.windows);
    for candidate in capture.windows {
        // A process with several windows needs only one presence entry. The
        // full identity is captured and checked below before becoming a row.
        if seen.contains(&candidate.process_id) {
            continue;
        }
        if observations.len() == MAX_APPLICATIONS {
            complete = false;
            skipped += 1;
            continue;
        }
        match observe_application(candidate, &own_executable) {
            Ok(Some(observed)) => {
                seen.insert(observed.candidate.process_id);
                observations.push(observed);
            }
            Ok(None) => {} // This executable is VibDock, including its resident.
            Err(()) => {
                complete = false;
                skipped += 1;
            }
        }
    }
    let mut rows = Vec::new();
    let mut identities = BTreeSet::new();
    for observation in observations {
        if !observation.current() {
            complete = false;
            skipped += 1;
            continue;
        }
        let identity = (
            observation.candidate.process_id,
            observation.started,
            observation.image.path.clone(),
        );
        if !identities.insert(identity) {
            continue;
        }
        rows.push((
            json!({
                "process_id":observation.candidate.process_id,
                "process_started":observation.started.to_string(),
                "executable_path":observation.image.path,
                "display_name":observation.display_name,
                "window_title":observation.candidate.title,
                "icon_b64":null
            }),
            observation.icon,
        ));
    }
    if complete {
        // Reuse the first pass's budget rather than granting another 64 native
        // observations. Churn or an exhausted comparison leaves absence unknown.
        let mut fence = Capture {
            own_process_id: capture.own_process_id,
            budget: capture.budget,
            windows: Vec::new(),
            skipped: 0,
        };
        let compared = EnumWindows(
            Some(enumerate),
            LPARAM((&mut fence as *mut Capture) as isize),
        );
        if compared.is_err()
            || fence.budget.truncated
            || fence.skipped != 0
            || window_set(&fence.windows) != initial_windows
        {
            complete = false;
            skipped += fence.skipped + 1;
        }
    }
    finish(rows, complete, skipped)
}

unsafe fn observe_application(
    candidate: Candidate,
    own_executable: &str,
) -> Result<Option<Observation>, ()> {
    if !same_window(&candidate) {
        return Err(());
    }
    // QUERY_LIMITED_INFORMATION | SYNCHRONIZE, without process writes/elevation.
    let raw = OpenProcess(0x1000 | 0x0010_0000, 0, candidate.process_id);
    if raw.is_null() {
        return Err(());
    }
    let process = Handle(raw);
    let started = process_started(&process, candidate.process_id).ok_or(())?;
    let image = LocalImage::from_process(&process).ok_or(())?;
    if image.path.eq_ignore_ascii_case(own_executable) {
        return Ok(None);
    }
    if !local_executable_path(&image.path) {
        return Err(());
    }
    let display_name = display_text(image.path.rsplit('\\').next().ok_or(())?).ok_or(())?;
    let icon = image.icon().map(|bytes| base64(&bytes));
    let observation = Observation {
        candidate,
        process,
        started,
        image,
        display_name,
        icon,
    };
    observation.current().then_some(Some(observation)).ok_or(())
}

impl Observation {
    unsafe fn current(&self) -> bool {
        same_window(&self.candidate)
            && process_started(&self.process, self.candidate.process_id) == Some(self.started)
            && process_name(&self.process, 0).as_deref() == Some(self.image.path.as_str())
            && process_name(&self.process, 1).as_deref() == Some(self.image.native.as_str())
            && self.image.unchanged()
    }
}

fn response(rows: &[Value], complete: bool, skipped: usize) -> Value {
    json!({"available":true,"complete":complete,"applications":rows,"skipped":skipped,
        "error":if complete { None } else { Some(PARTIAL) }})
}

fn fits(value: &Value) -> bool {
    serde_json::to_vec(value).is_ok_and(|bytes| bytes.len() <= MAX_OUTPUT)
}

/// Preserve exact row identities before considering optional image metadata.
fn finish(mut rows: Vec<(Value, Option<String>)>, mut complete: bool, mut skipped: usize) -> Value {
    let mut values: Vec<_> = rows.iter().map(|(row, _)| row.clone()).collect();
    while !fits(&response(&values, complete, skipped)) {
        if values.pop().is_none() {
            return unavailable("Application presence exceeded its response budget.");
        }
        rows.pop();
        complete = false;
        skipped += 1;
    }
    for (index, (_, icon)) in rows.into_iter().enumerate() {
        if let Some(icon) = icon {
            values[index]["icon_b64"] = json!(icon);
            if !fits(&response(&values, complete, skipped)) {
                values[index]["icon_b64"] = Value::Null;
            }
        }
    }
    response(&values, complete, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_window_budget_stops_before_observing_more_than_64_windows() {
        let mut budget = WindowBudget::default();
        for _ in 0..MAX_WINDOWS {
            assert!(budget.admit());
        }
        assert!(!budget.truncated);
        assert!(!budget.admit());
        assert_eq!(budget.visited, MAX_WINDOWS);
        assert!(budget.truncated);
        assert!(!budget.admit());
        assert_eq!(budget.visited, MAX_WINDOWS);
    }

    #[test]
    fn catalog_text_is_plain_bounded_and_keeps_unicode_boundaries() {
        assert_eq!(display_text("\t Example\n app\0 ").unwrap(), "Example app");
        assert!(display_text("\n\0\t ").is_none());
        assert_eq!(display_text(&"中".repeat(100)).unwrap().len(), 159);
        assert_eq!(display_text(&"a".repeat(200)).unwrap().len(), MAX_TEXT);
        assert_eq!(title_text(&[0x0041, 0xd83d]).unwrap(), "A");
        assert!(title_text(&[0xdc00]).is_none());
        assert!(title_text(&[0xd83d, 0x0041]).is_none());
        assert_eq!(title_text(&[0xd83d, 0xde00]).unwrap(), "😀");
    }

    #[test]
    fn catalog_window_comparison_detects_addition_removal_and_pid_reuse() {
        let candidates = |pairs: &[(usize, u32)]| {
            pairs
                .iter()
                .map(|(window, process_id)| Candidate {
                    window: HWND(*window as *mut _),
                    process_id: *process_id,
                    title: "Example".to_owned(),
                })
                .collect::<Vec<_>>()
        };
        let original = window_set(&candidates(&[(1, 5), (2, 6)]));
        assert_eq!(original, window_set(&candidates(&[(2, 6), (1, 5)])));
        for changed in [&[(1, 5)][..], &[(1, 5), (2, 7)], &[(1, 5), (2, 6), (3, 7)]] {
            assert_ne!(original, window_set(&candidates(changed)));
        }
    }

    fn row(index: usize, path: String) -> Value {
        json!({"process_id":index+5,"process_started":(index+1).to_string(),
            "executable_path":path,"display_name":"app.exe","window_title":"Example",
            "icon_b64":null})
    }

    #[test]
    fn catalog_budget_preserves_identities_before_optional_icons() {
        let rows: Vec<_> = (0..MAX_APPLICATIONS)
            .map(|index| {
                (
                    row(index, "C:\\app.exe".to_owned()),
                    Some("i".repeat(12000)),
                )
            })
            .collect();
        let result = finish(rows, true, 0);
        assert!(fits(&result));
        assert_eq!(result["complete"], true);
        let applications = result["applications"].as_array().unwrap();
        assert_eq!(applications.len(), MAX_APPLICATIONS);
        assert_eq!(applications[23]["process_id"], 28);
        assert!(applications.iter().any(|row| row["icon_b64"].is_null()));
        assert!(applications.iter().any(|row| row["icon_b64"].is_string()));
    }

    #[test]
    fn catalog_row_overflow_is_partial_and_failure_is_never_empty_success() {
        let rows = (0..MAX_APPLICATIONS)
            .map(|index| (row(index, "p".repeat(4096)), None))
            .collect();
        let result = finish(rows, true, 0);
        assert!(fits(&result));
        assert_eq!(result["complete"], false);
        assert!(result["applications"].as_array().unwrap().len() < MAX_APPLICATIONS);
        assert!(result["skipped"].as_u64().unwrap() > 0);
        assert_eq!(result["error"], PARTIAL);
        let failed = unavailable("test");
        assert_eq!(failed["available"], false);
        assert_eq!(failed["complete"], false);
        assert!(failed["error"].is_string());
    }
}
