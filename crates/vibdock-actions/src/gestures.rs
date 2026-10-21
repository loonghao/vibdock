//! Gesture-slot bindings for one physical control.
//!
//! A button may carry up to three saved actions: single, double and long press.
//! The store is separate from the generated-trigger action bindings because a
//! gesture button is provisioned with a sentinel key, not a registered trigger.
//! The host still owns the Windows monitor and the firmware write.

use super::{atomic_write, bounded_text, generate_token, token, CommandRegistry, Step, MAX_STEPS};
use serde_json::{json, Value};
use std::path::PathBuf;
use vibdock_input::hotkey::{Hotkey, Key};

pub const MAX_GESTURE_BINDINGS: usize = 9;
const MAX_FILE: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GestureKind {
    Single,
    Double,
    Long,
}

impl GestureKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Double => "double",
            Self::Long => "long",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "single" => Some(Self::Single),
            "double" => Some(Self::Double),
            "long" => Some(Self::Long),
            _ => None,
        }
    }

    /// Sentinel chord for one button. AU05 supports F1-F12 only, so the
    /// sentinel is Ctrl+Alt+Shift+F10/F11/F12 rather than bare F13-F15.
    pub fn sentinel_text(button: u8) -> Option<String> {
        (button <= 2).then(|| format!("Ctrl + Alt + Shift + F{}", 10 + button))
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    pub id: String,
    pub name: String,
    pub button: u8,
    pub gesture: GestureKind,
    pub steps: Vec<Step>,
    pub hold: Option<HoldBinding>,
}

/// A lifecycle-bound chord, never an executable macro or Pad action.
#[derive(Clone, Debug, PartialEq)]
pub struct HoldBinding {
    pub button: u8,
    pub chord: Hotkey,
    pub long_press_ms: u64,
}

pub fn hold_sentinel(button: u8) -> Option<Hotkey> {
    // F12 is reserved by Windows for debugging.
    (button <= 2).then(|| Hotkey::new([], Key::Function(9 + button)))
}

impl Binding {
    fn view(&self) -> Value {
        let mut value = json!({
            "id": self.id,
            "name": self.name,
            "button": self.button,
            "gesture": self.gesture.as_str(),
            "steps": self.steps.iter().map(Step::view).collect::<Vec<_>>(),
            "armed": true,
        });
        if let Some(hold) = &self.hold {
            value["hold_keys"] = json!(hold.chord.as_string());
            value["long_press_ms"] = json!(hold.long_press_ms);
        }
        value
    }
}

pub struct Store {
    path: PathBuf,
    revision: u64,
    bindings: Vec<Binding>,
    load_error: Option<String>,
}

impl Store {
    pub fn start(state_dir: PathBuf) -> Self {
        if state_dir.as_os_str().is_empty() {
            return Self::empty(
                PathBuf::new(),
                Some("Gesture storage is unavailable; existing data was not changed.".into()),
            );
        }
        Self::load(state_dir.join("gestures.json"))
    }

    fn empty(path: PathBuf, error: Option<String>) -> Self {
        Self {
            path,
            revision: 0,
            bindings: Vec::new(),
            load_error: error,
        }
    }

    fn load(path: PathBuf) -> Self {
        let mut store = Self::empty(path, None);
        let bytes = match std::fs::read(&store.path) {
            Ok(bytes) if bytes.len() <= MAX_FILE => bytes,
            Ok(_) => {
                store.load_error = Some(
                    "gestures.json is too large; original data is preserved and edits are disabled."
                        .into(),
                );
                return store;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return store,
            Err(_) => {
                store.load_error = Some(
                    "gestures.json cannot be read safely; original data is preserved and edits are disabled."
                        .into(),
                );
                return store;
            }
        };
        let Some((revision, bindings)) = parse_file(&bytes) else {
            store.load_error = Some(
                "gestures.json is invalid or unsupported; original bytes are preserved and gesture edits are disabled."
                    .into(),
            );
            return store;
        };
        store.revision = revision;
        store.bindings = bindings;
        store
    }

    pub fn view(&self) -> Value {
        json!({
            "schema_version": 1,
            "revision": self.revision,
            "gestures": self.bindings.iter().map(Binding::view).collect::<Vec<_>>(),
            "limit": MAX_GESTURE_BINDINGS,
            "storage_writable": self.load_error.is_none(),
            "error": self.load_error,
            "timing": {"double_window_ms": 300, "long_press_ms": 550},
        })
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    pub fn has_button(&self, button: u8) -> bool {
        self.bindings.iter().any(|binding| binding.button == button)
    }

    pub fn buttons(&self) -> Vec<u8> {
        let mut buttons: Vec<u8> = self.bindings.iter().map(|binding| binding.button).collect();
        buttons.sort_unstable();
        buttons.dedup();
        buttons
    }

    pub fn hold_bindings(&self) -> Vec<HoldBinding> {
        self.bindings
            .iter()
            .filter_map(|binding| binding.hold.clone())
            .collect()
    }

    pub fn has_hold(&self, button: u8) -> bool {
        self.bindings
            .iter()
            .any(|binding| binding.button == button && binding.hold.is_some())
    }

    pub fn sentinel(&self, button: u8) -> Option<Hotkey> {
        if self.has_hold(button) {
            hold_sentinel(button)
        } else {
            GestureKind::sentinel_text(button).and_then(|text| Hotkey::parse(&text).ok())
        }
    }

    pub fn conflicts_with_trigger(&self, trigger: &Hotkey) -> bool {
        self.buttons()
            .into_iter()
            .any(|button| self.sentinel(button).as_ref() == Some(trigger))
            || self
                .hold_bindings()
                .iter()
                .any(|hold| hold.chord == *trigger)
    }

    pub fn conflicts_with_chord(&self, text: &str) -> bool {
        Hotkey::parse(text)
            .ok()
            .or_else(|| Key::parse(text).map(|key| Hotkey::new([], key)))
            .is_some_and(|chord| self.conflicts_with_trigger(&chord))
    }

    pub fn steps_for(&self, button: u8, gesture: GestureKind) -> Option<Vec<Step>> {
        self.bindings
            .iter()
            .find(|binding| binding.button == button && binding.gesture == gesture)
            .filter(|binding| binding.hold.is_none())
            .map(|binding| binding.steps.clone())
    }

    pub fn name_for(&self, button: u8, gesture: GestureKind) -> Option<String> {
        self.bindings
            .iter()
            .find(|binding| binding.button == button && binding.gesture == gesture)
            .map(|binding| binding.name.clone())
    }

    pub fn add(
        &mut self,
        body: &str,
        commands: &CommandRegistry,
        resolve_application: &mut dyn FnMut(&str) -> Option<(String, String)>,
    ) -> (u16, Value) {
        if self.load_error.is_some() {
            return fail(
                500,
                "Gesture storage is unavailable; original data was not changed.",
            );
        }
        let value: Value = match serde_json::from_str(body) {
            Ok(value) => value,
            Err(_) => return fail(400, "Invalid JSON body"),
        };
        if value["revision"].as_u64() != Some(self.revision) {
            return fail(409, "Revision changed; reload and retry");
        }
        let Some(name) = bounded_text(value.get("name"), 80) else {
            return fail(400, "name must be 1-80 characters");
        };
        let Some(button) = value["button"]
            .as_u64()
            .and_then(|button| u8::try_from(button).ok())
            .filter(|button| *button <= 2)
        else {
            return fail(400, "button must be 0-2");
        };
        let Some(gesture) = value["gesture"].as_str().and_then(GestureKind::parse) else {
            return fail(400, "gesture must be single, double or long");
        };
        if self
            .bindings
            .iter()
            .any(|binding| binding.button == button && binding.gesture == gesture)
        {
            return fail(409, "That gesture slot is already configured");
        }
        if self.bindings.len() >= MAX_GESTURE_BINDINGS {
            return fail(409, "The gesture limit has been reached");
        }
        let hold = match parse_hold(&value, button, gesture) {
            Ok(hold) => hold,
            Err(error) => return fail(400, error),
        };
        if self.bindings.iter().any(|binding| {
            binding.button == button
                && ((hold.is_some() && binding.gesture == GestureKind::Double)
                    || (gesture == GestureKind::Double && binding.hold.is_some()))
        }) {
            return fail(
                409,
                "Tap/hold mode cannot share a button with a double-click slot",
            );
        }
        let Some(steps) = value.get("steps").and_then(Value::as_array) else {
            return fail(400, "steps must be a non-empty array");
        };
        if (hold.is_none() && steps.is_empty()) || steps.len() > MAX_STEPS {
            return fail(400, "steps must contain 1-8 entries");
        }
        let mut parsed_steps = Vec::with_capacity(steps.len());
        for raw in steps {
            match Step::parse_api(raw, resolve_application) {
                Ok(step) => parsed_steps.push(step),
                Err(error) => return fail(400, &error),
            }
        }
        if !parsed_steps
            .iter()
            .all(|step| step.validate_persisted(commands))
        {
            return fail(
                400,
                "A launch target or command is not available in local configuration",
            );
        }
        let steps = parsed_steps;
        let Ok(id) = generate_token() else {
            return fail(503, "Secure action identifiers are unavailable");
        };
        let binding = Binding {
            id,
            name,
            button,
            gesture,
            steps,
            hold,
        };
        let mut updated = self.bindings.clone();
        updated.push(binding);
        if let Err(error) = self.persist(&updated) {
            return fail(500, &error);
        }
        self.bindings = updated;
        self.revision += 1;
        (200, self.view())
    }

    pub fn remove(&mut self, body: &str) -> (u16, Value) {
        if self.load_error.is_some() {
            return fail(
                500,
                "Gesture storage is unavailable; original data was not changed.",
            );
        }
        let value: Value = match serde_json::from_str(body) {
            Ok(value) => value,
            Err(_) => return fail(400, "Invalid JSON body"),
        };
        if value["revision"].as_u64() != Some(self.revision) {
            return fail(409, "Revision changed; reload and retry");
        }
        let Some(id) = value["id"].as_str().filter(|id| token(id)) else {
            return fail(400, "id must be a saved gesture id");
        };
        let Some(index) = self.bindings.iter().position(|binding| binding.id == id) else {
            return fail(404, "No saved gesture with that id");
        };
        let mut updated = self.bindings.clone();
        updated.remove(index);
        if let Err(error) = self.persist(&updated) {
            return fail(500, &error);
        }
        self.bindings = updated;
        self.revision += 1;
        (200, self.view())
    }

    fn persist(&self, bindings: &[Binding]) -> Result<(), String> {
        if self.path.as_os_str().is_empty() {
            return Err("Gesture storage is unavailable".to_string());
        }
        let stored = json!({
            "schema_version": 1,
            "revision": self.revision + 1,
            "gestures": bindings.iter().map(|binding| {
                let mut value = binding.view();
                value.as_object_mut().unwrap().remove("armed");
                value["steps"] = json!(binding.steps.iter().map(Step::stored_json).collect::<Vec<_>>());
                value
            }).collect::<Vec<_>>(),
        });
        atomic_write(&self.path, stored.to_string().as_bytes())
            .map_err(|_| "Could not save gestures; previous state is unchanged".to_string())
    }
}

fn parse_file(bytes: &[u8]) -> Option<(u64, Vec<Binding>)> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    if value["schema_version"] != 1 {
        return None;
    }
    let revision = value["revision"].as_u64()?;
    let entries = value["gestures"].as_array()?;
    if entries.len() > MAX_GESTURE_BINDINGS {
        return None;
    }
    let mut bindings = Vec::with_capacity(entries.len());
    for entry in entries {
        let id = bounded_text(entry.get("id"), 64).filter(|id| token(id))?;
        let name = bounded_text(entry.get("name"), 80)?;
        let button = u8::try_from(entry["button"].as_u64()?)
            .ok()
            .filter(|b| *b <= 2)?;
        let gesture = entry["gesture"].as_str().and_then(GestureKind::parse)?;
        let hold = parse_hold(entry, button, gesture).ok()?;
        let steps = entry["steps"]
            .as_array()?
            .iter()
            .map(Step::parse_stored)
            .collect::<Option<Vec<_>>>()?;
        if (hold.is_none() && steps.is_empty()) || steps.len() > MAX_STEPS {
            return None;
        }
        if bindings
            .iter()
            .any(|binding: &Binding| binding.button == button && binding.gesture == gesture)
        {
            return None;
        }
        bindings.push(Binding {
            id,
            name,
            button,
            gesture,
            steps,
            hold,
        });
    }
    if bindings.iter().any(|binding| {
        binding.hold.is_some()
            && bindings
                .iter()
                .any(|other| other.button == binding.button && other.gesture == GestureKind::Double)
    }) {
        return None;
    }
    Some((revision, bindings))
}

fn parse_hold(
    value: &Value,
    button: u8,
    gesture: GestureKind,
) -> Result<Option<HoldBinding>, &'static str> {
    if value.get("hold_keys").is_none() {
        return if value.get("long_press_ms").is_none() {
            Ok(None)
        } else {
            Err("long_press_ms requires hold_keys")
        };
    }
    let keys = bounded_text(value.get("hold_keys"), 80)
        .ok_or("hold_keys requires a configured voice chord")?;
    let chord = Hotkey::parse(&keys).map_err(|_| "hold_keys must be a valid shortcut")?;
    let threshold = value
        .get("long_press_ms")
        .unwrap_or(&Value::from(550))
        .as_u64()
        .filter(|ms| (150..=5_000).contains(ms))
        .ok_or("long_press_ms must be 150-5000")?;
    if gesture != GestureKind::Long
        || value["steps"]
            .as_array()
            .is_none_or(|steps| !steps.is_empty())
    {
        return Err("hold_keys is only allowed in a long slot with empty steps");
    }
    // A held output must never become another managed trigger or Escape cancel.
    if matches!(chord.key(), Key::Function(9..=12) | Key::Escape) {
        return Err("hold_keys cannot use Escape or a managed function key");
    }
    Ok(Some(HoldBinding {
        button,
        chord,
        long_press_ms: threshold,
    }))
}

fn fail(code: u16, message: &str) -> (u16, Value) {
    (code, json!({"error": message}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn store() -> (Store, PathBuf) {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "vibdock-gestures-test-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir(&dir).unwrap();
        (Store::start(dir.clone()), dir)
    }

    fn add(store: &mut Store, body: &str) -> (u16, Value) {
        let executable = std::env::current_exe()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|_| r"C:\Fixture\App.exe".to_string());
        let mut resolve = move |_reference: &str| Some((executable.clone(), "App".to_string()));
        store.add(body, &CommandRegistry::empty_for_test(), &mut resolve)
    }

    fn body_at(revision: u64, name: &str, button: u8, gesture: &str) -> String {
        json!({
            "revision": revision,
            "name": name,
            "button": button,
            "gesture": gesture,
            "steps": [{"type":"send_keys","keys":"Ctrl + Shift + Space"}],
        })
        .to_string()
    }

    #[test]
    fn saves_and_reloads_a_gesture_slot() {
        let (mut store, dir) = store();
        let (code, view) = add(&mut store, &body_at(0, "Codex voice", 1, "double"));
        assert_eq!(code, 200, "{view}");
        assert_eq!(view["gestures"][0]["gesture"], "double");
        let reloaded = Store::start(dir.clone());
        assert_eq!(reloaded.bindings().len(), 1);
        assert_eq!(
            reloaded.name_for(1, GestureKind::Double).as_deref(),
            Some("Codex voice")
        );
        assert!(reloaded.steps_for(1, GestureKind::Double).is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn launch_steps_are_resolved_server_side() {
        let (mut store, dir) = store();
        let body = json!({
            "revision": 0,
            "name": "Launch",
            "button": 0,
            "gesture": "single",
            "steps": [{"type":"launch","application_reference":"a".repeat(64),"args":[]}],
        })
        .to_string();
        let (code, view) = add(&mut store, &body);
        assert_eq!(code, 200, "{view}");
        assert_eq!(view["gestures"][0]["steps"][0]["label"], "App");
        assert!(view["gestures"][0]["steps"][0].get("executable").is_none());
        assert!(!view.to_string().contains("App.exe"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rejects_duplicate_button_gesture_and_bad_revision() {
        let (mut store, dir) = store();
        assert_eq!(add(&mut store, &body_at(0, "A", 0, "single")).0, 200);
        assert_eq!(add(&mut store, &body_at(1, "B", 0, "single")).0, 409);
        let mut bad: Value = serde_json::from_str(&body_at(2, "B", 0, "double")).unwrap();
        bad["revision"] = json!(9);
        assert_eq!(add(&mut store, &bad.to_string()).0, 409);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn removes_by_id_and_keeps_three_slots() {
        let (mut store, dir) = store();
        let mut id = String::new();
        for gesture in ["single", "double", "long"] {
            let revision = store.revision();
            let (code, view) = add(&mut store, &body_at(revision, "X", 2, gesture));
            assert_eq!(code, 200);
            id = view["gestures"][0]["id"].as_str().unwrap().to_string();
        }
        assert_eq!(store.bindings().len(), 3);
        let removed = store.remove(&json!({"revision":3,"id":id}).to_string());
        assert_eq!(removed.0, 200);
        assert_eq!(store.bindings().len(), 2);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn hold_body(revision: u64) -> Value {
        json!({"revision":revision,"name":"WeChat hold","button":1,"gesture":"long",
            "steps":[],"hold_keys":"Ctrl + Win","long_press_ms":650})
    }

    #[test]
    fn tap_and_hold_reload_exact_endpoint_chord_and_threshold() {
        let (mut store, dir) = store();
        let single = json!({"revision":0,"name":"Microphone","button":1,"gesture":"single",
            "steps":[{"type":"toggle_microphone","device_id":"exact-endpoint-id"}]});
        assert_eq!(add(&mut store, &single.to_string()).0, 200);
        assert_eq!(add(&mut store, &hold_body(1).to_string()).0, 200);
        let mut reloaded = Store::start(dir.clone());
        assert_eq!(
            reloaded.hold_bindings(),
            vec![HoldBinding {
                button: 1,
                chord: Hotkey::parse("Ctrl + Win").unwrap(),
                long_press_ms: 650,
            }]
        );
        assert_eq!(reloaded.sentinel(1).unwrap().as_string(), "F10");
        assert_eq!(
            reloaded.view()["gestures"][0]["steps"][0]["device_id"],
            "exact-endpoint-id"
        );
        assert!(reloaded.steps_for(1, GestureKind::Long).is_none());
        assert!(reloaded.conflicts_with_chord("F10"));
        assert!(reloaded.conflicts_with_chord("Ctrl+Win"));
        let id = reloaded.view()["gestures"][1]["id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(
            reloaded
                .remove(&json!({"revision":2,"id":id}).to_string())
                .0,
            200
        );
        assert_eq!(
            reloaded.sentinel(1).unwrap().as_string(),
            "Ctrl + Alt + Shift + F11"
        );
        assert!(reloaded.steps_for(1, GestureKind::Single).is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn held_chords_reject_ambiguous_modes_and_unsafe_values_without_writing() {
        let (mut store, dir) = store();
        for (field, value) in [
            ("gesture", json!("single")),
            ("gesture", json!("double")),
            ("hold_keys", json!("")),
            ("hold_keys", json!(null)),
            ("hold_keys", json!("Ctrl+Escape")),
            ("hold_keys", json!("Ctrl+F10")),
            ("long_press_ms", json!(149)),
            ("long_press_ms", json!(5001)),
            ("long_press_ms", json!(true)),
            ("long_press_ms", json!(550.5)),
            ("steps", json!([{"type":"wait","ms":100}])),
        ] {
            let mut body = hold_body(0);
            body[field] = value;
            assert_eq!(add(&mut store, &body.to_string()).0, 400, "{body}");
            assert_eq!(store.revision(), 0);
            assert!(!dir.join("gestures.json").exists());
        }
        assert_eq!(add(&mut store, &body_at(0, "Double", 1, "double")).0, 200);
        let before = std::fs::read(dir.join("gestures.json")).unwrap();
        assert_eq!(add(&mut store, &hold_body(1).to_string()).0, 409);
        assert_eq!(before, std::fs::read(dir.join("gestures.json")).unwrap());
        std::fs::remove_dir_all(dir).unwrap();
        let (mut store, dir) = self::store();
        assert_eq!(add(&mut store, &hold_body(0).to_string()).0, 200);
        assert_eq!(add(&mut store, &body_at(1, "Double", 1, "double")).0, 409);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn malformed_held_configuration_preserves_original_bytes() {
        let (_, dir) = store();
        let bytes = json!({"schema_version":1,"revision":2,"gestures":[{
            "id":"a".repeat(64),"name":"Bad","button":0,"gesture":"single",
            "steps":[],"hold_keys":"Ctrl+Win","long_press_ms":550}]})
        .to_string();
        std::fs::write(dir.join("gestures.json"), &bytes).unwrap();
        let reloaded = Store::start(dir.clone());
        assert_eq!(reloaded.view()["storage_writable"], false);
        assert!(reloaded.hold_bindings().is_empty());
        assert_eq!(
            std::fs::read_to_string(dir.join("gestures.json")).unwrap(),
            bytes
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
