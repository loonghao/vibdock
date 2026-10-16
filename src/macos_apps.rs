//! Bounded macOS application-presence observations and separately persisted pins.
//! A saved bundle location is correlation metadata, never permission to launch it.
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
    time::{Duration, Instant},
};

const STORAGE_NAME: &str = "apps-macos.json";
const MAX_STORAGE: usize = 32768;
const MAX_ENTRIES: usize = 8;
const MAX_APPLICATIONS: usize = 64;
const MAX_ICON: usize = 16384;
const FRESH: Duration = Duration::from_secs(6);
const SELECTION_TTL: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, PartialEq)]
struct Entry {
    id: String,
    label: String,
    bundle_path: String,
    bundle_id: Option<String>,
}
impl Entry {
    fn json(&self) -> Value {
        json!({"id":self.id,"label":self.label,"bundle_path":self.bundle_path,"bundle_id":self.bundle_id})
    }
    fn parse(value: &Value) -> Option<Self> {
        if !only_fields(value, &["id", "label", "bundle_path", "bundle_id"]) {
            return None;
        }
        let id = string(value, "id", 64)?;
        let bundle_path = string(value, "bundle_path", 2048)?;
        if !token(&id) || !bundle_location(&bundle_path) {
            return None;
        }
        Some(Self {
            id,
            label: string(value, "label", 80)?,
            bundle_path,
            bundle_id: optional_string(value, "bundle_id", 255)?,
        })
    }
}
fn only_fields(value: &Value, names: &[&str]) -> bool {
    value
        .as_object()
        .is_some_and(|v| v.keys().all(|key| names.contains(&key.as_str())))
}
fn plain(value: &str, max: usize) -> bool {
    !value.is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
fn string(value: &Value, key: &str, max: usize) -> Option<String> {
    let s = value.get(key)?.as_str()?;
    plain(s, max).then(|| s.to_owned())
}
fn optional_string(value: &Value, key: &str, max: usize) -> Option<Option<String>> {
    match value.get(key) {
        None | Some(Value::Null) => Some(None),
        Some(Value::String(s)) if plain(s, max) => Some(Some(s.clone())),
        _ => None,
    }
}
fn token(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|c| c.is_ascii_hexdigit())
}
// Canonical POSIX spelling can be checked without touching a missing/unmounted
// saved application. The native observer resolves symlinks before publishing.
fn absolute_location(path: &str) -> bool {
    path.starts_with('/')
        && plain(path, 2048)
        && !path.contains('\\')
        && !path[1..]
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
}
fn bundle_location(path: &str) -> bool {
    absolute_location(path) && path.ends_with(".app") && !path.ends_with("/.app")
}
fn executable_location(bundle: &str, executable: &str) -> bool {
    absolute_location(executable)
        && executable
            .strip_prefix(bundle)
            .and_then(|suffix| suffix.strip_prefix("/Contents/MacOS/"))
            .is_some_and(|name| !name.is_empty())
}
fn bounded_label(value: &str) -> String {
    value
        .char_indices()
        .take_while(|(index, c)| index + c.len_utf8() <= 80)
        .map(|(_, c)| c)
        .collect()
}
fn launch_identity(value: &str) -> bool {
    // The native observer revalidates the kernel process start seconds and
    // microseconds around every observation. Preserve both integers exactly;
    // PID reuse and subsecond restarts must never inherit an old selection.
    value.len() == 32
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        && u64::from_str_radix(&value[..16], 16)
            .ok()
            .is_some_and(|seconds| seconds > 0)
        && u64::from_str_radix(&value[16..], 16)
            .ok()
            .is_some_and(|microseconds| microseconds < 1_000_000)
}

#[derive(Clone)]
struct Application {
    bundle_path: String,
    bundle_id: Option<String>,
    display_name: String,
    process_id: u32,
    key: String,
    icon: Option<String>,
}
impl Application {
    fn parse(value: &Value) -> Option<Self> {
        if !only_fields(
            value,
            &[
                "bundle_path",
                "executable_path",
                "bundle_id",
                "process_id",
                "process_started",
                "display_name",
                "running",
                "icon_b64",
            ],
        ) {
            return None;
        }
        let bundle_path = string(value, "bundle_path", 2048)?;
        let executable = string(value, "executable_path", 2048)?;
        let started = string(value, "process_started", 32)?;
        let pid = value.get("process_id")?.as_u64()?;
        let display_name = string(value, "display_name", 160)?;
        if !bundle_location(&bundle_path)
            || !executable_location(&bundle_path, &executable)
            || !launch_identity(&started)
            || pid == 0
            || pid > i32::MAX as u64
            || !value.get("running")?.as_bool()?
            || display_name.contains(['/', '\\'])
        {
            return None;
        }
        let icon = match value.get("icon_b64")? {
            Value::Null => None,
            Value::String(s) if decode_icon(s).is_some() => Some(s.clone()),
            _ => return None,
        };
        Some(Self {
            key: json!([bundle_path, executable, pid, started]).to_string(),
            bundle_path,
            bundle_id: optional_string(value, "bundle_id", 255)?,
            display_name,
            process_id: pid as u32,
            icon,
        })
    }
    fn choice(&self, selection: &str) -> Value {
        json!({"selection_id":selection,"display_name":self.display_name,"state":"running","icon_b64":self.icon,"icon_mime":"image/png","actions_enabled":false})
    }
}
struct Selection {
    key: String,
    issued: Instant,
}

pub(crate) struct Registry {
    path: PathBuf,
    entries: Vec<Entry>,
    revision: u64,
    generation: u64,
    load_error: Option<String>,
    persisted: Option<Vec<u8>>,
    available: bool,
    complete: bool,
    skipped: u64,
    observation_error: Option<&'static str>,
    applications: Vec<Application>,
    checked: Option<Instant>,
    selections: BTreeMap<String, Selection>,
    current_choices: Vec<(String, Application)>,
    worker: Option<Worker>,
}
impl Registry {
    pub(crate) fn start(enabled: bool) -> Self {
        let mut state = match crate::service::state_dir() {
            Ok(dir) => Self::load(dir.join(STORAGE_NAME)),
            Err(_) => Self::empty(
                PathBuf::new(),
                Some("Application storage is unavailable; existing data was not changed.".into()),
            ),
        };
        #[cfg(target_os = "macos")]
        if enabled {
            state.worker = Worker::start().ok();
            if state.worker.is_none() {
                state.observation_error = Some("Application observation worker could not start");
            }
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = enabled;
            state.observation_error =
                Some("Native application observation is unavailable on this platform");
        }
        state
    }
    pub(crate) fn empty(path: PathBuf, error: Option<String>) -> Self {
        let path_valid = path.file_name().is_some_and(|name| name == STORAGE_NAME);
        Self {
            path,
            entries: vec![],
            revision: 0,
            generation: 0,
            load_error: error.or_else(|| {
                (!path_valid)
                    .then(|| "Application storage path is unavailable; edits are disabled.".into())
            }),
            persisted: None,
            available: false,
            complete: false,
            skipped: 0,
            observation_error: Some("Waiting for native application observation"),
            applications: vec![],
            checked: None,
            selections: BTreeMap::new(),
            current_choices: vec![],
            worker: None,
        }
    }
    fn read_storage(path: &PathBuf) -> std::io::Result<Option<Vec<u8>>> {
        use std::io::Read;
        match std::fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Ok(metadata) if metadata.is_file() => {}
            _ => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Application storage is not a regular file",
                ))
            }
        }
        let file = match std::fs::File::open(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            result => result?,
        };
        let mut bytes = Vec::new();
        file.take((MAX_STORAGE + 1) as u64)
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_STORAGE {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Application storage exceeds its limit",
            ));
        }
        Ok(Some(bytes))
    }
    fn load(path: PathBuf) -> Self {
        let mut state = Self::empty(path, None);
        // In particular, never open or migrate the Windows apps.json file.
        if state.load_error.is_some() {
            return state;
        }
        let parsed = match Self::read_storage(&state.path) {
            Ok(None) => return state,
            Ok(Some(bytes)) => {
                let result = serde_json::from_slice::<Value>(&bytes).ok().and_then(|v| {
                    if !only_fields(&v, &["schema_version", "platform", "revision", "entries"])
                        || v["schema_version"] != 1
                        || v["platform"] != "macos"
                    {
                        return None;
                    }
                    let revision = v["revision"].as_u64()?;
                    let raw = v["entries"].as_array()?;
                    if raw.len() > MAX_ENTRIES {
                        return None;
                    }
                    let mut entries = Vec::new();
                    for item in raw {
                        let entry = Entry::parse(item)?;
                        if entries.iter().any(|old: &Entry| {
                            old.id == entry.id || old.bundle_path == entry.bundle_path
                        }) {
                            return None;
                        }
                        entries.push(entry);
                    }
                    Some((entries, revision))
                });
                state.persisted = Some(bytes);
                result
            }
            Err(_) => None,
        };
        if let Some((entries, revision)) = parsed {
            state.entries = entries;
            state.revision = revision;
        } else {
            state.load_error = Some("apps-macos.json is unreadable, invalid or unsupported; original data is preserved and application edits are disabled.".into());
        }
        state
    }
    pub(crate) fn poll(&mut self) {
        let next = self
            .worker
            .as_ref()
            .and_then(|worker| worker.receiver.try_iter().last());
        if let Some((collected, value)) = next {
            self.observe(value, collected);
        }
    }
    fn observe(&mut self, value: Value, collected: Instant) {
        self.generation = self.generation.saturating_add(1);
        self.checked = Some(collected);
        self.available = value["available"].as_bool().unwrap_or(false);
        self.skipped = value["skipped"].as_u64().unwrap_or(0);
        let mut malformed = !only_fields(
            &value,
            &["available", "complete", "skipped", "error", "applications"],
        ) || value["available"].as_bool().is_none()
            || value["complete"].as_bool().is_none()
            || value["skipped"].as_u64().is_none()
            || match value.get("error") {
                Some(Value::Null) => false,
                Some(Value::String(error)) => !plain(error, 256),
                _ => true,
            };
        let raw = value["applications"].as_array();
        malformed |= raw.is_none_or(|apps| apps.len() > MAX_APPLICATIONS);
        let mut applications = Vec::new();
        let mut seen = BTreeSet::new();
        let mut conflicting = BTreeSet::new();
        for item in raw.into_iter().flatten().take(MAX_APPLICATIONS) {
            match Application::parse(item) {
                Some(app) => {
                    if !seen.insert(app.process_id) {
                        malformed = true;
                        conflicting.insert(app.process_id);
                    } else {
                        applications.push(app);
                    }
                }
                None => {
                    malformed = true;
                    self.skipped = self.skipped.saturating_add(1);
                }
            }
        }
        applications.retain(|app| !conflicting.contains(&app.process_id));
        self.complete = self.available
            && value["complete"] == true
            && self.skipped == 0
            && !malformed
            && value["error"].is_null();
        self.observation_error = if malformed {
            Some("Application observation contained invalid or incomplete data")
        } else if !value["error"].is_null() || !self.available {
            // Observer diagnostics are deliberately not echoed: they can embed
            // private bundle locations, executable paths or OS error details.
            Some("Native application observation is incomplete or unavailable")
        } else if !self.complete {
            Some("Some applications could not be observed completely")
        } else {
            None
        };
        self.applications = if self.available { applications } else { vec![] };
        self.current_choices.clear();
        self.selections.retain(|_, selection| {
            selection.issued.elapsed() < SELECTION_TTL
                && self.applications.iter().any(|app| {
                    app.key == selection.key
                        && !self
                            .entries
                            .iter()
                            .any(|entry| entry.bundle_path == app.bundle_path)
                })
        });
        if !self.fresh() {
            self.selections.clear();
            return;
        }
        for app in &self.applications {
            if self
                .entries
                .iter()
                .any(|entry| entry.bundle_path == app.bundle_path)
            {
                continue;
            }
            let existing = self
                .selections
                .iter()
                .find(|(_, selection)| selection.key == app.key)
                .map(|(id, _)| id.clone());
            if let Some(id) = existing.or_else(|| crate::service::generate_token().ok()) {
                self.selections
                    .entry(id.clone())
                    .or_insert_with(|| Selection {
                        key: app.key.clone(),
                        issued: Instant::now(),
                    });
                self.current_choices.push((id, app.clone()));
            }
        }
    }
    fn fresh(&self) -> bool {
        self.available
            && self
                .checked
                .is_some_and(|checked| checked.elapsed() < FRESH)
    }
    pub(crate) fn view(&self, summary: bool) -> Value {
        let fresh = self.fresh();
        let complete = fresh && self.complete;
        let entries = self.entries.iter().map(|entry| {
            let current = fresh.then(|| self.applications.iter().find(|app| app.bundle_path == entry.bundle_path)).flatten();
            let state = if current.is_some() { "running" } else if complete { "not_running" } else { "unknown" };
            let playback = if current.is_some() { "Application running; playback/voice state unavailable" } else if complete { "Application not running; playback/voice state unavailable" } else { "Application presence unknown; playback/voice state unavailable" };
            json!({"id":entry.id,"label":entry.label,"state":state,"complete":complete,"playback_label":playback,"icon_b64":current.and_then(|app| app.icon.as_deref()),"icon_mime":"image/png","sessions":[],"actions_enabled":false,"identity_kind":"saved_application_location"})
        }).collect::<Vec<_>>();
        let choices = if fresh && !summary {
            self.current_choices
                .iter()
                .filter(|(id, app)| {
                    self.selections
                        .get(id)
                        .is_some_and(|selection| selection.issued.elapsed() < SELECTION_TTL)
                        && !self
                            .entries
                            .iter()
                            .any(|entry| entry.bundle_path == app.bundle_path)
                })
                .map(|(id, app)| app.choice(id))
                .collect::<Vec<_>>()
        } else {
            vec![]
        };
        json!({"platform":"macos","observation_kind":"application_presence","schema_version":1,"revision":self.revision,"generation":self.generation,"age_ms":self.checked.map(|checked| checked.elapsed().as_millis().min(u64::MAX as u128) as u64),"available":fresh,"complete":complete,"skipped":self.skipped,"error":self.load_error.as_deref().or(self.observation_error),"storage_writable":self.load_error.is_none(),"entries":entries,"choices":choices,"actions_enabled":false})
    }
    pub(crate) fn mutate(&mut self, operation: &str, body: &str) -> (u16, Value) {
        self.mutate_with_writer(operation, body, |path, bytes| {
            crate::service::atomic_write(path, bytes).is_ok()
        })
    }
    fn mutate_with_writer(
        &mut self,
        operation: &str,
        body: &str,
        write: impl FnOnce(&PathBuf, &[u8]) -> bool,
    ) -> (u16, Value) {
        let fail = |status, message: &str| (status, json!({"error":message}));
        if self.load_error.is_some() {
            return fail(
                503,
                "Application storage is unavailable; original data is preserved",
            );
        }
        let allowed = match operation {
            "pin" => &["revision", "selection_id", "label"][..],
            "remove" => &["revision", "id"][..],
            _ => return fail(404, "Unknown application operation"),
        };
        if body.len() > 4096 {
            return fail(400, "Application edit exceeds its size limit");
        }
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            return fail(400, "Expected a JSON object");
        };
        if !only_fields(&value, allowed) {
            return fail(
                400,
                "Unexpected application field; paths and icon URLs are not accepted",
            );
        }
        if value["revision"].as_u64() != Some(self.revision) {
            return fail(409, "Application entries changed; refresh before editing");
        }
        let mut entries = self.entries.clone();
        let mut used = None;
        if operation == "pin" {
            if !self.fresh() {
                return fail(
                    409,
                    "Application observation expired; refresh and select again",
                );
            }
            if entries.len() >= MAX_ENTRIES {
                return fail(409, "At most eight software entries can be pinned");
            }
            let Some(id) = value["selection_id"].as_str().filter(|id| token(id)) else {
                return fail(400, "A fresh selection_id is required");
            };
            let Some(selection) = self
                .selections
                .get(id)
                .filter(|selection| selection.issued.elapsed() < SELECTION_TTL)
            else {
                return fail(409, "Selection expired or already used");
            };
            let Some(current) = self
                .applications
                .iter()
                .find(|app| app.key == selection.key)
            else {
                return fail(409, "Selected application changed; select again");
            };
            if entries
                .iter()
                .any(|entry| entry.bundle_path == current.bundle_path)
            {
                return fail(409, "This application location is already pinned");
            }
            let label = match value.get("label") {
                None => bounded_label(&current.display_name),
                Some(_) => match string(&value, "label", 80) {
                    Some(label) => label,
                    None => return fail(400, "Label must be plain text of at most 80 bytes"),
                },
            };
            let Ok(entry_id) = crate::service::generate_token() else {
                return fail(503, "Secure entry identifiers are unavailable");
            };
            entries.push(Entry {
                id: entry_id,
                label,
                bundle_path: current.bundle_path.clone(),
                bundle_id: current.bundle_id.clone(),
            });
            used = Some(id.to_owned());
        } else {
            let Some(id) = value["id"].as_str().filter(|id| token(id)) else {
                return fail(400, "Entry id is required");
            };
            let before = entries.len();
            entries.retain(|entry| entry.id != id);
            if before == entries.len() {
                return fail(404, "Entry not found");
            }
        }
        let Some(revision) = self.revision.checked_add(1) else {
            return fail(409, "Revision limit reached");
        };
        if Self::read_storage(&self.path).ok().as_ref() != Some(&self.persisted) {
            self.load_error = Some("Application storage changed or became unreadable; original data is preserved and edits are disabled until restart.".into());
            return fail(
                503,
                "Application storage changed; original data is preserved",
            );
        }
        let stored = json!({"schema_version":1,"platform":"macos","revision":revision,"entries":entries.iter().map(Entry::json).collect::<Vec<_>>()}).to_string().into_bytes();
        // Escaping valid quotes in paths, labels and bundle IDs can make the
        // serialized document larger than the sum of the validated raw fields.
        // Never persist a file that our bounded loader could not reopen.
        if stored.len() > MAX_STORAGE {
            return fail(
                409,
                "Pinned application data exceeds its storage limit; previous state is unchanged",
            );
        }
        if !write(&self.path, &stored) {
            return fail(
                500,
                "Could not save application entries; previous state is unchanged",
            );
        }
        self.persisted = Some(stored);
        self.entries = entries;
        self.revision = revision;
        if let Some(id) = used {
            self.selections.remove(&id);
        }
        self.current_choices.retain(|(_, app)| {
            !self
                .entries
                .iter()
                .any(|entry| entry.bundle_path == app.bundle_path)
        });
        self.selections.retain(|id, selection| {
            self.current_choices
                .iter()
                .any(|(current, app)| current == id && app.key == selection.key)
        });
        (200, self.view(false))
    }
}

struct Worker {
    receiver: std::sync::mpsc::Receiver<(Instant, Value)>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let deadline = Instant::now() + Duration::from_millis(750);
            while !thread.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if thread.is_finished() {
                let _ = thread.join();
            }
            // Never block service exit indefinitely. A detached owner observes
            // stop and cannot restart a helper; the helper also has a watchdog.
        }
    }
}
#[cfg(target_os = "macos")]
impl Worker {
    fn start() -> std::io::Result<Self> {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            mpsc, Arc,
        };
        let executable = std::env::current_exe()?;
        let (sender, receiver) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("macos-app-observer-owner".into())
            .spawn(move || {
                while !flag.load(Ordering::Relaxed) {
                    let collected = Instant::now();
                    let (value, cleanup_confirmed) =
                        crate::macos_worker::collect(&executable, &flag);
                    if !cleanup_confirmed {
                        // Retain the terminal diagnostic instead of leaving an older
                        // success queued. Receiver drop releases this bounded send.
                        let _ = sender.send((collected, value));
                        break;
                    }
                    if sender.try_send((collected, value)).is_err() && flag.load(Ordering::Relaxed)
                    {
                        break;
                    }
                    for _ in 0..20 {
                        if flag.load(Ordering::Relaxed) {
                            return;
                        }
                        std::thread::sleep(Duration::from_millis(100));
                    }
                }
            })?;
        Ok(Self {
            receiver,
            stop,
            thread: Some(thread),
        })
    }
}

/// Internal helper entry: no user-controlled paths or commands are accepted.
pub fn print_snapshot() {
    #[cfg(target_os = "macos")]
    crate::macos_worker::print_snapshot();
    #[cfg(not(target_os = "macos"))]
    println!(
        "{}",
        json!({"available":false,"complete":false,"skipped":0,"error":"Native application observation is unavailable on this platform","applications":[]})
    );
}

/// Decode only bounded, CRC-checked native 32×32 RGB/RGBA PNGs. Never accept a
/// URL, path, SVG, profile, arbitrary metadata, or unbounded compressed payload.
pub(crate) fn decode_icon(value: &str) -> Option<Vec<u8>> {
    if value.is_empty() || value.len() > 21848 || !value.len().is_multiple_of(4) {
        return None;
    }
    let mut bytes = Vec::with_capacity(value.len() / 4 * 3);
    for (index, chunk) in value.as_bytes().as_chunks::<4>().0.iter().enumerate() {
        let mut word = 0u32;
        let mut padding = 0;
        for (position, c) in chunk.iter().enumerate() {
            let digit = match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                b'=' if position >= 2 && index + 1 == value.len() / 4 => {
                    padding += 1;
                    0
                }
                _ => return None,
            };
            if padding > 0 && *c != b'=' {
                return None;
            }
            word = (word << 6) | u32::from(digit);
        }
        if (padding == 2 && word & 0xffff != 0) || (padding == 1 && word & 0xff != 0) {
            return None;
        }
        bytes.push((word >> 16) as u8);
        if padding < 2 {
            bytes.push((word >> 8) as u8);
        }
        if padding == 0 {
            bytes.push(word as u8);
        }
    }
    valid_png(&bytes).then_some(bytes)
}
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}
fn valid_png(bytes: &[u8]) -> bool {
    fn validate(bytes: &[u8]) -> Option<()> {
        if bytes.len() > MAX_ICON || bytes.get(..8)? != b"\x89PNG\r\n\x1a\n" {
            return None;
        }
        let mut offset = 8usize;
        let mut color = None;
        let mut compressed = Vec::new();
        let mut seen = BTreeSet::new();
        while offset < bytes.len() {
            let size = u32::from_be_bytes(bytes.get(offset..offset + 4)?.try_into().ok()?) as usize;
            let end = offset.checked_add(size)?.checked_add(12)?;
            let chunk = bytes.get(offset + 4..end - 4)?;
            let kind = chunk.get(..4)?;
            let data = &chunk[4..];
            let crc = u32::from_be_bytes(bytes.get(end - 4..end)?.try_into().ok()?);
            if crc32(chunk) != crc || (offset == 8 && kind != b"IHDR") {
                return None;
            }
            if kind != b"IDAT" && !seen.insert(kind.to_vec()) {
                return None;
            }
            if kind != b"IDAT" && kind != b"IEND" && !compressed.is_empty() {
                return None;
            }
            match kind {
                b"IHDR" if offset == 8 && size == 13 => {
                    if data[..8] != [0, 0, 0, 32, 0, 0, 0, 32]
                        || data[8] != 8
                        || !matches!(data[9], 2 | 6)
                        || data[10..] != [0, 0, 0]
                    {
                        return None;
                    }
                    color = Some(data[9]);
                }
                b"IDAT" if color.is_some() => compressed.extend_from_slice(data),
                b"IEND" if size == 0 && end == bytes.len() && !compressed.is_empty() => {
                    let channels = if color? == 6 { 4 } else { 3 };
                    let stride = 32 * channels + 1;
                    let raw = inflate_bounded(&compressed, 32 * stride)?;
                    if raw.chunks_exact(stride).any(|row| row[0] > 4) {
                        return None;
                    }
                    return Some(());
                }
                b"gAMA" if size == 4 && u32::from_be_bytes(data.try_into().ok()?) > 0 => {}
                b"cHRM" if size == 32 => {}
                b"sRGB" if size == 1 && data[0] <= 3 => {}
                b"pHYs" if size == 9 && data[8] <= 1 => {}
                b"sBIT" if size == if color? == 6 { 4 } else { 3 } => {
                    if data.iter().any(|bits| !(1..=8).contains(bits)) {
                        return None;
                    }
                }
                b"bKGD"
                    if size == 6 && data.as_chunks::<2>().0.iter().all(|sample| sample[0] == 0) => {
                }
                b"tRNS"
                    if color == Some(2)
                        && size == 6
                        && data.as_chunks::<2>().0.iter().all(|sample| sample[0] == 0) => {}
                _ => return None,
            }
            offset = end;
        }
        None
    }
    validate(bytes).is_some()
}

// Accept only the native observer's uncompressed zlib/DEFLATE blocks. This
// intentionally small subset makes the maximum expansion explicit, without a
// general decompressor or an image decoder processing untrusted metadata.
fn inflate_bounded(bytes: &[u8], expected: usize) -> Option<Vec<u8>> {
    if bytes.len() < 11
        || bytes[0] & 15 != 8
        || bytes[0] >> 4 > 7
        || bytes[1] & 32 != 0
        || (u16::from(bytes[0]) * 256 + u16::from(bytes[1])) % 31 != 0
    {
        return None;
    }
    let mut output = Vec::with_capacity(expected);
    let mut offset = 2usize;
    loop {
        // Stored blocks start on a byte boundary; require canonical zero pad
        // bits as produced by the native encoder. Reject Huffman compression.
        let final_block = match *bytes.get(offset)? {
            0 => false,
            1 => true,
            _ => return None,
        };
        let size = u16::from_le_bytes(bytes.get(offset + 1..offset + 3)?.try_into().ok()?);
        let inverse = u16::from_le_bytes(bytes.get(offset + 3..offset + 5)?.try_into().ok()?);
        if size ^ inverse != u16::MAX || output.len().checked_add(usize::from(size))? > expected {
            return None;
        }
        offset += 5;
        let end = offset.checked_add(usize::from(size))?;
        if end > bytes.len().checked_sub(4)? {
            return None;
        }
        output.extend_from_slice(bytes.get(offset..end)?);
        offset = end;
        if final_block {
            break;
        }
    }
    if output.len() != expected || offset.checked_add(4)? != bytes.len() {
        return None;
    }
    let mut a = 1u32;
    let mut b = 0u32;
    for byte in &output {
        a = (a + u32::from(*byte)) % 65521;
        b = (b + a) % 65521;
    }
    let checksum = u32::from_be_bytes(bytes[offset..].try_into().ok()?);
    (checksum == ((b << 16) | a)).then_some(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Fixture {
        dir: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "vibdock-macos-apps-{}",
                crate::service::generate_token().unwrap()
            ));
            std::fs::create_dir(&dir).unwrap();
            Self { dir }
        }
        fn path(&self) -> PathBuf {
            self.dir.join(STORAGE_NAME)
        }
        fn registry(&self) -> Registry {
            Registry::load(self.path())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }
    fn kernel_start(seconds: u64, microseconds: u64) -> String {
        format!("{seconds:016x}{microseconds:016x}")
    }
    fn snapshot() -> Value {
        json!({"available":true,"complete":true,"skipped":0,"error":null,"applications":[{
            "bundle_path":"/Applications/Fixture.app","executable_path":"/Applications/Fixture.app/Contents/MacOS/Fixture",
            "bundle_id":"org.vibdock.fixture","process_id":123,"process_started":kernel_start(1_700_000_000, 123_456),
            "display_name":"Fixture","running":true,"icon_b64":null
        }]})
    }
    fn select(registry: &mut Registry) -> String {
        registry.observe(snapshot(), Instant::now());
        registry.view(false)["choices"][0]["selection_id"]
            .as_str()
            .unwrap()
            .to_owned()
    }
    fn pin(registry: &mut Registry, selection: &str) -> (u16, Value) {
        registry.mutate(
            "pin",
            &json!({"revision":registry.revision,"selection_id":selection}).to_string(),
        )
    }
    fn pinned(fixture: &Fixture) -> Registry {
        let mut registry = fixture.registry();
        let selection = select(&mut registry);
        assert_eq!(pin(&mut registry, &selection).0, 200);
        registry
    }
    fn b64(bytes: &[u8]) -> String {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut result = String::new();
        for chunk in bytes.chunks(3) {
            let word = (u32::from(chunk[0]) << 16)
                | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
                | u32::from(*chunk.get(2).unwrap_or(&0));
            result.push(ALPHABET[(word >> 18) as usize] as char);
            result.push(ALPHABET[((word >> 12) & 63) as usize] as char);
            result.push(if chunk.len() > 1 {
                ALPHABET[((word >> 6) & 63) as usize] as char
            } else {
                '='
            });
            result.push(if chunk.len() > 2 {
                ALPHABET[(word & 63) as usize] as char
            } else {
                '='
            });
        }
        result
    }
    fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
        let mut output = (data.len() as u32).to_be_bytes().to_vec();
        output.extend_from_slice(kind);
        output.extend_from_slice(data);
        let checksum = crc32(&output[4..]);
        output.extend_from_slice(&checksum.to_be_bytes());
        output
    }
    fn adler(bytes: &[u8]) -> u32 {
        let mut a = 1u32;
        let mut b = 0u32;
        for byte in bytes {
            a = (a + u32::from(*byte)) % 65521;
            b = (b + a) % 65521;
        }
        (b << 16) | a
    }
    fn zlib_stored(raw: &[u8]) -> Vec<u8> {
        let mut output = vec![0x78, 0x01, 0x01];
        let size = u16::try_from(raw.len()).unwrap();
        output.extend_from_slice(&size.to_le_bytes());
        output.extend_from_slice(&(!size).to_le_bytes());
        output.extend_from_slice(raw);
        output.extend_from_slice(&adler(raw).to_be_bytes());
        output
    }
    fn png_with_data(color: u8, compressed: &[u8], metadata: Option<(&[u8; 4], &[u8])>) -> Vec<u8> {
        let mut output = b"\x89PNG\r\n\x1a\n".to_vec();
        output.extend(chunk(
            b"IHDR",
            &[0, 0, 0, 32, 0, 0, 0, 32, 8, color, 0, 0, 0],
        ));
        if let Some((kind, value)) = metadata {
            output.extend(chunk(kind, value));
        }
        output.extend(chunk(b"IDAT", compressed));
        output.extend(chunk(b"IEND", &[]));
        output
    }
    fn png(color: u8) -> Vec<u8> {
        png_with_data(
            color,
            &zlib_stored(&vec![0; 32 * (32 * if color == 6 { 4 } else { 3 } + 1)]),
            None,
        )
    }

    #[test]
    fn pins_restart_and_remove_without_touching_windows_or_microphone_storage() {
        let fixture = Fixture::new();
        let windows = fixture.dir.join("apps.json");
        let microphone = fixture.dir.join("config.json");
        std::fs::write(&windows, b"Windows fixture, deliberately not JSON").unwrap();
        std::fs::write(&microphone, b"microphone fixture").unwrap();
        let registry = pinned(&fixture);
        let persisted: Value =
            serde_json::from_slice(&std::fs::read(fixture.path()).unwrap()).unwrap();
        assert_eq!(persisted["platform"], "macos");
        assert_eq!(persisted["schema_version"], 1);
        assert_eq!(persisted["revision"], 1);
        for key in [
            "process_id",
            "process_started",
            "executable_path",
            "icon_b64",
        ] {
            assert!(!persisted.to_string().contains(key));
        }
        let mut reopened = fixture.registry();
        assert_eq!(reopened.entries, registry.entries);
        assert_eq!(reopened.view(false)["entries"][0]["state"], "unknown");
        assert_eq!(reopened.view(false)["entries"][0]["icon_b64"], Value::Null);
        let entry = reopened.entries[0].id.clone();
        assert_eq!(
            reopened
                .mutate("remove", &json!({"revision":0,"id":entry}).to_string())
                .0,
            409
        );
        assert_eq!(
            reopened
                .mutate("remove", &json!({"revision":1,"id":entry}).to_string())
                .0,
            200
        );
        assert!(fixture.registry().entries.is_empty());
        assert_eq!(
            std::fs::read(windows).unwrap(),
            b"Windows fixture, deliberately not JSON"
        );
        assert_eq!(std::fs::read(microphone).unwrap(), b"microphone fixture");
    }
    #[test]
    fn windows_filename_is_never_opened_or_written_even_if_explicitly_passed() {
        let fixture = Fixture::new();
        let windows = fixture.dir.join("apps.json");
        std::fs::write(&windows, b"preserve Windows bytes").unwrap();
        let mut registry = Registry::load(windows.clone());
        assert!(registry.load_error.is_some());
        assert_eq!(registry.mutate("remove", "{}").0, 503);
        assert_eq!(std::fs::read(windows).unwrap(), b"preserve Windows bytes");
    }
    #[cfg(unix)]
    #[test]
    fn storage_symlinks_cannot_read_or_overwrite_windows_state() {
        let fixture = Fixture::new();
        let windows = fixture.dir.join("apps.json");
        std::fs::write(&windows, b"Windows-only bytes").unwrap();
        std::os::unix::fs::symlink(&windows, fixture.path()).unwrap();
        let mut registry = fixture.registry();
        assert!(registry.persisted.is_none());
        assert!(registry.load_error.is_some());
        assert_eq!(registry.mutate("pin", "{}").0, 503);
        assert_eq!(std::fs::read(windows).unwrap(), b"Windows-only bytes");
        assert!(std::fs::symlink_metadata(fixture.path())
            .unwrap()
            .file_type()
            .is_symlink());
    }
    #[test]
    fn corrupt_oversize_future_schema_and_wrong_platform_are_preserved() {
        let fixture = Fixture::new();
        for bytes in [
            b"not JSON".to_vec(),
            vec![b' '; MAX_STORAGE + 1],
            br#"{"platform":"macos","schema_version":2,"revision":0,"entries":[]}"#.to_vec(),
            br#"{"platform":"windows","schema_version":1,"revision":0,"entries":[]}"#.to_vec(),
            br#"{"schema_version":1,"revision":0,"entries":[]}"#.to_vec(),
        ] {
            std::fs::write(fixture.path(), &bytes).unwrap();
            let mut registry = fixture.registry();
            assert_eq!(registry.view(false)["storage_writable"], false);
            assert_eq!(registry.mutate("remove", "{}").0, 503);
            assert_eq!(std::fs::read(fixture.path()).unwrap(), bytes);
        }
    }
    #[test]
    fn failed_atomic_write_keeps_revision_entries_selection_and_disk() {
        let fixture = Fixture::new();
        let mut registry = fixture.registry();
        let selection = select(&mut registry);
        let body = json!({"revision":0,"selection_id":selection}).to_string();
        assert_eq!(
            registry.mutate_with_writer("pin", &body, |_, _| false).0,
            500
        );
        assert_eq!(registry.revision, 0);
        assert!(registry.entries.is_empty());
        assert!(registry.selections.contains_key(&selection));
        assert!(!fixture.path().exists());
        assert_eq!(pin(&mut registry, &selection).0, 200);
        let old = std::fs::read(fixture.path()).unwrap();
        let body = json!({"revision":1,"id":registry.entries[0].id}).to_string();
        assert_eq!(
            registry.mutate_with_writer("remove", &body, |_, _| false).0,
            500
        );
        assert_eq!(registry.revision, 1);
        assert_eq!(registry.entries.len(), 1);
        assert_eq!(std::fs::read(fixture.path()).unwrap(), old);
    }
    #[test]
    fn external_storage_changes_are_preserved_before_any_write() {
        let fixture = Fixture::new();
        let mut registry = pinned(&fixture);
        std::fs::write(fixture.path(), b"new external bytes").unwrap();
        let body = json!({"revision":1,"id":registry.entries[0].id}).to_string();
        assert_eq!(registry.mutate("remove", &body).0, 503);
        assert_eq!(registry.revision, 1);
        assert_eq!(registry.entries.len(), 1);
        assert_eq!(
            std::fs::read(fixture.path()).unwrap(),
            b"new external bytes"
        );
    }
    #[test]
    fn forged_expired_replayed_and_revision_conflicting_selections_fail_closed() {
        let fixture = Fixture::new();
        let mut registry = fixture.registry();
        let selection = select(&mut registry);
        assert_eq!(pin(&mut registry, &"0".repeat(64)).0, 409);
        assert_eq!(
            registry
                .mutate(
                    "pin",
                    &json!({"revision":9,"selection_id":selection}).to_string()
                )
                .0,
            409
        );
        registry.selections.get_mut(&selection).unwrap().issued = Instant::now() - SELECTION_TTL;
        assert_eq!(pin(&mut registry, &selection).0, 409);
        assert_eq!(registry.view(false)["choices"], json!([]));
        let fresh = select(&mut registry);
        assert_ne!(fresh, selection);
        assert_eq!(pin(&mut registry, &fresh).0, 200);
        assert_eq!(pin(&mut registry, &fresh).0, 409);
        registry.observe(snapshot(), Instant::now());
        assert_eq!(registry.view(false)["choices"], json!([]));
    }
    #[test]
    fn selection_is_bound_to_bundle_executable_pid_and_exact_kernel_start() {
        for (field, replacement) in [
            ("process_id", json!(124)),
            (
                "process_started",
                json!(kernel_start(1_700_000_000, 123_457)),
            ),
            (
                "process_started",
                json!(kernel_start(1_700_000_001, 123_456)),
            ),
            (
                "executable_path",
                json!("/Applications/Fixture.app/Contents/MacOS/Replaced"),
            ),
            ("bundle_path", json!("/Applications/Replaced.app")),
        ] {
            let fixture = Fixture::new();
            let mut registry = fixture.registry();
            let selection = select(&mut registry);
            let mut changed = snapshot();
            changed["applications"][0][field] = replacement;
            if field == "bundle_path" {
                changed["applications"][0]["executable_path"] =
                    json!("/Applications/Replaced.app/Contents/MacOS/Fixture");
            }
            registry.observe(changed, Instant::now());
            assert_eq!(pin(&mut registry, &selection).0, 409, "{field}");
            assert!(registry.entries.is_empty());
        }
    }
    #[test]
    fn absence_invalidates_a_selection_even_when_same_identity_returns() {
        let fixture = Fixture::new();
        let mut registry = fixture.registry();
        let old = select(&mut registry);
        let mut empty = snapshot();
        empty["applications"] = json!([]);
        registry.observe(empty, Instant::now());
        let new = select(&mut registry);
        assert_ne!(old, new);
        assert_eq!(pin(&mut registry, &old).0, 409);
        assert_eq!(pin(&mut registry, &new).0, 200);
    }
    #[test]
    fn revalidation_rejects_changed_current_identity_and_stale_snapshot() {
        let fixture = Fixture::new();
        let mut registry = fixture.registry();
        let selection = select(&mut registry);
        registry.applications[0].key.push('x');
        assert_eq!(pin(&mut registry, &selection).0, 409);
        let selection = select(&mut registry);
        registry.checked = Some(Instant::now() - FRESH);
        assert_eq!(pin(&mut registry, &selection).0, 409);
        assert_eq!(registry.view(false)["choices"], json!([]));
    }
    #[test]
    fn partial_absence_is_unknown_and_only_fresh_complete_absence_is_not_running() {
        let fixture = Fixture::new();
        let mut registry = pinned(&fixture);
        let mut empty = snapshot();
        empty["applications"] = json!([]);
        empty["complete"] = json!(false);
        registry.observe(empty.clone(), Instant::now());
        assert_eq!(registry.view(false)["entries"][0]["state"], "unknown");
        empty["complete"] = json!(true);
        empty["skipped"] = json!(1);
        registry.observe(empty.clone(), Instant::now());
        assert_eq!(registry.view(false)["entries"][0]["state"], "unknown");
        empty["skipped"] = json!(0);
        registry.observe(empty.clone(), Instant::now());
        assert_eq!(registry.view(false)["entries"][0]["state"], "not_running");
        registry.observe(empty, Instant::now() - FRESH);
        assert_eq!(registry.view(false)["entries"][0]["state"], "unknown");
        let mut partial = snapshot();
        partial["complete"] = json!(false);
        registry.observe(partial, Instant::now());
        assert_eq!(registry.view(false)["entries"][0]["state"], "running");
        assert_eq!(registry.view(false)["entries"][0]["complete"], false);
    }
    #[test]
    fn unavailable_and_stale_observations_clear_all_live_icons() {
        let fixture = Fixture::new();
        let mut registry = pinned(&fixture);
        let mut observed = snapshot();
        observed["applications"][0]["icon_b64"] = json!(b64(&png(6)));
        registry.observe(observed.clone(), Instant::now());
        assert!(registry.view(false)["entries"][0]["icon_b64"].is_string());
        registry.checked = Some(Instant::now() - FRESH);
        assert!(registry.view(false)["entries"][0]["icon_b64"].is_null());
        observed["available"] = json!(false);
        registry.observe(observed, Instant::now());
        assert!(registry.view(false)["entries"][0]["icon_b64"].is_null());
        assert_eq!(registry.view(false)["entries"][0]["state"], "unknown");
    }
    #[test]
    fn public_views_redact_locations_launch_identity_and_untrusted_diagnostics() {
        let fixture = Fixture::new();
        let mut registry = fixture.registry();
        select(&mut registry);
        for view in [registry.view(false), registry.view(true)] {
            let text = view.to_string();
            for private in [
                "/Applications/",
                "executable_path",
                "process_id",
                "process_started",
                "bundle_id",
                "bundle_path",
            ] {
                assert!(!text.contains(private), "{private}");
            }
            assert_eq!(view["platform"], "macos");
            assert_eq!(view["observation_kind"], "application_presence");
            assert_eq!(view["actions_enabled"], false);
        }
        assert_eq!(registry.view(true)["choices"], json!([]));
        let choice = registry.current_choices[0].0.clone();
        assert_eq!(pin(&mut registry, &choice).0, 200);
        let mut error = snapshot();
        error["error"] = json!("Cannot read /Users/private/Fixture.app/Contents/MacOS/Fixture");
        registry.observe(error, Instant::now());
        let view = registry.view(true);
        assert_eq!(view["entries"].as_array().unwrap().len(), 1);
        assert_eq!(
            view["entries"][0]["identity_kind"],
            "saved_application_location"
        );
        assert_eq!(view["entries"][0]["sessions"], json!([]));
        assert!(view["entries"][0]["playback_label"]
            .as_str()
            .unwrap()
            .contains("playback/voice state unavailable"));
        assert!(!view.to_string().contains("/Users/"));
        assert!(!view.to_string().contains("/Applications/"));
    }
    #[test]
    fn edits_accept_only_the_closed_selection_and_entry_contract() {
        let fixture = Fixture::new();
        let mut registry = fixture.registry();
        let selection = select(&mut registry);
        for field in [
            "bundle_path",
            "bundle_id",
            "executable_path",
            "process_id",
            "process_started",
            "icon_b64",
            "icon_url",
            "command",
            "url",
            "platform",
            "id",
        ] {
            let mut body = json!({"revision":0,"selection_id":selection});
            body[field] = json!("https://example.invalid/arbitrary");
            assert_eq!(registry.mutate("pin", &body.to_string()).0, 400, "{field}");
        }
        for label in [
            "".to_owned(),
            "x".repeat(81),
            "line\nbreak".to_owned(),
            "é".repeat(41),
        ] {
            assert_eq!(
                registry
                    .mutate(
                        "pin",
                        &json!({"revision":0,"selection_id":selection,"label":label}).to_string()
                    )
                    .0,
                400
            );
        }
        assert_eq!(registry.mutate("pin", "[]").0, 400);
        assert_eq!(registry.mutate("pin", "not JSON").0, 400);
        assert_eq!(registry.mutate("launch", "{}").0, 404);
        assert_eq!(
            registry
                .mutate(
                    "pin",
                    &json!({"revision":0,"selection_id":selection,"label":"é".repeat(40)})
                        .to_string()
                )
                .0,
            200
        );
        let entry = registry.entries[0].id.clone();
        assert_eq!(
            registry
                .mutate(
                    "remove",
                    &json!({"revision":1,"id":entry,"label":"changed"}).to_string()
                )
                .0,
            400
        );
    }
    #[test]
    fn kernel_launch_identity_requires_exact_lowercase_seconds_and_microseconds() {
        assert!(launch_identity(&kernel_start(1, 0)));
        assert!(launch_identity(&kernel_start(1_700_000_000, 999_999)));
        assert!(launch_identity(&kernel_start(1_700_000_000, 123_456)));
        for invalid in [
            kernel_start(0, 0),
            kernel_start(0, 999_999),
            kernel_start(1, 1_000_000),
            kernel_start(1, u64::MAX),
            format!("{:016x}", 1_700_000_000.123_456_f64.to_bits()),
            format!("0x{}", kernel_start(1_700_000_000, 123_456)),
            format!("{} ", kernel_start(1_700_000_000, 123_456)),
            kernel_start(1_700_000_000, 123_456).to_uppercase(),
            "g".repeat(32),
            "0".repeat(31),
            "0".repeat(33),
            "é".repeat(16),
        ] {
            assert!(!launch_identity(&invalid), "{invalid}");
        }
    }
    #[test]
    fn all_invalid_identity_fields_force_partial_observation() {
        let fixture = Fixture::new();
        let mut registry = fixture.registry();
        for (field, bad) in [
            ("bundle_path", json!("relative.app")),
            ("bundle_path", json!("/Applications/../Fixture.app")),
            ("bundle_path", json!("/Applications//Fixture.app")),
            ("bundle_path", json!("/Applications/Fixture.app/")),
            (
                "executable_path",
                json!("/Applications/Fixture.app.evil/Contents/MacOS/Fixture"),
            ),
            (
                "executable_path",
                json!("/Applications/Fixture.app/Contents/Resources/Fixture"),
            ),
            (
                "executable_path",
                json!("/Applications/Fixture.app/Contents/MacOS/../Fixture"),
            ),
            ("process_id", json!(0)),
            ("process_id", json!(-1)),
            ("process_id", json!(u32::MAX)),
            ("process_started", json!("1700000000")),
            (
                "process_started",
                json!(format!("{:016x}", 1_700_000_000.123_456_f64.to_bits())),
            ),
            ("process_started", json!(kernel_start(0, 123_456))),
            (
                "process_started",
                json!(kernel_start(1_700_000_000, 1_000_000)),
            ),
            (
                "process_started",
                json!(format!("0x{}", kernel_start(1_700_000_000, 123_456))),
            ),
            ("process_started", json!("g".repeat(32))),
            ("process_started", Value::Null),
            ("running", json!(false)),
            ("running", json!("true")),
            ("display_name", json!("/private/full/path")),
            ("display_name", json!("bad\nname")),
            ("bundle_id", json!("x".repeat(256))),
            ("icon_b64", json!("https://example.invalid/icon.png")),
        ] {
            let mut value = snapshot();
            value["applications"][0][field] = bad;
            registry.observe(value, Instant::now());
            assert_eq!(registry.view(false)["complete"], false, "{field}");
            assert_eq!(registry.view(false)["choices"], json!([]), "{field}");
        }
    }
    #[test]
    fn malformed_envelopes_and_conflicting_pids_cannot_prove_absence() {
        let fixture = Fixture::new();
        let mut registry = pinned(&fixture);
        for value in [
            Value::Null,
            json!({"available":true,"complete":true,"skipped":0,"error":null}),
            json!({"available":true,"complete":true,"applications":[],"error":null}),
            json!({"available":true,"complete":"true","applications":[],"skipped":0,"error":null}),
            json!({"available":true,"complete":true,"applications":[],"skipped":0,"error":7}),
        ] {
            registry.observe(value, Instant::now());
            assert_eq!(registry.view(false)["entries"][0]["state"], "unknown");
        }
        let mut duplicate = snapshot();
        let mut second = duplicate["applications"][0].clone();
        second["process_started"] = json!(kernel_start(1_700_000_001, 0));
        duplicate["applications"]
            .as_array_mut()
            .unwrap()
            .push(second);
        registry.observe(duplicate, Instant::now());
        assert_eq!(registry.view(false)["entries"][0]["state"], "unknown");
    }
    #[test]
    fn optional_bundle_id_is_not_process_identity_or_a_location_replacement() {
        let fixture = Fixture::new();
        let mut registry = fixture.registry();
        let selection = select(&mut registry);
        let mut changed = snapshot();
        changed["applications"][0]["bundle_id"] = Value::Null;
        registry.observe(changed, Instant::now());
        assert_eq!(pin(&mut registry, &selection).0, 200);
        assert_eq!(registry.entries[0].bundle_id, None);
        let mut moved = snapshot();
        moved["applications"][0]["bundle_path"] = json!("/Applications/Moved.app");
        moved["applications"][0]["executable_path"] =
            json!("/Applications/Moved.app/Contents/MacOS/Fixture");
        registry.observe(moved, Instant::now());
        assert_eq!(registry.view(false)["entries"][0]["state"], "not_running");
    }
    #[test]
    fn bounded_inventory_pins_and_random_identifiers() {
        let fixture = Fixture::new();
        let mut registry = fixture.registry();
        let mut value = snapshot();
        let original = value["applications"][0].clone();
        value["applications"] = json!((0..MAX_APPLICATIONS + 1)
            .map(|index| {
                let mut app = original.clone();
                app["process_id"] = json!(index + 1);
                app["bundle_path"] = json!(format!("/Applications/Fixture{index}.app"));
                app["executable_path"] = json!(format!(
                    "/Applications/Fixture{index}.app/Contents/MacOS/Fixture"
                ));
                app
            })
            .collect::<Vec<_>>());
        registry.observe(value, Instant::now());
        assert_eq!(registry.applications.len(), MAX_APPLICATIONS);
        assert_eq!(registry.view(false)["complete"], false);
        let choices = registry
            .current_choices
            .iter()
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        assert_eq!(
            choices.iter().collect::<BTreeSet<_>>().len(),
            MAX_APPLICATIONS
        );
        assert!(choices.iter().all(|id| token(id)));
        for choice in choices.iter().take(MAX_ENTRIES) {
            assert_eq!(pin(&mut registry, choice).0, 200);
        }
        assert_eq!(pin(&mut registry, &choices[MAX_ENTRIES]).0, 409);
        assert_eq!(
            registry
                .entries
                .iter()
                .map(|entry| &entry.id)
                .collect::<BTreeSet<_>>()
                .len(),
            MAX_ENTRIES
        );
        assert!(registry.entries.iter().all(|entry| token(&entry.id)));
        assert_eq!(fixture.registry().entries.len(), MAX_ENTRIES);
    }
    #[test]
    fn escaped_storage_limit_preserves_disk_memory_revision_and_selection() {
        let fixture = Fixture::new();
        let mut registry = fixture.registry();
        let mut observed = snapshot();
        let original = observed["applications"][0].clone();
        // Quotes are legal POSIX filename characters. Every path component is
        // below 255 bytes and both complete paths remain below 2,048 bytes.
        let parent = vec!["\"".repeat(245); 8].join("/");
        observed["applications"] = json!((0..MAX_ENTRIES)
            .map(|index| {
                let mut app = original.clone();
                let bundle = format!("/{parent}/Fixture{index}.app");
                app["bundle_path"] = json!(bundle);
                app["executable_path"] = json!(format!("{bundle}/Contents/MacOS/Fixture"));
                app["bundle_id"] = json!("\"".repeat(255));
                app["process_id"] = json!(index + 1);
                app
            })
            .collect::<Vec<_>>());
        registry.observe(observed, Instant::now());
        let choices = registry
            .current_choices
            .iter()
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        assert_eq!(choices.len(), MAX_ENTRIES);
        for choice in choices.iter().take(6) {
            let body =
                json!({"revision":registry.revision,"selection_id":choice,"label":"\"".repeat(80)})
                    .to_string();
            assert_eq!(registry.mutate("pin", &body).0, 200);
        }
        let disk = std::fs::read(fixture.path()).unwrap();
        assert!(disk.len() <= MAX_STORAGE);
        let entries = registry.entries.clone();
        let revision = registry.revision;
        let choice = &choices[6];
        let body =
            json!({"revision":revision,"selection_id":choice,"label":"\"".repeat(80)}).to_string();
        let response = registry.mutate_with_writer("pin", &body, |_, _| {
            panic!("Oversized storage must not reach the writer")
        });
        assert_eq!(response.0, 409);
        assert_eq!(registry.revision, revision);
        assert_eq!(registry.entries, entries);
        assert_eq!(registry.persisted.as_ref(), Some(&disk));
        assert_eq!(std::fs::read(fixture.path()).unwrap(), disk);
        assert!(registry.selections.contains_key(choice));
        assert!(registry.current_choices.iter().any(|(id, _)| id == choice));
        assert_eq!(registry.view(false)["storage_writable"], true);
        assert_eq!(fixture.registry().entries, entries);
        // A rejected selection stays usable once the user frees storage space.
        let remove = json!({"revision":revision,"id":registry.entries[0].id}).to_string();
        assert_eq!(registry.mutate("remove", &remove).0, 200);
        assert_eq!(pin(&mut registry, choice).0, 200);
        assert_eq!(fixture.registry().entries, registry.entries);
    }
    #[test]
    fn persisted_identity_rejects_duplicates_transient_fields_and_noncanonical_paths() {
        let fixture = Fixture::new();
        let registry = pinned(&fixture);
        let original: Value =
            serde_json::from_slice(&std::fs::read(fixture.path()).unwrap()).unwrap();
        for field in [
            "process_id",
            "process_started",
            "executable_path",
            "icon_b64",
        ] {
            let mut changed = original.clone();
            changed["entries"][0][field] = json!("not allowed");
            let bytes = changed.to_string();
            std::fs::write(fixture.path(), &bytes).unwrap();
            assert!(fixture.registry().load_error.is_some());
            assert_eq!(std::fs::read_to_string(fixture.path()).unwrap(), bytes);
        }
        for path in [
            "https://example.invalid/A.app",
            "file:///Applications/A.app",
            "/Applications/./A.app",
            "/Applications/A.app/",
            "/Applications/.app",
            "C:\\A.app",
        ] {
            let mut changed = original.clone();
            changed["entries"][0]["bundle_path"] = json!(path);
            std::fs::write(fixture.path(), changed.to_string()).unwrap();
            assert!(fixture.registry().load_error.is_some());
        }
        let mut duplicate = original.clone();
        duplicate["entries"]
            .as_array_mut()
            .unwrap()
            .push(registry.entries[0].json());
        std::fs::write(fixture.path(), duplicate.to_string()).unwrap();
        assert!(fixture.registry().load_error.is_some());
        duplicate["entries"][1]["id"] = json!(crate::service::generate_token().unwrap());
        std::fs::write(fixture.path(), duplicate.to_string()).unwrap();
        assert!(fixture.registry().load_error.is_some());
    }
    #[test]
    fn native_png_roundtrips_and_strict_base64_rejects_urls_and_noncanonical_padding() {
        for color in [2, 6] {
            let bytes = png(color);
            assert_eq!(decode_icon(&b64(&bytes)), Some(bytes));
        }
        for value in [
            "",
            "https://example.invalid/icon.png",
            "file:///icon.png",
            "data:image/png;base64,AAAA",
            "AAAA\n",
            "====",
            "AA=A",
            "AB==",
            "AAF=",
        ] {
            assert!(decode_icon(value).is_none(), "{value}");
        }
        assert!(decode_icon(&"A".repeat(21852)).is_none());
    }
    #[test]
    fn png_rejects_wrong_dimensions_depth_color_crc_truncation_and_trailing_data() {
        let original = png(6);
        for index in [16, 20, 24, 25, 26, 27, 28] {
            let mut changed = original.clone();
            changed[index] ^= 1;
            let checksum = crc32(&changed[12..29]);
            changed[29..33].copy_from_slice(&checksum.to_be_bytes());
            assert!(decode_icon(&b64(&changed)).is_none(), "header byte {index}");
        }
        let mut corrupt = original.clone();
        corrupt[50] ^= 1;
        assert!(decode_icon(&b64(&corrupt)).is_none());
        for end in [0, 7, 8, 10, 32, 33, original.len() - 1] {
            assert!(decode_icon(&b64(&original[..end])).is_none());
        }
        let mut trailing = original.clone();
        trailing.push(0);
        assert!(decode_icon(&b64(&trailing)).is_none());
        let mut huge_chunk = original;
        huge_chunk[8..12].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(decode_icon(&b64(&huge_chunk)).is_none());
    }
    #[test]
    fn png_rejects_profiles_text_unknown_chunks_and_bad_fixed_metadata() {
        let compressed = zlib_stored(&vec![0; 4128]);
        for kind in [
            b"iCCP", b"tEXt", b"zTXt", b"iTXt", b"eXIf", b"ABCD", b"vpAg",
        ] {
            assert!(decode_icon(&b64(&png_with_data(
                6,
                &compressed,
                Some((kind, b"private metadata"))
            )))
            .is_none());
        }
        for (kind, data) in [
            (b"sRGB", &[0][..]),
            (b"gAMA", &[0, 0, 177, 143][..]),
            (b"pHYs", &[0, 0, 0, 1, 0, 0, 0, 1, 1][..]),
            (b"sBIT", &[8, 8, 8, 8][..]),
        ] {
            assert!(
                decode_icon(&b64(&png_with_data(6, &compressed, Some((kind, data))))).is_some()
            );
        }
        assert!(decode_icon(&b64(&png_with_data(6, &compressed, Some((b"sRGB", &[4]))))).is_none());
    }
    #[test]
    fn png_inflation_is_exact_bounded_filtered_and_checksum_verified() {
        for raw in [vec![0; 4127], vec![0; 4129], vec![0; 65535], vec![5; 4128]] {
            let compressed = zlib_stored(&raw);
            assert!(decode_icon(&b64(&png_with_data(6, &compressed, None))).is_none());
        }
        let mut compressed = zlib_stored(&vec![0; 4128]);
        *compressed.last_mut().unwrap() ^= 1;
        assert!(decode_icon(&b64(&png_with_data(6, &compressed, None))).is_none());
        let mut compressed = zlib_stored(&vec![0; 4128]);
        compressed.insert(compressed.len() - 4, 0);
        assert!(decode_icon(&b64(&png_with_data(6, &compressed, None))).is_none());
    }
    #[test]
    fn compressed_deflate_is_rejected_outside_the_native_png_subset() {
        let compressed = [
            120, 156, 237, 193, 1, 1, 0, 0, 0, 130, 32, 255, 175, 110, 72, 64, 1, 0, 0, 0, 239, 6,
            16, 32, 0, 1,
        ];
        assert!(inflate_bounded(&compressed, 4128).is_none());
        assert!(decode_icon(&b64(&png_with_data(6, &compressed, None))).is_none());
    }
}
