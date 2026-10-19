//! Saved hardware action bindings.
//!
//! A binding maps one physical control (an AU05 button or a gamepad button) to
//! an ordered list of steps: launch an application, send a chord, toggle a
//! microphone, run a locally defined command with parameters, or wait. This is
//! the flexible layer above the two AU05 provisioning modes:
//!
//! - an AU05 pairing writes a generated, otherwise-unused trigger chord into the
//!   button and registers it so the service can run the steps on press;
//! - a gamepad pairing is fed by the gamepad HID monitor and needs no firmware
//!   write.
//!
//! Commands are structured `executable + argv + declared parameters`; the
//! service never builds a shell command string from request text. The command
//! table is local configuration (`commands.json`). Argument values are passed
//! as separate argv entries, so a value cannot inject another command.
//!
//! Steps that are not implemented (AI application voice start/stop) are
//! rejected at save time rather than saved as permanently dead actions.

#![cfg_attr(not(windows), allow(dead_code))]

use crate::hotkey::Hotkey;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

const MAX_BINDINGS: usize = 16;
const MAX_STEPS: usize = 8;
const MAX_COMMANDS: usize = 32;
const MAX_FILE: usize = 64 * 1024;
const MAX_ARGUMENTS: usize = 16;
const MAX_ARGUMENT_LEN: usize = 512;
const MAX_PARAMETERS: usize = 8;
/// Hotkey ids 2000..2999 are reserved for action bindings; the microphone uses
/// id 1 and per-application bindings use 1000+.
pub(crate) const HOTKEY_ID_BASE: i32 = 2000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    Au05Button(u8),
    Gamepad {
        device_key: String,
        usage: u16,
        label: String,
    },
}

impl Source {
    fn json(&self) -> Value {
        match self {
            Source::Au05Button(button) => json!({"kind":"au05","button":button}),
            Source::Gamepad {
                device_key,
                usage,
                label,
            } => json!({"kind":"gamepad","device_key":device_key,"usage":usage,"label":label}),
        }
    }

    fn parse(value: &Value) -> Option<Self> {
        match value["kind"].as_str()? {
            "au05" => {
                let button = u8::try_from(value["button"].as_u64()?).ok()?;
                (button <= 2).then_some(Source::Au05Button(button))
            }
            "gamepad" => {
                let device_key = bounded_text(value.get("device_key"), 512)?;
                let usage = u16::try_from(value["usage"].as_u64()?).ok()?;
                let label = bounded_text(value.get("label"), 80)?;
                (usage > 0).then_some(Source::Gamepad {
                    device_key,
                    usage,
                    label,
                })
            }
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Step {
    Launch {
        label: String,
        executable: String,
        args: Vec<String>,
    },
    SendKeys {
        chord: String,
        parsed: Hotkey,
    },
    ToggleMicrophone {
        device_id: String,
    },
    Command {
        command_id: String,
        parameters: BTreeMap<String, String>,
    },
    Wait {
        ms: u32,
    },
}

impl Step {
    fn view(&self) -> Value {
        match self {
            Step::Launch { label, args, .. } => {
                json!({"type":"launch","label":label,"args":args})
            }
            Step::SendKeys { chord, .. } => json!({"type":"send_keys","keys":chord}),
            Step::ToggleMicrophone { device_id } => {
                json!({"type":"toggle_microphone","device_id":device_id})
            }
            Step::Command {
                command_id,
                parameters,
            } => json!({"type":"command","command_id":command_id,"parameters":parameters}),
            Step::Wait { ms } => json!({"type":"wait","ms":ms}),
        }
    }

    fn stored_json(&self) -> Value {
        match self {
            Step::Launch {
                label,
                executable,
                args,
            } => json!({"type":"launch","label":label,"executable":executable,"args":args}),
            other => other.view(),
        }
    }

    /// Parses one request step. A launch step names an opaque application
    /// reference that the caller resolves server-side; the resolved executable
    /// path is never accepted from the browser.
    fn parse_api(
        value: &Value,
        resolve_application: &mut dyn FnMut(&str) -> Option<(String, String)>,
    ) -> Result<Self, String> {
        match value["type"].as_str().unwrap_or_default() {
            "launch" => {
                let reference = bounded_text(value.get("application_reference"), 128)
                    .ok_or("launch requires an application_reference")?;
                let (executable, default_label) = resolve_application(&reference)
                    .ok_or("The selected application is no longer current; select it again")?;
                let label = match value.get("label") {
                    None | Some(Value::Null) => default_label,
                    Some(_) => bounded_text(value.get("label"), 80)
                        .ok_or("Launch label must be plain text under 80 bytes")?,
                };
                let args = argument_list(value.get("args"))?;
                Ok(Step::Launch {
                    label,
                    executable,
                    args,
                })
            }
            "send_keys" => {
                let keys =
                    bounded_text(value.get("keys"), 80).ok_or("send_keys requires a keys chord")?;
                let parsed = Hotkey::parse(&keys).map_err(|error| error.to_string())?;
                Ok(Step::SendKeys {
                    chord: parsed.as_string(),
                    parsed,
                })
            }
            "toggle_microphone" => {
                let device_id = bounded_text(value.get("device_id"), 512)
                    .ok_or("toggle_microphone requires a device_id")?;
                Ok(Step::ToggleMicrophone { device_id })
            }
            "command" => {
                let command_id = bounded_text(value.get("command_id"), 40)
                    .ok_or("command requires a command_id")?;
                let parameters = match value.get("parameters") {
                    None | Some(Value::Null) => BTreeMap::new(),
                    Some(Value::Object(map)) => {
                        let mut out = BTreeMap::new();
                        for (key, value) in map {
                            let value =
                                value.as_str().ok_or("command parameters must be strings")?;
                            out.insert(key.clone(), value.to_string());
                        }
                        out
                    }
                    Some(_) => return Err("command parameters must be an object".into()),
                };
                Ok(Step::Command {
                    command_id,
                    parameters,
                })
            }
            "wait" => {
                let ms = value["ms"].as_u64().unwrap_or(0);
                if ms > 10_000 {
                    return Err("wait must be between 0 and 10000 ms".into());
                }
                Ok(Step::Wait { ms: ms as u32 })
            }
            other => Err(format!("Unknown action step type: {other}")),
        }
    }

    fn parse_stored(value: &Value) -> Option<Self> {
        match value["type"].as_str()? {
            "launch" => Some(Step::Launch {
                label: bounded_text(value.get("label"), 80)?,
                executable: bounded_text(value.get("executable"), 1024)?,
                args: argument_list(value.get("args")).ok()?,
            }),
            "send_keys" => {
                let chord = bounded_text(value.get("keys"), 80)?;
                let parsed = Hotkey::parse(&chord).ok()?;
                Some(Step::SendKeys { chord, parsed })
            }
            "toggle_microphone" => Some(Step::ToggleMicrophone {
                device_id: bounded_text(value.get("device_id"), 512)?,
            }),
            "command" => {
                let command_id = bounded_text(value.get("command_id"), 40)?;
                let parameters = match value.get("parameters") {
                    None | Some(Value::Null) => BTreeMap::new(),
                    Some(Value::Object(map)) => map
                        .iter()
                        .map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
                        .collect::<Option<BTreeMap<_, _>>>()?,
                    _ => return None,
                };
                Some(Step::Command {
                    command_id,
                    parameters,
                })
            }
            "wait" => Some(Step::Wait {
                ms: u32::try_from(value["ms"].as_u64()?).ok()?,
            }),
            _ => None,
        }
    }

    fn validate_persisted(&self, commands: &CommandRegistry) -> bool {
        match self {
            Step::Launch { executable, .. } => {
                crate::resident_action_protocol::local_executable_path(executable)
            }
            Step::Command {
                command_id,
                parameters,
            } => commands.expand(command_id, parameters).is_ok(),
            _ => true,
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Binding {
    pub(crate) id: String,
    pub(crate) name: String,
    pub(crate) source: Source,
    pub(crate) trigger: Option<String>,
    pub(crate) steps: Vec<Step>,
}

impl Binding {
    fn parse(value: &Value, commands: &CommandRegistry) -> Option<Self> {
        let id = bounded_text(value.get("id"), 64)?;
        if !token(&id) {
            return None;
        }
        let name = bounded_text(value.get("name"), 80)?;
        let source = Source::parse(value.get("source")?)?;
        let trigger = match value.get("trigger") {
            None | Some(Value::Null) => None,
            Some(_) => {
                let trigger = bounded_text(value.get("trigger"), 80)?;
                Hotkey::parse(&trigger).ok()?;
                Some(trigger)
            }
        };
        let raw = value["steps"].as_array()?;
        if raw.is_empty() || raw.len() > MAX_STEPS {
            return None;
        }
        let steps = raw
            .iter()
            .map(Step::parse_stored)
            .collect::<Option<Vec<_>>>()?;
        if !steps.iter().all(|step| step.validate_persisted(commands)) {
            return None;
        }
        match source {
            Source::Au05Button(_) => {
                trigger.as_ref()?;
            }
            Source::Gamepad { .. } => {
                if trigger.is_some() {
                    return None;
                }
            }
        }
        Some(Binding {
            id,
            name,
            source,
            trigger,
            steps,
        })
    }

    fn view(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "source": self.source.json(),
            "trigger": self.trigger,
            "steps": self.steps.iter().map(Step::view).collect::<Vec<_>>(),
            "armed": live_state(self).0,
            "error": live_state(self).1,
        })
    }
}

#[cfg(windows)]
fn live_state(binding: &Binding) -> (bool, Option<String>) {
    if binding.trigger.is_none() {
        // Gamepad bindings are always armed while the monitor runs.
        return (true, None);
    }
    LIVE_ARMED
        .with(|armed| armed.borrow().contains(&binding.id))
        .then_some(true)
        .map_or((false, Some("Not registered".to_string())), |_| {
            (true, None)
        })
}

#[cfg(not(windows))]
fn live_state(_binding: &Binding) -> (bool, Option<String>) {
    (false, None)
}

#[cfg(windows)]
thread_local! {
    static LIVE_ARMED: std::cell::RefCell<BTreeSet<String>> = const { std::cell::RefCell::new(BTreeSet::new()) };
}

/// Other bindings that already own a trigger, button, or direct chord.
#[derive(Default)]
pub(crate) struct Reserved {
    pub(crate) mic_hotkey: Option<Hotkey>,
    pub(crate) app_triggers: Vec<Hotkey>,
    pub(crate) app_dispatch_chords: Vec<Hotkey>,
    pub(crate) app_buttons: Vec<u8>,
    pub(crate) direct_buttons: Vec<u8>,
    pub(crate) direct_chords: Vec<Hotkey>,
}

pub(crate) struct Registry {
    path: PathBuf,
    revision: u64,
    bindings: Vec<Binding>,
    commands: CommandRegistry,
    load_error: Option<String>,
    register: bool,
    /// Live registrations keep their hotkeys armed; dropping a handle
    /// unregisters it, so they must stay owned by this registry.
    #[cfg(windows)]
    live_hotkeys: Vec<crate::windows::RegisteredHotkey>,
}

impl Registry {
    pub(crate) fn start(enabled: bool, commands: CommandRegistry) -> Self {
        let path = crate::service::state_dir().map(|dir| dir.join("actions.json"));
        let mut registry = match path {
            Ok(path) => Self::load(path, commands),
            Err(_) => Self::empty(
                PathBuf::new(),
                commands,
                Some("Action storage is unavailable; existing data was not changed.".into()),
            ),
        };
        registry.register = enabled && !cfg!(test);
        #[cfg(windows)]
        if enabled {
            registry.rebind_all();
        }
        registry
    }

    fn empty(path: PathBuf, commands: CommandRegistry, error: Option<String>) -> Self {
        Self {
            path,
            revision: 0,
            bindings: Vec::new(),
            commands,
            load_error: error,
            register: !cfg!(test),
            #[cfg(windows)]
            live_hotkeys: Vec::new(),
        }
    }

    #[cfg(test)]
    pub(crate) fn empty_for_test(commands: CommandRegistry) -> Self {
        Self::empty(PathBuf::new(), commands, None)
    }

    fn load(path: PathBuf, commands: CommandRegistry) -> Self {
        let mut registry = Self::empty(path, commands, None);
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
                if let Some((bindings, revision)) = parse_file(&bytes, &registry.commands) {
                    registry.bindings = bindings;
                    registry.revision = revision;
                } else {
                    registry.load_error = Some(
                        "actions.json is invalid or unsupported; original bytes are preserved and action edits are disabled."
                            .into(),
                    );
                }
            }
            _ => {
                registry.load_error = Some(
                    "actions.json cannot be read safely; original data is preserved and action edits are disabled."
                        .into(),
                );
            }
        }
        registry
    }

    pub(crate) fn commands(&self) -> &CommandRegistry {
        &self.commands
    }

    pub(crate) fn view(&self) -> Value {
        json!({
            "schema_version": 1,
            "platform": if cfg!(windows) { "windows" } else { "unsupported" },
            "revision": self.revision,
            "bindings": self.bindings.iter().map(Binding::view).collect::<Vec<_>>(),
            "limit": MAX_BINDINGS,
            "dispatch_available": cfg!(windows),
            "storage_writable": self.load_error.is_none(),
            "error": self.load_error,
        })
    }

    pub(crate) fn has_button(&self, button: u8) -> bool {
        self.bindings
            .iter()
            .any(|binding| binding.source == Source::Au05Button(button))
    }

    #[allow(dead_code)]
    pub(crate) fn has_gamepad_button(&self, device_key: &str, usage: u16) -> bool {
        self.bindings.iter().any(|binding| {
            matches!(&binding.source, Source::Gamepad { device_key: key, usage: u, .. } if key == device_key && *u == usage)
        })
    }

    #[allow(dead_code)]
    pub(crate) fn gamepad_binding(&self, device_key: &str, usage: u16) -> Option<&Binding> {
        self.bindings.iter().find(|binding| {
            matches!(&binding.source, Source::Gamepad { device_key: key, usage: u, .. } if key == device_key && *u == usage)
        })
    }

    pub(crate) fn triggers(&self) -> Vec<Hotkey> {
        self.bindings
            .iter()
            .filter_map(|binding| {
                binding
                    .trigger
                    .as_deref()
                    .and_then(|t| Hotkey::parse(t).ok())
            })
            .collect()
    }

    /// True when `trigger` equals a saved action binding trigger.
    pub(crate) fn conflicts_with_trigger(&self, trigger: &Hotkey) -> bool {
        self.bindings.iter().any(|binding| {
            binding
                .trigger
                .as_deref()
                .and_then(|text| Hotkey::parse(text).ok())
                .is_some_and(|saved| saved == *trigger)
        })
    }

    pub(crate) fn fired(&self, id: i32) -> Option<(String, Vec<Step>)> {
        let index = usize::try_from(id - HOTKEY_ID_BASE).ok()?;
        let binding = self.bindings.get(index)?;
        binding.trigger.as_ref()?;
        Some((binding.name.clone(), binding.steps.clone()))
    }

    /// Display data for a fired action hotkey, for the recent-trigger log.
    pub(crate) fn describe(&self, id: i32) -> Option<(String, Option<String>, Option<u8>)> {
        let index = usize::try_from(id - HOTKEY_ID_BASE).ok()?;
        let binding = self.bindings.get(index)?;
        let button = match binding.source {
            Source::Au05Button(button) => Some(button),
            Source::Gamepad { .. } => None,
        };
        Some((binding.name.clone(), binding.trigger.clone(), button))
    }

    /// The AU05 button carried by one saved action binding, if any.
    pub(crate) fn button_for_id(&self, id: &str) -> Option<u8> {
        self.bindings
            .iter()
            .find(|binding| binding.id == id)
            .and_then(|binding| match binding.source {
                Source::Au05Button(button) => Some(button),
                Source::Gamepad { .. } => None,
            })
    }

    pub(crate) fn bindings(&self) -> &[Binding] {
        &self.bindings
    }

    pub(crate) fn remove(&mut self, body: &str) -> (u16, Value) {
        let fail = |code, error: &str| (code, json!({"error":error}));
        if self.load_error.is_some() {
            return fail(
                503,
                "Action storage is unavailable; original data is preserved",
            );
        }
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            return fail(400, "Expected a JSON object");
        };
        if value.as_object().is_none_or(|object| {
            object
                .keys()
                .any(|key| !matches!(key.as_str(), "revision" | "id"))
        }) {
            return fail(400, "Unexpected action field");
        }
        if value["revision"].as_u64() != Some(self.revision) {
            return fail(409, "Action bindings changed; refresh before editing");
        }
        let Some(id) = bounded_text(value.get("id"), 64).filter(|id| token(id)) else {
            return fail(400, "A saved action id is required");
        };
        let mut bindings = self.bindings.clone();
        let before = bindings.len();
        bindings.retain(|binding| binding.id != id);
        if before == bindings.len() {
            return fail(404, "Action binding not found");
        }
        self.commit(bindings)
    }

    pub(crate) fn add(
        &mut self,
        body: &str,
        reserved: &Reserved,
        resolve_application: &mut dyn FnMut(&str) -> Option<(String, String)>,
    ) -> (u16, Value) {
        let fail = |code, error: &str| (code, json!({"error":error}));
        if self.load_error.is_some() {
            return fail(
                503,
                "Action storage is unavailable; original data is preserved",
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
            .any(|key| !matches!(key.as_str(), "revision" | "name" | "source" | "steps"))
        {
            return fail(
                400,
                "Unexpected action field; executable paths are server-owned",
            );
        }
        if value["revision"].as_u64() != Some(self.revision) {
            return fail(409, "Action bindings changed; refresh before editing");
        }
        if self.bindings.len() >= MAX_BINDINGS {
            return fail(409, "At most sixteen action bindings can be saved");
        }
        let Some(name) = bounded_text(value.get("name"), 80) else {
            return fail(400, "Action name must be plain text under 80 bytes");
        };
        let Some(source) = Source::parse(&value["source"]) else {
            return fail(400, "source must be an AU05 button or a gamepad button");
        };
        let duplicate = match &source {
            Source::Au05Button(button) => self.has_button(*button),
            Source::Gamepad {
                device_key, usage, ..
            } => self.has_gamepad_button(device_key, *usage),
        };
        if duplicate {
            return fail(409, "That physical control already has an action binding");
        }
        match &source {
            Source::Au05Button(button) => {
                if reserved.app_buttons.contains(button) {
                    return fail(
                        409,
                        "That Vibe Key button already has an application shortcut; remove it first",
                    );
                }
                if reserved.direct_buttons.contains(button) {
                    return fail(
                        409,
                        "That Vibe Key button already has a direct chord; clear it first",
                    );
                }
            }
            Source::Gamepad { .. } => {}
        }
        let Some(raw_steps) = value["steps"].as_array() else {
            return fail(400, "steps must be an array");
        };
        if raw_steps.is_empty() || raw_steps.len() > MAX_STEPS {
            return fail(400, "An action needs between one and eight ordered steps");
        }
        let mut steps = Vec::with_capacity(raw_steps.len());
        for raw in raw_steps {
            match Step::parse_api(raw, resolve_application) {
                Ok(step) => steps.push(step),
                Err(error) => return fail(400, &error),
            }
        }
        for step in &steps {
            if let Step::Command {
                command_id,
                parameters,
            } = step
            {
                if let Err(error) = self.commands.expand(command_id, parameters) {
                    return fail(400, &error);
                }
            }
        }
        let trigger = match source {
            Source::Au05Button(_) => match self.choose_trigger(reserved) {
                Some(trigger) => Some(trigger),
                None => {
                    return fail(
                        409,
                        "No unused Ctrl+Alt+Shift+F1-F12 trigger remains for AU05 actions",
                    )
                }
            },
            Source::Gamepad { .. } => None,
        };
        let Ok(id) = crate::service::generate_token() else {
            return fail(503, "Secure action identifiers are unavailable");
        };
        let mut bindings = self.bindings.clone();
        bindings.push(Binding {
            id,
            name,
            source,
            trigger,
            steps,
        });
        self.commit(bindings)
    }

    fn choose_trigger(&self, reserved: &Reserved) -> Option<String> {
        let mut used: BTreeSet<String> = self
            .triggers()
            .into_iter()
            .map(|hotkey| hotkey.as_string())
            .collect();
        for trigger in &reserved.app_triggers {
            used.insert(trigger.as_string());
        }
        for chord in &reserved.app_dispatch_chords {
            used.insert(chord.as_string());
        }
        for chord in &reserved.direct_chords {
            used.insert(chord.as_string());
        }
        if let Some(mic) = &reserved.mic_hotkey {
            used.insert(mic.as_string());
        }
        for number in 1..=12u8 {
            let candidate = Hotkey::new(
                [
                    crate::hotkey::Modifier::Control,
                    crate::hotkey::Modifier::Alt,
                    crate::hotkey::Modifier::Shift,
                ],
                crate::hotkey::Key::Function(number),
            );
            let text = candidate.as_string();
            if !used.contains(&text) {
                return Some(text);
            }
        }
        None
    }

    fn commit(&mut self, bindings: Vec<Binding>) -> (u16, Value) {
        #[cfg(windows)]
        {
            if self.register {
                let previous = self.bindings.clone();
                if let Err(error) = self.try_rebind(&bindings) {
                    self.rebind_all();
                    return (409, json!({"error":error}));
                }
                if let Err(error) = self.persist(&bindings) {
                    self.bindings = previous;
                    self.rebind_all();
                    return (500, json!({"error":error}));
                }
                self.bindings = bindings;
                self.revision += 1;
                return (200, self.view());
            }
        }
        #[cfg(all(windows, not(test)))]
        {
            // Registration disabled (read-only preview): persist without
            // touching the global hotkey table.
            if let Err(error) = self.persist(&bindings) {
                return (500, json!({"error":error}));
            }
            self.bindings = bindings;
            self.revision += 1;
            (200, self.view())
        }
        #[cfg(any(not(windows), test))]
        {
            if let Err(error) = self.persist(&bindings) {
                return (500, json!({"error":error}));
            }
            self.bindings = bindings;
            self.revision += 1;
            (200, self.view())
        }
        #[cfg(all(not(windows), not(test)))]
        {
            let _ = bindings;
            (
                501,
                json!({"error":"Hardware action execution is unavailable on this platform"}),
            )
        }
    }

    fn persist(&self, bindings: &[Binding]) -> Result<(), String> {
        if self.path.as_os_str().is_empty() {
            return Err("Action storage is unavailable".to_string());
        }
        let stored = json!({
            "schema_version": 1,
            "revision": self.revision + 1,
            "bindings": bindings.iter().map(|binding| json!({
                "id": binding.id,
                "name": binding.name,
                "source": binding.source.json(),
                "trigger": binding.trigger,
                "steps": binding.steps.iter().map(Step::stored_json).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        });
        crate::service::atomic_write(&self.path, stored.to_string().as_bytes())
            .map_err(|_| "Could not save actions; previous state is unchanged".to_string())
    }

    #[cfg(windows)]
    fn rebind_all(&mut self) {
        self.live_hotkeys.clear();
        LIVE_ARMED.with(|armed| armed.borrow_mut().clear());
        if !self.register {
            return;
        }
        for (index, binding) in self.bindings.iter().enumerate() {
            let Some(trigger) = binding.trigger.as_deref() else {
                continue;
            };
            let Ok(hotkey) = Hotkey::parse(trigger) else {
                continue;
            };
            if let Ok(handle) =
                crate::windows::RegisteredHotkey::register(&hotkey, action_hotkey_id(index))
            {
                self.live_hotkeys.push(handle);
                LIVE_ARMED.with(|armed| {
                    armed.borrow_mut().insert(binding.id.clone());
                });
            }
        }
    }

    #[cfg(windows)]
    fn try_rebind(&mut self, bindings: &[Binding]) -> Result<(), String> {
        self.live_hotkeys.clear();
        LIVE_ARMED.with(|armed| armed.borrow_mut().clear());
        if !self.register {
            return Ok(());
        }
        let mut handles = Vec::new();
        for (index, binding) in bindings.iter().enumerate() {
            let Some(trigger) = binding.trigger.as_deref() else {
                continue;
            };
            let hotkey =
                Hotkey::parse(trigger).map_err(|error| format!("{}: {error}", binding.name))?;
            let handle =
                crate::windows::RegisteredHotkey::register(&hotkey, action_hotkey_id(index))
                    .map_err(|error| format!("{}: {error}", binding.name))?;
            handles.push(handle);
            LIVE_ARMED.with(|armed| {
                armed.borrow_mut().insert(binding.id.clone());
            });
        }
        self.live_hotkeys = handles;
        Ok(())
    }
}

#[cfg(windows)]
fn action_hotkey_id(index: usize) -> i32 {
    HOTKEY_ID_BASE + i32::try_from(index).unwrap_or(i32::MAX - HOTKEY_ID_BASE)
}

fn parse_file(bytes: &[u8], commands: &CommandRegistry) -> Option<(Vec<Binding>, u64)> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    if value["schema_version"] != 1 {
        return None;
    }
    let revision = value["revision"].as_u64()?;
    let raw = value["bindings"].as_array()?;
    if raw.len() > MAX_BINDINGS {
        return None;
    }
    let mut bindings = Vec::new();
    for item in raw {
        let binding = Binding::parse(item, commands)?;
        if bindings
            .iter()
            .any(|existing: &Binding| existing.id == binding.id)
        {
            return None;
        }
        if bindings
            .iter()
            .any(|existing: &Binding| existing.source == binding.source)
        {
            return None;
        }
        bindings.push(binding);
    }
    Some((bindings, revision))
}

/// A structured local command definition.
#[derive(Clone, Debug)]
pub(crate) struct CommandDefinition {
    pub(crate) id: String,
    pub(crate) label: String,
    executable: String,
    args: Vec<String>,
    parameters: Vec<ParameterDefinition>,
}

#[derive(Clone, Debug)]
pub(crate) struct ParameterDefinition {
    pub(crate) name: String,
    pub(crate) label: String,
    pub(crate) required: bool,
    pub(crate) max_length: usize,
}

impl CommandDefinition {
    fn view(&self) -> Value {
        json!({
            "id": self.id,
            "label": self.label,
            "parameters": self.parameters.iter().map(|parameter| json!({
                "name": parameter.name,
                "label": parameter.label,
                "required": parameter.required,
                "max_length": parameter.max_length,
            })).collect::<Vec<_>>(),
        })
    }

    fn parse(value: &Value) -> Option<Self> {
        let id = bounded_text(value.get("id"), 40)?;
        if !command_token(&id) {
            return None;
        }
        let label = bounded_text(value.get("label"), 80)?;
        let executable = bounded_text(value.get("executable"), 1024)?;
        if !crate::resident_action_protocol::local_executable_path(&executable) {
            return None;
        }
        let args = argument_list(value.get("args")).ok()?;
        let parameters = match value.get("parameters") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(items)) => items
                .iter()
                .map(|item| {
                    Some(ParameterDefinition {
                        name: parameter_name(item.get("name").and_then(Value::as_str)?)?,
                        label: bounded_text(item.get("label"), 80)?,
                        required: item["required"].as_bool().unwrap_or(false),
                        max_length: usize::try_from(item["max_length"].as_u64().unwrap_or(120))
                            .ok()?
                            .clamp(1, 256),
                    })
                })
                .collect::<Option<Vec<_>>>()?,
            _ => return None,
        };
        if parameters.len() > MAX_PARAMETERS {
            return None;
        }
        let definition = Self {
            id,
            label,
            executable,
            args,
            parameters,
        };
        definition.validate_placeholders().then_some(definition)
    }

    fn validate_placeholders(&self) -> bool {
        let names: BTreeSet<&str> = self.parameters.iter().map(|p| p.name.as_str()).collect();
        self.args.iter().all(|argument| {
            placeholders(argument)
                .into_iter()
                .all(|name| names.contains(name))
        })
    }
}

pub(crate) struct CommandRegistry {
    path: PathBuf,
    revision: u64,
    definitions: Vec<CommandDefinition>,
    load_error: Option<String>,
}

impl CommandRegistry {
    #[cfg(test)]
    pub(crate) fn empty_for_test() -> Self {
        Self {
            path: PathBuf::new(),
            revision: 0,
            definitions: Vec::new(),
            load_error: None,
        }
    }

    pub(crate) fn start() -> Self {
        let path = crate::service::state_dir().map(|dir| dir.join("commands.json"));
        match path {
            Ok(path) => Self::load(path),
            Err(_) => Self {
                path: PathBuf::new(),
                revision: 0,
                definitions: Vec::new(),
                load_error: Some(
                    "Command storage is unavailable; existing data was not changed.".into(),
                ),
            },
        }
    }

    fn load(path: PathBuf) -> Self {
        let mut registry = Self {
            path,
            revision: 0,
            definitions: Vec::new(),
            load_error: None,
        };
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
                if let Some((definitions, revision)) = parse_commands(&bytes) {
                    registry.definitions = definitions;
                    registry.revision = revision;
                } else {
                    registry.load_error = Some(
                        "commands.json is invalid or unsupported; original bytes are preserved and command edits are disabled."
                            .into(),
                    );
                }
            }
            _ => {
                registry.load_error = Some(
                    "commands.json cannot be read safely; original data is preserved and command edits are disabled."
                        .into(),
                );
            }
        }
        registry
    }

    pub(crate) fn view(&self) -> Value {
        json!({
            "schema_version": 1,
            "revision": self.revision,
            "commands": self.definitions.iter().map(CommandDefinition::view).collect::<Vec<_>>(),
            "limit": MAX_COMMANDS,
            "storage_writable": self.load_error.is_none(),
            "error": self.load_error,
            "note": "Commands are executable + argv entries. Values are passed as separate argv items, never as a shell string.",
        })
    }

    pub(crate) fn expand(
        &self,
        command_id: &str,
        parameters: &BTreeMap<String, String>,
    ) -> Result<(String, Vec<String>), String> {
        if self.load_error.is_some() {
            return Err("commands.json is invalid; command execution is disabled".into());
        }
        let definition = self
            .definitions
            .iter()
            .find(|definition| definition.id == command_id)
            .ok_or_else(|| format!("Unknown command id: {command_id}"))?;
        let mut values = BTreeMap::new();
        for parameter in &definition.parameters {
            let value = parameters.get(&parameter.name).cloned().unwrap_or_default();
            if parameter.required && value.is_empty() {
                return Err(format!("Command parameter {} is required", parameter.name));
            }
            if value.len() > parameter.max_length || value.chars().any(char::is_control) {
                return Err(format!(
                    "Command parameter {} is longer than {} bytes or contains control characters",
                    parameter.name, parameter.max_length
                ));
            }
            values.insert(parameter.name.as_str(), value);
        }
        if parameters
            .keys()
            .any(|key| !definition.parameters.iter().any(|p| &p.name == key))
        {
            return Err("Unknown command parameter".into());
        }
        let mut args = Vec::with_capacity(definition.args.len());
        for argument in &definition.args {
            let mut expanded = argument.clone();
            for (name, value) in &values {
                expanded = expanded.replace(&format!("{{{name}}}"), value);
            }
            args.push(expanded);
        }
        Ok((definition.executable.clone(), args))
    }
}

fn parse_commands(bytes: &[u8]) -> Option<(Vec<CommandDefinition>, u64)> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    if value["schema_version"] != 1 {
        return None;
    }
    let revision = value["revision"].as_u64()?;
    let raw = value["commands"].as_array()?;
    if raw.len() > MAX_COMMANDS {
        return None;
    }
    let definitions = raw
        .iter()
        .map(CommandDefinition::parse)
        .collect::<Option<Vec<_>>>()?;
    let ids: BTreeSet<&str> = definitions
        .iter()
        .map(|definition| definition.id.as_str())
        .collect();
    (ids.len() == definitions.len()).then_some((definitions, revision))
}

fn placeholders(argument: &str) -> Vec<&str> {
    let mut names = Vec::new();
    let mut rest = argument;
    while let Some(start) = rest.find('{') {
        let Some(end) = rest[start + 1..].find('}') else {
            break;
        };
        let name = &rest[start + 1..start + 1 + end];
        if !name.is_empty() {
            names.push(name);
        }
        rest = &rest[start + 1 + end + 1..];
    }
    names
}

fn argument_list(value: Option<&Value>) -> Result<Vec<String>, String> {
    let items = match value {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::Array(items)) => items,
        Some(_) => return Err("args must be an array of strings".into()),
    };
    if items.len() > MAX_ARGUMENTS {
        return Err(format!("At most {MAX_ARGUMENTS} arguments are allowed"));
    }
    items
        .iter()
        .map(|item| {
            let text = item
                .as_str()
                .ok_or("arguments must be strings")?
                .to_string();
            if text.len() > MAX_ARGUMENT_LEN || text.chars().any(char::is_control) {
                return Err("Argument is too long or contains control characters".into());
            }
            Ok(text)
        })
        .collect()
}

fn bounded_text(value: Option<&Value>, max: usize) -> Option<String> {
    let text = value?.as_str()?;
    (!text.is_empty() && text.len() <= max && !text.chars().any(char::is_control))
        .then(|| text.to_owned())
}

fn token(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn command_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

fn parameter_name(value: &str) -> Option<String> {
    let mut chars = value.chars();
    let first = chars.next()?;
    if !first.is_ascii_alphabetic() {
        return None;
    }
    value
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '_')
        .then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> (Registry, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "vibdock-actions-test-{}",
            crate::service::generate_token().unwrap()
        ));
        std::fs::create_dir(&dir).unwrap();
        (
            Registry {
                path: dir.join("actions.json"),
                revision: 0,
                bindings: Vec::new(),
                commands: CommandRegistry::empty_for_test(),
                load_error: None,
                register: false,
                #[cfg(windows)]
                live_hotkeys: Vec::new(),
            },
            dir,
        )
    }

    fn add_body(source: Value, steps: Value) -> String {
        serde_json::json!({
            "revision":0,
            "name":"测试动作",
            "source":source,
            "steps":steps,
        })
        .to_string()
    }

    #[test]
    fn an_au05_action_gets_a_free_trigger_and_roundtrips() {
        let (mut registry, dir) = registry();
        let body = add_body(
            serde_json::json!({"kind":"au05","button":1}),
            serde_json::json!([{"type":"wait","ms":100}]),
        );
        let (code, view) = registry.add(&body, &Reserved::default(), &mut |_| None);
        assert_eq!(code, 200, "{view}");
        assert_eq!(view["bindings"][0]["trigger"], "Ctrl + Alt + Shift + F1");
        assert_eq!(view["bindings"][0]["source"]["kind"], "au05");
        assert_eq!(view["bindings"][0]["steps"][0]["type"], "wait");
        let stored = std::fs::read_to_string(registry.path.clone()).unwrap();
        assert!(stored.contains("\"bindings\""));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_gamepad_action_has_no_trigger_or_firmware_write() {
        let (mut registry, dir) = registry();
        let body = add_body(
            serde_json::json!({"kind":"gamepad","device_key":"045e:0b13:abc","usage":5,"label":"Button 5"}),
            serde_json::json!([{"type":"wait","ms":1}]),
        );
        let (code, view) = registry.add(&body, &Reserved::default(), &mut |_| None);
        assert_eq!(code, 200, "{view}");
        assert!(view["bindings"][0]["trigger"].is_null());
        assert_eq!(
            registry.gamepad_binding("045e:0b13:abc", 5).unwrap().name,
            "测试动作"
        );
        let id = view["bindings"][0]["id"].as_str().unwrap().to_string();
        let (code, view) = registry.remove(&serde_json::json!({"revision":1,"id":id}).to_string());
        assert_eq!(code, 200, "{view}");
        assert!(registry.bindings().is_empty());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn action_triggers_skip_reserved_chords() {
        let (mut registry, dir) = registry();
        let reserved = Reserved {
            mic_hotkey: Some(Hotkey::parse("Ctrl + Alt + Shift + F1").unwrap()),
            app_triggers: vec![Hotkey::parse("Ctrl + Alt + Shift + F2").unwrap()],
            ..Reserved::default()
        };
        let body = add_body(
            serde_json::json!({"kind":"au05","button":0}),
            serde_json::json!([{"type":"wait","ms":1}]),
        );
        let (code, view) = registry.add(&body, &reserved, &mut |_| None);
        assert_eq!(code, 200, "{view}");
        assert_eq!(view["bindings"][0]["trigger"], "Ctrl + Alt + Shift + F3");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_au05_button_reserved_by_another_mode_is_rejected() {
        let (mut registry, dir) = registry();
        let mut reserved = Reserved::default();
        reserved.app_buttons.push(2);
        let body = add_body(
            serde_json::json!({"kind":"au05","button":2}),
            serde_json::json!([{"type":"wait","ms":1}]),
        );
        assert_eq!(registry.add(&body, &reserved, &mut |_| None).0, 409);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn launch_steps_are_resolved_server_side_and_redacted_in_the_view() {
        let (mut registry, dir) = registry();
        let body = add_body(
            serde_json::json!({"kind":"gamepad","device_key":"pad","usage":1,"label":"Button 1"}),
            serde_json::json!([{"type":"launch","application_reference":"a".repeat(64),"args":[]}]),
        );
        let mut resolve =
            |_reference: &str| Some((r"C:\Fixture\Codex.exe".to_string(), "Codex".to_string()));
        let (code, view) = registry.add(&body, &Reserved::default(), &mut resolve);
        assert_eq!(code, 200, "{view}");
        assert_eq!(view["bindings"][0]["steps"][0]["label"], "Codex");
        assert!(view["bindings"][0]["steps"][0].get("executable").is_none());
        assert!(!view.to_string().contains("Codex.exe"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn commands_file() -> (super::CommandRegistry, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "vibdock-commands-test-{}",
            crate::service::generate_token().unwrap()
        ));
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("commands.json");
        let bytes = serde_json::json!({
            "schema_version":1,
            "revision":0,
            "commands":[{
                "id":"build",
                "label":"构建项目",
                "executable":"C:\\Fixture\\build.exe",
                "args":["--project","{project}"],
                "parameters":[{"name":"project","label":"项目","required":true,"max_length":40}]
            }]
        })
        .to_string();
        std::fs::write(&path, bytes).unwrap();
        (super::CommandRegistry::load(path.clone()), dir)
    }

    #[test]
    fn commands_expand_parameters_as_separate_arguments() {
        let (commands, dir) = commands_file();
        let mut parameters = std::collections::BTreeMap::new();
        parameters.insert("project".to_string(), "vib dock; rm -rf".to_string());
        let (executable, args) = commands.expand("build", &parameters).unwrap();
        assert!(executable.ends_with("build.exe"));
        assert_eq!(args, vec!["--project", "vib dock; rm -rf"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn commands_reject_missing_and_unknown_parameters() {
        let (commands, dir) = commands_file();
        assert!(commands
            .expand("build", &std::collections::BTreeMap::new())
            .is_err());
        let mut unknown = std::collections::BTreeMap::new();
        unknown.insert("project".to_string(), "x".to_string());
        unknown.insert("injected".to_string(), "y".to_string());
        assert!(commands.expand("build", &unknown).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn commands_reject_unknown_placeholders_and_nonlocal_paths() {
        let value = serde_json::json!({
            "id":"bad",
            "label":"bad",
            "executable":"C:\\Fixture\\bad.exe",
            "args":["{missing}"]
        });
        assert!(super::CommandDefinition::parse(&value).is_none());
        let value = serde_json::json!({
            "id":"bad",
            "label":"bad",
            "executable":"relative.exe",
            "args":[]
        });
        assert!(super::CommandDefinition::parse(&value).is_none());
    }
}
