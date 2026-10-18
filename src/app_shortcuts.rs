//! Saved per-application shortcuts and their live registration.
//!
//! A binding names one observed application location, a VibDock-owned global
//! trigger, and a shortcut to dispatch to that application. The default scope
//! requires the saved executable to own the foreground window; an explicit
//! `running` scope exists for background input methods and sends the chord to
//! the current foreground application while the saved executable runs.
//! Dispatch is deliberately reported as dispatched, never as a verified
//! application state: the application may ignore the chord, be busy, or
//! interpret it differently.
//!
//! The dispatch chord must not equal any VibDock-owned trigger. Windows delivers
//! injected keys to the same global hotkey registrations as physical keys, so
//! such a binding would make VibDock re-trigger itself.

// The dispatch/registration surface is Windows-only. Other platforms keep the
// module for the read-only view and platform-independent tests, so the unused
// Windows-only helpers do not fail their lint build.
#![cfg_attr(not(windows), allow(dead_code))]

use crate::hotkey::Hotkey;
use serde_json::{json, Value};
use std::path::PathBuf;

const MAX_ENTRIES: usize = 8;
const MAX_FILE: usize = 32 * 1024;
/// Hotkey ids 0..999 stay reserved for the single microphone binding and
/// existing tests. Per-application bindings use this base plus their index.
#[cfg(windows)]
pub(crate) const HOTKEY_ID_BASE: i32 = 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scope {
    /// The saved executable must own the current foreground window.
    Foreground,
    /// The saved executable must be running; the chord goes to whatever
    /// application is foreground. For global input methods and tray tools.
    Running,
}
impl Scope {
    fn as_str(self) -> &'static str {
        match self {
            Scope::Foreground => "foreground",
            Scope::Running => "running",
        }
    }
    fn parse(value: &str) -> Option<Self> {
        match value {
            "foreground" => Some(Scope::Foreground),
            "running" => Some(Scope::Running),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct Entry {
    id: String,
    label: String,
    executable: String,
    trigger: String,
    dispatch: String,
    scope: Scope,
}
impl Entry {
    #[cfg(any(windows, test))]
    fn json(&self) -> Value {
        json!({"id":self.id,"label":self.label,"executable_path":self.executable,"trigger":self.trigger,"dispatch":self.dispatch,"scope":self.scope.as_str()})
    }
    fn parse(value: &Value) -> Option<Self> {
        let id = text(value, "id", 64)?;
        if !token(&id) {
            return None;
        }
        let executable = text(value, "executable_path", 1024)?;
        if !crate::resident_action_protocol::local_executable_path(&executable) {
            return None;
        }
        let trigger = text(value, "trigger", 80)?;
        let dispatch = text(value, "dispatch", 80)?;
        let trigger_key = Hotkey::parse(&trigger).ok()?;
        let dispatch_key = Hotkey::parse(&dispatch).ok()?;
        if trigger_key == dispatch_key {
            return None;
        }
        Some(Self {
            id,
            label: text(value, "label", 80)?,
            executable,
            trigger,
            dispatch,
            scope: match value.get("scope") {
                None => Scope::Foreground,
                Some(Value::String(scope)) => Scope::parse(scope)?,
                Some(_) => return None,
            },
        })
    }
}
fn text(value: &Value, key: &str, max: usize) -> Option<String> {
    let s = value.get(key)?.as_str()?;
    (!s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)).then(|| s.to_owned())
}
fn token(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|c| c.is_ascii_hexdigit())
}
/// Compares two user-entered chords by their parsed identity, so spacing and
/// modifier order cannot hide a conflict.
fn same_hotkey(left: &str, right: &str) -> bool {
    match (Hotkey::parse(left), Hotkey::parse(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

/// A stored set is safe only when triggers are unique and no dispatch chord is
/// also a trigger. A hand-edited file must not be able to create a feedback
/// loop through the global hotkey table.
fn safe_entries(entries: &[Entry]) -> bool {
    for (index, entry) in entries.iter().enumerate() {
        for other in &entries[index + 1..] {
            if same_hotkey(&entry.trigger, &other.trigger) {
                return false;
            }
        }
        for other in entries {
            if same_hotkey(&entry.dispatch, &other.trigger) {
                return false;
            }
        }
    }
    true
}
fn bounded_label(value: &str) -> String {
    let mut label = String::new();
    for c in value.chars() {
        if label.len() + c.len_utf8() > 80 {
            break;
        }
        label.push(c);
    }
    label
}
#[cfg(windows)]
pub(crate) fn hotkey_id(index: usize) -> i32 {
    HOTKEY_ID_BASE + i32::try_from(index).unwrap_or(i32::MAX - HOTKEY_ID_BASE)
}

pub(crate) struct Registry {
    path: PathBuf,
    entries: Vec<Entry>,
    revision: u64,
    load_error: Option<String>,
    /// Unit tests never touch the real global hotkey table. Production starts
    /// with registration enabled.
    #[cfg_attr(not(windows), allow(dead_code))]
    register: bool,
    #[cfg(windows)]
    live: Vec<Option<crate::windows::RegisteredHotkey>>,
    #[cfg(windows)]
    errors: Vec<Option<String>>,
}

/// Converts a dispatch attempt into the receipt shown to the user. A detected
/// focus change is never reported as a successful dispatch, even though input
/// may already have been injected.
#[cfg(windows)]
fn dispatch_receipt(
    result: std::result::Result<crate::windows::DispatchDisposition, String>,
) -> std::result::Result<String, String> {
    result.and_then(|disposition| match disposition {
        crate::windows::DispatchDisposition::Dispatched => Ok("DISPATCHED".to_string()),
        crate::windows::DispatchDisposition::FocusChanged => Err(
            "The foreground window changed during dispatch; the shortcut may have been sent to another application. Nothing is verified."
                .to_string(),
        ),
    })
}

impl Registry {
    pub(crate) fn start(enabled: bool) -> Self {
        let path = crate::service::state_dir().map(|dir| dir.join("shortcuts.json"));
        let mut registry = match path {
            Ok(path) => Self::load(path),
            Err(_) => Self::empty(
                PathBuf::new(),
                Some("Shortcut storage is unavailable; existing data was not changed.".into()),
            ),
        };
        // The read-only design preview must not grab a global hotkey: two
        // instances would fight over the same chord and a preview would change
        // the user's desktop behaviour.
        registry.register = enabled && !cfg!(test);
        // Registration is bound to the accepting thread's message loop, so this
        // must run where the registry is used, not on a worker.
        #[cfg(windows)]
        if enabled {
            registry.rebind_all();
        }
        registry
    }

    fn empty(path: PathBuf, error: Option<String>) -> Self {
        Self {
            path,
            entries: vec![],
            revision: 0,
            load_error: error,
            register: !cfg!(test),
            #[cfg(windows)]
            live: vec![],
            #[cfg(windows)]
            errors: vec![],
        }
    }

    #[cfg(test)]
    pub(crate) fn empty_for_test() -> Self {
        Self::empty(PathBuf::new(), None)
    }

    fn load(path: PathBuf) -> Self {
        let mut registry = Self::empty(path, None);
        let result = (|| -> std::io::Result<Vec<u8>> {
            use std::io::Read;
            let file = std::fs::File::open(&registry.path)?;
            let mut bytes = Vec::new();
            file.take((MAX_FILE + 1) as u64).read_to_end(&mut bytes)?;
            Ok(bytes)
        })();
        match result {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(bytes) if bytes.len() <= MAX_FILE => {
                let parsed = serde_json::from_slice::<Value>(&bytes)
                    .ok()
                    .and_then(|value| {
                        if value["schema_version"] != 1 {
                            return None;
                        }
                        let revision = value["revision"].as_u64()?;
                        let raw = value["entries"].as_array()?;
                        if raw.len() > MAX_ENTRIES {
                            return None;
                        }
                        let mut entries = Vec::new();
                        for item in raw {
                            let entry = Entry::parse(item)?;
                            if entries.iter().any(|old: &Entry| old.id == entry.id) {
                                return None;
                            }
                            entries.push(entry);
                        }
                        if !safe_entries(&entries) {
                            return None;
                        }
                        Some((entries, revision))
                    });
                if let Some((entries, revision)) = parsed {
                    registry.entries = entries;
                    registry.revision = revision;
                } else {
                    registry.load_error = Some(
                        "shortcuts.json is invalid or unsupported; original bytes are preserved and shortcut edits are disabled."
                            .into(),
                    );
                }
            }
            _ => {
                registry.load_error = Some(
                    "shortcuts.json cannot be read safely; original data is preserved and shortcut edits are disabled."
                        .into(),
                )
            }
        }
        registry
    }

    #[cfg(windows)]
    fn rebind_all(&mut self) {
        self.live.clear();
        self.errors = vec![None; self.entries.len()];
        if !self.register {
            return;
        }
        for index in 0..self.entries.len() {
            let trigger = self.entries[index].trigger.clone();
            match Hotkey::parse(&trigger)
                .map_err(|error| error.to_string())
                .and_then(|hotkey| {
                    crate::windows::RegisteredHotkey::register(&hotkey, hotkey_id(index))
                        .map_err(|error| error.to_string())
                }) {
                Ok(registration) => self.live.push(Some(registration)),
                Err(error) => {
                    self.errors[index] = Some(error);
                    self.live.push(None);
                }
            }
        }
    }

    /// Builds the replacement registration set. The caller drops the old set
    /// first, so a trigger is never rejected merely because it was already
    /// registered by this same process.
    #[cfg(windows)]
    fn try_rebind(&mut self, entries: &[Entry]) -> Result<(), String> {
        self.live.clear();
        if !self.register {
            return Ok(());
        }
        for (index, entry) in entries.iter().enumerate() {
            let hotkey = Hotkey::parse(&entry.trigger)
                .map_err(|error| format!("{}: {error}", entry.label))?;
            match crate::windows::RegisteredHotkey::register(&hotkey, hotkey_id(index)) {
                Ok(registration) => self.live.push(Some(registration)),
                Err(error) => {
                    self.live.clear();
                    return Err(format!("{}: {error}", entry.label));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn view(&self) -> Value {
        let entries = self
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let (armed, error) = live_state(self, index);
                json!({
                    "id":entry.id,
                    "label":entry.label,
                    "trigger":entry.trigger,
                    "dispatch":entry.dispatch,
                    "scope":entry.scope.as_str(),
                    "armed":armed,
                    "error":error
                })
            })
            .collect::<Vec<_>>();
        json!({
            "schema_version":1,
            "platform":if cfg!(windows){"windows"}else{"unsupported"},
            "revision":self.revision,
            "entries":entries,
            "limit":MAX_ENTRIES,
            "dispatch_available":cfg!(windows),
            "storage_writable":self.load_error.is_none(),
            "error":self.load_error
        })
    }

    /// True when `trigger` would collide with a saved dispatch chord.
    ///
    /// Used before committing a microphone trigger: dispatching that chord
    /// would otherwise be captured by VibDock's own global registration.
    pub(crate) fn blocks_trigger(&self, trigger: &str) -> bool {
        self.entries
            .iter()
            .any(|entry| same_hotkey(&entry.dispatch, trigger))
    }

    /// Handles one fired hotkey id, if it belongs to a saved binding.
    ///
    /// Returns the recorded state and the label captured with the action. A
    /// binding that is not armed (startup registration failed) never runs.
    #[cfg(windows)]
    pub(crate) fn fired(
        &self,
        id: i32,
        running: impl Fn(&str) -> bool,
    ) -> Option<(Result<String, String>, Option<String>)> {
        let index = usize::try_from(id - HOTKEY_ID_BASE).ok()?;
        let entry = self.entries.get(index)?;
        if !self
            .live
            .get(index)
            .is_some_and(std::option::Option::is_some)
        {
            return None;
        }
        let dispatch = Hotkey::parse(&entry.dispatch).ok()?;
        let result = dispatch_receipt(match entry.scope {
            Scope::Foreground => {
                crate::windows::dispatch_to_foreground(&entry.executable, &dispatch)
            }
            Scope::Running => {
                if running(&entry.executable) {
                    crate::windows::dispatch_to_foreground_app(&dispatch)
                } else {
                    Err("The bound application is not running; nothing was dispatched.".into())
                }
            }
        });
        Some((result, Some(entry.label.clone())))
    }

    #[cfg(not(windows))]
    pub(crate) fn fired(
        &self,
        _id: i32,
        _running: impl Fn(&str) -> bool,
    ) -> Option<(Result<String, String>, Option<String>)> {
        None
    }

    pub(crate) fn mutate(
        &mut self,
        operation: &str,
        body: &str,
        mic_trigger: Option<&str>,
        application: Option<(String, String, bool)>,
    ) -> (u16, Value) {
        if cfg!(all(not(windows), not(test))) {
            return (
                501,
                json!({"error":"Application shortcut dispatch is unavailable on this platform"}),
            );
        }
        if self.load_error.is_some() {
            return (
                503,
                json!({"error":"Shortcut storage is unavailable; original data is preserved"}),
            );
        }
        match operation {
            "add" => self.add(body, mic_trigger, application),
            "remove" => self.remove(body),
            _ => (404, json!({"error":"Unknown shortcut operation"})),
        }
    }

    fn add(
        &mut self,
        body: &str,
        mic_trigger: Option<&str>,
        application: Option<(String, String, bool)>,
    ) -> (u16, Value) {
        let fail = |code, error: &str| (code, json!({"error":error}));
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            return fail(400, "Expected a JSON object");
        };
        let Some(object) = value.as_object() else {
            return fail(400, "Expected a JSON object");
        };
        if object.keys().any(|key| {
            !matches!(
                key.as_str(),
                "revision" | "application_reference" | "trigger" | "dispatch" | "label" | "scope"
            )
        }) {
            return fail(
                400,
                "Unexpected shortcut field; executable paths are server-owned",
            );
        }
        if value["revision"].as_u64() != Some(self.revision) {
            return fail(409, "Shortcut entries changed; refresh before editing");
        }
        if self.entries.len() >= MAX_ENTRIES {
            return fail(409, "At most eight application shortcuts can be saved");
        }
        let Some(_reference) = text(&value, "application_reference", 128) else {
            return fail(400, "A fresh application_reference is required");
        };
        let Some(trigger) = text(&value, "trigger", 80) else {
            return fail(400, "A valid trigger shortcut is required");
        };
        let Some(dispatch) = text(&value, "dispatch", 80) else {
            return fail(400, "A valid dispatch shortcut is required");
        };
        let Ok(trigger_key) = Hotkey::parse(&trigger) else {
            return fail(
                400,
                "trigger must be a supported combination of at least one modifier and one key",
            );
        };
        let Ok(dispatch_key) = Hotkey::parse(&dispatch) else {
            return fail(
                400,
                "dispatch must be a supported combination of at least one modifier and one key",
            );
        };
        let trigger_text = trigger_key.as_string();
        let dispatch_text = dispatch_key.as_string();
        if same_hotkey(&trigger_text, &dispatch_text) {
            return fail(400, "The trigger and dispatch shortcut must differ");
        }
        if mic_trigger.is_some_and(|mic| !mic.is_empty() && same_hotkey(mic, &trigger_text)) {
            return fail(
                409,
                "That trigger is already used by the microphone binding",
            );
        }
        if self
            .entries
            .iter()
            .any(|entry| same_hotkey(&entry.trigger, &trigger_text))
        {
            return fail(
                409,
                "That trigger is already assigned to another application",
            );
        }
        if mic_trigger.is_some_and(|mic| !mic.is_empty() && same_hotkey(mic, &dispatch_text))
            || self.entries.iter().any(|entry| {
                same_hotkey(&entry.trigger, &dispatch_text)
                    || same_hotkey(&entry.dispatch, &trigger_text)
            })
        {
            return fail(
                409,
                "A dispatch shortcut must not equal a VibDock-owned trigger",
            );
        }
        let Some((executable, default_label, background)) = application else {
            return fail(
                409,
                "Selected application is no longer available; select again",
            );
        };
        let scope = match value.get("scope") {
            None => Scope::Foreground,
            Some(Value::String(scope)) => match Scope::parse(scope) {
                Some(scope) => scope,
                None => return fail(400, "scope must be foreground or running"),
            },
            Some(_) => return fail(400, "scope must be a string"),
        };
        if background && scope != Scope::Running {
            return fail(
                400,
                "A background application has no foreground window; choose the running scope",
            );
        }
        if !crate::resident_action_protocol::local_executable_path(&executable) {
            return fail(409, "Selected application cannot be resolved");
        }
        if self
            .entries
            .iter()
            .any(|entry| entry.executable.eq_ignore_ascii_case(&executable))
        {
            return fail(409, "That application already has a shortcut binding");
        }
        let label = match value.get("label") {
            None => bounded_label(&default_label),
            Some(_) => match text(&value, "label", 80) {
                Some(label) => label,
                None => return fail(400, "Label must be plain text under 80 bytes"),
            },
        };
        let Ok(id) = crate::service::generate_token() else {
            return fail(503, "Secure shortcut identifiers are unavailable");
        };
        let mut entries = self.entries.clone();
        entries.push(Entry {
            id,
            label,
            executable,
            trigger: trigger_text,
            dispatch: dispatch_text,
            scope,
        });
        // Reject a stale reference after the selection TTL. The caller supplies
        // the application only from a scan it just received.
        self.commit(entries)
    }

    fn remove(&mut self, body: &str) -> (u16, Value) {
        let fail = |code, error: &str| (code, json!({"error":error}));
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            return fail(400, "Expected a JSON object");
        };
        let Some(object) = value.as_object() else {
            return fail(400, "Expected a JSON object");
        };
        if object
            .keys()
            .any(|key| !matches!(key.as_str(), "revision" | "id"))
        {
            return fail(400, "Unexpected shortcut field");
        }
        if value["revision"].as_u64() != Some(self.revision) {
            return fail(409, "Shortcut entries changed; refresh before editing");
        }
        let Some(id) = text(&value, "id", 64).filter(|id| token(id)) else {
            return fail(400, "A saved shortcut id is required");
        };
        let mut entries = self.entries.clone();
        let before = entries.len();
        entries.retain(|entry| entry.id != id);
        if before == entries.len() {
            return fail(404, "Shortcut not found");
        }
        self.commit(entries)
    }

    /// Registers the replacement set before persisting it, and restores the
    /// previous registrations on either failure.
    fn commit(&mut self, entries: Vec<Entry>) -> (u16, Value) {
        #[cfg(windows)]
        {
            let previous_entries = self.entries.clone();
            self.live.clear();
            if let Err(error) = self.try_rebind(&entries) {
                self.live.clear();
                self.rebind_all();
                return (409, json!({"error":error}));
            }
            let Some(revision) = self.revision.checked_add(1) else {
                self.rebind_all();
                return (409, json!({"error":"Revision limit reached"}));
            };
            let stored = json!({"schema_version":1,"revision":revision,"entries":entries.iter().map(Entry::json).collect::<Vec<_>>()});
            if crate::service::atomic_write(&self.path, stored.to_string().as_bytes()).is_err() {
                self.entries = previous_entries;
                self.rebind_all();
                return (
                    500,
                    json!({"error":"Could not save shortcuts; previous state is unchanged"}),
                );
            }
            self.entries = entries;
            self.revision = revision;
            self.errors = vec![None; self.entries.len()];
            (200, self.view())
        }
        #[cfg(all(not(windows), test))]
        {
            let Some(revision) = self.revision.checked_add(1) else {
                return (409, json!({"error":"Revision limit reached"}));
            };
            let stored = json!({"schema_version":1,"revision":revision,"entries":entries.iter().map(Entry::json).collect::<Vec<_>>()});
            if crate::service::atomic_write(&self.path, stored.to_string().as_bytes()).is_err() {
                return (
                    500,
                    json!({"error":"Could not save shortcuts; previous state is unchanged"}),
                );
            }
            self.entries = entries;
            self.revision = revision;
            (200, self.view())
        }
        #[cfg(all(not(windows), not(test)))]
        {
            let _ = entries;
            (
                501,
                json!({"error":"Application shortcut dispatch is unavailable on this platform"}),
            )
        }
    }
}

/// Reports one entry's live registration without exposing the platform handle.
fn live_state(_registry: &Registry, _index: usize) -> (bool, Option<String>) {
    #[cfg(windows)]
    {
        (
            _registry
                .live
                .get(_index)
                .is_some_and(std::option::Option::is_some),
            _registry.errors.get(_index).cloned().flatten(),
        )
    }
    #[cfg(not(windows))]
    {
        (false, None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "vibdock-shortcuts-test-{}",
            crate::service::generate_token().unwrap()
        ));
        std::fs::create_dir(&path).unwrap();
        path.join("shortcuts.json")
    }
    fn registry() -> Registry {
        Registry::load(temp())
    }
    fn application() -> Option<(String, String, bool)> {
        Some((r"C:\Fixture\Codex.exe".into(), "Codex".into(), false))
    }
    fn add_body(revision: u64, trigger: &str, dispatch: &str) -> String {
        json!({"revision":revision,"application_reference":"reference","trigger":trigger,"dispatch":dispatch}).to_string()
    }

    #[test]
    fn saves_lists_and_removes_one_application_binding() {
        let mut registry = registry();
        let (status, view) = registry.add(
            &add_body(0, "Ctrl + Alt + F10", "Ctrl + Shift + Space"),
            None,
            application(),
        );
        assert_eq!(status, 200, "{view}");
        assert_eq!(view["entries"][0]["label"], "Codex");
        assert_eq!(view["entries"][0]["trigger"], "Ctrl + Alt + F10");
        assert_eq!(view["entries"].as_array().unwrap().len(), 1);
        let id = view["entries"][0]["id"].as_str().unwrap().to_string();
        assert!(!view.to_string().contains("Fixture"));
        let (status, view) = registry.remove(&json!({"revision":1,"id":id}).to_string());
        assert_eq!(status, 200);
        assert!(view["entries"].as_array().unwrap().is_empty());
        let reopened = Registry::load(registry.path.clone());
        assert!(reopened.entries.is_empty());
        std::fs::remove_dir_all(registry.path.parent().unwrap()).unwrap();
    }

    #[test]
    fn rejects_stale_and_malformed_edits_without_writing() {
        let mut registry = registry();
        for (body, expected) in [
            (add_body(1, "Ctrl + Alt + F10", "Ctrl + Shift + Space"), 409),
            (add_body(0, "F10", "Ctrl + Shift + Space"), 400),
            (add_body(0, "Ctrl + Alt + F10", "F10"), 400),
            (
                add_body(0, "Ctrl + Alt + F10", "Ctrl + Alt + F10"),
                400,
            ),
            (
                json!({"revision":0,"application_reference":"r","trigger":"Ctrl + Alt + F10","dispatch":"Ctrl + Shift + Space","executable_path":"C:\\Bad.exe"}).to_string(),
                400,
            ),
        ] {
            assert_eq!(registry.add(&body, None, application()).0, expected, "{body}");
        }
        assert!(registry.entries.is_empty());
        assert_eq!(registry.revision, 0);
        assert!(!registry.path.exists());
    }

    #[test]
    fn rejects_triggers_and_dispatches_that_would_loop_or_conflict() {
        let mut registry = registry();
        assert_eq!(
            registry
                .add(
                    &add_body(0, "Ctrl + Alt + F10", "Ctrl + Shift + Space"),
                    Some("Ctrl + Alt + F10"),
                    application()
                )
                .0,
            409
        );
        assert_eq!(
            registry
                .add(
                    &add_body(0, "Ctrl + Alt + F10", "Ctrl + Alt + F9"),
                    Some("Ctrl + Alt + F9"),
                    application()
                )
                .0,
            409
        );
        assert_eq!(
            registry
                .add(
                    &add_body(0, "Ctrl + Alt + F10", "Ctrl + Shift + Space"),
                    None,
                    application()
                )
                .0,
            200
        );
        // A second entry cannot reuse the first trigger, and its dispatch may
        // not equal a VibDock trigger.
        assert_eq!(
            registry
                .add(
                    &add_body(1, "Ctrl + Alt + F10", "Ctrl + Shift + Space"),
                    None,
                    application()
                )
                .0,
            409
        );
        assert_eq!(
            registry
                .add(
                    &add_body(1, "Ctrl + Alt + F11", "Ctrl + Alt + F10"),
                    None,
                    application()
                )
                .0,
            409
        );
        assert_eq!(registry.entries.len(), 1);
        assert!(registry.blocks_trigger("Ctrl + Shift + Space"));
        assert!(!registry.blocks_trigger("Ctrl + Alt + F12"));
    }

    #[test]
    fn malformed_files_preserve_bytes_and_disable_edits() {
        for bytes in [
            "not-json".to_string(),
            json!({"schema_version":2,"revision":0,"entries":[]}).to_string(),
            json!({"schema_version":1,"revision":0,"entries":[{"id":"x","label":"x","executable_path":"C:\\X.exe","trigger":"Ctrl + F1","dispatch":"Ctrl + F2"}]}).to_string(),
        ] {
            let path = temp();
            std::fs::write(&path, &bytes).unwrap();
            let mut registry = Registry::load(path.clone());
            assert!(registry.load_error.is_some());
            assert_eq!(
                registry
                    .mutate("remove", "{}", None, None)
                    .0,
                503
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), bytes);
            std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
        }
    }

    #[test]
    fn persisted_canonical_loops_and_duplicate_triggers_are_rejected() {
        for entries in [
            json!([
                {"id":"a".repeat(64),"label":"One","executable_path":"C:\\Fixture\\One.exe","trigger":"Ctrl+Alt+F10","dispatch":"Ctrl + Alt + F10"}
            ]),
            json!([
                {"id":"a".repeat(64),"label":"One","executable_path":"C:\\Fixture\\One.exe","trigger":"Ctrl + Alt + F10","dispatch":"Ctrl + Shift + Space"},
                {"id":"b".repeat(64),"label":"Two","executable_path":"C:\\Fixture\\Two.exe","trigger":"Alt+Ctrl+F10","dispatch":"Ctrl + Shift + F12"}
            ]),
            json!([
                {"id":"a".repeat(64),"label":"One","executable_path":"C:\\Fixture\\One.exe","trigger":"Ctrl + Alt + F10","dispatch":"Ctrl + Shift + Space"},
                {"id":"b".repeat(64),"label":"Two","executable_path":"C:\\Fixture\\Two.exe","trigger":"Ctrl + Alt + F11","dispatch":"Ctrl + Alt + F10"}
            ]),
        ] {
            let path = temp();
            let bytes = json!({"schema_version":1,"revision":0,"entries":entries}).to_string();
            std::fs::write(&path, &bytes).unwrap();
            let registry = Registry::load(path.clone());
            assert!(registry.load_error.is_some(), "{bytes}");
            assert!(registry.entries.is_empty());
            assert_eq!(std::fs::read_to_string(&path).unwrap(), bytes);
            std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
        }
    }

    #[test]
    fn background_applications_require_the_running_scope() {
        let mut registry = registry();
        let background = Some((r"C:\Fixture\WeType.exe".into(), "微信输入法".into(), true));
        assert_eq!(
            registry
                .add(
                    &add_body(0, "Ctrl + Alt + F10", "Ctrl + Shift + Space"),
                    None,
                    background.clone()
                )
                .0,
            400
        );
        let body = json!({"revision":0,"application_reference":"r","trigger":"Ctrl + Alt + F10","dispatch":"Ctrl + Shift + Space","scope":"running"}).to_string();
        let (status, view) = registry.add(&body, None, background);
        assert_eq!(status, 200, "{view}");
        assert_eq!(view["entries"][0]["scope"], "running");
        assert_eq!(view["entries"][0]["armed"], false);
        let reopened = Registry::load(registry.path.clone());
        assert_eq!(reopened.entries[0].scope, Scope::Running);
        assert!(reopened.entries[0].executable.ends_with("WeType.exe"));
        std::fs::remove_dir_all(registry.path.parent().unwrap()).unwrap();
    }

    #[test]
    fn invalid_scope_values_are_rejected() {
        let mut registry = registry();
        for scope in [json!("global"), json!(1), json!(null), json!(true)] {
            let body = json!({"revision":0,"application_reference":"r","trigger":"Ctrl + Alt + F10","dispatch":"Ctrl + Shift + Space","scope":scope}).to_string();
            assert_eq!(registry.add(&body, None, application()).0, 400, "{scope}");
        }
    }

    #[test]
    fn bounds_are_enforced() {
        let mut registry = registry();
        for index in 0..MAX_ENTRIES {
            let (status, view) = registry.add(
                &add_body(
                    index as u64,
                    &format!("Ctrl + Alt + F{}", index + 10),
                    &format!("Ctrl + Shift + F{}", index + 10),
                ),
                None,
                Some((
                    format!(r"C:\Fixture\App{index}.exe"),
                    format!("App {index}"),
                    false,
                )),
            );
            assert_eq!(status, 200, "{view}");
        }
        assert_eq!(
            registry
                .add(
                    &add_body(MAX_ENTRIES as u64, "Ctrl + Alt + F24", "Ctrl + Shift + F24"),
                    None,
                    application()
                )
                .0,
            409
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_focus_change_during_dispatch_is_not_reported_as_success() {
        assert_eq!(
            dispatch_receipt(Ok(crate::windows::DispatchDisposition::Dispatched)).unwrap(),
            "DISPATCHED"
        );
        let error =
            dispatch_receipt(Ok(crate::windows::DispatchDisposition::FocusChanged)).unwrap_err();
        assert!(error.contains("another application"), "{error}");
    }
}
