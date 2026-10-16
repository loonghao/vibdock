//! Persisted software locations and bounded read-only playback observations.
//! A saved path is a correlation location, never authority to execute or mute.
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    time::{Duration, Instant},
};

const MAX_ENTRIES: usize = 8;
const MAX_SESSIONS: usize = 24;
const FRESH: Duration = Duration::from_secs(6);
const SELECTION_TTL: Duration = Duration::from_secs(15);

#[derive(Clone, Debug, PartialEq)]
struct Entry {
    id: String,
    label: String,
    endpoint: String,
    executable: String,
}
impl Entry {
    fn json(&self) -> Value {
        json!({"id":self.id,"label":self.label,"endpoint_id":self.endpoint,"executable_path":self.executable})
    }
    fn parse(value: &Value) -> Option<Self> {
        let id = string(value, "id", 64)?;
        if !token(&id) {
            return None;
        }
        let executable = string(value, "executable_path", 1024)?;
        if !local_executable(&executable) {
            return None;
        }
        Some(Self {
            id,
            label: string(value, "label", 80)?,
            endpoint: string(value, "endpoint_id", 512)?,
            executable,
        })
    }
}
fn string(value: &Value, key: &str, max: usize) -> Option<String> {
    let s = value.get(key)?.as_str()?;
    (!s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)).then(|| s.to_owned())
}
fn token(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|c| c.is_ascii_hexdigit())
}
fn local_executable(path: &str) -> bool {
    let b = path.as_bytes();
    b.len() > 3
        && b[0].is_ascii_alphabetic()
        && b[1] == b':'
        && b[2] == b'\\'
        && !path[2..].contains(':')
        && !path.contains('/')
        && !path.split('\\').any(|p| p == "." || p == "..")
        && !path.chars().any(char::is_control)
        && path.to_ascii_lowercase().ends_with(".exe")
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

fn same_location(a: &str, b: &str) -> bool {
    a == b
}

#[derive(Clone)]
struct Session {
    value: Value,
    key: String,
}
impl Session {
    fn parse(value: Value) -> Option<Self> {
        let endpoint = string(&value, "endpoint_id", 512)?;
        let instance = string(&value, "session_instance_id", 1024)?;
        let exe = string(&value, "executable_path", 1024)?;
        if !local_executable(&exe) {
            return None;
        }
        let pid = value.get("process_id")?.as_u64()?;
        if pid == 0 || pid > u32::MAX as u64 {
            return None;
        }
        let started = string(&value, "process_started", 32)?;
        if !started.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        string(&value, "display_name", 160)?;
        value.get("active")?.as_bool()?;
        value.get("muted")?.as_bool()?;
        let key = json!([endpoint, instance, pid, started, exe]).to_string();
        let mut value = value;
        if value["icon_b64"].as_str().and_then(decode_icon).is_none() {
            value["icon_b64"] = Value::Null;
        }
        Some(Self { value, key })
    }
    fn public(&self) -> Value {
        json!({"process_id":self.value["process_id"],"process_started":self.value["process_started"],"display_name":self.value["display_name"],"active":self.value["active"],"muted":self.value["muted"],"session_state":self.value["session_state"],"icon_b64":self.value["icon_b64"],"actions_enabled":false})
    }
}
struct Selection {
    session: Session,
    issued: Instant,
}

pub(crate) struct Registry {
    path: PathBuf,
    entries: Vec<Entry>,
    revision: u64,
    generation: u64,
    load_error: Option<String>,
    snapshot: Value,
    sessions: Vec<Session>,
    checked: Option<Instant>,
    selections: BTreeMap<String, Selection>,
    current_choices: Vec<(String, Session)>,
    worker: Option<Worker>,
}
impl Registry {
    pub(crate) fn start(enabled: bool) -> Self {
        let path = crate::service::state_dir().map(|d| d.join("apps.json"));
        let mut state = match path {
            Ok(path) => Self::load(path),
            Err(_) => Self::empty(
                PathBuf::new(),
                Some("App storage is unavailable; existing data was not changed.".into()),
            ),
        };
        #[cfg(windows)]
        {
            if enabled {
                state.worker = Worker::start().ok();
                if state.worker.is_none() {
                    state.snapshot = unavailable("Observation worker could not start");
                }
            }
        }
        #[cfg(not(windows))]
        {
            let _ = enabled;
            state.snapshot =
                unavailable("Native playback observation is unavailable on this platform");
        }
        state
    }
    pub(crate) fn empty(path: PathBuf, error: Option<String>) -> Self {
        Self {
            path,
            entries: vec![],
            revision: 0,
            generation: 0,
            load_error: error,
            snapshot: unavailable("Waiting for native playback observation"),
            sessions: vec![],
            checked: None,
            selections: BTreeMap::new(),
            current_choices: vec![],
            worker: None,
        }
    }
    fn load(path: PathBuf) -> Self {
        let mut state = Self::empty(path, None);
        let result = (|| -> std::io::Result<Vec<u8>> {
            use std::io::Read;
            let file = std::fs::File::open(&state.path)?;
            let mut bytes = Vec::new();
            file.take(32769).read_to_end(&mut bytes)?;
            Ok(bytes)
        })();
        match result {
            Err(e) if e.kind()==std::io::ErrorKind::NotFound=>{},
            Ok(bytes) if bytes.len()<=32768=>{
                let parsed=serde_json::from_slice::<Value>(&bytes).ok().and_then(|v| {
                    if v["schema_version"]!=1 {return None;}
                    let revision=v["revision"].as_u64()?;
                    let raw=v["entries"].as_array()?;
                    if raw.len()>MAX_ENTRIES {return None;}
                    let mut entries=Vec::new();
                    for item in raw { let e=Entry::parse(item)?; if entries.iter().any(|old:&Entry|old.id==e.id){return None;} entries.push(e); }
                    Some((entries,revision))
                });
                if let Some((entries,revision))=parsed {state.entries=entries;state.revision=revision;}
                else {state.load_error=Some("apps.json is invalid or unsupported; original bytes are preserved and app edits are disabled.".into());}
            },
            _=>state.load_error=Some("apps.json cannot be read safely; original data is preserved and app edits are disabled.".into()),
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
    fn observe(&mut self, value: Value, now: Instant) {
        self.generation = self.generation.saturating_add(1);
        self.snapshot = value;
        self.checked = Some(now);
        self.sessions = self.snapshot["sessions"]
            .as_array()
            .into_iter()
            .flatten()
            .take(MAX_SESSIONS)
            .filter_map(|v| Session::parse(v.clone()))
            .collect();
        if self.snapshot["sessions"].as_array().map_or(0, Vec::len) != self.sessions.len() {
            self.snapshot["complete"] = json!(false);
            if let Some(endpoints) = self.snapshot["endpoints"].as_array_mut() {
                for endpoint in endpoints {
                    endpoint["complete"] = json!(false);
                }
            }
        }
        self.selections
            .retain(|_, v| now.saturating_duration_since(v.issued) < SELECTION_TTL);
        self.current_choices.clear();
        for session in &self.sessions {
            let existing = self
                .selections
                .iter()
                .find(|(_, selection)| selection.session.key == session.key)
                .map(|(id, _)| id.clone());
            if let Some(id) = existing.or_else(|| crate::service::generate_token().ok()) {
                self.selections
                    .entry(id.clone())
                    .or_insert_with(|| Selection {
                        session: session.clone(),
                        issued: now,
                    });
                self.current_choices.push((id, session.clone()));
            }
        }
        // One bounded map, never one background task per request or selection.
        if self.selections.len() > MAX_SESSIONS * 8 {
            self.selections.retain(|id, _| {
                self.current_choices
                    .iter()
                    .any(|(current, _)| id == current)
            });
        }
    }
    fn fresh(&self) -> bool {
        self.checked.is_some_and(|t| t.elapsed() < FRESH) && self.snapshot["available"] == true
    }
    pub(crate) fn view(&self, summary: bool) -> Value {
        let fresh = self.fresh();
        let entries=self.entries.iter().map(|e| {
            let sessions:Vec<Value>=if fresh { self.sessions.iter().filter(|s|s.value["endpoint_id"]==e.endpoint && s.value["executable_path"].as_str().is_some_and(|p|same_location(p,&e.executable))).map(Session::public).collect() } else {vec![]};
            let complete=fresh && self.snapshot["endpoints"].as_array().is_some_and(|endpoints|endpoints.iter().any(|v|v["id"]==e.endpoint && v["complete"]==true));
            let state=if !sessions.is_empty(){"connected"}else if complete{"not_connected"}else{"unknown"};
            let icon=sessions.iter().find_map(|s|s.get("icon_b64").and_then(Value::as_str)).map(str::to_owned);
            let playback=if !complete && !sessions.is_empty(){"Partial observation; aggregate playback state unknown"}else if sessions.is_empty(){"Playback state unknown"}else if sessions.iter().any(|s|s["active"]==true){"Playback session active"}else{"Playback session idle"};
            let muted=if !complete || sessions.is_empty(){""}else if sessions.iter().all(|s|s["muted"]==true){"all muted"}else if sessions.iter().all(|s|s["muted"]==false){"not muted"}else{"mixed mute states"};
            let mut row=json!({"playback_label":format!("{playback}; {muted}"),"id":e.id,"label":e.label,"state":state,"complete":complete,"sessions":sessions,"actions_enabled":false,"identity_kind":"saved_executable_location","icon_b64":icon});
            if summary {row["sessions"]=json!([]);}
            row
        }).collect::<Vec<_>>();
        let choices = if fresh && !summary {
            self.current_choices
                .iter()
                .map(|(id, s)| {
                    let mut v = s.public();
                    v["selection_id"] = json!(id);
                    v
                })
                .collect::<Vec<_>>()
        } else {
            vec![]
        };
        json!({"schema_version":1,"revision":self.revision,"generation":self.generation,"age_ms":self.checked.map(|t|t.elapsed().as_millis().min(u64::MAX as u128) as u64),"skipped":self.snapshot["skipped"],"available":fresh,"complete":fresh&&self.snapshot["complete"]==true,"error":self.load_error.as_deref().or_else(||self.snapshot["error"].as_str()),"storage_writable":self.load_error.is_none(),"entries":entries,"choices":choices,"actions_enabled":false})
    }
    pub(crate) fn mutate(&mut self, operation: &str, body: &str) -> (u16, Value) {
        let fail = |status, message: &str| (status, json!({"error":message}));
        if self.load_error.is_some() {
            return fail(
                503,
                "App storage is unavailable; original data is preserved",
            );
        }
        let Ok(v) = serde_json::from_str::<Value>(body) else {
            return fail(400, "Expected a JSON object");
        };
        let Some(obj) = v.as_object() else {
            return fail(400, "Expected a JSON object");
        };
        let allowed = if operation == "pin" {
            &["revision", "selection_id", "label"][..]
        } else {
            &["revision", "id"][..]
        };
        if obj.keys().any(|k| !allowed.contains(&k.as_str())) {
            return fail(
                400,
                "Unexpected app field; paths and icon URLs are not accepted",
            );
        }
        if v["revision"].as_u64() != Some(self.revision) {
            return fail(409, "App entries changed; refresh before editing");
        }
        let mut entries = self.entries.clone();
        let mut used = None;
        match operation {
            "pin" => {
                if !self.fresh() {
                    return fail(
                        409,
                        "Playback observation expired; refresh and select again",
                    );
                }
                if entries.len() >= MAX_ENTRIES {
                    return fail(409, "At most eight software entries can be pinned");
                }
                let Some(id) = v["selection_id"].as_str() else {
                    return fail(400, "A fresh selection_id is required");
                };
                let Some(selection) = self
                    .selections
                    .get(id)
                    .filter(|s| s.issued.elapsed() < SELECTION_TTL)
                else {
                    return fail(409, "Selection expired or already used");
                };
                let Some(current) = self
                    .sessions
                    .iter()
                    .find(|s| s.key == selection.session.key)
                else {
                    return fail(409, "Selected session changed; select again");
                };
                let Some(exe) = current.value["executable_path"].as_str() else {
                    return fail(409, "Selected process cannot be resolved");
                };
                let endpoint = current.value["endpoint_id"].as_str().unwrap();
                if entries
                    .iter()
                    .any(|e| e.endpoint == endpoint && same_location(&e.executable, exe))
                {
                    return fail(
                        409,
                        "This software location is already pinned on that output",
                    );
                }
                let label = match v.get("label") {
                    None => bounded_label(current.value["display_name"].as_str().unwrap()),
                    Some(_) => match string(&v, "label", 80) {
                        Some(s) => s,
                        None => return fail(400, "Label must be plain text under 80 bytes"),
                    },
                };
                let Ok(entry_id) = crate::service::generate_token() else {
                    return fail(503, "Secure entry identifiers are unavailable");
                };
                entries.push(Entry {
                    id: entry_id,
                    label,
                    endpoint: endpoint.into(),
                    executable: exe.into(),
                });
                used = Some(id.to_string());
            }
            "remove" => {
                let Some(id) = v["id"].as_str() else {
                    return fail(400, "Entry id is required");
                };
                let before = entries.len();
                entries.retain(|e| e.id != id);
                if before == entries.len() {
                    return fail(404, "Entry not found");
                }
            }
            _ => return fail(404, "Unknown app operation"),
        }
        let Some(revision) = self.revision.checked_add(1) else {
            return fail(409, "Revision limit reached");
        };
        let stored = json!({"schema_version":1,"revision":revision,"entries":entries.iter().map(Entry::json).collect::<Vec<_>>()});
        if crate::service::atomic_write(&self.path, stored.to_string().as_bytes()).is_err() {
            return fail(
                500,
                "Could not save app entries; previous state is unchanged",
            );
        }
        self.entries = entries;
        self.revision = revision;
        if let Some(id) = used {
            self.selections.remove(&id);
            self.current_choices.retain(|(choice, _)| choice != &id);
        }
        (200, self.view(false))
    }
}
fn unavailable(message: &str) -> Value {
    json!({"available":false,"complete":false,"endpoints":[],"sessions":[],"skipped":0,"error":message})
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
            // Never block service exit indefinitely on an OS call. A detached
            // owner cannot restart: stop stays set. Process exit also closes its
            // non-inherited kill-on-close job handle. No COM thread lives here.
        }
    }
}
#[cfg(windows)]
impl Worker {
    fn start() -> std::io::Result<Self> {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            mpsc, Arc,
        };
        let (sender, receiver) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let executable = std::env::current_exe()?;
        let thread = std::thread::Builder::new()
            .name("app-observer-owner".into())
            .spawn(move || {
                while !flag.load(Ordering::Relaxed) {
                    let collected = Instant::now();
                    let (value, cleanup_confirmed) =
                        crate::resident_worker::collect(&executable, &flag);
                    if !cleanup_confirmed {
                        // All child/job/file resources have been released. Do
                        // not drop this terminal diagnostic behind an older
                        // queued success. Receiver drop unblocks this send.
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
    #[cfg(windows)]
    let value = {
        // The parent sends this one-byte gate only after assigning the helper
        // to its private kill-on-close job. No COM/file scanning before then.
        use std::io::Read;
        let mut gate = [0u8];
        if std::io::stdin().read_exact(&mut gate).is_ok() && gate == [0x56] {
            crate::resident_windows::snapshot()
        } else {
            unavailable("Observation helper was not activated")
        }
    };
    #[cfg(not(windows))]
    let value = unavailable("Native playback observation is unavailable on this platform");
    println!("{value}");
}

/// Decode a bounded native ICO. Never accept a URL, path, SVG or arbitrary MIME.
pub(crate) fn decode_icon(value: &str) -> Option<Vec<u8>> {
    if value.is_empty() || value.len() > 10924 || !value.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::new();
    for (index, chunk) in value.as_bytes().chunks_exact(4).enumerate() {
        let mut n = 0u32;
        let mut pad = 0;
        for (position, c) in chunk.iter().enumerate() {
            let v = match c {
                b'A'..=b'Z' => c - b'A',
                b'a'..=b'z' => c - b'a' + 26,
                b'0'..=b'9' => c - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                b'=' if position >= 2 && index == value.len() / 4 - 1 => {
                    pad += 1;
                    0
                }
                _ => return None,
            };
            if pad > 0 && *c != b'=' {
                return None;
            }
            n = (n << 6) | u32::from(v);
        }
        out.push((n >> 16) as u8);
        if pad < 2 {
            out.push((n >> 8) as u8);
        }
        if pad < 1 {
            out.push(n as u8);
        }
    }
    if out.len() < 22
        || out.len() > 8192
        || out[..6] != [0, 0, 1, 0, 1, 0]
        || !matches!(out[6], 16 | 24 | 32)
        || !matches!(out[7], 16 | 24 | 32)
    {
        return None;
    }
    let size = u32::from_le_bytes(out[14..18].try_into().ok()?) as usize;
    let offset = u32::from_le_bytes(out[18..22].try_into().ok()?) as usize;
    if offset != 22 || size == 0 || offset.checked_add(size) != Some(out.len()) {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn temp() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "vibdock-apps-test-{}",
            crate::service::generate_token().unwrap()
        ));
        std::fs::create_dir(&path).unwrap();
        path.join("apps.json")
    }
    fn snapshot(pid: u32, instance: &str) -> Value {
        json!({"available":true,"complete":true,"endpoints":[{"id":"speaker-fixture","complete":true}],"sessions":[{"endpoint_id":"speaker-fixture","session_instance_id":instance,"process_id":pid,"process_started":"123","executable_path":"C:\\Fixture\\Example.exe","display_name":"Example.exe","active":true,"muted":false,"session_state":"active","icon_b64":null}],"skipped":0,"error":null})
    }
    fn select(registry: &mut Registry) -> String {
        registry.observe(snapshot(123, "session-a"), Instant::now());
        registry.current_choices[0].0.clone()
    }
    #[test]
    fn pin_roundtrip_remove_preserves_separate_microphone_config() {
        let path = temp();
        let mic = path.with_file_name("config.json");
        std::fs::write(&mic, b"microphone-fixture").unwrap();
        let mut r = Registry::load(path.clone());
        let id = select(&mut r);
        assert_eq!(
            r.mutate("pin", &json!({"revision":0,"selection_id":id}).to_string())
                .0,
            200
        );
        assert_eq!(
            r.mutate("pin", &json!({"revision":1,"selection_id":id}).to_string())
                .0,
            409
        );
        let mut reopened = Registry::load(path.clone());
        assert_eq!(reopened.entries, r.entries);
        assert_eq!(reopened.view(false)["entries"][0]["state"], "unknown");
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
        assert!(Registry::load(path.clone()).entries.is_empty());
        assert_eq!(std::fs::read(mic).unwrap(), b"microphone-fixture");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn rejects_forged_expired_and_recycled_session_selections() {
        let path = temp();
        let mut r = Registry::load(path.clone());
        let id = select(&mut r);
        assert_eq!(
            r.mutate(
                "pin",
                &json!({"revision":0,"selection_id":"0".repeat(64)}).to_string()
            )
            .0,
            409
        );
        r.observe(snapshot(124, "session-a"), Instant::now());
        assert_eq!(
            r.mutate("pin", &json!({"revision":0,"selection_id":id}).to_string())
                .0,
            409
        );
        let id = select(&mut r);
        r.selections.get_mut(&id).unwrap().issued = Instant::now() - Duration::from_secs(16);
        assert_eq!(
            r.mutate("pin", &json!({"revision":0,"selection_id":id}).to_string())
                .0,
            409
        );
        let id = select(&mut r);
        assert_eq!(
            r.mutate(
                "pin",
                &json!({"revision":0,"selection_id":id,"executable_path":"C:\\Other.exe"})
                    .to_string()
            )
            .0,
            400
        );
        assert!(!path.exists());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn partial_or_stale_scans_cannot_claim_disconnection_or_revive_removed_entries() {
        let path = temp();
        let mut r = Registry::load(path.clone());
        let id = select(&mut r);
        assert_eq!(
            r.mutate("pin", &json!({"revision":0,"selection_id":id}).to_string())
                .0,
            200
        );
        let mut empty = snapshot(123, "session-a");
        empty["sessions"] = json!([]);
        empty["endpoints"][0]["complete"] = json!(false);
        r.observe(empty.clone(), Instant::now());
        assert_eq!(r.view(false)["entries"][0]["state"], "unknown");
        empty["endpoints"][0]["complete"] = json!(true);
        r.observe(empty, Instant::now());
        assert_eq!(r.view(false)["entries"][0]["state"], "not_connected");
        r.checked = Some(Instant::now() - Duration::from_secs(7));
        assert_eq!(r.view(false)["entries"][0]["state"], "unknown");
        let entry = r.entries[0].id.clone();
        r.mutate("remove", &json!({"revision":1,"id":entry}).to_string());
        r.observe(snapshot(123, "session-a"), Instant::now());
        assert!(r.view(false)["entries"].as_array().unwrap().is_empty());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn corrupt_unknown_and_unwritable_storage_preserve_data() {
        let path = temp();
        for bytes in [
            "not-json",
            r#"{"schema_version":99,"revision":0,"entries":[]}"#,
        ] {
            std::fs::write(&path, bytes).unwrap();
            let mut r = Registry::load(path.clone());
            assert!(r.load_error.is_some());
            assert_eq!(r.mutate("remove", r#"{"revision":0,"id":"x"}"#).0, 503);
            assert_eq!(std::fs::read_to_string(&path).unwrap(), bytes);
        }
        std::fs::remove_file(&path).unwrap();
        let mut r = Registry::load(path.clone());
        let id = select(&mut r);
        std::fs::create_dir(&path).unwrap();
        assert_eq!(
            r.mutate("pin", &json!({"revision":0,"selection_id":id}).to_string())
                .0,
            500
        );
        assert!(r.entries.is_empty());
        assert_eq!(r.revision, 0);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn public_view_redacts_paths_and_never_enables_actions() {
        let path = temp();
        let mut r = Registry::load(path.clone());
        let mut data = snapshot(123, r"C:\PrivateUser\PrivateFile.exe|session-a");
        data["sessions"][0]["endpoint_id"] = json!(r"C:\PrivateUser\endpoint");
        r.observe(data, Instant::now());
        let view = r.view(false);
        assert!(!view.to_string().contains("Fixture"));
        assert!(!view.to_string().contains("PrivateUser"));
        assert_eq!(view["actions_enabled"], false);
        assert_eq!(view["choices"][0]["actions_enabled"], false);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn partial_aggregates_and_delayed_snapshots_stay_unknown() {
        let path = temp();
        let mut r = Registry::load(path.clone());
        let id = select(&mut r);
        assert_eq!(
            r.mutate("pin", &json!({"revision":0,"selection_id":id}).to_string())
                .0,
            200
        );
        let mut data = snapshot(123, "session-a");
        data["endpoints"][0]["complete"] = json!(false);
        data["sessions"][0]["active"] = json!(false);
        data["sessions"][0]["muted"] = json!(true);
        r.observe(data.clone(), Instant::now());
        let view = r.view(false);
        let label = view["entries"][0]["playback_label"].as_str().unwrap();
        assert!(label.contains("aggregate playback state unknown"));
        assert!(!label.contains("all muted"));
        assert!(!label.contains("session idle"));
        assert_eq!(view["entries"][0]["sessions"][0]["muted"], true);
        r.observe(data, Instant::now() - Duration::from_secs(20));
        assert!(!r.fresh());
        assert!(r.view(false)["choices"].as_array().unwrap().is_empty());
        assert_eq!(r.view(false)["entries"][0]["state"], "unknown");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn terminal_failure_is_delivered_after_a_queued_success() {
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        sender
            .send((Instant::now(), snapshot(123, "queued")))
            .unwrap();
        let terminal = std::thread::spawn(move || {
            sender
                .send((
                    Instant::now(),
                    unavailable("Native helper cleanup is unconfirmed; observation stopped"),
                ))
                .unwrap()
        });
        assert_eq!(receiver.recv().unwrap().1["available"], true);
        assert_eq!(
            receiver.recv_timeout(Duration::from_secs(1)).unwrap().1["available"],
            false
        );
        terminal.join().unwrap();
    }
    #[test]
    fn queued_snapshot_does_not_renew_freshness_or_selection_lifetime() {
        let path = temp();
        let mut registry = Registry::load(path.clone());
        let (sender, receiver) = std::sync::mpsc::channel();
        registry.worker = Some(Worker {
            receiver,
            stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            thread: None,
        });
        sender
            .send((
                Instant::now() - Duration::from_secs(20),
                snapshot(123, "queued"),
            ))
            .unwrap();
        registry.poll();
        assert!(!registry.fresh());
        assert!(registry.view(false)["choices"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(registry
            .selections
            .values()
            .all(|selection| selection.issued.elapsed() >= SELECTION_TTL));
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn case_sensitive_locations_do_not_merge() {
        assert!(!same_location(r"C:\Fixture\App.exe", r"C:\Fixture\app.exe"));
        let a = snapshot(1, "same");
        let mut b = a.clone();
        b["sessions"][0]["executable_path"] = json!(r"C:\Fixture\example.exe");
        assert_ne!(
            Session::parse(a["sessions"][0].clone()).unwrap().key,
            Session::parse(b["sessions"][0].clone()).unwrap().key
        );
    }
    #[test]
    fn location_and_icon_parsing_fail_closed() {
        for path in [
            r"\\server\share\app.exe",
            r"\\?\C:\app.exe",
            r"C:\..\app.exe",
            r"C:\app.exe:stream",
            r"C:/app.exe",
        ] {
            assert!(!local_executable(path));
        }
        for icon in ["", "https://example.test/a.ico", "AAAA", "====", "<svg/>"] {
            assert!(decode_icon(icon).is_none());
        }
    }
}
