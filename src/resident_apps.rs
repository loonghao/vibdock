//! Persisted locations, bounded playback observations and explicit exact-session actions.
//! A saved path is a correlation location, never authority to execute or mute.
use serde_json::{json, Value};
#[path = "app_route.rs"]
pub(crate) mod app_route;
#[path = "audio_probe.rs"]
pub(crate) mod audio_probe;
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
    endpoint: Option<String>,
    executable: String,
    device_id: Option<String>,
}
impl Entry {
    fn json(&self) -> Value {
        json!({"id":self.id,"label":self.label,"endpoint_id":self.endpoint,"executable_path":self.executable,"device_id":self.device_id})
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
        let device_id = match value.get("device_id") {
            None | Some(Value::Null) => None,
            Some(_) => Some(string(value, "device_id", 512).filter(|id| !id.trim().is_empty())?),
        };
        Some(Self {
            id,
            label: string(value, "label", 80)?,
            endpoint: match value.get("endpoint_id") {
                None | Some(Value::Null) => None,
                Some(_) => Some(string(value, "endpoint_id", 512)?),
            },
            executable,
            device_id,
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

#[derive(Clone)]
struct Application {
    value: Value,
    key: String,
}
impl Application {
    fn parse(value: Value) -> Option<Self> {
        let exe = string(&value, "executable_path", 1024)?;
        if !local_executable(&exe) {
            return None;
        }
        let pid = value["process_id"]
            .as_u64()
            .filter(|pid| *pid > 4 && *pid <= u32::MAX as u64)?;
        let started = string(&value, "process_started", 32)?;
        if !started.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        string(&value, "display_name", 160)?;
        string(&value, "window_title", 160)?;
        let key = json!([pid, started, exe]).to_string();
        let mut value = value;
        if value["icon_b64"].as_str().and_then(decode_icon).is_none() {
            value["icon_b64"] = Value::Null;
        }
        Some(Self { value, key })
    }
    fn public(&self) -> Value {
        json!({"process_id":self.value["process_id"],"display_name":self.value["display_name"],"window_title":self.value["window_title"],"icon_b64":self.value["icon_b64"],"presence":"visible_window","playback_state":"unknown","voice_state":"unknown"})
    }
}
struct CatalogSelection {
    application: Application,
    issued: Instant,
}
struct RouteOperation {
    principal: String,
    revision: u64,
    job: app_route::Job,
    result: Value,
    completed: Option<Instant>,
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
    microphone_device: Option<Option<String>>,
    microphone_changed: Instant,
    selections: BTreeMap<String, Selection>,
    current_choices: Vec<(String, Session)>,
    catalog: Vec<Application>,
    catalog_selections: BTreeMap<String, CatalogSelection>,
    catalog_choices: Vec<(String, Application)>,
    routes: BTreeMap<String, RouteOperation>,
    worker: Option<Worker>,
    actions: crate::resident_actions::Actions,
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
                state.worker = Worker::start(state.revision).ok();
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
            microphone_device: None,
            microphone_changed: Instant::now(),
            selections: BTreeMap::new(),
            current_choices: vec![],
            catalog: vec![],
            catalog_selections: BTreeMap::new(),
            catalog_choices: vec![],
            routes: BTreeMap::new(),
            worker: None,
            actions: crate::resident_actions::Actions::default(),
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
        // Never coalesce action completions with best-effort observation frames.
        if let Some(worker) = &self.worker {
            for (id, observed_since, result) in worker.results.try_iter() {
                if let Some(operation) = self.routes.get_mut(&id) {
                    operation.result = if app_route::valid_result(&result, &operation.job) {
                        result
                    } else {
                        app_route::failure(&operation.job, true, "route_result_invalid")
                    };
                    if operation.revision != self.revision
                        && operation.result["state"] == "verified"
                    {
                        operation.result = app_route::failure(
                            &operation.job,
                            operation.result["write_attempted"] == true,
                            "route_configuration_changed",
                        );
                    }
                    operation.completed = Some(observed_since);
                } else {
                    self.actions.complete(&id, result, observed_since);
                }
            }
            if !worker.alive.load(std::sync::atomic::Ordering::Acquire) {
                self.actions.owner_stopped();
                for operation in self
                    .routes
                    .values_mut()
                    .filter(|operation| operation.completed.is_none())
                {
                    operation.result =
                        app_route::failure(&operation.job, true, "route_owner_stopped");
                    operation.completed = Some(Instant::now());
                }
            }
        }
        let next = self
            .worker
            .as_ref()
            .and_then(|worker| worker.receiver.try_iter().last());
        if let Some((collected, value)) = next {
            self.observe(value, collected);
        }
    }
    /// The service's committed configuration is authoritative, never a platform default.
    #[cfg(any(windows, test))]
    pub(crate) fn set_microphone_device(&mut self, device_id: Option<&str>) {
        let device = device_id.map(str::to_owned);
        if self.microphone_device.as_ref() == Some(&device) {
            return;
        }
        self.microphone_device = Some(device.clone());
        self.microphone_changed = Instant::now();
        if let Some(worker) = &self.worker {
            if let Ok(mut target) = worker.microphone.lock() {
                *target = device;
            }
        }
    }
    fn microphone_view(&self) -> Value {
        let Some(device) = &self.microphone_device else {
            return audio_probe::unknown(None, "Microphone configuration is unavailable");
        };
        let Some(id) = device.as_deref() else {
            let mut value = audio_probe::unknown(None, "");
            value["state"] = json!("unconfigured");
            value["error"] = Value::Null;
            value["age_ms"] = json!(0);
            return value;
        };
        let Some(checked) = self
            .checked
            .filter(|checked| *checked >= self.microphone_changed)
        else {
            return audio_probe::unknown(
                Some(id),
                "Waiting for the configured microphone observation",
            );
        };
        if checked.elapsed() >= FRESH {
            let mut value = audio_probe::unknown(Some(id), "Microphone observation expired");
            value["age_ms"] = json!(checked.elapsed().as_millis().min(u64::MAX as u128) as u64);
            return value;
        }
        let microphone = &self.snapshot["microphone"];
        if !audio_probe::valid_microphone(microphone, id) {
            return audio_probe::unknown(
                Some(id),
                "Microphone observation is unavailable or changed",
            );
        }
        let mut value = microphone.clone();
        value["age_ms"] = json!(checked.elapsed().as_millis().min(u64::MAX as u128) as u64);
        value
    }
    fn observe(&mut self, value: Value, now: Instant) {
        self.generation = self.generation.saturating_add(1);
        self.snapshot = value;
        self.checked = Some(now);
        self.catalog = self.snapshot["catalog"]["applications"]
            .as_array()
            .into_iter()
            .flatten()
            .take(MAX_SESSIONS)
            .filter_map(|v| Application::parse(v.clone()))
            .collect();
        if self.snapshot["catalog"]["applications"]
            .as_array()
            .map_or(0, Vec::len)
            != self.catalog.len()
        {
            self.snapshot["catalog"]["complete"] = json!(false);
        }
        self.catalog_selections
            .retain(|_, selection| now.saturating_duration_since(selection.issued) < SELECTION_TTL);
        self.catalog_choices.clear();
        for application in &self.catalog {
            let existing = self
                .catalog_selections
                .iter()
                .find(|(_, selection)| selection.application.key == application.key)
                .map(|(id, _)| id.clone());
            if let Some(id) = existing.or_else(|| crate::service::generate_token().ok()) {
                self.catalog_selections
                    .entry(id.clone())
                    .or_insert_with(|| CatalogSelection {
                        application: application.clone(),
                        issued: now,
                    });
                self.catalog_choices.push((id, application.clone()));
            }
        }
        if self.catalog_selections.len() > MAX_SESSIONS * 8 {
            self.catalog_selections.retain(|id, _| {
                self.catalog_choices
                    .iter()
                    .any(|(current, _)| id == current)
            });
        }
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
    fn catalog_fresh(&self) -> bool {
        self.checked.is_some_and(|at| at.elapsed() < FRESH)
            && self.snapshot["catalog"]["available"] == true
    }
    fn route_pending(&self) -> bool {
        self.routes
            .values()
            .any(|operation| operation.completed.is_none())
    }
    fn routing_capabilities(&self) -> Value {
        let value = &self.snapshot["routing_capabilities"];
        let fresh = self.checked.is_some_and(|at| at.elapsed() < FRESH);
        let consistent = value["stream_state"] == "unknown"
            && value["app_microphone_mute"] == "unsupported"
            && match value["state"].as_str() {
                Some("available") => {
                    value["preferred_input"] == "available"
                        && value["preferred_output"] == "available"
                        && value["error"].is_null()
                }
                Some("unknown" | "unsupported") => {
                    value["preferred_input"] == "unknown"
                        && value["preferred_output"] == "unknown"
                        && string(value, "error", 2048).is_some()
                }
                _ => false,
            };
        if fresh && consistent {
            json!({"state":value["state"],"preferred_input":value["preferred_input"],"preferred_output":value["preferred_output"],"stream_state":"unknown","app_microphone_mute":"unsupported","error":value["error"]})
        } else {
            json!({"state":"unknown","preferred_input":"unknown","preferred_output":"unknown","stream_state":"unknown","app_microphone_mute":"unsupported","error":"Preferred routing observation is unavailable or expired"})
        }
    }
    fn route_target(&self, id: &str) -> Option<Value> {
        if self.catalog_fresh() {
            if let Some(selection) = self
                .catalog_selections
                .get(id)
                .filter(|selection| selection.issued.elapsed() < SELECTION_TTL)
            {
                if let Some(application) = self
                    .catalog
                    .iter()
                    .find(|application| application.key == selection.application.key)
                {
                    return Some(application.value.clone());
                }
            }
        }
        if self.fresh() {
            if let Some(selection) = self
                .selections
                .get(id)
                .filter(|selection| selection.issued.elapsed() < SELECTION_TTL)
            {
                if let Some(session) = self
                    .sessions
                    .iter()
                    .find(|session| session.key == selection.session.key)
                {
                    return Some(session.value.clone());
                }
            }
        }
        None
    }
    fn entry_routing_selection(&self, entry: &Entry) -> Option<String> {
        let identity = |value: &Value| {
            json!([
                value["process_id"],
                value["process_started"],
                value["executable_path"]
            ])
            .to_string()
        };
        let mut identities: BTreeMap<String, Option<String>> = BTreeMap::new();
        // Consumed/expired capabilities cannot hide another observed process.
        // A saved location alone never authorizes choosing one of several PIDs.
        if self.catalog_fresh() {
            for application in &self.catalog {
                if application.value["executable_path"] == entry.executable {
                    identities.insert(identity(&application.value), None);
                }
            }
        }
        if self.fresh() {
            for session in &self.sessions {
                if session.value["executable_path"] == entry.executable {
                    identities.insert(identity(&session.value), None);
                }
            }
        }
        let mut add = |id: &str, value: &Value| {
            if value["executable_path"] == entry.executable {
                if let Some(token) = identities.get_mut(&identity(value)) {
                    if token.is_none() {
                        *token = Some(id.to_owned());
                    }
                }
            }
        };
        if self.catalog_fresh() {
            for (id, application) in &self.catalog_choices {
                if self
                    .catalog_selections
                    .get(id)
                    .is_some_and(|selection| selection.issued.elapsed() < SELECTION_TTL)
                {
                    add(id, &application.value);
                }
            }
        }
        if self.fresh() {
            for (id, session) in &self.current_choices {
                if self
                    .selections
                    .get(id)
                    .is_some_and(|selection| selection.issued.elapsed() < SELECTION_TTL)
                {
                    add(id, &session.value);
                }
            }
        }
        if identities.len() == 1 {
            identities.into_values().next().flatten()
        } else {
            None
        }
    }
    pub(crate) fn route(
        &mut self,
        post: bool,
        get: bool,
        path: &str,
        principal: &str,
        body: &str,
    ) -> Option<(u16, Value)> {
        if path != "/api/apps/routes" && !path.starts_with("/api/apps/routes/") {
            return None;
        }
        let fail = |code, error: &str| (code, json!({"error":error}));
        if principal.is_empty() {
            return Some(fail(401, "Authorization is required"));
        }
        self.poll();
        self.routes.retain(|_, operation| {
            operation
                .completed
                .is_none_or(|at| at.elapsed() < Duration::from_secs(120))
        });
        if get && path.starts_with("/api/apps/routes/") {
            let id = path.trim_start_matches("/api/apps/routes/");
            return Some(
                match self
                    .routes
                    .get(id)
                    .filter(|operation| token(id) && operation.principal == principal)
                {
                    Some(operation) => {
                        let mut result = operation.result.clone();
                        result["revision"] = json!(operation.revision);
                        (200, result)
                    }
                    None => fail(404, "Route operation is unavailable"),
                },
            );
        }
        if !post || path != "/api/apps/routes" {
            return Some(fail(404, "Unknown route operation"));
        }
        let request = match app_route::parse_request(body.as_bytes()) {
            Ok(request) => request,
            Err(error) => return Some(fail(400, error)),
        };
        if self.load_error.is_some() {
            return Some(fail(
                503,
                "App storage is unavailable; original data is preserved",
            ));
        }
        if request.revision != self.revision {
            return Some(fail(409, "App entries changed; refresh before routing"));
        }
        if !cfg!(windows) {
            return Some(fail(
                501,
                "Native preferred endpoint routing is unavailable on this platform",
            ));
        }
        if self.routes.len() >= 16 || self.route_pending() || self.actions.pending() {
            return Some(fail(
                409,
                "An operation is pending or the receipt limit was reached",
            ));
        }
        if !self.checked.is_some_and(|at| at.elapsed() < FRESH)
            || self.routing_capabilities()["state"] != "available"
        {
            return Some(fail(
                503,
                "Native preferred endpoint routing has not passed its read-only availability probe",
            ));
        }
        let Some(target) = self.route_target(&request.selection_id) else {
            return Some(fail(
                409,
                "Selected application expired or changed; refresh and select again",
            ));
        };
        let Some(worker) = self
            .worker
            .as_ref()
            .filter(|worker| worker.alive.load(std::sync::atomic::Ordering::Acquire))
        else {
            return Some(fail(503, "Native action worker is unavailable"));
        };
        let Ok(id) = crate::service::generate_token() else {
            return Some(fail(503, "Secure route identifiers are unavailable"));
        };
        let mut runtime_target = serde_json::Map::new();
        for field in ["process_id", "process_started", "executable_path"] {
            runtime_target.insert(field.into(), target[field].clone());
        }
        let job = json!({"kind":"preferred_endpoint_route","operation_id":id,"target":runtime_target,"flow":request.flow,"device_id":request.device_id,"roles":request.roles});
        let job = match app_route::parse_job(job.to_string().as_bytes()) {
            Ok(job) => job,
            Err(error) => return Some(fail(409, error)),
        };
        let mut queued = job.json();
        queued["revision"] = json!(request.revision);
        if worker.commands.try_send(queued).is_err() {
            return Some(fail(409, "Native action queue is busy"));
        }
        let result = json!({"operation_id":id,"process_id":job.process_id,"flow":job.flow,"device_id":job.device_id,"roles":job.roles,"state":"pending","stream_state":"unknown","error":null});
        self.routes.insert(
            id,
            RouteOperation {
                principal: principal.into(),
                revision: request.revision,
                job,
                result: result.clone(),
                completed: None,
            },
        );
        Some((202, result))
    }
    pub(crate) fn view(&self, summary: bool) -> Value {
        let fresh = self.fresh();
        let entries=self.entries.iter().map(|e| {
            let sessions:Vec<Value>=if fresh { self.sessions.iter().filter(|s|e.endpoint.as_deref().is_some_and(|endpoint|s.value["endpoint_id"]==endpoint) && s.value["executable_path"].as_str().is_some_and(|p|same_location(p,&e.executable))).map(|session| {
                let mut v=session.public();
                if let Some((id,_))=self.current_choices.iter().find(|(id,s)|s.key==session.key && self.selections.get(id).is_some_and(|selection|selection.issued.elapsed()<SELECTION_TTL)) {
                    v["session_reference"]=json!(id);
                    v["actions_enabled"]=json!(self.worker.as_ref().is_some_and(|worker|worker.alive.load(std::sync::atomic::Ordering::Acquire)) && self.load_error.is_none() && !self.actions.pending() && !self.route_pending() && self.unique_session(session));
                }
                // Creation stamps remain internal action identity, never a browser authority.
                v.as_object_mut().unwrap().remove("process_started");
                v
            }).collect() } else {vec![]};
            let complete=fresh && e.endpoint.as_deref().is_some_and(|endpoint|self.snapshot["endpoints"].as_array().is_some_and(|endpoints|endpoints.iter().any(|v|v["id"]==endpoint && v["complete"]==true)));
            let state=if !sessions.is_empty(){"connected"}else if complete{"not_connected"}else{"unknown"};
            let icon=sessions.iter().find_map(|s|s.get("icon_b64").and_then(Value::as_str)).map(str::to_owned);
            let playback=if !complete && !sessions.is_empty(){"Partial observation; aggregate playback state unknown"}else if sessions.is_empty(){"Playback state unknown"}else if sessions.iter().any(|s|s["active"]==true){"Playback session active"}else{"Playback session idle"};
            let muted=if !complete || sessions.is_empty(){""}else if sessions.iter().all(|s|s["muted"]==true){"all muted"}else if sessions.iter().all(|s|s["muted"]==false){"not muted"}else{"mixed mute states"};
            let enabled=sessions.iter().any(|s|s["actions_enabled"]==true);
            let mut row=json!({"playback_label":format!("{playback}; {muted}"),"id":e.id,"label":e.label,"device_id":e.device_id,"capabilities":{"start_voice":"not_implemented","end_voice":"not_implemented","input_routing":"not_implemented"},"state":state,"complete":complete,"sessions":sessions,"actions_enabled":enabled,"identity_kind":"saved_executable_location","icon_b64":icon});
            let presence=self.catalog_fresh().then(||self.catalog.iter().find(|app|app.value["executable_path"]==e.executable)).flatten();
            row["presence"]=json!(if presence.is_some(){"visible_window"}else if self.catalog_fresh() && self.snapshot["catalog"]["complete"]==true {"not_observed"}else{"unknown"});
            if let Some(application)=presence {
                row["window_title"]=application.value["window_title"].clone();
                if row["icon_b64"].is_null() {row["icon_b64"]=application.value["icon_b64"].clone();}
            }
            if let Some(id)=self.entry_routing_selection(e){row["routing_selection_id"]=json!(id);}
            if summary {for session in row["sessions"].as_array_mut().unwrap() {session.as_object_mut().unwrap().remove("icon_b64");}}
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
        let actions_enabled = entries.iter().any(|e| e["actions_enabled"] == true);
        let mut result = json!({"schema_version":1,"platform":if cfg!(windows){"windows"}else{"unsupported"},"revision":self.revision,"generation":self.generation,"age_ms":self.checked.map(|t|t.elapsed().as_millis().min(u64::MAX as u128) as u64),"skipped":self.snapshot["skipped"],"available":fresh,"complete":fresh&&self.snapshot["complete"]==true,"error":self.load_error.as_deref().or_else(||self.snapshot["error"].as_str()),"storage_writable":self.load_error.is_none(),"entries":entries,"choices":choices,"actions_enabled":actions_enabled});
        result["microphone"] = self.microphone_view();
        let route_fresh = self.checked.is_some_and(|at| at.elapsed() < FRESH)
            && self.snapshot["routing_devices"]["available"] == true;
        let mut routing_devices = json!({"capture":[],"render":[],"available":route_fresh,"complete":route_fresh&&self.snapshot["routing_devices"]["complete"]==true,"age_ms":self.checked.map(|at|at.elapsed().as_millis().min(u64::MAX as u128) as u64),"error":self.snapshot["routing_devices"]["error"]});
        if route_fresh {
            for direction in ["capture", "render"] {
                let rows = self.snapshot["routing_devices"][direction]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .take(16)
                    .filter_map(|row| {
                        Some(json!({"id":string(row,"id",512)?,"name":string(row,"name",160)?}))
                    })
                    .collect::<Vec<_>>();
                if self.snapshot["routing_devices"][direction]
                    .as_array()
                    .map_or(0, Vec::len)
                    != rows.len()
                {
                    routing_devices["complete"] = json!(false);
                }
                routing_devices[direction] = json!(rows);
            }
        }
        result["routing_devices"] = routing_devices;
        result["routing_capabilities"] = self.routing_capabilities();
        let catalog_fresh = self.catalog_fresh();
        let catalog_rows = if catalog_fresh && !summary {
            self.catalog_choices
                .iter()
                .filter(|(id, _)| {
                    self.catalog_selections
                        .get(id)
                        .is_some_and(|selection| selection.issued.elapsed() < SELECTION_TTL)
                })
                .map(|(id, application)| {
                    let mut row = application.public();
                    row["selection_id"] = json!(id);
                    row
                })
                .collect::<Vec<_>>()
        } else {
            vec![]
        };
        result["catalog"] = json!({"available":catalog_fresh,"complete":catalog_fresh&&self.snapshot["catalog"]["complete"]==true,"age_ms":self.checked.map(|at|at.elapsed().as_millis().min(u64::MAX as u128) as u64),"applications":catalog_rows,"skipped":self.snapshot["catalog"]["skipped"],"error":self.snapshot["catalog"]["error"]});
        // Preserve every exact session reference under the native transport budget.
        // Duplicate saved locations can expand rows; dropping optional images is
        // safer than silently dropping sessions or letting a large response stall.
        if summary && result.to_string().len() > 120_000 {
            for entry in result["entries"].as_array_mut().unwrap() {
                entry["icon_b64"] = Value::Null;
            }
            result["icons_omitted"] = json!(true);
        }
        result
    }
    pub(crate) fn action_route(
        &mut self,
        post: bool,
        get: bool,
        path: &str,
        body: &str,
        principal: &str,
    ) -> (u16, Value) {
        let fail = |code, error: &str| (code, json!({"error":error}));
        if !cfg!(windows) {
            return fail(501, "Playback controls are unavailable on this platform");
        }
        if principal.is_empty() {
            return fail(401, "Authorization is required");
        }
        self.poll();
        match (post, get, path) {
            (true, _, "/api/apps/actions/prepare") => self.prepare_action(principal, body),
            (true, _, "/api/apps/actions/execute") => self.execute_action(principal, body),
            (_, true, _) => {
                let id = path.trim_start_matches("/api/apps/actions/");
                if !token(id) {
                    return fail(404, "Unknown operation");
                }
                self.actions.view(principal, id)
            }
            _ => fail(404, "Unknown app action route"),
        }
    }
    fn unique_session(&self, session: &Session) -> bool {
        self.sessions
            .iter()
            .filter(|s| {
                s.value["endpoint_id"] == session.value["endpoint_id"]
                    && s.value["session_instance_id"] == session.value["session_instance_id"]
            })
            .count()
            == 1
    }
    fn exact_current(&self, entry_id: &str, key: &str) -> Option<&Session> {
        if !self.fresh() || self.load_error.is_some() {
            return None;
        }
        let entry = self.entries.iter().find(|e| e.id == entry_id)?;
        let mut matched = self.sessions.iter().filter(|s| {
            s.key == key
                && entry
                    .endpoint
                    .as_deref()
                    .is_some_and(|endpoint| s.value["endpoint_id"] == endpoint)
                && s.value["executable_path"] == entry.executable
        });
        let session = matched.next()?;
        (matched.next().is_none() && self.unique_session(session)).then_some(session)
    }
    fn prepare_action(&mut self, principal: &str, body: &str) -> (u16, Value) {
        let fail = |code, error: &str| (code, json!({"error":error}));
        if self.route_pending() {
            return fail(409, "A preferred endpoint operation is pending");
        }
        let Ok(value) = serde_json::from_str::<Value>(body) else {
            return fail(400, "Expected a JSON object");
        };
        let Some(object) = value.as_object() else {
            return fail(400, "Expected a JSON object");
        };
        if object.len() != 3
            || object
                .keys()
                .any(|key| !matches!(key.as_str(), "entry_id" | "session_reference" | "action"))
        {
            return fail(
                400,
                "Unexpected action field; runtime targets are server-owned",
            );
        }
        let (Some(entry_id), Some(reference), Some(action)) = (
            value["entry_id"].as_str(),
            value["session_reference"].as_str(),
            value["action"].as_str(),
        ) else {
            return fail(
                400,
                "Entry, displayed session and fixed action are required",
            );
        };
        let requested = match action {
            "mute" => true,
            "unmute" => false,
            _ => return fail(400, "Only explicit mute or unmute is supported"),
        };
        if self
            .worker
            .as_ref()
            .is_none_or(|worker| !worker.alive.load(std::sync::atomic::Ordering::Acquire))
        {
            return fail(503, "Native action worker is unavailable");
        }
        let Some(selection) = self
            .selections
            .get(reference)
            .filter(|s| s.issued.elapsed() < SELECTION_TTL)
        else {
            return fail(409, "Displayed session expired; refresh and select again");
        };
        let Some(session) = self.exact_current(entry_id, &selection.session.key) else {
            return fail(409, "Displayed session changed; refresh and select again");
        };
        let key = session.key.clone();
        let mut target = serde_json::Map::new();
        for field in [
            "endpoint_id",
            "session_instance_id",
            "process_id",
            "process_started",
            "executable_path",
        ] {
            target.insert(field.into(), session.value[field].clone());
        }
        let label = self
            .entries
            .iter()
            .find(|e| e.id == entry_id)
            .unwrap()
            .label
            .clone();
        self.actions.prepare(
            principal,
            entry_id,
            &key,
            Value::Object(target),
            &label,
            requested,
        )
    }
    fn execute_action(&mut self, principal: &str, body: &str) -> (u16, Value) {
        if self.route_pending() {
            return (
                409,
                json!({"error":"A preferred endpoint operation is pending"}),
            );
        }
        let (id, entry, key, job) = match self.actions.consume(principal, body) {
            Ok(v) => v,
            Err(error) => return error,
        };
        if self.exact_current(&entry, &key).is_none() {
            self.actions.reject(&id, "displayed_session_changed");
        } else if self
            .worker
            .as_ref()
            .is_none_or(|worker| worker.commands.try_send(job).is_err())
        {
            self.actions.reject(&id, "action_queue_unavailable");
        }
        let (_, receipt) = self.actions.view(principal, &id);
        (
            if receipt["state"] == "pending" {
                202
            } else {
                409
            },
            receipt,
        )
    }
    pub(crate) fn mutate(&mut self, operation: &str, body: &str) -> (u16, Value) {
        if operation == "bind" {
            return (404, json!({"error":"Use the microphone association API"}));
        }
        self.edit(operation, body, |_| Ok(()))
    }
    #[cfg(any(windows, test))]
    pub(crate) fn bind(
        &mut self,
        body: &str,
        validate_device: impl FnOnce(&str) -> std::result::Result<(), String>,
    ) -> (u16, Value) {
        self.edit("bind", body, validate_device)
    }
    fn edit(
        &mut self,
        operation: &str,
        body: &str,
        validate_device: impl FnOnce(&str) -> std::result::Result<(), String>,
    ) -> (u16, Value) {
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
        let allowed = match operation {
            "pin" => &["revision", "selection_id", "label"][..],
            "bind" => &["revision", "id", "device_id"][..],
            _ => &["revision", "id"][..],
        };
        if obj.keys().any(|k| !allowed.contains(&k.as_str())) {
            return fail(
                400,
                "Unexpected app field; paths and icon URLs are not accepted",
            );
        }
        if operation == "bind" && (obj.len() != 3 || v["revision"].as_u64().is_none()) {
            return fail(400, "revision, id and device_id are required");
        }
        if v["revision"].as_u64() != Some(self.revision) {
            return fail(409, "App entries changed; refresh before editing");
        }
        let mut entries = self.entries.clone();
        let mut used = None;
        match operation {
            "pin" => {
                if entries.len() >= MAX_ENTRIES {
                    return fail(409, "At most eight software entries can be pinned");
                }
                let Some(id) = v["selection_id"].as_str() else {
                    return fail(400, "A fresh selection_id is required");
                };
                let selected = if self.fresh() {
                    self.selections
                        .get(id)
                        .filter(|s| s.issued.elapsed() < SELECTION_TTL)
                        .and_then(|selection| {
                            self.sessions
                                .iter()
                                .find(|s| s.key == selection.session.key)
                        })
                        .map(|session| {
                            (
                                Some(session.value["endpoint_id"].as_str().unwrap().to_owned()),
                                session.value.clone(),
                            )
                        })
                } else {
                    None
                };
                let selected = selected.or_else(|| {
                    if self.catalog_fresh() {
                        self.catalog_selections
                            .get(id)
                            .filter(|s| s.issued.elapsed() < SELECTION_TTL)
                            .and_then(|selection| {
                                self.catalog
                                    .iter()
                                    .find(|app| app.key == selection.application.key)
                            })
                            .map(|app| (None, app.value.clone()))
                    } else {
                        None
                    }
                });
                let Some((endpoint, current)) = selected else {
                    return fail(
                        409,
                        "Selection expired or changed; refresh and select again",
                    );
                };
                let exe = current["executable_path"].as_str().unwrap();
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
                    None => bounded_label(current["display_name"].as_str().unwrap()),
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
                    endpoint,
                    executable: exe.into(),
                    device_id: None,
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
            "bind" => {
                let Some(id) = string(&v, "id", 64).filter(|id| token(id)) else {
                    return fail(400, "A saved entry id is required");
                };
                let device_id = match v.get("device_id") {
                    Some(Value::Null) => None,
                    Some(Value::String(_)) => {
                        match string(&v, "device_id", 512).filter(|id| !id.trim().is_empty()) {
                            Some(id) => Some(id),
                            None => return fail(
                                400,
                                "device_id must be a plain identifier of at most 512 bytes or null",
                            ),
                        }
                    }
                    _ => return fail(400, "device_id must be a string or explicit null"),
                };
                let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) else {
                    return fail(404, "Entry not found");
                };
                if let Some(id) = device_id.as_deref() {
                    if let Err(error) = validate_device(id) {
                        return fail(409, &error);
                    }
                }
                entry.device_id = device_id;
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
        if let Some(worker) = &self.worker {
            worker
                .revision
                .store(revision, std::sync::atomic::Ordering::Release);
        }
        if let Some(id) = used {
            self.selections.remove(&id);
            self.current_choices.retain(|(choice, _)| choice != &id);
            self.catalog_selections.remove(&id);
            self.catalog_choices.retain(|(choice, _)| choice != &id);
        }
        (200, self.view(false))
    }
}
fn unavailable(message: &str) -> Value {
    json!({"available":false,"complete":false,"endpoints":[],"sessions":[],"skipped":0,"error":message})
}

struct Worker {
    receiver: std::sync::mpsc::Receiver<(Instant, Value)>,
    commands: std::sync::mpsc::SyncSender<Value>,
    results: std::sync::mpsc::Receiver<(String, Instant, Value)>,
    alive: std::sync::Arc<std::sync::atomic::AtomicBool>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    #[cfg(any(windows, test))]
    microphone: std::sync::Arc<std::sync::Mutex<Option<String>>>,
    revision: std::sync::Arc<std::sync::atomic::AtomicU64>,
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
    fn start(revision: u64) -> std::io::Result<Self> {
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            mpsc, Arc,
        };
        let (sender, receiver) = mpsc::sync_channel(1);
        let (commands, requests) = mpsc::sync_channel::<Value>(1);
        let (completions, results) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let alive = Arc::new(AtomicBool::new(true));
        let alive_flag = alive.clone();
        let executable = std::env::current_exe()?;
        let microphone = Arc::new(std::sync::Mutex::new(None::<String>));
        let target = microphone.clone();
        let revision = Arc::new(std::sync::atomic::AtomicU64::new(revision));
        let route_revision = revision.clone();
        let thread = std::thread::Builder::new()
            .name("app-observer-owner".into())
            .spawn(move || {
                struct Alive(Arc<AtomicBool>);
                impl Drop for Alive {fn drop(&mut self){self.0.store(false,Ordering::Release);}}
                let _alive=Alive(alive_flag);
                while !flag.load(Ordering::Relaxed) {
                    let collected = Instant::now();
                    let device_id = target.lock().ok().and_then(|id| id.clone());
                    let (value, cleanup_confirmed) =
                        crate::resident_worker::collect(&executable, &flag, device_id.as_deref());
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
                    let next_scan=Instant::now()+Duration::from_secs(2);
                    while Instant::now()<next_scan {
                        if flag.load(Ordering::Relaxed) {return;}
                        match requests.recv_timeout(Duration::from_millis(50)) {
                            Ok(mut job) => {
                                let id=job["operation_id"].as_str().unwrap_or("").to_owned();
                                let observed_since=Instant::now();
                                let (result,cleaned)=if job["kind"]=="preferred_endpoint_route" {
                                    let expected=job["revision"].as_u64().unwrap_or(u64::MAX);
                                    job.as_object_mut().unwrap().remove("revision");
                                    crate::resident_worker::preferred_route(&executable,&flag,&job,&route_revision,expected)
                                }else{crate::resident_worker::execute(&executable,&flag,&job)};
                                if completions.send((id,observed_since,result)).is_err() {return;}
                                if !cleaned {
                                    let _=sender.send((Instant::now(),unavailable("Native helper cleanup is unconfirmed; observation stopped")));
                                    return;
                                }
                                break; // Immediately refresh live state after the completed operation.
                            },
                            Err(mpsc::RecvTimeoutError::Timeout)=>{},
                            Err(mpsc::RecvTimeoutError::Disconnected)=>return,
                        }
                    }
                }
            })?;
        Ok(Self {
            receiver,
            commands,
            results,
            alive,
            stop,
            thread: Some(thread),
            microphone,
            revision,
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
        let mut input = std::io::stdin().lock();
        let mut gate = [0u8];
        let mut bytes = Vec::new();
        if input.read_exact(&mut gate).is_ok()
            && gate == [0x56]
            && input
                .take(app_route::MAX_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .is_ok()
        {
            if serde_json::from_slice::<Value>(&bytes)
                .ok()
                .is_some_and(|value| value["kind"] == "preferred_endpoint_route")
            {
                match app_route::parse_job(&bytes) {
                    Ok(job) => crate::resident_windows::preferred_route(&job),
                    Err(_) => unavailable("Routing helper request was invalid"),
                }
            } else {
                match audio_probe::decode_request(&bytes) {
                    Ok(request) if request.operation.is_some() => {
                        crate::resident_windows::microphone_action(
                            request.device_id.as_deref().unwrap(),
                            request.operation.as_deref().unwrap(),
                        )
                    }
                    Ok(request) if request.test => crate::resident_windows::microphone_test(
                        request.device_id.as_deref().unwrap(),
                    ),
                    Ok(request) => crate::resident_windows::snapshot(request.device_id.as_deref()),
                    Err(_) => unavailable("Observation helper request was invalid"),
                }
            }
        } else {
            unavailable("Observation helper was not activated")
        }
    };
    #[cfg(not(windows))]
    let value = unavailable("Native playback observation is unavailable on this platform");
    println!("{value}");
}

/// Call only on an owned background job; native capture is contained and bounded.
#[cfg(windows)]
pub(crate) fn run_microphone_test(
    executable: &std::path::Path,
    stop: &std::sync::atomic::AtomicBool,
    device_id: &str,
) -> (Value, bool) {
    crate::resident_worker::probe_microphone(executable, stop, device_id)
}

#[cfg(windows)]
pub(crate) fn run_microphone_action(
    executable: &std::path::Path,
    stop: &std::sync::atomic::AtomicBool,
    device_id: &str,
    operation: &str,
) -> (Value, bool) {
    crate::resident_worker::microphone_action(executable, stop, device_id, operation)
}

/// Owned contained helper, activated only after the parent assigns its job.
#[cfg(windows)]
pub fn print_action() {
    use std::io::Read;
    let mut input = std::io::stdin().lock();
    let mut gate = [0u8];
    let mut bytes = Vec::new();
    let value = if input.read_exact(&mut gate).is_ok()
        && gate == [0x56]
        && input
            .take(crate::resident_action_protocol::MAX_JOB_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .is_ok()
    {
        match crate::resident_action_protocol::decode_job(&bytes) {
            Ok(job) => crate::resident_windows::execute_action(&job),
            Err(_) => json!({"state":"rejected_before_write","error":"invalid_action_job"}),
        }
    } else {
        json!({"state":"rejected_before_write","error":"helper_not_activated"})
    };
    println!("{value}");
}

/// Decode a bounded native ICO. Never accept a URL, path, SVG or arbitrary MIME.
pub(crate) fn decode_icon(value: &str) -> Option<Vec<u8>> {
    if value.is_empty() || value.len() > 10924 || !value.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::new();
    for (index, chunk) in value.as_bytes().as_chunks::<4>().0.iter().enumerate() {
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
    fn catalog_snapshot(pid: u32, started: &str) -> Value {
        let mut value = unavailable("Playback is unavailable independently");
        value["catalog"] = json!({"available":true,"complete":true,"applications":[{"process_id":pid,"process_started":started,"executable_path":"C:\\Fixture\\Visible.exe","display_name":"Visible app","window_title":"Unrelated document title","icon_b64":null}],"skipped":0,"error":null});
        value["routing_capabilities"] = json!({"state":"available","preferred_input":"available","preferred_output":"available","stream_state":"unknown","app_microphone_mute":"unsupported","error":null});
        value["routing_devices"] = json!({"available":true,"complete":true,"capture":[{"id":"chosen","name":"Desk microphone"}],"render":[{"id":"speaker","name":"Speakers"}],"error":null});
        value
    }
    #[test]
    fn catalog_presence_pin_roundtrips_without_inventing_an_audio_endpoint() {
        let path = temp();
        let mut r = Registry::load(path.clone());
        r.observe(catalog_snapshot(42, "123"), Instant::now());
        let view = r.view(false);
        assert_eq!(view["available"], false);
        assert_eq!(view["catalog"]["available"], true);
        assert!(view["choices"].as_array().unwrap().is_empty());
        let selection = view["catalog"]["applications"][0]["selection_id"].clone();
        assert!(!view.to_string().contains("executable_path"));
        assert!(!view.to_string().contains("process_started"));
        assert_eq!(
            r.mutate(
                "pin",
                &json!({"revision":0,"selection_id":selection}).to_string()
            )
            .0,
            200
        );
        let disk: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert!(disk["entries"][0]["endpoint_id"].is_null());
        let entry = &r.view(false)["entries"][0];
        assert_eq!(entry["presence"], "visible_window");
        assert_eq!(entry["state"], "unknown");
        assert_eq!(entry["actions_enabled"], false);
        assert!(entry["sessions"].as_array().unwrap().is_empty());
        let loaded = Registry::load(path.clone());
        assert_eq!(loaded.entries, r.entries);
        assert!(loaded.load_error.is_none());
        let before = std::fs::read(&path).unwrap();
        assert_eq!(
            r.mutate(
                "pin",
                &json!({"revision":0,"selection_id":selection}).to_string()
            )
            .0,
            409
        );
        assert_eq!(std::fs::read(&path).unwrap(), before);
        clean_fixture(r);
    }
    #[test]
    fn catalog_changed_expired_and_delayed_selection_cannot_pin_or_route() {
        let path = temp();
        let mut r = Registry::load(path);
        r.observe(catalog_snapshot(42, "123"), Instant::now());
        let id = r.catalog_choices[0].0.clone();
        r.observe(catalog_snapshot(42, "124"), Instant::now());
        assert_eq!(
            r.mutate("pin", &json!({"revision":0,"selection_id":id}).to_string())
                .0,
            409
        );
        assert!(r.route_target(&id).is_none());
        let id = r.catalog_choices[0].0.clone();
        r.catalog_selections.get_mut(&id).unwrap().issued =
            Instant::now() - Duration::from_secs(16);
        assert!(r.route_target(&id).is_none());
        assert_eq!(
            r.mutate("pin", &json!({"revision":0,"selection_id":id}).to_string())
                .0,
            409
        );
        r.observe(
            catalog_snapshot(42, "124"),
            Instant::now() - Duration::from_secs(7),
        );
        let view = r.view(false);
        assert_eq!(view["catalog"]["available"], false);
        assert!(view["catalog"]["applications"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(view["routing_devices"]["capture"]
            .as_array()
            .unwrap()
            .is_empty());
        assert_eq!(view["routing_capabilities"]["state"], "unknown");
        clean_fixture(r);
    }
    #[test]
    fn catalog_bad_rows_preserve_partial_uncertainty_and_do_not_expose_paths() {
        let mut r = Registry::empty(PathBuf::new(), None);
        let mut snapshot = catalog_snapshot(42, "123");
        let mut invalid = snapshot["catalog"]["applications"][0].clone();
        invalid["executable_path"] = json!("https://invalid/app.exe");
        snapshot["catalog"]["applications"]
            .as_array_mut()
            .unwrap()
            .push(invalid);
        r.observe(snapshot, Instant::now());
        let view = r.view(false);
        assert_eq!(view["catalog"]["complete"], false);
        assert_eq!(view["catalog"]["applications"].as_array().unwrap().len(), 1);
        assert!(!view.to_string().contains("Fixture"));
    }
    #[test]
    fn saved_routing_selection_requires_one_exact_live_process_across_sources() {
        let path = temp();
        let mut r = Registry::load(path);
        let id = select(&mut r);
        assert_eq!(
            r.mutate("pin", &json!({"revision":0,"selection_id":id}).to_string())
                .0,
            200
        );
        r.observe(snapshot(123, "session-a"), Instant::now());
        assert!(r.view(false)["entries"][0]["routing_selection_id"]
            .as_str()
            .is_some());
        let mut observed = snapshot(123, "session-a");
        let mut catalog = catalog_snapshot(123, "123");
        catalog["catalog"]["applications"][0]["executable_path"] =
            json!("C:\\Fixture\\Example.exe");
        observed["catalog"] = catalog["catalog"].clone();
        r.observe(observed.clone(), Instant::now());
        assert!(r.view(false)["entries"][0]["routing_selection_id"]
            .as_str()
            .is_some());
        observed["catalog"]["applications"][0]["process_id"] = json!(124);
        r.observe(observed, Instant::now());
        assert!(r.view(false)["entries"][0]
            .get("routing_selection_id")
            .is_none());
        clean_fixture(r);
    }
    #[test]
    fn consumed_catalog_token_cannot_hide_an_ambiguous_saved_process() {
        let path = temp();
        let mut r = Registry::load(path);
        let mut observed = catalog_snapshot(42, "123");
        let mut second = observed["catalog"]["applications"][0].clone();
        second["process_id"] = json!(43);
        observed["catalog"]["applications"]
            .as_array_mut()
            .unwrap()
            .push(second);
        r.observe(observed, Instant::now());
        let id = r.catalog_choices[0].0.clone();
        assert_eq!(
            r.mutate("pin", &json!({"revision":0,"selection_id":id}).to_string())
                .0,
            200
        );
        assert_eq!(r.catalog_choices.len(), 1);
        assert!(r.view(false)["entries"][0]
            .get("routing_selection_id")
            .is_none());
        clean_fixture(r);
    }
    #[cfg(windows)]
    #[test]
    fn route_receipts_scope_principal_revision_and_never_dispatch_browser_paths() {
        let (mut r, requests, completions) = control_fixture();
        r.observe(catalog_snapshot(42, "123"), Instant::now());
        let selection = r.catalog_choices[0].0.clone();
        let body = json!({"revision":r.revision,"selection_id":selection,"flow":"capture","device_id":"chosen","roles":[0,1,2]});
        assert_eq!(
            r.route(true, false, "/api/apps/routes", "", &body.to_string())
                .unwrap()
                .0,
            401
        );
        let mut bad = body.clone();
        bad["executable_path"] = json!("C:\\Forged.exe");
        assert_eq!(
            r.route(true, false, "/api/apps/routes", "first", &bad.to_string())
                .unwrap()
                .0,
            400
        );
        assert!(requests.try_recv().is_err());
        bad = body.clone();
        bad["revision"] = json!(99);
        assert_eq!(
            r.route(true, false, "/api/apps/routes", "first", &bad.to_string())
                .unwrap()
                .0,
            409
        );
        let (status, receipt) = r
            .route(true, false, "/api/apps/routes", "first", &body.to_string())
            .unwrap();
        assert_eq!(status, 202);
        let id = receipt["operation_id"].as_str().unwrap().to_owned();
        let path = format!("/api/apps/routes/{id}");
        let mut queued = requests.try_recv().unwrap();
        assert_eq!(queued["revision"], r.revision);
        queued.as_object_mut().unwrap().remove("revision");
        let job = app_route::parse_job(queued.to_string().as_bytes()).unwrap();
        assert_eq!(job.executable_path, "C:\\Fixture\\Visible.exe");
        assert_eq!(r.route(false, true, &path, "second", "").unwrap().0, 404);
        assert_eq!(
            r.route(true, false, "/api/apps/routes", "first", &body.to_string())
                .unwrap()
                .0,
            409
        );
        let result = json!({"operation_id":id,"process_id":42,"flow":"capture","device_id":"chosen","state":"verified","write_attempted":true,"rollback_attempted":false,"rollback_verified":false,"roles":[{"role":0,"device_id":"chosen","verified":true},{"role":1,"device_id":"chosen","verified":true},{"role":2,"device_id":"chosen","verified":true}],"stream_state":"unknown","error":null});
        assert!(app_route::valid_result(&result, &job));
        let entry = r.entries[0].id.clone();
        let old = r.revision;
        assert_eq!(
            r.mutate("remove", &json!({"revision":old,"id":entry}).to_string())
                .0,
            200
        );
        assert_eq!(
            r.worker
                .as_ref()
                .unwrap()
                .revision
                .load(std::sync::atomic::Ordering::Acquire),
            r.revision
        );
        completions.send((id, Instant::now(), result)).unwrap();
        let result = r.route(false, true, &path, "first", "").unwrap().1;
        assert_eq!(result["state"], "outcome_unconfirmed");
        assert_eq!(result["error"], "route_configuration_changed");
        assert_eq!(result["stream_state"], "unknown");
        clean_fixture(r);
    }
    fn microphone_snapshot(id: &str, muted: bool) -> Value {
        let mut value = snapshot(123, "session-a");
        value["microphone"] = json!({"device_id":id,"name":"Desk microphone","state":if muted {"muted"} else {"unmuted"},"muted":muted,"age_ms":0,"error":null,"peak":null,"meter_source":"software","level_state":"unknown"});
        value
    }
    #[test]
    fn microphone_summary_exists_without_pins_and_is_independent_of_playback_failure() {
        let mut r = Registry::empty(PathBuf::new(), None);
        assert_eq!(r.view(true)["microphone"]["state"], "unknown");
        r.set_microphone_device(None);
        assert_eq!(r.view(true)["microphone"]["state"], "unconfigured");
        r.set_microphone_device(Some("chosen"));
        let mut observed = microphone_snapshot("chosen", true);
        observed["available"] = json!(false);
        observed["error"] = json!("Playback backend unavailable");
        r.observe(observed, Instant::now());
        let view = r.view(true);
        assert!(view["entries"].as_array().unwrap().is_empty());
        assert_eq!(view["available"], false);
        assert_eq!(view["microphone"]["device_id"], "chosen");
        assert_eq!(view["microphone"]["name"], "Desk microphone");
        assert_eq!(view["microphone"]["state"], "muted");
        assert!(view["microphone"]["age_ms"].as_u64().is_some());
    }
    #[test]
    fn microphone_rebind_fences_old_scans_even_when_identity_returns_to_the_same_id() {
        let mut r = Registry::empty(PathBuf::new(), None);
        r.set_microphone_device(Some("chosen"));
        r.observe(microphone_snapshot("chosen", false), Instant::now());
        assert_eq!(r.view(true)["microphone"]["state"], "unmuted");
        let before_rebind = Instant::now() - Duration::from_secs(1);
        r.set_microphone_device(Some("another"));
        r.set_microphone_device(Some("chosen"));
        r.observe(microphone_snapshot("chosen", true), before_rebind);
        assert_eq!(r.view(true)["microphone"]["state"], "unknown");
        r.observe(microphone_snapshot("another", true), Instant::now());
        assert_eq!(r.view(true)["microphone"]["state"], "unknown");
        assert_eq!(r.view(true)["microphone"]["device_id"], "chosen");
        r.observe(microphone_snapshot("chosen", true), Instant::now());
        assert_eq!(r.view(true)["microphone"]["state"], "muted");
        r.set_microphone_device(None);
        assert_eq!(r.view(true)["microphone"]["state"], "unconfigured");
        assert_eq!(r.view(true)["microphone"]["device_id"], Value::Null);
    }
    #[test]
    fn stale_malformed_and_failed_microphone_observations_stay_unknown_without_fallback() {
        let mut r = Registry::empty(PathBuf::new(), None);
        r.set_microphone_device(Some("chosen"));
        let mut failed = microphone_snapshot("chosen", false);
        failed["microphone"]["state"] = json!("disconnected");
        failed["microphone"]["muted"] = Value::Null;
        failed["microphone"]["error"] = json!("Selected device is disconnected");
        r.observe(failed, Instant::now());
        assert_eq!(r.view(true)["microphone"]["state"], "disconnected");
        r.checked = Some(Instant::now() - Duration::from_secs(7));
        // A scan may be old without a binding change; freshness remains required.
        r.microphone_changed = r.checked.unwrap() - Duration::from_secs(1);
        assert_eq!(r.view(true)["microphone"]["state"], "unknown");
        assert_eq!(r.view(true)["microphone"]["device_id"], "chosen");
        let mut malformed = microphone_snapshot("chosen", false);
        malformed["microphone"]["muted"] = json!(true);
        r.observe(malformed, Instant::now());
        assert_eq!(r.view(true)["microphone"]["state"], "unknown");
        let mut unavailable = unavailable("Helper failed");
        unavailable["microphone"] = audio_probe::unknown(Some("chosen"), "Helper failed");
        r.observe(unavailable, Instant::now());
        assert_eq!(r.view(true)["microphone"]["state"], "unknown");
        assert_eq!(r.view(true)["microphone"]["error"], "Helper failed");
    }
    fn legacy_registry() -> Registry {
        let path = temp();
        let saved = json!({"schema_version":1,"revision":7,"entries":[{"id":"a".repeat(64),"label":"Example","endpoint_id":"speaker-fixture","executable_path":"C:\\Fixture\\Example.exe"}]});
        std::fs::write(&path, saved.to_string()).unwrap();
        Registry::load(path)
    }
    #[test]
    fn microphone_association_loads_legacy_entries_roundtrips_and_explicitly_clears() {
        let mut r = legacy_registry();
        let original_entry = r.entries[0].clone();
        let config = r.path.with_file_name("config.json");
        std::fs::write(&config, b"separate-global-microphone").unwrap();
        assert_eq!(r.view(false)["entries"][0]["device_id"], Value::Null);
        for capability in ["start_voice", "end_voice", "input_routing"] {
            assert_eq!(
                r.view(false)["entries"][0]["capabilities"][capability],
                "not_implemented"
            );
        }
        for (revision, device_id) in [(7, "mic-fixture-a"), (8, "mic-fixture-b")] {
            let body = json!({"revision":revision,"id":original_entry.id,"device_id":device_id});
            let (status, view) = r.bind(&body.to_string(), |requested| {
                assert_eq!(requested, device_id);
                Ok(())
            });
            assert_eq!(status, 200);
            assert_eq!(view["entries"][0]["device_id"], device_id);
            assert_eq!(r.view(true)["entries"][0]["device_id"], device_id);
            let disk: Value = serde_json::from_slice(&std::fs::read(&r.path).unwrap()).unwrap();
            assert_eq!(disk["revision"], revision + 1);
            assert_eq!(disk["entries"][0]["device_id"], device_id);
            r = Registry::load(r.path.clone());
            assert_eq!(r.entries[0].device_id.as_deref(), Some(device_id));
        }
        assert_eq!(
            r.bind(
                &json!({"revision":9,"id":original_entry.id,"device_id":null}).to_string(),
                |_| panic!("Clearing must never choose or probe another microphone")
            )
            .0,
            200
        );
        let reopened = Registry::load(r.path.clone());
        assert_eq!(reopened.revision, 10);
        assert_eq!(reopened.entries[0], original_entry);
        assert_eq!(
            std::fs::read(config).unwrap(),
            b"separate-global-microphone"
        );
        std::fs::remove_dir_all(r.path.parent().unwrap()).unwrap();
    }
    #[test]
    fn microphone_association_rejects_bad_stale_and_unknown_targets_before_validation() {
        let mut r = legacy_registry();
        let bytes = std::fs::read(&r.path).unwrap();
        let id = r.entries[0].id.clone();
        let mut invalid = vec![
            (json!({"revision":6,"id":id,"device_id":"mic-fixture"}), 409),
            (json!({"revision":7,"id":id}), 400),
            (json!({"id":id,"device_id":"mic-fixture"}), 400),
            (
                json!({"revision":null,"id":id,"device_id":"mic-fixture"}),
                400,
            ),
            (json!({"revision":7,"device_id":"mic-fixture"}), 400),
            (
                json!({"revision":7,"id":id,"device_id":"mic-fixture","executable_path":"C:\\Other.exe"}),
                400,
            ),
            (
                json!({"revision":7,"id":"invalid","device_id":"mic-fixture"}),
                400,
            ),
            (
                json!({"revision":7,"id":"b".repeat(64),"device_id":"mic-fixture"}),
                404,
            ),
            (json!([]), 400),
        ];
        for device_id in [
            json!(""),
            json!("   "),
            json!("x".repeat(513)),
            json!("mic\nfixture"),
            json!("mic\0fixture"),
            json!(123),
            json!(false),
            json!([]),
            json!({}),
        ] {
            invalid.push((json!({"revision":7,"id":id,"device_id":device_id}), 400));
        }
        for (body, expected) in invalid {
            assert_eq!(
                r.bind(&body.to_string(), |_| panic!(
                    "Invalid request probed a microphone"
                ))
                .0,
                expected,
                "{body}"
            );
        }
        assert_eq!(
            r.bind("not-json", |_| panic!("Invalid JSON probed a microphone"))
                .0,
            400
        );
        assert_eq!(r.revision, 7);
        assert_eq!(r.entries[0].device_id, None);
        assert_eq!(std::fs::read(&r.path).unwrap(), bytes);
        std::fs::remove_dir_all(r.path.parent().unwrap()).unwrap();
    }
    #[test]
    fn unavailable_microphone_and_failed_save_preserve_association_revision_and_bytes() {
        let mut r = legacy_registry();
        let path = r.path.clone();
        let bytes = std::fs::read(&path).unwrap();
        let entries = r.entries.clone();
        let body =
            json!({"revision":7,"id":r.entries[0].id,"device_id":"missing-microphone"}).to_string();
        let (status, value) = r.bind(&body, |id| {
            assert_eq!(id, "missing-microphone");
            Err("Selected microphone is unavailable".into())
        });
        assert_eq!(status, 409);
        assert_eq!(value["error"], "Selected microphone is unavailable");
        // A file as the destination's parent makes atomic_write fail on every platform.
        r.path = path.join("cannot-write-here.json");
        assert_eq!(r.bind(&body, |_| Ok(())).0, 500);
        assert_eq!(r.revision, 7);
        assert_eq!(r.entries, entries);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn malformed_saved_microphone_association_preserves_original_data() {
        let r = legacy_registry();
        let path = r.path.clone();
        let mut saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for value in [
            json!(false),
            json!(""),
            json!("x".repeat(513)),
            json!("mic\nfixture"),
        ] {
            saved["entries"][0]["device_id"] = value;
            let bytes = saved.to_string();
            std::fs::write(&path, &bytes).unwrap();
            let mut reopened = Registry::load(path.clone());
            assert!(reopened.load_error.is_some());
            assert_eq!(
                reopened
                    .bind("{}", |_| panic!("Invalid storage probed a microphone"))
                    .0,
                503
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), bytes);
        }
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
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
            commands: std::sync::mpsc::sync_channel(1).0,
            results: std::sync::mpsc::channel().1,
            alive: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            thread: None,
            microphone: std::sync::Arc::new(std::sync::Mutex::new(None)),
            revision: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
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
    fn control_fixture() -> (
        Registry,
        std::sync::mpsc::Receiver<Value>,
        std::sync::mpsc::Sender<(String, Instant, Value)>,
    ) {
        let path = temp();
        let mut r = Registry::load(path);
        let selection = select(&mut r);
        assert_eq!(
            r.mutate(
                "pin",
                &json!({"revision":0,"selection_id":selection}).to_string()
            )
            .0,
            200
        );
        r.observe(snapshot(123, "session-a"), Instant::now());
        let (commands, requests) = std::sync::mpsc::sync_channel(1);
        let (completions, results) = std::sync::mpsc::channel();
        r.worker = Some(Worker {
            receiver: std::sync::mpsc::channel().1,
            commands,
            results,
            alive: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true)),
            stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            thread: None,
            microphone: std::sync::Arc::new(std::sync::Mutex::new(None)),
            revision: std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0)),
        });
        (r, requests, completions)
    }
    fn prepare_displayed(r: &mut Registry) -> Value {
        let view = r.view(true);
        let row = &view["entries"][0];
        assert_eq!(row["sessions"][0]["actions_enabled"], true);
        let (code,p)=r.prepare_action("fixture-browser",&json!({"entry_id":row["id"],"session_reference":row["sessions"][0]["session_reference"],"action":"mute"}).to_string());
        assert_eq!(code, 200);
        p
    }
    fn execute_body(prepared: &Value) -> String {
        json!({"operation_id":prepared["operation_id"],"capability":prepared["capability"]})
            .to_string()
    }
    fn clean_fixture(r: Registry) {
        let path = r.path.clone();
        drop(r);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
    #[test]
    fn displayed_identity_changes_reject_before_dispatch() {
        for change in [
            "pid",
            "start",
            "instance",
            "path",
            "endpoint",
            "duplicate",
            "remove",
            "stale",
        ] {
            let (mut r, requests, _) = control_fixture();
            let prepared = prepare_displayed(&mut r);
            let mut changed = snapshot(123, "session-a");
            match change {
                "pid" => changed["sessions"][0]["process_id"] = json!(999),
                "start" => changed["sessions"][0]["process_started"] = json!("987654"),
                "instance" => changed["sessions"][0]["session_instance_id"] = json!("replacement"),
                "path" => changed["sessions"][0]["executable_path"] = json!("C:\\Other\\App.exe"),
                "endpoint" => changed["sessions"][0]["endpoint_id"] = json!("other-output"),
                "duplicate" => {
                    let mut duplicate = changed["sessions"][0].clone();
                    duplicate["process_id"] = json!(999);
                    changed["sessions"].as_array_mut().unwrap().push(duplicate);
                }
                "remove" => r.entries.clear(),
                _ => {}
            }
            r.observe(changed, Instant::now());
            if change == "stale" {
                r.checked = Some(Instant::now() - Duration::from_secs(7));
            }
            let (code, receipt) = r.execute_action("fixture-browser", &execute_body(&prepared));
            assert_eq!(code, 409, "{change}");
            assert_eq!(receipt["state"], "rejected_before_write", "{change}");
            assert!(requests.try_recv().is_err());
            clean_fixture(r);
        }
    }
    #[test]
    fn explicit_execute_is_one_use_and_completions_are_not_coalesced() {
        let (mut r, requests, completions) = control_fixture();
        let prepared = prepare_displayed(&mut r);
        let id = prepared["operation_id"].as_str().unwrap();
        assert!(requests.try_recv().is_err());
        assert_eq!(
            r.execute_action("fixture-browser", &execute_body(&prepared))
                .0,
            202
        );
        let job = requests.try_recv().unwrap();
        assert_eq!(job["requested_muted"], true);
        assert_eq!(job["target"]["process_id"], 123);
        assert_eq!(
            r.execute_action("fixture-browser", &execute_body(&prepared))
                .0,
            409
        );
        assert!(requests.try_recv().is_err());
        let (snapshots, frames) = std::sync::mpsc::channel();
        r.worker.as_mut().unwrap().receiver = frames;
        snapshots
            .send((Instant::now(), snapshot(123, "new-session")))
            .unwrap();
        snapshots
            .send((Instant::now(), snapshot(123, "newer-session")))
            .unwrap();
        completions.send((id.into(),Instant::now()-Duration::from_millis(250),json!({"operation_id":id,"requested_muted":true,"state":"verified","observed_muted":true,"error":null}))).unwrap();
        r.poll();
        let receipt = r.actions.view("fixture-browser", id).1;
        assert_eq!(receipt["state"], "verified");
        assert!(receipt["readback_age_ms"].as_u64().unwrap() >= 250);
        assert_eq!(r.sessions[0].value["session_instance_id"], "newer-session");
        clean_fixture(r);
    }
    #[test]
    fn queue_failure_and_owner_stop_are_distinct() {
        let (mut r, requests, _) = control_fixture();
        let p = prepare_displayed(&mut r);
        drop(requests);
        let receipt = r.execute_action("fixture-browser", &execute_body(&p)).1;
        assert_eq!(receipt["state"], "rejected_before_write");
        clean_fixture(r);
        let (mut r, _requests, _) = control_fixture();
        let p = prepare_displayed(&mut r);
        let id = p["operation_id"].as_str().unwrap();
        assert_eq!(
            r.execute_action("fixture-browser", &execute_body(&p)).0,
            202
        );
        r.worker
            .as_ref()
            .unwrap()
            .alive
            .store(false, std::sync::atomic::Ordering::Release);
        r.poll();
        let receipt = r.actions.view("fixture-browser", id).1;
        assert_eq!(receipt["state"], "outcome_unconfirmed");
        assert_eq!(r.view(false)["entries"][0]["actions_enabled"], false);
        clean_fixture(r);
    }
    #[test]
    fn action_prepare_rejects_raw_paths_stale_references_and_changed_session() {
        let (mut r, _requests, _) = control_fixture();
        let row = r.view(true)["entries"][0].clone();
        let body = json!({"entry_id":row["id"],"session_reference":row["sessions"][0]["session_reference"],"action":"mute"});
        for field in [
            "executable_path",
            "process_id",
            "endpoint_id",
            "requested_muted",
        ] {
            let mut altered = body.clone();
            altered[field] = json!("client-supplied");
            assert_eq!(
                r.prepare_action("fixture-browser", &altered.to_string()).0,
                400
            );
        }
        r.observe(snapshot(999, "session-a"), Instant::now());
        assert_eq!(
            r.prepare_action("fixture-browser", &body.to_string()).0,
            409
        );
        clean_fixture(r);
    }
    #[test]
    fn maximum_summary_preserves_sessions_inside_proved_response_budget() {
        let (mut r, _requests, _) = control_fixture();
        let base = r.entries[0].clone();
        r.entries = (0..MAX_ENTRIES)
            .map(|i| {
                let mut entry = base.clone();
                entry.id = format!("{i:064x}");
                entry.label = "\\".repeat(80);
                entry
            })
            .collect();
        let mut scan = snapshot(123, "session-a");
        scan["sessions"] = json!((0..MAX_SESSIONS)
            .map(|i| {
                let mut v = snapshot(123, &format!("session-{i}"))["sessions"][0].clone();
                v["display_name"] = json!("\\".repeat(160));
                v
            })
            .collect::<Vec<_>>());
        r.observe(scan, Instant::now());
        // Test the already-validated resource-size upper bound independently of
        // icon decoding fixtures. Images are optional; identities may not vanish.
        for session in &mut r.sessions {
            session.value["icon_b64"] = json!("A".repeat(10924));
        }
        let summary = r.view(true);
        assert!(summary.to_string().len() + 1024 < 128 * 1024);
        assert_eq!(summary["entries"].as_array().unwrap().len(), MAX_ENTRIES);
        for row in summary["entries"].as_array().unwrap() {
            assert_eq!(row["sessions"].as_array().unwrap().len(), MAX_SESSIONS);
            assert!(row["sessions"]
                .as_array()
                .unwrap()
                .iter()
                .all(|session| session.get("icon_b64").is_none()
                    && session["session_reference"].is_string()));
        }
        clean_fixture(r);
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
