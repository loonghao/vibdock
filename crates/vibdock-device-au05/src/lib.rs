//! Windows adapter for direct Ulanzi AU05 Vibe Key shortcut provisioning.
//!
//! The AU05 exposes three physical buttons whose firmware emits a keyboard
//! chord. VibDock can write that chord into the device over its vendor HID
//! interface (`VID 0xFFF1`, `PID 0x00DD`, usage page `0xFFFC`), so a button can
//! trigger a saved application binding without Ulanzi Studio being installed or
//! running. Writes are verified by reading the slot back: "written" is only
//! reported when the device returns exactly the requested chord.
//!
//! This adapter deliberately does not report a dispatched application action.
//! It only controls what the firmware will emit; whether the bound application
//! accepts the dispatched chord is a separate, unverifiable fact.
//!
//! Other platforms keep the same value types and a stub that reports hardware
//! control as unavailable, so the service, CLI and page share one contract.

use serde_json::{json, Value};
use std::path::PathBuf;
use vibdock_au05::{self as au05, Button};
use vibdock_input::hotkey::Hotkey;

/// Atomic replacement for the small hardware store. The host passes the state
/// directory in, so this crate does not depend on the service crate.
fn atomic_write(path: &PathBuf, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path
        .parent()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no parent"))?;
    std::fs::create_dir_all(dir)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let temp = dir.join(format!(
        ".{}.{}.{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("hardware"),
        std::process::id(),
        stamp
    ));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    let written = file.write_all(contents).and_then(|_| file.sync_all());
    drop(file);
    let result = written.and_then(|_| std::fs::rename(&temp, path));
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// One saved binding that should be carried by an AU05 button.
#[derive(Clone, Debug)]
pub struct HardwareBinding {
    /// Saved shortcut id, used only to join adapter state back into the page.
    pub binding_id: String,
    /// Display label of the bound application.
    pub binding_label: String,
    /// Zero-based device button index (`0..=2`).
    pub button: u8,
    /// Canonical chord text shown to the user.
    pub trigger_text: String,
    /// Parsed chord written into the firmware.
    pub trigger: Hotkey,
}

/// A chord written straight into the device firmware, with no host hotkey.
///
/// This is the escape hatch for chords VibDock cannot register itself, such as
/// `Ctrl + Win` for an input method's voice shortcut. The button emits a real
/// hardware key chord; the service neither registers nor dispatches anything.
#[derive(Clone, Debug)]
pub struct DirectEntry {
    pub button: u8,
    pub chord: String,
    pub label: Option<String>,
    pub entries: Vec<vibdock_au05::KeyEntry>,
}

#[cfg_attr(test, allow(dead_code))]
const DIRECT_FILE_LIMIT: usize = 16 * 1024;
const DIRECT_MAX_ENTRIES: usize = 3;

struct DirectStore {
    path: PathBuf,
    revision: u64,
    entries: Vec<DirectEntry>,
    load_error: Option<String>,
    /// True when no hardware.json existed and the built-in button defaults are
    /// waiting for their first write.
    seed_defaults: bool,
}

impl DirectStore {
    /// The out-of-the-box AU05 layout:
    /// button 1 = WeChat Input voice (`Ctrl + Win`), 2 = Enter, 3 = Esc.
    fn defaults() -> Vec<DirectEntry> {
        [
            (0u8, "Ctrl + Win", Some("微信语音")),
            (1, "Enter", Some("回车")),
            (2, "Esc", Some("取消")),
        ]
        .into_iter()
        .filter_map(|(button, chord, label)| {
            Some(DirectEntry {
                button,
                chord: au05::direct_canonical(chord).ok()?,
                label: label.map(str::to_string),
                entries: au05::direct_entries(chord).ok()?,
            })
        })
        .collect()
    }

    #[cfg(test)]
    fn empty_for_test() -> Self {
        Self {
            path: PathBuf::new(),
            revision: 0,
            entries: Vec::new(),
            load_error: None,
            seed_defaults: false,
        }
    }

    #[cfg_attr(test, allow(dead_code))]
    fn load(state_dir: PathBuf) -> Self {
        let path = if state_dir.as_os_str().is_empty() {
            PathBuf::new()
        } else {
            state_dir.join("hardware.json")
        };
        let mut store = Self {
            path,
            revision: 0,
            entries: Vec::new(),
            load_error: None,
            seed_defaults: false,
        };
        if store.path.as_os_str().is_empty() {
            store.load_error = Some(
                "Hardware shortcut storage is unavailable; existing data was not changed."
                    .to_string(),
            );
            return store;
        }
        let result = (|| -> std::io::Result<Vec<u8>> {
            use std::io::Read;
            let file = std::fs::File::open(&store.path)?;
            let mut bytes = Vec::new();
            file.take((DIRECT_FILE_LIMIT + 1) as u64)
                .read_to_end(&mut bytes)?;
            Ok(bytes)
        })();
        match result {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                store.entries = Self::defaults();
                store.seed_defaults = true;
            }
            Ok(bytes) if bytes.len() <= DIRECT_FILE_LIMIT => match Self::parse(&bytes) {
                Some((entries, revision)) => {
                    store.entries = entries;
                    store.revision = revision;
                }
                None => {
                    store.load_error = Some(
                            "hardware.json is invalid or unsupported; original bytes are preserved and hardware edits are disabled."
                                .into(),
                        );
                }
            },
            _ => {
                store.load_error = Some(
                    "hardware.json cannot be read safely; original data is preserved and hardware edits are disabled."
                        .into(),
                );
            }
        }
        store
    }

    fn parse(bytes: &[u8]) -> Option<(Vec<DirectEntry>, u64)> {
        let value: Value = serde_json::from_slice(bytes).ok()?;
        if value["schema_version"] != 1 {
            return None;
        }
        let revision = value["revision"].as_u64()?;
        let raw = value["direct"].as_array()?;
        if raw.len() > DIRECT_MAX_ENTRIES {
            return None;
        }
        let mut entries = Vec::new();
        for item in raw {
            let button = u8::try_from(item["button"].as_u64()?).ok()?;
            let chord = direct_text(item.get("chord"), 80)?;
            let entries_for_chord = au05::direct_entries(&chord).ok()?;
            let chord = au05::direct_canonical(&chord).ok()?;
            let label = match item.get("label") {
                None | Some(Value::Null) => None,
                Some(value) => Some(direct_text(Some(value), 80)?),
            };
            if button > 2
                || entries
                    .iter()
                    .any(|entry: &DirectEntry| entry.button == button)
            {
                return None;
            }
            entries.push(DirectEntry {
                button,
                chord,
                label,
                entries: entries_for_chord,
            });
        }
        Some((entries, revision))
    }

    fn has_button(&self, button: u8) -> bool {
        self.entries.iter().any(|entry| entry.button == button)
    }

    fn entry(&self, button: u8) -> Option<&DirectEntry> {
        self.entries.iter().find(|entry| entry.button == button)
    }

    fn entries(&self) -> &[DirectEntry] {
        &self.entries
    }

    fn view(&self) -> Value {
        json!({
            "revision": self.revision,
            "entries": self.entries.iter().map(|entry| json!({
                "button": entry.button,
                "chord": entry.chord,
                "label": entry.label,
            })).collect::<Vec<_>>(),
            "limit": DIRECT_MAX_ENTRIES,
            "storage_writable": self.load_error.is_none(),
            "error": self.load_error,
        })
    }

    fn set(&mut self, body: &str) -> (u16, Value) {
        let fail = |code, error: &str| (code, json!({"error": error}));
        if self.load_error.is_some() {
            return (
                503,
                json!({"error":"Hardware shortcut storage is unavailable; original data is preserved"}),
            );
        }
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            return fail(400, "Expected a JSON object");
        };
        let Some(object) = value.as_object() else {
            return fail(400, "Expected a JSON object");
        };
        if object
            .keys()
            .any(|key| !matches!(key.as_str(), "revision" | "button" | "chord" | "label"))
        {
            return fail(400, "Unexpected hardware shortcut field");
        }
        if value["revision"].as_u64() != Some(self.revision) {
            return fail(409, "Hardware shortcuts changed; refresh before editing");
        }
        let Some(button) = value["button"]
            .as_u64()
            .and_then(|value| u8::try_from(value).ok())
        else {
            return fail(400, "button must be 0, 1, or 2");
        };
        if button > 2 {
            return fail(400, "button must be 0, 1, or 2");
        }
        let Some(chord) = direct_text(value.get("chord"), 80) else {
            return fail(400, "A valid chord is required");
        };
        let Ok(entries_for_chord) = au05::direct_entries(&chord) else {
            return fail(
                400,
                "chord must be a supported key or a combination of modifiers and one key",
            );
        };
        let Ok(chord) = au05::direct_canonical(&chord) else {
            return fail(400, "chord is not a supported AU05 chord");
        };
        let label = match value.get("label") {
            None | Some(Value::Null) => None,
            Some(_) => match direct_text(value.get("label"), 80) {
                Some(label) => Some(label),
                None => return fail(400, "Label must be plain text under 80 bytes"),
            },
        };
        let mut entries = self.entries.clone();
        entries.retain(|entry| entry.button != button);
        entries.push(DirectEntry {
            button,
            chord,
            label,
            entries: entries_for_chord,
        });
        entries.sort_by_key(|entry| entry.button);
        self.commit(entries)
    }

    fn remove(&mut self, body: &str) -> (u16, Value) {
        let fail = |code, error: &str| (code, json!({"error": error}));
        if self.load_error.is_some() {
            return (
                503,
                json!({"error":"Hardware shortcut storage is unavailable; original data is preserved"}),
            );
        }
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            return fail(400, "Expected a JSON object");
        };
        let Some(object) = value.as_object() else {
            return fail(400, "Expected a JSON object");
        };
        if object
            .keys()
            .any(|key| !matches!(key.as_str(), "revision" | "button"))
        {
            return fail(400, "Unexpected hardware shortcut field");
        }
        if value["revision"].as_u64() != Some(self.revision) {
            return fail(409, "Hardware shortcuts changed; refresh before editing");
        }
        let Some(button) = value["button"]
            .as_u64()
            .and_then(|value| u8::try_from(value).ok())
        else {
            return fail(400, "button must be 0, 1, or 2");
        };
        let mut entries = self.entries.clone();
        let before = entries.len();
        entries.retain(|entry| entry.button != button);
        if before == entries.len() {
            return fail(404, "That button has no direct chord");
        }
        self.commit(entries)
    }

    fn commit(&mut self, entries: Vec<DirectEntry>) -> (u16, Value) {
        let Some(revision) = self.revision.checked_add(1) else {
            return (409, json!({"error":"Revision limit reached"}));
        };
        let stored = json!({
            "schema_version": 1,
            "revision": revision,
            "direct": entries.iter().map(|entry| json!({
                "button": entry.button,
                "chord": entry.chord,
                "label": entry.label,
            })).collect::<Vec<_>>(),
        });
        if atomic_write(&self.path, stored.to_string().as_bytes()).is_err() {
            return (
                500,
                json!({"error":"Could not save hardware shortcuts; previous state is unchanged"}),
            );
        }
        self.entries = entries;
        self.revision = revision;
        (200, self.view())
    }
}

fn direct_text(value: Option<&Value>, max: usize) -> Option<String> {
    let text = value?.as_str()?;
    (!text.is_empty() && text.len() <= max && !text.chars().any(char::is_control))
        .then(|| text.to_owned())
}

#[derive(Clone, Debug)]
struct Detection {
    connected: bool,
    product: Option<String>,
    microphone: Option<String>,
    error: Option<String>,
}

impl Detection {
    fn disabled() -> Self {
        Self {
            connected: false,
            product: None,
            microphone: None,
            error: Some(
                "Direct AU05 control requires the authenticated Windows service.".to_string(),
            ),
        }
    }

    fn not_found() -> Self {
        Self {
            connected: false,
            product: None,
            microphone: None,
            error: Some(
                "AU05 was not found. Plug in its 2.4G receiver, power the device on, \
                 then press any AU05 key to wake it and refresh."
                    .to_string(),
            ),
        }
    }
}

#[derive(Clone, Debug)]
struct LastOutcome {
    binding_id: Option<String>,
    state: &'static str,
    error: Option<String>,
}

/// Provisioning state for the three device buttons, owned by the service loop.
pub struct Hardware {
    enabled: bool,
    last: std::collections::BTreeMap<u8, LastOutcome>,
    last_direct: std::collections::BTreeMap<u8, LastOutcome>,
    direct: DirectStore,
    cached: Option<(std::time::Instant, Detection)>,
    microphone: Option<String>,
}

impl Hardware {
    pub fn start(enabled: bool, state_dir: PathBuf, microphone: Option<String>) -> Self {
        #[cfg(test)]
        let direct = DirectStore::empty_for_test();
        let _ = state_dir;
        #[cfg(not(test))]
        let direct = DirectStore::load(state_dir);
        Self {
            enabled,
            last: std::collections::BTreeMap::new(),
            last_direct: std::collections::BTreeMap::new(),
            direct,
            cached: None,
            microphone,
        }
    }

    /// Saves one direct firmware chord. The caller must reject a button that
    /// also carries an application binding.
    pub fn configure_direct(&mut self, body: &str) -> (u16, Value) {
        self.direct.set(body)
    }

    /// Removes one direct firmware chord.
    pub fn remove_direct(&mut self, body: &str) -> (u16, Value) {
        self.direct.remove(body)
    }

    /// Write one registered trigger chord into a button without touching the
    /// application/direct binding tables. Used by hardware action bindings.
    pub fn write_trigger(
        &mut self,
        button: u8,
        hotkey: &Hotkey,
    ) -> std::result::Result<(), String> {
        if !self.enabled {
            return Err("Hardware control requires the authenticated Windows service.".into());
        }
        #[cfg(windows)]
        {
            let detection = {
                self.cached = None;
                self.detection()
            };
            if !detection.connected {
                return Err(detection
                    .error
                    .unwrap_or_else(|| "AU05 is not connected.".to_string()));
            }
            let index =
                Button::from_index(button).ok_or("Unknown AU05 button index.".to_string())?;
            let mut device = windows_impl::Device::open()?;
            device.provision(index, hotkey)?;
            self.last.remove(&button);
            self.cached = None;
            Ok(())
        }
        #[cfg(not(windows))]
        {
            let _ = (button, hotkey);
            Err("AU05 hardware control requires Windows.".into())
        }
    }

    /// Direct chords currently configured, for cross-mode conflict checks.
    pub fn direct_chords(&self) -> Vec<Hotkey> {
        self.direct
            .entries()
            .iter()
            .filter_map(|entry| Hotkey::parse(&entry.chord).ok())
            .collect()
    }

    /// Buttons currently carrying a direct chord.
    pub fn direct_buttons(&self) -> Vec<u8> {
        self.direct
            .entries()
            .iter()
            .map(|entry| entry.button)
            .collect()
    }

    /// Applies the built-in button layout on a first run.
    ///
    /// Returns true when defaults were seeded and a write was attempted. A
    /// button that already has an application shortcut keeps that binding; the
    /// default chord only fills untouched buttons.
    pub fn ensure_defaults(&mut self, bindings: &[HardwareBinding], action_buttons: &[u8]) -> bool {
        if !self.direct.seed_defaults {
            return false;
        }
        self.direct.seed_defaults = false;
        let mut reserved: std::collections::BTreeSet<u8> =
            bindings.iter().map(|binding| binding.button).collect();
        reserved.extend(action_buttons.iter().copied());
        self.direct
            .entries
            .retain(|entry| !reserved.contains(&entry.button));
        let entries = self.direct.entries.clone();
        if !self.direct.path.as_os_str().is_empty() {
            let _ = self.direct.commit(entries);
        }
        self.provision(bindings);
        true
    }

    /// True when a direct firmware chord currently names this button.
    pub fn direct_configured(&self, button: u8) -> bool {
        self.direct.has_button(button)
    }

    /// True when a direct chord equals one of VibDock's registered triggers.
    ///
    /// Such a chord would be captured by the global hotkey registration as
    /// well as emitted to the foreground application, so the two modes must
    /// not share it.
    pub fn direct_conflicts_with_trigger(&self, trigger: &Hotkey) -> bool {
        self.direct.entries().iter().any(|entry| {
            Hotkey::parse(&entry.chord)
                .ok()
                .is_some_and(|parsed| parsed == *trigger)
        })
    }

    /// Re-detect the device and write every saved binding, then return the
    /// current status view. Buttons that no longer have a binding in this set
    /// but were provisioned by an earlier sync are cleared.
    pub fn provision(&mut self, bindings: &[HardwareBinding]) -> Value {
        if !self.enabled {
            return self.view(bindings);
        }
        #[cfg(windows)]
        {
            self.provision_windows(bindings);
        }
        self.view(bindings)
    }

    /// Clear one button after its binding was removed.
    pub fn clear_button(&mut self, button: u8) -> std::result::Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        #[cfg(windows)]
        {
            self.clear_windows(button)?;
        }
        #[cfg(not(windows))]
        {
            let _ = button;
        }
        self.last.remove(&button);
        if !self.direct.has_button(button) {
            self.last_direct.remove(&button);
        }
        self.cached = None;
        Ok(())
    }

    /// Current status view. This never writes to the device.
    pub fn view(&mut self, bindings: &[HardwareBinding]) -> Value {
        let detection = if self.enabled {
            self.detection()
        } else {
            Detection::disabled()
        };
        let mut buttons = Vec::new();
        for button in Button::all() {
            let index = button.index();
            let binding = bindings.iter().find(|binding| binding.button == index);
            let direct = self.direct.entry(index);
            let (app_state, app_error) = match binding {
                None => ("unbound", None),
                Some(binding) => match self.last.get(&index) {
                    Some(last)
                        if last.binding_id.as_deref() == Some(binding.binding_id.as_str()) =>
                    {
                        (last.state, last.error.clone())
                    }
                    _ if detection.connected => ("pending", None),
                    _ => (
                        "failed",
                        detection.error.clone().or_else(|| {
                            Some(
                                "AU05 is not connected; the stored trigger could not be verified."
                                    .to_string(),
                            )
                        }),
                    ),
                },
            };
            let (direct_state, direct_error) = match direct {
                None => ("unbound", None),
                Some(_) => match self.last_direct.get(&index) {
                    Some(last) => (last.state, last.error.clone()),
                    None if detection.connected => ("pending", None),
                    None => (
                        "failed",
                        detection.error.clone().or_else(|| {
                            Some(
                                "AU05 is not connected; the direct chord could not be verified."
                                    .to_string(),
                            )
                        }),
                    ),
                },
            };
            let (state, error) = match (binding.is_some(), direct.is_some()) {
                (true, true) => (
                    "failed",
                    Some(
                        "This button has both an application shortcut and a direct chord; clear one of them."
                            .to_string(),
                    ),
                ),
                (true, false) => (app_state, app_error),
                (false, true) => (direct_state, direct_error.clone()),
                (false, false) => ("unbound", None),
            };
            buttons.push(json!({
                "button": index,
                "label": button.label(),
                "binding_id": binding.map(|binding| binding.binding_id.clone()),
                "binding_label": binding.map(|binding| binding.binding_label.clone()),
                "trigger": binding.map(|binding| binding.trigger_text.clone()),
                "direct_chord": direct.map(|entry| entry.chord.clone()),
                "direct_label": direct.and_then(|entry| entry.label.clone()),
                "direct_state": direct.map(|_| direct_state),
                "direct_error": direct.and_then(|_| direct_error.clone()),
                "state": state,
                "error": error,
            }));
        }
        let device = if detection.connected {
            json!({
                "name": detection.product.clone().unwrap_or_else(|| "AU05".to_string()),
                "vendor_id": format!("{:04x}", au05::VENDOR_ID),
                "product_id": format!("{:04x}", au05::PRODUCT_ID),
                "microphone": detection.microphone.clone(),
            })
        } else {
            Value::Null
        };
        json!({
            "schema_version": 1,
            "platform": if cfg!(windows) { "windows" } else { "unsupported" },
            "available": cfg!(windows) && self.enabled,
            "enabled": self.enabled,
            "connected": detection.connected,
            "device": device,
            "buttons": buttons,
            "direct": self.direct.view(),
            "error": detection.error,
            "studio_note": "Direct AU05 control does not require Ulanzi Studio. \
                            Quit Ulanzi Studio while VibDock owns the device.",
        })
    }

    #[cfg(windows)]
    fn detection(&mut self) -> Detection {
        const CACHE: std::time::Duration = std::time::Duration::from_secs(2);
        if let Some((at, detection)) = &self.cached {
            if at.elapsed() < CACHE {
                return detection.clone();
            }
        }
        let detection = match windows_impl::enumerate() {
            Ok(Some(found)) => Detection {
                connected: true,
                product: Some(found.product),
                microphone: self.microphone.clone(),
                error: None,
            },
            Ok(None) => Detection::not_found(),
            Err(error) => Detection {
                connected: false,
                product: None,
                microphone: None,
                error: Some(error),
            },
        };
        self.cached = Some((std::time::Instant::now(), detection.clone()));
        detection
    }

    #[cfg(not(windows))]
    fn detection(&mut self) -> Detection {
        Detection::disabled()
    }

    #[cfg(windows)]
    fn provision_windows(&mut self, bindings: &[HardwareBinding]) {
        self.cached = None;
        let direct_entries: Vec<DirectEntry> = self.direct.entries().to_vec();
        let detection = self.detection();
        if !detection.connected {
            self.mark_all_failed(bindings, &direct_entries, detection.error.clone());
            return;
        }
        let mut device = match windows_impl::Device::open() {
            Ok(device) => device,
            Err(error) => {
                self.mark_all_failed(bindings, &direct_entries, Some(error));
                return;
            }
        };
        for binding in bindings {
            if direct_entries
                .iter()
                .any(|entry| entry.button == binding.button)
            {
                self.last.insert(
                    binding.button,
                    LastOutcome {
                        binding_id: Some(binding.binding_id.clone()),
                        state: "failed",
                        error: Some(
                            "This button also has a direct chord; clear one of them.".to_string(),
                        ),
                    },
                );
                continue;
            }
            let outcome = match Button::from_index(binding.button) {
                Some(button) => match device.provision(button, &binding.trigger) {
                    Ok(()) => LastOutcome {
                        binding_id: Some(binding.binding_id.clone()),
                        state: "provisioned",
                        error: None,
                    },
                    Err(error) => LastOutcome {
                        binding_id: Some(binding.binding_id.clone()),
                        state: "failed",
                        error: Some(error),
                    },
                },
                None => LastOutcome {
                    binding_id: Some(binding.binding_id.clone()),
                    state: "failed",
                    error: Some("Unknown AU05 button index.".to_string()),
                },
            };
            self.last.insert(binding.button, outcome);
        }
        for entry in &direct_entries {
            if bindings
                .iter()
                .any(|binding| binding.button == entry.button)
            {
                self.last_direct.insert(
                    entry.button,
                    LastOutcome {
                        binding_id: None,
                        state: "failed",
                        error: Some(
                            "This button also has an application shortcut; clear one of them."
                                .to_string(),
                        ),
                    },
                );
                continue;
            }
            let outcome = match Button::from_index(entry.button) {
                Some(button) => match device.provision_entries(button, &entry.entries) {
                    Ok(()) => LastOutcome {
                        binding_id: None,
                        state: "provisioned",
                        error: None,
                    },
                    Err(error) => LastOutcome {
                        binding_id: None,
                        state: "failed",
                        error: Some(error),
                    },
                },
                None => LastOutcome {
                    binding_id: None,
                    state: "failed",
                    error: Some("Unknown AU05 button index.".to_string()),
                },
            };
            self.last_direct.insert(entry.button, outcome);
        }
        // A button that used to be managed by us and no longer carries either
        // kind of binding is put back into a neutral state.
        let active: std::collections::BTreeSet<u8> = bindings
            .iter()
            .map(|binding| binding.button)
            .chain(direct_entries.iter().map(|entry| entry.button))
            .collect();
        let stale: std::collections::BTreeSet<u8> = self
            .last
            .keys()
            .chain(self.last_direct.keys())
            .copied()
            .filter(|button| !active.contains(button))
            .collect();
        for button in stale {
            if let Some(index) = Button::from_index(button) {
                let _ = device.clear(index);
            }
            self.last.remove(&button);
            self.last_direct.remove(&button);
        }
        self.cached = None;
    }

    #[cfg(windows)]
    fn mark_all_failed(
        &mut self,
        bindings: &[HardwareBinding],
        direct_entries: &[DirectEntry],
        error: Option<String>,
    ) {
        for binding in bindings {
            self.last.insert(
                binding.button,
                LastOutcome {
                    binding_id: Some(binding.binding_id.clone()),
                    state: "failed",
                    error: error.clone(),
                },
            );
        }
        for entry in direct_entries {
            self.last_direct.insert(
                entry.button,
                LastOutcome {
                    binding_id: None,
                    state: "failed",
                    error: error.clone(),
                },
            );
        }
    }

    #[cfg(windows)]
    fn clear_windows(&mut self, button: u8) -> std::result::Result<(), String> {
        let detection = {
            self.cached = None;
            self.detection()
        };
        if !detection.connected {
            return Err(detection
                .error
                .unwrap_or_else(|| "AU05 is not connected.".to_string()));
        }
        let mut device = windows_impl::Device::open()?;
        // Clearing an application binding must not erase a direct chord that
        // the user configured independently; rewrite that chord instead.
        if let Some(entry) = self.direct.entry(button).cloned() {
            let index =
                Button::from_index(button).ok_or("Unknown AU05 button index.".to_string())?;
            device.provision_entries(index, &entry.entries)?;
            self.last_direct.insert(
                button,
                LastOutcome {
                    binding_id: None,
                    state: "provisioned",
                    error: None,
                },
            );
            return Ok(());
        }
        let button = Button::from_index(button).ok_or("Unknown AU05 button index.".to_string())?;
        device.clear(button)
    }
}

#[cfg(windows)]
mod windows_impl {
    use super::{au05, Button, Hotkey};
    use std::time::{Duration, Instant};
    use windows::core::PCWSTR;
    use windows::Win32::Devices::DeviceAndDriverInstallation::{
        SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInterfaces, SetupDiGetClassDevsW,
        SetupDiGetDeviceInterfaceDetailW, DIGCF_DEVICEINTERFACE, DIGCF_PRESENT,
        SP_DEVICE_INTERFACE_DATA, SP_DEVICE_INTERFACE_DETAIL_DATA_W,
    };
    use windows::Win32::Devices::HumanInterfaceDevice::{
        HidD_FreePreparsedData, HidD_GetAttributes, HidD_GetHidGuid, HidD_GetPreparsedData,
        HidD_GetProductString, HidP_GetCaps, HIDD_ATTRIBUTES, HIDP_CAPS, PHIDP_PREPARSED_DATA,
    };
    use windows::Win32::Foundation::{
        CloseHandle, ERROR_IO_PENDING, GENERIC_READ, GENERIC_WRITE, HANDLE, WAIT_TIMEOUT,
    };
    use windows::Win32::Storage::FileSystem::{
        CreateFileW, ReadFile, WriteFile, FILE_ATTRIBUTE_NORMAL, FILE_FLAG_OVERLAPPED,
        FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
    };
    use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
    use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};

    /// Holds a kernel handle and always closes it on drop.
    struct OwnedHandle(HANDLE);

    impl Drop for OwnedHandle {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    pub(super) struct Found {
        pub(super) path: String,
        pub(super) product: String,
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn last_error(context: &str, error: windows::core::Error) -> String {
        format!("{context}: {error}")
    }

    /// Opens the vendor interface path with overlapped I/O and a shared lock.
    unsafe fn open_path(path: &str) -> std::result::Result<OwnedHandle, String> {
        let path = wide(path);
        let handle = CreateFileW(
            PCWSTR(path.as_ptr()),
            GENERIC_READ.0 | GENERIC_WRITE.0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
            None,
        )
        .map_err(|error| last_error("Could not open the AU05 vendor interface", error))?;
        Ok(OwnedHandle(handle))
    }

    fn open_read_only(path: &str) -> std::result::Result<OwnedHandle, String> {
        unsafe {
            let path = wide(path);
            let handle = CreateFileW(
                PCWSTR(path.as_ptr()),
                GENERIC_READ.0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                None,
                OPEN_EXISTING,
                FILE_ATTRIBUTE_NORMAL | FILE_FLAG_OVERLAPPED,
                None,
            )
            .map_err(|error| last_error("Could not inspect a HID interface", error))?;
            Ok(OwnedHandle(handle))
        }
    }

    fn product_string(handle: HANDLE) -> Option<String> {
        let mut buffer = [0u16; 128];
        let ok = unsafe {
            HidD_GetProductString(
                handle,
                buffer.as_mut_ptr() as *mut std::ffi::c_void,
                std::mem::size_of_val(&buffer) as u32,
            )
        };
        if !ok {
            return None;
        }
        let end = buffer
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(buffer.len());
        (end != 0).then(|| String::from_utf16_lossy(&buffer[..end]))
    }

    /// Finds the AU05 vendor HID collection.
    ///
    /// The device exposes several HID collections (keyboard, consumer control,
    /// mouse) and the vendor channel is the one whose top-level usage page is
    /// `0xFFFC`. Filtering on the vendor/product pair alone would pick the
    /// first keyboard collection and silently write to the wrong interface.
    fn vendor_interfaces() -> std::result::Result<Vec<Found>, String> {
        unsafe {
            let guid = HidD_GetHidGuid();
            let set = SetupDiGetClassDevsW(
                Some(&guid),
                PCWSTR::null(),
                None,
                DIGCF_PRESENT | DIGCF_DEVICEINTERFACE,
            )
            .map_err(|error| last_error("Windows could not enumerate HID devices", error))?;
            if set.is_invalid() {
                return Err("Windows could not enumerate HID devices.".to_string());
            }
            let set = SetGuard(set);
            let mut found = Vec::new();
            let mut index = 0u32;
            loop {
                let mut interface = SP_DEVICE_INTERFACE_DATA {
                    cbSize: std::mem::size_of::<SP_DEVICE_INTERFACE_DATA>() as u32,
                    ..Default::default()
                };
                if SetupDiEnumDeviceInterfaces(set.0, None, &guid, index, &mut interface).is_err() {
                    break;
                }
                index += 1;
                let mut required = 0u32;
                let _ = SetupDiGetDeviceInterfaceDetailW(
                    set.0,
                    &interface,
                    None,
                    0,
                    Some(&mut required),
                    None,
                );
                if required == 0 {
                    continue;
                }
                let mut buffer = vec![0u8; required as usize];
                let detail = buffer.as_mut_ptr() as *mut SP_DEVICE_INTERFACE_DETAIL_DATA_W;
                // `cbSize` is 8 for the x64 ABI, which is the only Windows
                // target this project builds.
                (*detail).cbSize = 8;
                if SetupDiGetDeviceInterfaceDetailW(
                    set.0,
                    &interface,
                    Some(detail),
                    required,
                    Some(&mut required),
                    None,
                )
                .is_err()
                {
                    continue;
                }
                let path = PCWSTR::from_raw((*detail).DevicePath.as_ptr())
                    .to_string()
                    .unwrap_or_default();
                if path.is_empty() {
                    continue;
                }
                let Ok(handle) = open_read_only(&path) else {
                    continue;
                };
                let mut attributes = HIDD_ATTRIBUTES {
                    Size: std::mem::size_of::<HIDD_ATTRIBUTES>() as u32,
                    ..Default::default()
                };
                if !HidD_GetAttributes(handle.0, &mut attributes) {
                    continue;
                }
                if attributes.VendorID != au05::VENDOR_ID
                    || attributes.ProductID != au05::PRODUCT_ID
                {
                    continue;
                }
                let mut preparsed = PHIDP_PREPARSED_DATA::default();
                if !HidD_GetPreparsedData(handle.0, &mut preparsed) {
                    continue;
                }
                let mut caps: HIDP_CAPS = std::mem::zeroed();
                let status = HidP_GetCaps(preparsed, &mut caps);
                let _ = HidD_FreePreparsedData(preparsed);
                if status.0 < 0 || caps.UsagePage != 0xfffc {
                    continue;
                }
                let product = product_string(handle.0).unwrap_or_else(|| "AU05".to_string());
                found.push(Found { path, product });
            }
            Ok(found)
        }
    }

    pub(super) fn enumerate() -> std::result::Result<Option<Found>, String> {
        Ok(vendor_interfaces()?.into_iter().next())
    }

    struct SetGuard(windows::Win32::Devices::DeviceAndDriverInstallation::HDEVINFO);
    impl Drop for SetGuard {
        fn drop(&mut self) {
            unsafe {
                let _ = SetupDiDestroyDeviceInfoList(self.0);
            }
        }
    }

    pub(super) struct Device {
        handle: OwnedHandle,
    }

    impl Device {
        pub(super) fn open() -> std::result::Result<Self, String> {
            let found = enumerate()?.ok_or_else(|| {
                "AU05 was not found. Plug in its 2.4G receiver, power the device on, \
                 then press any AU05 key to wake it and retry."
                    .to_string()
            })?;
            unsafe {
                let handle = open_path(&found.path)?;
                Ok(Self { handle })
            }
        }

        /// Writes the fixed-function clear + shortcut and verifies both slots.
        pub(super) fn provision(
            &mut self,
            button: Button,
            hotkey: &Hotkey,
        ) -> std::result::Result<(), String> {
            let entries = au05::hotkey_entries(hotkey)?;
            self.provision_entries(button, &entries)
        }

        /// Writes already-derived key entries, used for direct firmware chords
        /// that may be a single bare key such as Enter.
        pub(super) fn provision_entries(
            &mut self,
            button: Button,
            entries: &[vibdock_au05::KeyEntry],
        ) -> std::result::Result<(), String> {
            // A heartbeat is harmless and gives a sleeping vendor channel one
            // more chance to answer before the real write is attempted.
            let heartbeat = au05::encode_frame(au05::HEARTBEAT_HEADER, &[]);
            self.write(&heartbeat)?;
            std::thread::sleep(Duration::from_millis(80));
            let fixed_clear = au05::encode_frame(
                au05::SET_FIXED_FUNCTION_HEADER,
                &au05::fixed_function_clear_payload(button),
            );
            self.write(&fixed_clear)?;
            std::thread::sleep(Duration::from_millis(120));
            let payload = au05::shortcut_payload_entries(button, entries);
            let frame = au05::encode_frame(au05::SET_SHORTCUT_HEADER, &payload);
            self.write(&frame)?;
            std::thread::sleep(Duration::from_millis(180));
            let Some(plain) = self.request(
                au05::GET_SHORTCUT_HEADER,
                &[button.index()],
                au05::GET_SHORTCUT_HEADER,
                1000,
            )?
            else {
                return Err(
                    "AU05 did not answer the shortcut readback. Press any AU05 key to wake it, \
                     then retry."
                        .to_string(),
                );
            };
            if !au05::shortcut_readback_matches_entries(&plain, button, entries) {
                return Err(
                    "AU05 stored a different shortcut than the one requested; the button was \
                     not armed."
                        .to_string(),
                );
            }
            let Some(fixed) = self.request(
                au05::GET_FIXED_FUNCTION_HEADER,
                &au05::fixed_function_get_payload(button),
                au05::GET_FIXED_FUNCTION_HEADER,
                1000,
            )?
            else {
                return Err(
                    "AU05 did not answer the fixed-function readback; the slot could not be \
                     verified."
                        .to_string(),
                );
            };
            match au05::parse_fixed_function_readback(&fixed) {
                Some((index, 0)) if index == button.index() => Ok(()),
                Some((_, function)) => Err(format!(
                    "AU05 kept fixed function {function} on this button; the stored shortcut \
                     may be suppressed."
                )),
                None => Err("AU05 returned a malformed fixed-function readback.".to_string()),
            }
        }

        /// Clears both the shortcut slot and any fixed function, then verifies
        /// the cleared state when the device answers.
        pub(super) fn clear(&mut self, button: Button) -> std::result::Result<(), String> {
            let frame = au05::encode_frame(
                au05::SET_SHORTCUT_HEADER,
                &au05::clear_shortcut_payload(button),
            );
            self.write(&frame)?;
            std::thread::sleep(Duration::from_millis(120));
            let fixed = au05::encode_frame(
                au05::SET_FIXED_FUNCTION_HEADER,
                &au05::fixed_function_clear_payload(button),
            );
            self.write(&fixed)?;
            std::thread::sleep(Duration::from_millis(150));
            // Readback is best-effort on clear: the write is the state change;
            // a timeout after it is reported rather than claimed as success.
            let Some(plain) = self.request(
                au05::GET_SHORTCUT_HEADER,
                &[button.index()],
                au05::GET_SHORTCUT_HEADER,
                1000,
            )?
            else {
                return Ok(());
            };
            match au05::parse_shortcut_readback(&plain) {
                Some(readback) if readback.entries.is_empty() => Ok(()),
                Some(_) => Err(
                    "AU05 still reports a shortcut on the cleared button; it was not cleared."
                        .to_string(),
                ),
                None => Err("AU05 returned a malformed shortcut readback.".to_string()),
            }
        }

        fn write(&self, frame: &[u8; 64]) -> std::result::Result<(), String> {
            let event = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }
                .map_err(|error| last_error("Could not create a HID event", error))?;
            let _event = OwnedHandle(event);
            let mut overlapped = OVERLAPPED {
                hEvent: event,
                ..Default::default()
            };
            let mut written = 0u32;
            let result = unsafe {
                WriteFile(
                    self.handle.0,
                    Some(frame.as_slice()),
                    Some(&mut written),
                    Some(&mut overlapped),
                )
            };
            match result {
                Ok(()) if written as usize == frame.len() => Ok(()),
                Ok(()) => Err(format!("Short AU05 HID write ({written} bytes).")),
                Err(error) if error.code() == ERROR_IO_PENDING.to_hresult() => {
                    let wait = unsafe { WaitForSingleObject(event, 1200) };
                    if wait == WAIT_TIMEOUT {
                        unsafe {
                            let _ = CancelIoEx(self.handle.0, Some(&overlapped));
                            let mut transferred = 0u32;
                            let _ = GetOverlappedResult(
                                self.handle.0,
                                &overlapped,
                                &mut transferred,
                                true,
                            );
                        }
                        return Err(
                            "AU05 did not accept the write. Press any AU05 key to wake it, then \
                             retry."
                                .to_string(),
                        );
                    }
                    let mut transferred = 0u32;
                    unsafe {
                        GetOverlappedResult(self.handle.0, &overlapped, &mut transferred, false)
                    }
                    .map_err(|error| last_error("AU05 HID write failed", error))?;
                    if transferred as usize == frame.len() {
                        Ok(())
                    } else {
                        Err(format!("Short AU05 HID write ({transferred} bytes)."))
                    }
                }
                Err(error) => Err(last_error("AU05 HID write failed", error)),
            }
        }

        fn read(&self, timeout_ms: u32) -> std::result::Result<Option<[u8; 64]>, String> {
            let event = unsafe { CreateEventW(None, true, false, PCWSTR::null()) }
                .map_err(|error| last_error("Could not create a HID event", error))?;
            let _event = OwnedHandle(event);
            let mut overlapped = OVERLAPPED {
                hEvent: event,
                ..Default::default()
            };
            let mut buffer = [0u8; 64];
            let mut read = 0u32;
            let result = unsafe {
                ReadFile(
                    self.handle.0,
                    Some(&mut buffer),
                    Some(&mut read),
                    Some(&mut overlapped),
                )
            };
            match result {
                Ok(()) => {
                    if read == 0 {
                        return Ok(None);
                    }
                    buffer[read as usize..].fill(0);
                    Ok(Some(buffer))
                }
                Err(error) if error.code() == ERROR_IO_PENDING.to_hresult() => {
                    let wait = unsafe { WaitForSingleObject(event, timeout_ms) };
                    if wait == WAIT_TIMEOUT {
                        unsafe {
                            let _ = CancelIoEx(self.handle.0, Some(&overlapped));
                            let mut transferred = 0u32;
                            let _ = GetOverlappedResult(
                                self.handle.0,
                                &overlapped,
                                &mut transferred,
                                true,
                            );
                        }
                        return Ok(None);
                    }
                    let mut transferred = 0u32;
                    unsafe {
                        GetOverlappedResult(self.handle.0, &overlapped, &mut transferred, false)
                    }
                    .map_err(|error| last_error("AU05 HID read failed", error))?;
                    if transferred == 0 {
                        return Ok(None);
                    }
                    buffer[transferred as usize..].fill(0);
                    Ok(Some(buffer))
                }
                Err(error) => Err(last_error("AU05 HID read failed", error)),
            }
        }

        /// Sends a request and waits for the response whose b1/b2 match.
        fn request(
            &self,
            header: [u8; 4],
            payload: &[u8],
            expected: [u8; 4],
            timeout_ms: u32,
        ) -> std::result::Result<Option<[u8; 64]>, String> {
            let frame = au05::encode_frame(header, payload);
            self.write(&frame)?;
            let deadline = Instant::now() + Duration::from_millis(u64::from(timeout_ms));
            while Instant::now() < deadline {
                let remaining = deadline
                    .saturating_duration_since(Instant::now())
                    .as_millis()
                    .clamp(20, 300) as u32;
                match self.read(remaining)? {
                    Some(report) => {
                        if let Some(plain) = au05::decode_report(&report) {
                            if au05::response_matches(&plain, expected) {
                                return Ok(Some(plain));
                            }
                        }
                    }
                    None => continue,
                }
            }
            Ok(None)
        }
    }
}

#[cfg(test)]
mod direct_tests {
    use super::DirectStore;

    fn store() -> (DirectStore, std::path::PathBuf) {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "vibdock-hardware-test-{}-{stamp}",
            std::process::id()
        ));
        std::fs::create_dir(&dir).expect("temp dir");
        (
            DirectStore {
                path: dir.join("hardware.json"),
                revision: 0,
                entries: Vec::new(),
                load_error: None,
                seed_defaults: false,
            },
            dir,
        )
    }

    #[test]
    fn ctrl_win_is_a_valid_direct_chord_and_roundtrips() {
        let (mut store, dir) = store();
        let body =
            serde_json::json!({"revision":0,"button":0,"chord":"Ctrl + Win","label":"微信语音"})
                .to_string();
        let (code, view) = store.set(&body);
        assert_eq!(code, 200, "{view}");
        assert_eq!(store.entries.len(), 1);
        assert_eq!(store.entries[0].button, 0);
        assert_eq!(store.entries[0].chord, "Ctrl + Win");
        assert_eq!(view["entries"][0]["chord"], "Ctrl + Win");
        let bytes = std::fs::read(store.path.clone()).unwrap();
        let parsed = DirectStore::parse(&bytes).expect("persisted file parses");
        assert_eq!(parsed.0.len(), 1);
        assert_eq!(parsed.0[0].chord, "Ctrl + Win");

        // Re-setting the same button replaces rather than duplicates it.
        let (code, _) = store.set(
            &serde_json::json!({"revision":1,"button":0,"chord":"Ctrl + Shift + Win"}).to_string(),
        );
        assert_eq!(code, 200);
        assert_eq!(store.entries.len(), 1);
        assert_eq!(store.entries[0].chord, "Ctrl + Shift + Win");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn direct_edits_fail_closed() {
        let (mut store, dir) = store();
        for (body, expected) in [
            (
                serde_json::json!({"revision":1,"button":0,"chord":"Ctrl + Win"}).to_string(),
                409,
            ),
            (
                serde_json::json!({"revision":0,"button":3,"chord":"Ctrl + Win"}).to_string(),
                400,
            ),
            (
                serde_json::json!({"revision":0,"button":0,"chord":"F13"}).to_string(),
                400,
            ),
            (
                serde_json::json!({"revision":0,"button":0,"chord":"Ctrl + Win","path":"C:\\x"})
                    .to_string(),
                400,
            ),
        ] {
            assert_eq!(store.set(&body).0, expected, "{body}");
        }
        assert!(store.entries.is_empty());
        let (code, _) = store.remove(&serde_json::json!({"revision":0,"button":0}).to_string());
        assert_eq!(code, 404);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn direct_chord_can_be_cleared() {
        let (mut store, dir) = store();
        assert_eq!(
            store
                .set(&serde_json::json!({"revision":0,"button":2,"chord":"Ctrl + Win"}).to_string())
                .0,
            200
        );
        assert!(store.has_button(2));
        let (code, view) = store.remove(&serde_json::json!({"revision":1,"button":2}).to_string());
        assert_eq!(code, 200, "{view}");
        assert!(!store.has_button(2));
        assert!(store.entries.is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn built_in_defaults_are_wechat_enter_and_escape() {
        let defaults = DirectStore::defaults();
        assert_eq!(defaults.len(), 3);
        assert_eq!(defaults[0].button, 0);
        assert_eq!(defaults[0].chord, "Ctrl + Win");
        assert_eq!(defaults[0].label.as_deref(), Some("微信语音"));
        assert_eq!(defaults[1].button, 1);
        assert_eq!(defaults[1].chord, "Enter");
        assert_eq!(defaults[2].button, 2);
        assert_eq!(defaults[2].chord, "Escape");
        assert_eq!(defaults[0].entries.len(), 2);
        assert_eq!(defaults[1].entries.len(), 1);
        assert_eq!(defaults[2].entries.len(), 1);
    }

    #[test]
    fn direct_chord_conflicts_are_detected_for_registered_triggers() {
        let direct = DirectStore {
            path: std::path::PathBuf::new(),
            revision: 0,
            entries: DirectStore::defaults(),
            load_error: None,
            seed_defaults: false,
        };
        let hardware = super::Hardware {
            enabled: false,
            last: std::collections::BTreeMap::new(),
            last_direct: std::collections::BTreeMap::new(),
            direct,
            cached: None,
            microphone: None,
        };
        let ctrl_win = vibdock_input::hotkey::Hotkey::parse("Ctrl + Win").unwrap();
        assert!(hardware.direct_conflicts_with_trigger(&ctrl_win));
        let other = vibdock_input::hotkey::Hotkey::parse("Ctrl + Alt + F1").unwrap();
        assert!(!hardware.direct_conflicts_with_trigger(&other));
    }

    #[test]
    fn defaults_do_not_overwrite_an_application_button() {
        let direct = DirectStore {
            path: std::path::PathBuf::new(),
            revision: 0,
            entries: DirectStore::defaults(),
            load_error: None,
            seed_defaults: true,
        };
        let mut hardware = super::Hardware {
            enabled: false,
            last: std::collections::BTreeMap::new(),
            last_direct: std::collections::BTreeMap::new(),
            direct,
            cached: None,
            microphone: None,
        };
        let binding = super::HardwareBinding {
            binding_id: "a".repeat(64),
            binding_label: "Codex".to_string(),
            button: 1,
            trigger_text: "Ctrl + Alt + Shift + F9".to_string(),
            trigger: vibdock_input::hotkey::Hotkey::parse("Ctrl + Alt + Shift + F9").unwrap(),
        };
        assert!(hardware.ensure_defaults(&[binding], &[]));
        assert!(!hardware.direct.has_button(1));
        assert!(hardware.direct.has_button(0));
        assert!(hardware.direct.has_button(2));
    }
}
