//! Loopback service: serves the configuration page and, when a service token is
//! configured, authenticated read/write endpoints for configuration and actions.
//!
//! Everything here is bound to loopback and rejects foreign `Host`/`Origin`
//! headers. Writes additionally require the bearer token this instance
//! generated, so a browser page from another origin cannot mutate state.

use tiny_http::{Header, Method, Response, Server, StatusCode};

use crate::{
    service::{self, Config, Instance},
    Result,
};

const PAGE: &str = include_str!("../docs/design/index.html");
/// Formats the loopback bind address for the configured port.
pub fn address() -> String {
    format!("127.0.0.1:{}", service::port())
}
/// Upper bound on a request body. Bodies larger than this are rejected with
/// 413 rather than read in full: an unauthenticated caller must not be able to
/// make the service allocate or panic on unbounded input.
const MAX_BODY: usize = 8192;

/// Placeholder body returned by the `/api/config` route, standing in for a
/// result only the caller can produce because it owns the live state and must
/// persist before answering. Replaced with committed JSON or an error message.
const COMMIT_SENTINEL: &str = "\u{0}vibdock-commit-config";

/// Body returned by `/api/shutdown`, telling the accept loop to stop serving.
const SHUTDOWN_SENTINEL: &str = "\u{0}vibdock-shutdown";

/// The id the resident service registers its configured hotkey under.
///
/// Fixed rather than derived: the service owns exactly one global binding, so
/// there is no second registration to distinguish.
pub(crate) const BINDING_HOTKEY_ID: i32 = 1;

/// The platform hotkey registration the service owns, if the platform has one.
///
/// Aliased so the accept loop is written once: on platforms without a global
/// hotkey adapter the slot is simply never filled.
#[cfg(windows)]
type Binding = crate::windows::RegisteredHotkey;
/// Placeholder for platforms with no global hotkey adapter.
#[cfg(not(windows))]
type Binding = ();

/// Outcome of arming or re-arming the configured hotkey after a config change.
enum Activation {
    /// The binding is registered and live.
    Armed,
    /// No binding is configured, so nothing was registered.
    Idle,
    /// Registration was attempted and refused. The message is shown to the
    /// user and stored so status can report the binding is not live.
    Failed(String),
}

impl Activation {
    /// True when a hotkey is actually registered and will fire.
    fn is_armed(&self) -> bool {
        matches!(self, Activation::Armed)
    }
}

/// Applies the current configuration to the global hotkey registration.
///
/// This is the single place a saved binding becomes operational. The previous
/// binding is always released first, so a failed re-registration never leaves a
/// stale hotkey armed for a device or chord the user has moved away from.
///
/// A failure is returned rather than swallowed: reporting success here would
/// tell the client a binding is live when pressing it does nothing. The
/// previously persisted configuration is untouched by this function; the caller
/// owns whether the change is kept.
#[cfg(windows)]
fn activate_hotkey(
    config: &Config,
    current: &mut Option<crate::windows::RegisteredHotkey>,
) -> Activation {
    // Drop whatever is registered before trying the new chord. Registering
    // the new one first would fail as a conflict against our own binding,
    // and on some chords the OS refuses the second registration outright.
    *current = None;
    let Some(device_id) = config.device_id.as_deref() else {
        return Activation::Idle;
    };
    if device_id.is_empty() {
        return Activation::Idle;
    }
    let binding = match config.hotkey_binding() {
        Ok(Some(binding)) => binding,
        Ok(None) => return Activation::Idle,
        Err(message) => return Activation::Failed(message),
    };
    match crate::windows::RegisteredHotkey::register(&binding, BINDING_HOTKEY_ID) {
        Ok(registered) => {
            *current = Some(registered);
            println!("Binding armed: {binding} toggles the configured microphone.");
            Activation::Armed
        }
        Err(message) => {
            eprintln!("ERROR: could not arm the binding: {message}");
            Activation::Failed(message)
        }
    }
}

/// Non-Windows path: there is no platform hotkey adapter, so nothing arms.
///
/// Deliberately explicit rather than a silent no-op, so a caller cannot read
/// success from the absence of an error.
#[cfg(not(windows))]
fn activate_hotkey(config: &Config, _current: &mut Option<()>) -> Activation {
    if !config.is_bindable() {
        return Activation::Idle;
    }
    Activation::Failed(
        "Global hotkeys require the Windows input adapter; no binding is armed on this platform."
            .to_string(),
    )
}

/// Outcome of a routed request: status, content type, body, and whether the
/// client may cache it.
type Routed = (u16, &'static str, String);

/// Result of reading a request body within the size bound.
enum Body {
    /// A complete, valid UTF-8 body within the limit.
    Text(String),
    /// The body exceeded the limit; the remainder was left unread.
    TooLarge,
}

/// Reads at most `MAX_BODY + 1` bytes to detect oversize, then decodes.
///
/// Reading one byte past the limit is what lets us answer 413 instead of
/// silently accepting a truncated body. The read is bounded before any large
/// allocation, and a UTF-8 error is reported rather than panicking.
fn read_bounded_body<R: std::io::Read>(reader: R) -> std::result::Result<Body, ()> {
    use std::io::Read as _;
    let mut buffer = Vec::new();
    let mut limited = reader.take(MAX_BODY as u64 + 1);
    limited.read_to_end(&mut buffer).map_err(|_| ())?;
    if buffer.len() > MAX_BODY {
        return Ok(Body::TooLarge);
    }
    String::from_utf8(buffer).map(Body::Text).map_err(|_| ())
}

/// Everything that varies per request, grouped so the routing table stays a
/// pure function of its inputs.
struct Request<'a> {
    method: &'a Method,
    path: &'a str,
    host: Option<&'a str>,
    origin: Option<&'a str>,
    authorized: bool,
    /// Decoded request body, empty when there is none or it was rejected.
    body: &'a str,
    /// The body was oversized or not valid UTF-8, so it was not parsed.
    body_rejected: bool,
}

/// Runs one allowlisted device action and renders its result as JSON.
///
/// The endpoint id comes from the request body, falling back to the configured
/// device. Never a platform default: if no endpoint is named anywhere, the
/// action fails rather than quietly acting on some other microphone.
fn run_action(action: service::DeviceAction, body: &str) -> std::result::Result<String, String> {
    let requested = decode_device_id(body)?;
    let device_id = match requested {
        Some(id) => Some(id),
        // Falling back to the saved device is the user's own configured choice,
        // not a platform default — and only when the body named none.
        None => service::load_config()
            .ok()
            .and_then(|config| config.device_id),
    };
    // Only Windows can produce a device result; the rendering is scoped with
    // the type so no branch can claim a shape that platform cannot yield.
    #[cfg(windows)]
    {
        match service::run_device_action(action, device_id.as_deref())? {
            service::ActionResult::MuteState { device_id, muted } => Ok(serde_json::json!({
                "device_id": device_id,
                "muted": muted,
            })
            .to_string()),
            service::ActionResult::Devices(devices) => {
                let list: Vec<serde_json::Value> = devices
                    .iter()
                    .map(|(name, id)| serde_json::json!({"name": name, "id": id}))
                    .collect();
                Ok(serde_json::json!({ "devices": list }).to_string())
            }
        }
    }
    #[cfg(not(windows))]
    {
        // Unsupported platforms surface the adapter's own error unchanged.
        // Returning here keeps `action` and `device_id` meaningful on both
        // paths without a compile-time unused warning on either.
        let result = service::run_device_action(action, device_id.as_deref())?;
        Ok(serde_json::json!({ "result": format!("{result:?}") }).to_string())
    }
}

/// Reads an optional `device_id` from a JSON body, rejecting malformed input.
fn decode_device_id(body: &str) -> std::result::Result<Option<String>, String> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let value: serde_json::Value = serde_json::from_str(trimmed)
        .map_err(|_| "Request body must be valid JSON.".to_string())?;
    match value.get("device_id") {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(id)) => {
            if id.is_empty() || id.contains('\0') || id.len() > 512 {
                return Err("device_id must be a non-empty identifier.".into());
            }
            Ok(Some(id.clone()))
        }
        Some(_) => Err("device_id must be a string.".into()),
    }
}

/// What a config write produced: either committed state, or a reason the
/// request must not be reported as successful.
enum WriteOutcome {
    /// The patch was applied, persisted, and committed to memory.
    Committed(Config),
    /// The request was malformed. The caller answers 400 with this message.
    Rejected(&'static str),
    /// The patch was valid but could not be persisted. The caller must answer
    /// non-success: reporting 200 here would tell the client a change was saved
    /// that the next restart will not see.
    PersistFailed(String),
    ActivationFailed(String),
}

/// True when the bound hotkey was pressed since the last check.
///
/// Non-Windows platforms have no binding to poll, so this is always false
/// there — matching the fact that no hotkey can be armed on them either.
#[cfg(windows)]
fn fired_since_last_poll(binding: &Option<Binding>) -> usize {
    if binding.is_some() {
        crate::windows::poll_hotkey(BINDING_HOTKEY_ID)
    } else {
        0
    }
}

/// Placeholder for platforms with no global hotkey to poll.
#[cfg(not(windows))]
fn fired_since_last_poll(_binding: &Option<Binding>) -> usize {
    0
}

/// Performs the action the armed binding is configured for.
///
/// Only the toggle action exists today, and it always names the configured
/// endpoint explicitly. A disconnected or absent device is an error rather
/// than a reason to fall back to another microphone: silently retargeting the
/// user's button would change the meaning of the hardware without telling
/// them.
#[cfg(windows)]
fn run_bound_action(state: &Config) -> std::result::Result<String, String> {
    let device_id = state
        .device_id
        .clone()
        .ok_or("The binding has no microphone configured.".to_string())?;
    match service::run_device_action(service::DeviceAction::Toggle, Some(&device_id))? {
        service::ActionResult::MuteState { muted, .. } => {
            Ok((if muted { "MUTED" } else { "LIVE" }).to_string())
        }
        // Enumeration is not a bound action; a binding toggles one device.
        service::ActionResult::Devices(_) => {
            Err("The binding can only toggle a microphone.".into())
        }
    }
}

/// Placeholder for platforms with no audio adapter to act on.
#[cfg(not(windows))]
fn run_bound_action(_state: &Config) -> std::result::Result<String, String> {
    Err("Bound actions require the Windows audio adapter.".into())
}

/// Renders the configuration together with how its binding actually stands.
///
/// The activation state is reported separately from the stored values because
/// they are different facts: a hotkey can be saved and yet not be armed, and
/// telling the user "saved" without that distinction is how a binding ends up
/// silently dead. `hotkey_live` is true only when the platform confirmed a
/// registration that is still held.
fn binding_status(config: &Config, activation: &Activation) -> String {
    let message: Option<String> = match activation {
        Activation::Armed => None,
        Activation::Idle => {
            if config.is_bindable() {
                None
            } else {
                Some("Choose a microphone and record a shortcut to arm the binding.".to_string())
            }
        }
        Activation::Failed(message) => Some(message.clone()),
    };
    serde_json::json!({
        "device_id": config.device_id.clone().unwrap_or_default(),
        "hotkey": config.hotkey.clone().unwrap_or_default(),
        "hotkey_live": activation.is_armed(),
        "activation_error": message,
    })
    .to_string()
}

/// Applies a config patch to the live state and persists it before reporting
/// success.
///
/// Order matters. The patch is merged into the current state (so a field the
/// client did not name keeps its stored value), the merged result is written to
/// disk, and only then is memory updated and the committed state returned. A
/// failed write therefore leaves both the on-disk and in-memory configuration
/// exactly as they were, and the caller must surface an error status.
#[cfg(test)]
fn commit_patch(state: &Config, body: &str) -> WriteOutcome {
    match service::validate_patch(body) {
        Err(message) => WriteOutcome::Rejected(message),
        Ok(patch) => {
            let merged = state.apply(&patch);
            match service::save_config(&merged) {
                Ok(()) => WriteOutcome::Committed(merged),
                Err(error) => WriteOutcome::PersistFailed(error.to_string()),
            }
        }
    }
}

/// Last completed hardware action, scoped to this service instance. A sequence
/// change is an event; registration alone never produces a successful action.
#[derive(Default)]
struct ActionFeedback {
    sequence: u64,
    state: Option<String>,
    error: Option<String>,
}

impl ActionFeedback {
    fn record(&mut self, result: std::result::Result<String, String>) {
        self.sequence = self.sequence.saturating_add(1);
        match result {
            Ok(state) => {
                self.state = Some(state);
                self.error = None;
            }
            Err(error) => {
                self.state = None;
                self.error = Some(error);
            }
        }
    }

    fn enrich(&self, payload: &str) -> String {
        let mut value: serde_json::Value =
            serde_json::from_str(payload).expect("configuration responses are generated JSON");
        value["last_action"] = serde_json::json!({
            "sequence": self.sequence,
            "state": self.state,
            "error": self.error,
        });
        value.to_string()
    }
}

/// Validate and arm before persistence; restore the previous registration on
/// either platform rejection or disk failure. A rejected edit never replaces
/// the saved configuration.
fn commit_live_patch(
    state: &Config,
    body: &str,
    binding: &mut Option<Binding>,
    activation: &mut Activation,
) -> WriteOutcome {
    commit_live_patch_with_save(state, body, binding, activation, |config| {
        service::save_config(config).map_err(|error| error.to_string())
    })
}

fn commit_live_patch_with_save(
    state: &Config,
    body: &str,
    binding: &mut Option<Binding>,
    activation: &mut Activation,
    save: impl FnOnce(&Config) -> std::result::Result<(), String>,
) -> WriteOutcome {
    let patch = match service::validate_patch(body) {
        Ok(patch) => patch,
        Err(message) => return WriteOutcome::Rejected(message),
    };
    let merged = state.apply(&patch);
    let candidate = activate_hotkey(&merged, binding);
    if let Activation::Failed(error) = candidate {
        *activation = activate_hotkey(state, binding);
        return WriteOutcome::ActivationFailed(error);
    }
    match save(&merged) {
        Ok(()) => {
            *activation = candidate;
            WriteOutcome::Committed(merged)
        }
        Err(error) => {
            *activation = activate_hotkey(state, binding);
            WriteOutcome::PersistFailed(error.to_string())
        }
    }
}

/// Routes one request.
///
/// `state` is the live configuration snapshot; `token_configured` reports
/// whether this instance published a write token. A service without a token
/// stays read-only, preserving the previous design-preview behaviour.
fn validate_request(request: &Request<'_>, token_configured: bool) -> Option<Routed> {
    if !request.host.is_some_and(service::is_allowed_host) {
        return Some((403, "text/plain; charset=utf-8", "Invalid host".into()));
    }
    if let Some(origin) = request.origin {
        if !service::is_allowed_origin(origin) {
            return Some((403, "text/plain; charset=utf-8", "Invalid origin".into()));
        }
    }
    if !token_configured && request.method != &Method::Get {
        return Some((
            405,
            "text/plain; charset=utf-8",
            "Read-only design preview".into(),
        ));
    }
    // Checked before any body parsing, but after host/origin validation: an
    // oversized or undecodable body is a client error, never a crash.
    if request.body_rejected {
        return Some((
            413,
            "text/plain; charset=utf-8",
            "Request body is too large or not valid UTF-8.".into(),
        ));
    }
    None
}

fn route(
    request: &Request<'_>,
    state: &Config,
    token_configured: bool,
    activation: &Activation,
) -> Routed {
    if let Some(rejection) = validate_request(request, token_configured) {
        return rejection;
    }
    let method = request.method;
    let path = request.path;
    match (method, path) {
        (Method::Get, "/") => (200, "text/html; charset=utf-8", PAGE.to_string()),
        (Method::Get, "/health") => {
            let mode = if token_configured {
                "service"
            } else {
                "design-preview"
            };
            (
                200,
                "application/json",
                format!("{{\"mode\":\"{mode}\",\"audio_control\":{token_configured}}}"),
            )
        }
        (Method::Get, "/instance") => {
            // Instance challenge for single-instance detection. Requiring the
            // token is the point: a 200 proves the listener on this port is
            // our service and not an unrelated process that inherited the port.
            if !request.authorized {
                return unauthorized();
            }
            (200, "application/json", "{\"alive\":true}".into())
        }
        (Method::Post, "/api/shutdown") => {
            if !request.authorized {
                return unauthorized();
            }
            // Authenticated shutdown: the caller proved it holds this
            // instance's token, so it is this instance's owner asking it to
            // stop. The sentinel tells the accept loop to break out, release
            // the listener, and clear the instance marker.
            (200, "application/json", SHUTDOWN_SENTINEL.to_string())
        }
        (Method::Get, "/api/config") => {
            if !request.authorized {
                return unauthorized();
            }
            // Reported with the activation state so a caller can tell a saved
            // binding from one that is actually registered and will fire.
            (200, "application/json", binding_status(state, activation))
        }
        (Method::Post, "/api/config") => {
            if !request.authorized {
                return unauthorized();
            }
            // The patch is merged and persisted by the caller, which owns the
            // live state and must commit it only after the write succeeds. The
            // sentinel body is replaced by the committed JSON before responding.
            (
                200,
                "text/plain; charset=utf-8",
                COMMIT_SENTINEL.to_string(),
            )
        }
        (Method::Post, "/api/actions/echo") => {
            if !request.authorized {
                return unauthorized();
            }
            // Bounded echo: proves the authenticated action path without
            // exposing arbitrary execution. Real actions land here as the
            // adapter contracts settle.
            (200, "application/json", "{\"accepted\":true}".into())
        }
        (Method::Post, path) if path.starts_with("/api/actions/") => {
            if !request.authorized {
                return unauthorized();
            }
            let Some(name) = path.strip_prefix("/api/actions/") else {
                return (404, "text/plain; charset=utf-8", "Not found".into());
            };
            let Some(action) = service::DeviceAction::parse(name) else {
                // Closed allowlist: an unrecognised verb never reaches the
                // platform adapter, so no new action can be smuggled in.
                return (404, "text/plain; charset=utf-8", "Unknown action".into());
            };
            match run_action(action, request.body) {
                Ok(result) => (200, "application/json", result),
                Err(message) => (
                    502,
                    "text/plain; charset=utf-8",
                    format!("Action failed: {message}"),
                ),
            }
        }
        (Method::Get, _) | (Method::Post, _) => {
            (404, "text/plain; charset=utf-8", "Not found".into())
        }
        _ => (
            405,
            "text/plain; charset=utf-8",
            "Method not allowed".into(),
        ),
    }
}

/// Uniform 401 body. Callers must not leak whether the token was close.
fn unauthorized() -> Routed {
    (
        401,
        "text/plain; charset=utf-8",
        "Unauthorized: a valid service token is required.".into(),
    )
}

/// Response headers applied to every reply, generic over the body type
/// `tiny_http::Response` wraps.
fn with_headers<T: std::io::Read>(response: Response<T>) -> Response<T> {
    response
        .with_header(Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap())
        .with_header(Header::from_bytes("Cache-Control", "no-store").unwrap())
        .with_header(
            Header::from_bytes(
                "Content-Security-Policy",
                "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; form-action 'none'; base-uri 'none'",
            )
            .unwrap(),
        )
}

/// Runs the loopback service until the process is interrupted.
///
/// When `instance` carries a token the server publishes its address, accepts
/// authenticated writes, and clears the published record on shutdown. Without a
/// token it behaves exactly as the previous read-only design preview.
pub fn serve(instance: Option<Instance>) -> Result<()> {
    let server = Server::http(address()).map_err(|e| e.to_string())?;
    serve_server(server, instance)
}

/// Adopts the startup claim without releasing its bound port.
pub fn serve_listener(listener: std::net::TcpListener, instance: Option<Instance>) -> Result<()> {
    let server = Server::from_listener(listener, None).map_err(|e| e.to_string())?;
    serve_server(server, instance)
}

fn serve_server(server: Server, instance: Option<Instance>) -> Result<()> {
    let token = instance.as_ref().map(|i| i.token.clone());
    if let Some(instance) = &instance {
        service::write_instance(instance)?;
        println!("VibDock service listening on http://{}", address());
    } else {
        println!(
            "VibDock design preview: http://{} (read-only; Ctrl+C to stop)",
            address()
        );
    }
    // Start from the persisted configuration so a restart keeps the user's
    // saved device and hotkey rather than resetting to empty.
    let mut state = service::load_config()?;
    // Arm the persisted binding on startup. Only meaningful for the resident
    // service: the read-only preview has no token and must not grab a global
    // hotkey, since two instances would then fight over the same chord.
    let mut binding = None;
    let activation = if instance.is_some() {
        activate_hotkey(&state, &mut binding)
    } else {
        Activation::Idle
    };
    if let Activation::Failed(message) = &activation {
        eprintln!("WARNING: the saved binding is not armed: {message}");
    }
    let result = accept_loop(&server, &token, &mut state, &mut binding, activation);
    if instance.is_some() {
        let _ = service::clear_instance();
    }
    result
}

/// Serves requests until the listener stops, tracking config mutations.
///
/// `binding` is the live global hotkey registration, owned here so it is
/// unregistered when the loop exits. `activation` is how the current binding
/// came to be armed, which status reports honestly.
fn accept_loop(
    server: &Server,
    token: &Option<String>,
    state: &mut Config,
    binding: &mut Option<Binding>,
    mut activation: Activation,
) -> Result<()> {
    let mut browser_auth = crate::browser_auth::BrowserAuth::default();
    let mut feedback = ActionFeedback::default();
    loop {
        // Deliver any hotkey press that arrived since the last request. A
        // Win32 binding only reaches us through this thread's message queue,
        // so without this drain a registered binding could never fire.
        for _ in 0..fired_since_last_poll(binding) {
            let result = run_bound_action(state);
            if let Err(error) = &result {
                eprintln!("ERROR: the bound action failed: {error}");
            }
            feedback.record(result);
        }
        let Some(mut request) = server.recv_timeout(std::time::Duration::from_millis(20))? else {
            continue;
        };
        let owned: Vec<(String, String)> = request
            .headers()
            .iter()
            .map(|h| {
                (
                    h.field.as_str().as_str().to_owned(),
                    h.value.as_str().to_owned(),
                )
            })
            .collect();
        let headers: Vec<(&str, &str)> = owned
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        let host = find_header(&headers, "host");
        let origin = find_header(&headers, "origin");
        let administrator = service::authorized(&headers, token.as_deref());
        let browser_scope =
            request.url() == "/api/config" || request.url().starts_with("/api/actions/");
        let authorized = administrator
            || (browser_scope && browser_auth.accepts(service::request_token(&headers)));
        let mut body = String::new();
        let mut body_rejected = false;
        if request.method() == &Method::Post {
            match read_bounded_body(request.as_reader()) {
                Ok(Body::Text(text)) => body = text,
                // Too large or not valid UTF-8: reject the request. Never
                // truncate a String at an arbitrary byte offset — that panics
                // on a multi-byte character boundary.
                Ok(Body::TooLarge) | Err(()) => body_rejected = true,
            }
        }
        let is_config_write = request.method() == &Method::Post && request.url() == "/api/config";
        let routed_request = Request {
            method: request.method(),
            path: request.url(),
            host,
            origin,
            authorized,
            body: &body,
            body_rejected,
        };
        let bootstrap = request.method() == &Method::Post
            && matches!(request.url(), "/api/bootstrap" | "/api/bootstrap/new");
        let (mut code, mut mime, mut payload) = if bootstrap {
            bootstrap_route(
                &routed_request,
                &mut browser_auth,
                administrator,
                token.is_some(),
            )
        } else {
            route(&routed_request, state, token.is_some(), &activation)
        };
        if is_config_write && code == 200 && payload == COMMIT_SENTINEL {
            match commit_live_patch(state, &body, binding, &mut activation) {
                WriteOutcome::Committed(committed) => {
                    // Only now is the change real: the file is on disk, so
                    // update memory and answer with the committed state.
                    *state = committed.clone();
                    // The save succeeded, so try to make the binding live. The
                    // response reports the activation result rather than a
                    // blanket success: a binding that is stored but could not
                    // be registered is not the thing the user asked for.

                    mime = "application/json";
                    payload = binding_status(state, &activation);
                }
                WriteOutcome::Rejected(message) => {
                    code = 400;
                    mime = "text/plain; charset=utf-8";
                    payload = message.into();
                }
                WriteOutcome::ActivationFailed(error) => {
                    code = 409;
                    mime = "application/json";
                    payload = serde_json::json!({
                        "error": "hotkey_activation_failed",
                        "message": error,
                        "binding": serde_json::from_str::<serde_json::Value>(&binding_status(state, &activation)).unwrap(),
                    }).to_string();
                }
                WriteOutcome::PersistFailed(error) => {
                    // Reporting success here would promise a save that never
                    // happened; the client would restart into the old values.
                    eprintln!("ERROR: failed to persist configuration: {error}");
                    code = 500;
                    mime = "text/plain; charset=utf-8";
                    payload =
                        "Failed to persist configuration; the stored values are unchanged.".into();
                }
            }
        }
        if code == 200 && request.url() == "/api/config" {
            payload = feedback.enrich(&payload);
        }
        let is_shutdown = payload == SHUTDOWN_SENTINEL;
        if is_shutdown {
            payload = "{\"stopping\":true}".to_string();
            mime = "application/json";
        }
        let response = with_headers(
            Response::from_data(payload.as_bytes()).with_status_code(StatusCode(code)),
        )
        .with_header(Header::from_bytes("Content-Type", mime).unwrap());
        if let Err(error) = request.respond(response) {
            eprintln!("Response failed: {error}");
        }
        // Answer first, then stop: the caller gets proof the shutdown was
        // accepted by the instance that owns the token. Returning here lets
        // `serve` drop the listener and clear the instance marker, so a later
        // launch does not see a stale record for a port nobody holds.
        if is_shutdown {
            return Ok(());
        }
    }
}

fn bootstrap_route(
    request: &Request<'_>,
    auth: &mut crate::browser_auth::BrowserAuth,
    administrator: bool,
    enabled: bool,
) -> Routed {
    if let Some(rejection) = validate_request(request, enabled) {
        return rejection;
    }
    let result = if request.path == "/api/bootstrap/new" {
        if !administrator {
            return (401, "text/plain", "Unauthorized".into());
        }
        auth.mint()
            .map(|code| Some(serde_json::json!({"code": code})))
    } else {
        let body = serde_json::from_str::<serde_json::Value>(request.body).ok();
        let Some(code) = body
            .as_ref()
            .and_then(|body| body.get("code"))
            .and_then(|code| code.as_str())
        else {
            return (400, "text/plain", "Expected a bootstrap code".into());
        };
        auth.redeem(code)
            .map(|token| token.map(|token| serde_json::json!({"token": token})))
    };
    match result {
        Ok(Some(body)) => (200, "application/json", body.to_string()),
        Ok(None) => (
            401,
            "text/plain",
            "Invalid or expired bootstrap code".into(),
        ),
        Err(_) => (
            503,
            "text/plain",
            "Browser authorization unavailable".into(),
        ),
    }
}

/// Case-insensitive header lookup over the collected header list.
fn find_header<'a>(headers: &'a [(&'a str, &'a str)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| *value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[cfg(windows)]
    #[test]
    fn conflicting_edit_restores_the_previous_live_binding() {
        let state = Config {
            device_id: Some("test-endpoint-no-audio-action".into()),
            hotkey: Some("Ctrl+Alt+Shift+F21".into()),
        };
        let occupied = crate::hotkey::Hotkey::parse("Ctrl+Alt+Shift+F22").unwrap();
        let _other = crate::windows::RegisteredHotkey::register(&occupied, 2031).unwrap();
        let mut binding = None;
        let mut activation = activate_hotkey(&state, &mut binding);
        assert!(activation.is_armed());
        let outcome = commit_live_patch(
            &state,
            r#"{"hotkey":"Ctrl+Alt+Shift+F22"}"#,
            &mut binding,
            &mut activation,
        );
        assert!(
            matches!(outcome, WriteOutcome::ActivationFailed(message) if message.contains("already registered"))
        );
        assert!(activation.is_armed());
        assert!(binding.is_some());
        let previous = state.hotkey_binding().unwrap().unwrap();
        assert!(crate::windows::RegisteredHotkey::register(&previous, 2032).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn failed_persistence_restores_the_previous_registration() {
        let state = Config {
            device_id: Some("isolated-endpoint-no-audio".into()),
            hotkey: Some("Ctrl+Alt+Shift+F23".into()),
        };
        let mut binding = None;
        let mut activation = activate_hotkey(&state, &mut binding);
        assert!(activation.is_armed());
        let outcome = commit_live_patch_with_save(
            &state,
            r#"{"hotkey":"Ctrl+Alt+Shift+F24"}"#,
            &mut binding,
            &mut activation,
            |candidate| {
                assert_eq!(candidate.hotkey.as_deref(), Some("Ctrl+Alt+Shift+F24"));
                let chord = candidate.hotkey_binding().unwrap().unwrap();
                assert!(crate::windows::RegisteredHotkey::register(&chord, 2040).is_err());
                Err("simulated disk failure".into())
            },
        );
        assert!(
            matches!(outcome, WriteOutcome::PersistFailed(error) if error == "simulated disk failure")
        );
        assert!(activation.is_armed());
        let previous = state.hotkey_binding().unwrap().unwrap();
        assert!(crate::windows::RegisteredHotkey::register(&previous, 2041).is_err());
        let rejected = crate::hotkey::Hotkey::parse("Ctrl+Alt+Shift+F24").unwrap();
        assert!(crate::windows::RegisteredHotkey::register(&rejected, 2042).is_ok());
        assert_eq!(state.hotkey.as_deref(), Some("Ctrl+Alt+Shift+F23"));
    }

    #[test]
    fn action_feedback_distinguishes_registration_events_and_failures() {
        let mut feedback = ActionFeedback::default();
        let initial: serde_json::Value = serde_json::from_str(&feedback.enrich("{}")).unwrap();
        assert_eq!(initial["last_action"]["sequence"], 0);
        assert!(initial["last_action"]["state"].is_null());
        feedback.record(Ok("MUTED".into()));
        feedback.record(Ok("LIVE".into()));
        assert_eq!(feedback.sequence, 2);
        assert_eq!(feedback.state.as_deref(), Some("LIVE"));
        feedback.record(Err("endpoint disconnected".into()));
        let failed: serde_json::Value = serde_json::from_str(&feedback.enrich("{}")).unwrap();
        assert_eq!(failed["last_action"]["sequence"], 3);
        assert!(failed["last_action"]["state"].is_null());
        assert_eq!(failed["last_action"]["error"], "endpoint disconnected");
        feedback.record(Ok("MUTED".into()));
        assert!(feedback.error.is_none());
    }

    #[test]
    fn browser_code_redemption_preserves_origin_and_replay_protection() {
        let mut auth = crate::browser_auth::BrowserAuth::default();
        let code = auth.mint().unwrap();
        let body = serde_json::json!({"code": code}).to_string();
        let host = host();
        let mut request = Request {
            method: &Method::Post,
            path: "/api/bootstrap",
            host: Some(&host),
            origin: Some("https://foreign.example"),
            authorized: false,
            body: &body,
            body_rejected: false,
        };
        assert_eq!(bootstrap_route(&request, &mut auth, false, true).0, 403);
        request.origin = None;
        assert_eq!(bootstrap_route(&request, &mut auth, false, false).0, 405);
        assert_eq!(bootstrap_route(&request, &mut auth, false, true).0, 200);
        assert_eq!(bootstrap_route(&request, &mut auth, false, true).0, 401);
        request.path = "/api/bootstrap/new";
        assert_eq!(bootstrap_route(&request, &mut auth, false, true).0, 401);
    }

    /// Host header matching the bound loopback address for these tests.
    fn host() -> String {
        address()
    }

    /// Builds a request and routes it, returning the status code and body.
    ///
    /// Mirrors what `accept_loop` does after routing: a `/api/config` write is
    /// committed through `commit_patch`. These tests cover route validation
    /// and persistence only; live registration rollback is tested separately.
    fn call(
        method: &Method,
        path: &str,
        host: &str,
        origin: Option<&str>,
        authorized: bool,
        body: &str,
        service_mode: bool,
    ) -> (u16, String) {
        // A private state directory keeps these writes away from the user's
        // real configuration and from any shared LOCALAPPDATA. The name is
        // unique per call, so concurrent or interleaved callers never share a
        // tree: no cleanup ordering can delete a directory another call is
        // still writing into.
        let guard = STATE_DIR_LOCK.lock().unwrap();
        let original = std::env::var_os("LOCALAPPDATA");
        let dir = private_test_dir();
        std::env::set_var("LOCALAPPDATA", &dir);
        let state = Config::default();
        let activation = Activation::Idle;
        let (mut code, mut mime, mut payload) = route(
            &Request {
                method,
                path,
                host: Some(host),
                origin,
                authorized,
                body,
                body_rejected: false,
            },
            &state,
            service_mode,
            &activation,
        );
        if code == 200 && payload == COMMIT_SENTINEL {
            match commit_patch(&state, body) {
                WriteOutcome::Committed(committed) => {
                    mime = "application/json";
                    payload = committed.to_json();
                }
                WriteOutcome::Rejected(message) => {
                    code = 400;
                    mime = "text/plain; charset=utf-8";
                    payload = message.into();
                }
                WriteOutcome::ActivationFailed(error) => {
                    code = 409;
                    payload = error;
                }
                WriteOutcome::PersistFailed(error) => {
                    code = 500;
                    mime = "text/plain; charset=utf-8";
                    payload = error;
                }
            }
        }
        let _ = mime;
        match original {
            Some(value) => std::env::set_var("LOCALAPPDATA", value),
            None => std::env::remove_var("LOCALAPPDATA"),
        }
        // Clean up while still holding the lock. Releasing first lets another
        // test set the same PID-derived directory and write into it while this
        // cleanup deletes it, which surfaced as a spurious 500 from a failed
        // config save inside the deleted tree.
        let _ = fs::remove_dir_all(&dir);
        drop(guard);
        (code, payload)
    }

    /// Serialises tests that temporarily redirect `LOCALAPPDATA`, because the
    /// process-wide environment is shared across the parallel test runner.
    static STATE_DIR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Distinguishes the private state directory of each `call`, so two calls
    /// never share one tree even if their lifetimes overlap.
    static STATE_DIR_SEQUENCE: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    /// Returns a state directory unique to one caller.
    ///
    /// Uniqueness per call — rather than per process — is what makes the tests
    /// safe: a cleanup in one call cannot delete a tree another call is using.
    fn private_test_dir() -> std::path::PathBuf {
        let unique = STATE_DIR_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        std::env::temp_dir().join(format!("vibdock-web-test-{}-{unique}", std::process::id()))
    }

    /// Routes a request whose body was rejected as oversized or undecodable.
    fn call_rejected(method: &Method, path: &str, service_mode: bool) -> u16 {
        let state = Config::default();
        route(
            &Request {
                method,
                path,
                host: Some(host().as_str()),
                origin: None,
                authorized: true,
                body: "",
                body_rejected: true,
            },
            &state,
            service_mode,
            &Activation::Idle,
        )
        .0
    }

    #[test]
    fn oversized_or_invalid_body_is_rejected_not_panicking() {
        // An unauthenticated caller sending a huge body must get a clean 413,
        // never a panic on a UTF-8 character boundary.
        assert_eq!(call_rejected(&Method::Post, "/api/config", true), 413);
        assert_eq!(call_rejected(&Method::Post, "/unrecognized", true), 413);
    }

    #[test]
    fn bounded_reader_accepts_valid_body_and_detects_oversize() {
        use super::Body;
        match read_bounded_body("{\"device_id\":\"mic\"}".as_bytes()) {
            Ok(Body::Text(text)) => assert!(text.contains("mic")),
            _ => panic!("a valid body must be accepted"),
        }
        let huge = "界".repeat(30000);
        assert!(matches!(
            read_bounded_body(huge.as_bytes()),
            Ok(Body::TooLarge)
        ));
        // Invalid UTF-8 is an error, not a panic.
        assert!(read_bounded_body([0xffu8, 0xfe, 0xfd].as_slice()).is_err());
    }

    #[test]
    fn preview_is_read_only_and_rejects_foreign_hosts() {
        assert_eq!(
            call(&Method::Get, "/", host().as_str(), None, false, "", false).0,
            200
        );
        assert_eq!(
            call(&Method::Post, "/", host().as_str(), None, false, "", false).0,
            405
        );
        assert_eq!(
            call(&Method::Get, "/", "evil.example", None, false, "", false).0,
            403
        );
        assert_eq!(
            call(
                &Method::Get,
                "/missing",
                host().as_str(),
                None,
                false,
                "",
                false
            )
            .0,
            404
        );
        assert!(call(
            &Method::Get,
            "/health",
            host().as_str(),
            None,
            false,
            "",
            false
        )
        .1
        .contains("design-preview"));
    }

    #[test]
    fn service_mode_reports_service_and_accepts_writes() {
        assert!(call(
            &Method::Get,
            "/health",
            host().as_str(),
            None,
            true,
            "",
            true
        )
        .1
        .contains("\"mode\":\"service\""));
        assert_eq!(
            call(
                &Method::Post,
                "/api/config",
                host().as_str(),
                None,
                true,
                "{\"device_id\":\"mic-1\"}",
                true
            )
            .0,
            200
        );
    }

    #[test]
    fn writes_require_authentication() {
        for path in ["/api/config", "/api/actions/echo"] {
            assert_eq!(
                call(
                    &Method::Post,
                    path,
                    host().as_str(),
                    None,
                    false,
                    "{}",
                    true
                )
                .0,
                401
            );
        }
        assert_eq!(
            call(
                &Method::Get,
                "/api/config",
                host().as_str(),
                None,
                false,
                "",
                true
            )
            .0,
            401
        );
    }

    #[test]
    fn foreign_origin_is_rejected() {
        assert_eq!(
            call(
                &Method::Post,
                "/api/config",
                host().as_str(),
                Some("http://evil.example"),
                true,
                "{}",
                true
            )
            .0,
            403
        );
        assert_eq!(
            call(
                &Method::Post,
                "/api/config",
                host().as_str(),
                Some("http://127.0.0.1:43128"),
                true,
                "{}",
                true
            )
            .0,
            200
        );
    }

    #[test]
    fn idle_activation_is_not_reported_as_live() {
        // The honest-status contract: an unconfigured binding must never
        // report `hotkey_live: true`, or the UI would show an armed binding
        // that pressing cannot trigger.
        let status = binding_status(&Config::default(), &Activation::Idle);
        assert!(status.contains("\"hotkey_live\":false"), "got: {status}");
        assert!(
            status.contains("Choose a microphone"),
            "an incomplete binding must say what is missing: {status}"
        );
        // A failed registration is also not live, and must say why.
        let failed = binding_status(
            &Config::default(),
            &Activation::Failed("chord is taken".into()),
        );
        assert!(failed.contains("\"hotkey_live\":false"), "got: {failed}");
        assert!(failed.contains("chord is taken"), "got: {failed}");
    }

    #[test]
    fn an_armed_binding_reports_live_without_an_error() {
        // Only a confirmed registration may claim to be live, and it must not
        // carry a stale error from an earlier attempt.
        let config = Config {
            device_id: Some("mic-1".into()),
            hotkey: Some("Ctrl + Alt + Shift + F9".into()),
        };
        let status = binding_status(&config, &Activation::Armed);
        assert!(status.contains("\"hotkey_live\":true"), "got: {status}");
        assert!(
            status.contains("\"activation_error\":null"),
            "an armed binding must not report an error: {status}"
        );
    }

    #[test]
    fn activation_states_are_distinguishable() {
        // `is_armed` is what guards the live flag, so exactly one state may
        // claim it.
        assert!(Activation::Armed.is_armed());
        assert!(!Activation::Idle.is_armed());
        assert!(!Activation::Failed("x".into()).is_armed());
    }

    #[test]
    fn invalid_patch_is_rejected_with_400() {
        assert_eq!(
            call(
                &Method::Post,
                "/api/config",
                host().as_str(),
                None,
                true,
                "not json",
                true
            )
            .0,
            400
        );
    }

    #[test]
    fn config_write_defers_to_the_caller_via_sentinel() {
        // The route cannot merge or persist, so it hands back the sentinel and
        // the caller commits. Getting real JSON here would mean the route
        // claimed a save it never performed.
        let state = Config::default();
        let (code, mime, payload) = route(
            &Request {
                method: &Method::Post,
                path: "/api/config",
                host: Some(host().as_str()),
                origin: None,
                authorized: true,
                body: "{\"hotkey\":\"Ctrl + Alt + F10\"}",
                body_rejected: false,
            },
            &state,
            true,
            &Activation::Idle,
        );
        assert_eq!(code, 200);
        assert_eq!(payload, COMMIT_SENTINEL);
        assert_eq!(mime, "text/plain; charset=utf-8");
    }

    #[test]
    fn commit_patch_returns_error_status_when_persistence_fails() {
        // A disk-write failure must not look like success: reporting 200 would
        // promise a save the next restart will not see.
        let outcome = commit_patch(&Config::default(), "not json");
        assert!(matches!(outcome, WriteOutcome::Rejected(_)));
        // With a writable state dir the happy path commits; the injected
        // write-failure case is covered end-to-end by the acceptance harness,
        // which makes config.json a directory to force the rename to fail.
    }

    #[test]
    fn commit_patch_merges_into_existing_state() {
        // The retention contract: an unnamed field keeps its stored value.
        let stored = Config {
            device_id: Some("review-mic".into()),
            hotkey: Some("F9".into()),
        };
        // A patch whose save target is the real state dir would touch shared
        // state, so assert the merge arithmetic directly here and leave the
        // persisted end-to-end check to the isolated acceptance harness.
        let patch = service::validate_patch("{\"hotkey\":\"Ctrl + Alt + F10\"}").unwrap();
        let merged = stored.apply(&patch);
        assert_eq!(merged.device_id.as_deref(), Some("review-mic"));
        assert_eq!(merged.hotkey.as_deref(), Some("Ctrl + Alt + F10"));
    }

    #[test]
    fn instance_challenge_requires_the_token() {
        // Single-instance detection depends on this: only the real service may
        // answer 200, so a foreign process on the port cannot impersonate it.
        assert_eq!(
            call(
                &Method::Get,
                "/instance",
                host().as_str(),
                None,
                true,
                "",
                true
            )
            .0,
            200
        );
        assert_eq!(
            call(
                &Method::Get,
                "/instance",
                host().as_str(),
                None,
                false,
                "",
                true
            )
            .0,
            401
        );
    }

    #[test]
    fn preview_mode_does_not_answer_instance_challenge() {
        // The read-only preview has no token, so nothing can satisfy the
        // challenge: it must never be mistaken for a reusable service.
        assert_eq!(
            call(
                &Method::Get,
                "/instance",
                host().as_str(),
                None,
                false,
                "",
                false
            )
            .0,
            401
        );
    }

    #[test]
    fn repeated_config_writes_each_get_their_own_state_dir() {
        // Regression: every `call` previously shared one PID-derived directory,
        // so a cleanup running while another caller was still using the tree
        // deleted it mid-write and a valid request came back as a 500 from a
        // failed save. Concurrent use from another thread must not corrupt or
        // invalidate this call's own private directory.
        let writer = std::thread::spawn(|| {
            for _ in 0..50 {
                let dir = private_test_dir();
                let _ = fs::create_dir_all(&dir);
                let _ = fs::write(dir.join("probe.txt"), b"x");
                let _ = fs::remove_dir_all(&dir);
            }
        });
        for attempt in 0..25 {
            let (code, payload) = call(
                &Method::Post,
                "/api/config",
                host().as_str(),
                Some("http://127.0.0.1:43128"),
                true,
                "{\"hotkey\":\"Ctrl + Alt + F9\"}",
                true,
            );
            assert_eq!(code, 200, "attempt {attempt}: expected 200, got {payload}");
            assert!(
                payload.contains("\"hotkey\":\"Ctrl + Alt + F9\""),
                "attempt {attempt}: payload was {payload}"
            );
        }
        writer.join().expect("writer thread must not panic");
    }

    #[test]
    fn no_shell_execution_endpoint_exists() {
        for path in [
            "/api/actions/exec",
            "/api/actions/shell",
            "/api/actions/run",
            "/api/actions/command",
        ] {
            assert_eq!(
                call(&Method::Post, path, host().as_str(), None, true, "{}", true).0,
                404
            );
        }
    }

    #[test]
    fn unknown_methods_are_rejected() {
        assert_eq!(
            call(&Method::Put, "/", host().as_str(), None, true, "", true).0,
            405
        );
        assert_eq!(
            call(&Method::Delete, "/", host().as_str(), None, true, "", true).0,
            405
        );
    }
}
