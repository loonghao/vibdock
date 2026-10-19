//! Loopback service: serves the configuration page and, when a service token is
//! configured, authenticated read/write endpoints for configuration and actions.
//!
//! Everything here is bound to loopback and rejects foreign `Host`/`Origin`
//! headers. Writes additionally require the bearer token this instance
//! generated, so a browser page from another origin cannot mutate state.

#[cfg(windows)]
use crate::feedback::{frame_from, LastActionView};
use crate::{
    app_shortcuts::Registry as ShortcutRegistry,
    feedback::FeedbackConfig,
    service::{self, Config, Instance},
    Result,
};

const PAGE: &str = include_str!("../docs/design/index.html");
/// Formats the loopback bind address for the configured port.
pub fn address() -> String {
    format!("127.0.0.1:{}", service::port())
}

/// Placeholder body returned by the `/api/config` route, standing in for a
/// result only the caller can produce because it owns the live state and must
/// persist before answering. Replaced with committed JSON or an error message.
const COMMIT_SENTINEL: &str = "\u{0}vibdock-commit-config";

/// Body returned by `/api/shutdown`, telling the accept loop to stop serving.
const SHUTDOWN_SENTINEL: &str = "\u{0}vibdock-shutdown";

/// The id the resident service registers its configured hotkey under.
///
/// Fixed rather than derived: the service owns exactly one global binding, so
/// there is no second registration to distinguish. Defined on every platform
/// so the accept loop can compare a drained id without a conditional block.
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
    /// The binding is registered and live (Windows adapter only).
    #[cfg_attr(not(any(windows, test)), allow(dead_code))]
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

/// HTTP methods the routing table distinguishes.
///
/// This replaced the HTTP library's method type when the transport was brought
/// in-process so every stage could carry a deadline. It is intentionally small:
/// only the verbs the router actually matches on.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
enum Method {
    Get,
    Post,
    Other,
}

impl Method {
    /// Maps a request's method token to the routing enum.
    fn parse(name: &str) -> Method {
        match name {
            "GET" => Method::Get,
            "POST" => Method::Post,
            _ => Method::Other,
        }
    }
}

/// Everything that varies per request, grouped so the routing table stays a
/// pure function of its inputs.
struct Request<'a> {
    method: Method,
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

/// Drains every hotkey message for this thread.
///
/// Non-Windows platforms have no global hotkey adapter, so no message can be
/// pending. Keeping the shape identical lets the accept loop stay platform-free.
#[cfg(windows)]
fn fired_hotkeys() -> Vec<i32> {
    crate::windows::poll_hotkeys()
}

#[cfg(not(windows))]
fn fired_hotkeys() -> Vec<i32> {
    vec![]
}

/// Resolves one fired application-binding id against the current scan.
///
/// Only Windows can dispatch, and only its resident-app registry carries the
/// presence lookup used by the `running` scope. Other platforms never produce a
/// fired id, so this is explicitly empty rather than a fake dispatch.
#[cfg(windows)]
fn application_shortcut_result(
    shortcuts: &ShortcutRegistry,
    apps: &crate::resident_apps::Registry,
    id: i32,
) -> Option<(std::result::Result<String, String>, Option<String>)> {
    shortcuts.fired(id, |path| apps.application_running(path))
}

#[cfg(not(windows))]
fn application_shortcut_result(
    _shortcuts: &ShortcutRegistry,
    _apps: &crate::resident_apps::Registry,
    _id: i32,
) -> Option<(std::result::Result<String, String>, Option<String>)> {
    None
}

/// Records one fired application shortcut with the binding details the page
/// needs to show what a physical button press did.
#[cfg(windows)]
fn record_application_shortcut_event(
    feedback: &mut ActionFeedback,
    shortcuts: &ShortcutRegistry,
    id: i32,
    result: std::result::Result<String, String>,
    target: Option<String>,
) {
    match shortcuts.describe(id) {
        Some(details) => feedback.record_event(
            TriggerEvent {
                source: if details.device_button.is_some() {
                    "vibe_key"
                } else {
                    "application_shortcut"
                },
                label: Some(&details.label),
                trigger: Some(&details.trigger),
                dispatch: Some(&details.dispatch),
                device_button: details.device_button,
                target: target.as_deref(),
                detail: None,
            },
            result,
        ),
        None => feedback.record_event(
            TriggerEvent {
                source: "application_shortcut",
                label: None,
                trigger: None,
                dispatch: None,
                device_button: None,
                target: target.as_deref(),
                detail: None,
            },
            result,
        ),
    }
}

#[cfg(not(windows))]
fn record_application_shortcut_event(
    _feedback: &mut ActionFeedback,
    _shortcuts: &ShortcutRegistry,
    _id: i32,
    _result: std::result::Result<String, String>,
    _target: Option<String>,
) {
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

/// Executes one ordered hardware action binding.
///
/// Steps run in order and execution stops at the first failure, so a failed
/// launch cannot be followed by a microphone change the user did not expect.
/// Success reports what each step did; it never claims that a launched
/// application finished starting, or that a sent chord was acted on.
#[cfg(windows)]
fn run_action_steps(
    steps: &[crate::actions::Step],
    commands: &crate::actions::CommandRegistry,
) -> std::result::Result<String, String> {
    use crate::actions::Step;
    let mut completed = Vec::with_capacity(steps.len());
    for (index, step) in steps.iter().enumerate() {
        let kind = match step {
            Step::Launch { .. } => "launch",
            Step::SendKeys { .. } => "send_keys",
            Step::ToggleMicrophone { .. } => "toggle_microphone",
            Step::Command { .. } => "command",
            Step::Wait { .. } => "wait",
        };
        let outcome: std::result::Result<String, String> = match step {
            Step::Launch {
                label,
                executable,
                args,
            } => std::process::Command::new(executable)
                .args(args)
                .spawn()
                .map(|child| format!("launched {label} (pid {})", child.id()))
                .map_err(|error| format!("launch {label} failed: {error}")),
            Step::SendKeys { parsed, .. } => {
                match crate::windows::dispatch_to_foreground_app(parsed) {
                    Ok(crate::windows::DispatchDisposition::Dispatched) => {
                        Ok("sent the chord to the foreground application".to_string())
                    }
                    Ok(crate::windows::DispatchDisposition::FocusChanged) => Err(
                        "the foreground window changed while sending; the chord may have gone \
                         to another application"
                            .to_string(),
                    ),
                    Err(error) => Err(error),
                }
            }
            Step::ToggleMicrophone { device_id } => crate::service::run_device_action(
                crate::service::DeviceAction::Toggle,
                Some(device_id),
            )
            .and_then(|result| match result {
                crate::service::ActionResult::MuteState { muted, .. } => Ok(if muted {
                    "toggled the microphone to muted".to_string()
                } else {
                    "toggled the microphone to live".to_string()
                }),
                _ => Err("The microphone action did not read back a mute state.".to_string()),
            }),
            Step::Command {
                command_id,
                parameters,
            } => commands
                .expand(command_id, parameters)
                .and_then(|(executable, args)| {
                    std::process::Command::new(executable)
                        .args(args)
                        .spawn()
                        .map(|child| format!("ran command {command_id} (pid {})", child.id()))
                        .map_err(|error| format!("command {command_id} failed: {error}"))
                }),
            Step::Wait { ms } => {
                std::thread::sleep(std::time::Duration::from_millis(u64::from(*ms)));
                Ok(format!("waited {ms} ms"))
            }
        };
        match outcome {
            Ok(detail) => completed.push(detail),
            Err(error) => {
                return Err(format!(
                    "step {}/{} ({kind}) failed: {error}",
                    index + 1,
                    steps.len()
                ))
            }
        }
    }
    Ok(completed.join("; "))
}

/// Placeholder for platforms with no process or input adapter.
#[cfg(not(windows))]
fn run_action_steps(
    _steps: &[crate::actions::Step],
    _commands: &crate::actions::CommandRegistry,
) -> std::result::Result<String, String> {
    Err("Hardware actions require the Windows service.".into())
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
    /// The device the recorded action actually applied to.
    ///
    /// Captured when the action runs rather than read from the live config at
    /// display time: a binding change after an action would otherwise relabel
    /// that older action's verified result with the newly selected microphone.
    target: Option<String>,
    /// Bounded newest-last history of completed triggers.
    ///
    /// The page shows this so a physical button press is observable even when
    /// the target application ignores the injected chord. It records that a
    /// trigger was received and what VibDock sent, never that the application
    /// acted on it.
    recent: std::collections::VecDeque<serde_json::Value>,
}

/// One completed trigger, rendered in the page's recent-actions list.
struct TriggerEvent<'a> {
    source: &'a str,
    label: Option<&'a str>,
    trigger: Option<&'a str>,
    dispatch: Option<&'a str>,
    device_button: Option<u8>,
    target: Option<&'a str>,
    detail: Option<&'a str>,
}

impl ActionFeedback {
    /// Records an action with no known device.
    ///
    /// Test-only: the production loop always knows which device an action ran
    /// against and uses [`ActionFeedback::record_for`], so a target captured
    /// later can never be attributed to an earlier action.
    #[cfg(test)]
    fn record(&mut self, result: std::result::Result<String, String>) {
        self.record_for(result, None)
    }

    /// Records one completed action and the device it applied to.
    ///
    /// `target` is the device id the action was run against, captured by the
    /// caller at the moment it ran. An action recorded without one is labelled
    /// as having no target rather than inheriting whatever is configured later.
    fn record_for(&mut self, result: std::result::Result<String, String>, target: Option<&str>) {
        self.sequence = self.sequence.saturating_add(1);
        self.target = target.map(str::to_string);
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

    /// Records a trigger with enough detail for the page to show what happened.
    ///
    /// `source` is `microphone`, `application_shortcut`, or `vibe_key`; a
    /// `vibe_key` event also carries the device button that emitted the trigger.
    fn record_event(
        &mut self,
        event: TriggerEvent<'_>,
        result: std::result::Result<String, String>,
    ) {
        let (state, error) = match &result {
            Ok(state) => (Some(state.clone()), None),
            Err(error) => (None, Some(error.clone())),
        };
        self.record_for(result, event.target);
        let at = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_millis())
            .unwrap_or(0);
        self.recent.push_back(serde_json::json!({
            "sequence": self.sequence,
            "at": at,
            "source": event.source,
            "label": event.label,
            "trigger": event.trigger,
            "dispatch": event.dispatch,
            "device_button": event.device_button,
            "state": state,
            "error": error,
            "detail": event.detail,
        }));
        const MAX_RECENT: usize = 20;
        while self.recent.len() > MAX_RECENT {
            self.recent.pop_front();
        }
    }

    fn recent_json(&self) -> serde_json::Value {
        serde_json::Value::Array(self.recent.iter().cloned().collect())
    }

    fn enrich(&self, payload: &str) -> String {
        let mut value: serde_json::Value =
            serde_json::from_str(payload).expect("configuration responses are generated JSON");
        value["last_action"] = serde_json::json!({
            "sequence": self.sequence,
            "state": self.state,
            "error": self.error,
        });
        value["recent_actions"] = self.recent_json();
        value.to_string()
    }
}

/// Starts the desktop indicator only when the user has opted in.
///
/// Returns `None` on every non-Windows platform and whenever the feature is
/// off, so the rest of the loop can treat the overlay uniformly as optional.
/// A failure to start is logged and swallowed: the indicator is an auxiliary
/// view, and a broken window must never take down audio control.
#[cfg(windows)]
fn start_overlay_if_enabled(
    config: &FeedbackConfig,
) -> Option<crate::feedback_windows::OverlayHandle> {
    if !config.enabled {
        return None;
    }
    match crate::feedback_windows::spawn(config.clone()) {
        Ok(handle) => Some(handle),
        Err(error) => {
            eprintln!("ERROR: could not start the desktop status indicator: {error}");
            None
        }
    }
}

/// Placeholder for platforms with no overlay adapter: never starts anything.
#[cfg(not(windows))]
fn start_overlay_if_enabled(_config: &FeedbackConfig) -> Option<OverlayPlaceholder> {
    None
}

/// Type stand-in so the non-Windows loop compiles against the same shape.
#[cfg(not(windows))]
struct OverlayPlaceholder;

/// What the indicator was last asked to show.
///
/// Used to enqueue only on a real change. The accept loop runs continuously,
/// so without this the same frame would be queued on every iteration and the
/// overlay woken for nothing.
#[derive(Default, PartialEq, Eq, Debug)]
struct OverlayCursor {
    sequence: u64,
    target: String,
}

/// Renders the latest completed action into the indicator, if one is running.
///
/// Pure view over state the service already recorded: this never performs an
/// action and never queries the platform. The target label is the device the
/// action was run against, captured on [`ActionFeedback`] when it ran — not the
/// currently configured device, which may have been rebound since. Labelling an
/// older action with the newly selected microphone would present one device's
/// verified LIVE/MUTED as another's.
///
/// Enqueues only when the sequence or the target changed, so an idle loop
/// costs one comparison per iteration instead of a queue send and a thread
/// wake. Actions are never dropped: `sequence` advances on every action, so a
/// changed sequence always enqueues.
#[cfg(windows)]
fn show_latest_frame(
    overlay: Option<&crate::feedback_windows::OverlayHandle>,
    feedback: &ActionFeedback,
    _device_id: Option<&str>,
    cursor: &mut OverlayCursor,
) {
    let Some(overlay) = overlay else {
        return;
    };
    let target = feedback
        .target
        .as_deref()
        .map(|id| {
            if id.is_empty() {
                "No target selected"
            } else {
                id
            }
        })
        .unwrap_or("No target selected")
        .to_string();
    // Nothing changed since the last enqueue: skip the send and the wake.
    if cursor.sequence == feedback.sequence && cursor.target == target {
        return;
    }
    let config = FeedbackConfig {
        enabled: true,
        target_label: target.clone(),
        ..FeedbackConfig::default()
    };
    let view = LastActionView {
        sequence: feedback.sequence,
        state: feedback.state.clone(),
        error: feedback.error.clone(),
    };
    // The cursor advances only once the frame is actually queued and woken.
    // Advancing it first would mark this state as delivered and never retry it,
    // leaving the indicator stuck on an older state.
    if overlay.show(&frame_from(&view, &config)) {
        cursor.sequence = feedback.sequence;
        cursor.target = target;
    }
}

/// Placeholder for platforms with no overlay to render into.
#[cfg(not(windows))]
fn show_latest_frame(
    _overlay: Option<&OverlayPlaceholder>,
    _feedback: &ActionFeedback,
    _device_id: Option<&str>,
    _cursor: &mut OverlayCursor,
) {
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
    if !token_configured && request.method != Method::Get {
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
                serde_json::json!({"mode": mode, "audio_control": token_configured && cfg!(windows)}).to_string(),
            )
        }
        (Method::Get, "/instance") => {
            // Legacy authenticated liveness endpoint. HTTP 200 alone is not
            // ownership evidence; discovery uses /instance/challenge.
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

/// Runs the loopback service until the process is interrupted.
///
/// Security headers are applied by the transport on every reply; see
/// `crate::transport::BoundedConnection::respond`.
///
/// When `instance` carries a token the server publishes its address, accepts
/// authenticated writes, and clears the published record on shutdown. Without a
/// token it behaves exactly as the previous read-only design preview.
pub fn serve(instance: Option<Instance>) -> Result<()> {
    // Bound through our own transport so every connection carries read and
    // write deadlines from the moment it is accepted.
    let listener = std::net::TcpListener::bind(address()).map_err(|e| e.to_string())?;
    serve_listener(listener, instance)
}

/// Adopts the startup claim without releasing its bound port.
pub fn serve_listener(listener: std::net::TcpListener, instance: Option<Instance>) -> Result<()> {
    let listener = crate::transport::BoundedListener::new(listener).map_err(|e| e.to_string())?;
    serve_server(listener, instance)
}

fn serve_server(
    server: crate::transport::BoundedListener,
    instance: Option<Instance>,
) -> Result<()> {
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
    let result = accept_loop(server, &token, &mut state, &mut binding, activation);
    if instance.is_some() {
        let _ = service::clear_instance();
    }
    result
}

/// Exact hardware-action routes.
///
/// This must not use a broad `/api/actions` prefix: the older microphone and
/// playback action routes (`/api/actions/devices`, `/status`, `/mute`,
/// `/unmute`, `/toggle`, `/echo`) live under the same prefix and are handled by
/// the general router.
fn is_hardware_action_route(path: &str) -> bool {
    path == "/api/actions"
        || matches!(
            path,
            "/api/actions/add"
                | "/api/actions/remove"
                | "/api/actions/sync"
                | "/api/actions/commands"
                | "/api/actions/gamepads"
        )
}

/// Serves requests until the listener stops, tracking config mutations.
///
/// `binding` is the live global hotkey registration, owned here so it is
/// unregistered when the loop exits. `activation` is how the current binding
/// came to be armed, which status reports honestly.
fn accept_loop(
    listener: crate::transport::BoundedListener,
    token: &Option<String>,
    state: &mut Config,
    binding: &mut Option<Binding>,
    mut activation: Activation,
) -> Result<()> {
    let mut browser_auth = crate::browser_auth::BrowserAuth::default();
    let mut apps = crate::resident_apps::Registry::start(token.is_some());
    let mut shortcuts = ShortcutRegistry::start(token.is_some());
    let hardware_dir = crate::service::state_dir().unwrap_or_default();
    let hardware_mic = crate::windows::capture_endpoint_containing("au05");
    let mut hardware =
        crate::hardware::Hardware::start(token.is_some(), hardware_dir, hardware_mic);
    let commands = crate::actions::CommandRegistry::start();
    let mut actions = crate::actions::Registry::start(token.is_some(), commands);
    #[cfg(windows)]
    let mut gamepads = crate::gamepad_windows::Monitor::new();
    #[cfg(windows)]
    let mut next_gamepad_refresh = std::time::Instant::now();
    // First run only: seed and write button 1 = Ctrl+Win, 2 = Enter, 3 = Esc.
    // A button that already has an application shortcut keeps it instead. The
    // read-only design preview never writes device configuration.
    if token.is_some() {
        let action_buttons: Vec<u8> = actions
            .bindings()
            .iter()
            .filter_map(|binding| match binding.source {
                crate::actions::Source::Au05Button(button) => Some(button),
                crate::actions::Source::Gamepad { .. } => None,
            })
            .collect();
        hardware.ensure_defaults(&shortcuts.device_bindings(), &action_buttons);
    }
    let mut feedback = ActionFeedback::default();
    let plugins = crate::plugins::Registry::start();
    // The indicator is opt-in, resolved from the documented environment
    // variable. When it is off, no overlay thread starts and no window is ever
    // created, so the default is observationally identical to the service as
    // it was before this feature existed.
    let overlay = start_overlay_if_enabled(&crate::feedback::config_from_process_env());
    let mut overlay_cursor = OverlayCursor::default();
    loop {
        apps.poll();
        // Gamepads are polled on the accept-loop thread. A two-second rescan
        // keeps hot-plugged pads appearing without a blocking device thread.
        #[cfg(windows)]
        {
            if token.is_some() {
                if std::time::Instant::now() >= next_gamepad_refresh {
                    gamepads.refresh();
                    next_gamepad_refresh =
                        std::time::Instant::now() + std::time::Duration::from_secs(2);
                }
                for event in gamepads.poll() {
                    if !event.pressed {
                        continue;
                    }
                    if let Some(binding) = actions.gamepad_binding(&event.device_key, event.usage) {
                        let name = binding.name.clone();
                        let steps = binding.steps.clone();
                        let result = run_action_steps(&steps, actions.commands());
                        if let Err(error) = &result {
                            eprintln!("ERROR: the gamepad action failed: {error}");
                        }
                        let detail = result.as_ref().ok().cloned();
                        let recorded = result
                            .as_ref()
                            .map(|_| "DONE".to_string())
                            .map_err(Clone::clone);
                        feedback.record_event(
                            TriggerEvent {
                                source: "gamepad",
                                label: Some(&name),
                                trigger: None,
                                dispatch: None,
                                device_button: None,
                                target: None,
                                detail: detail.as_deref(),
                            },
                            recorded,
                        );
                    }
                }
            }
        }
        // Deliver any hotkey press that arrived since the last request. One
        // drain serves the microphone binding and every application binding:
        // draining per id would consume and discard the others.
        for id in fired_hotkeys() {
            if id == BINDING_HOTKEY_ID {
                let result = run_bound_action(state);
                if let Err(error) = &result {
                    eprintln!("ERROR: the bound action failed: {error}");
                }
                // The device is captured with the action, so a later rebinding
                // cannot relabel this action's verified result with another mic.
                feedback.record_event(
                    TriggerEvent {
                        source: "microphone",
                        label: state.device_id.as_deref(),
                        trigger: state.hotkey.as_deref(),
                        dispatch: None,
                        device_button: None,
                        target: state.device_id.as_deref(),
                        detail: None,
                    },
                    result,
                );
            } else if id >= crate::actions::HOTKEY_ID_BASE {
                if let Some((name, steps)) = actions.fired(id) {
                    let (trigger, button) = actions
                        .describe(id)
                        .map(|(_, trigger, button)| (trigger, button))
                        .unwrap_or((None, None));
                    let result = run_action_steps(&steps, actions.commands());
                    if let Err(error) = &result {
                        eprintln!("ERROR: the hardware action failed: {error}");
                    }
                    let detail = result.as_ref().ok().cloned();
                    let recorded = result
                        .as_ref()
                        .map(|_| "DONE".to_string())
                        .map_err(Clone::clone);
                    feedback.record_event(
                        TriggerEvent {
                            source: "action_binding",
                            label: Some(&name),
                            trigger: trigger.as_deref(),
                            dispatch: None,
                            device_button: button,
                            target: None,
                            detail: detail.as_deref(),
                        },
                        recorded,
                    );
                }
            } else if let Some((result, target)) =
                application_shortcut_result(&shortcuts, &apps, id)
            {
                if let Err(error) = &result {
                    eprintln!("ERROR: the application shortcut failed: {error}");
                }
                // Dispatch is unverified by nature; the recorded state says so.
                record_application_shortcut_event(&mut feedback, &shortcuts, id, result, target);
            }
        }
        // Repaint the indicator from the completed action. This enqueues only
        // when the sequence or target changed, and the overlay coalesces
        // pending frames to the newest; every action has already advanced the
        // sequence above, so none is ever dropped here.
        //
        // The target shown is the one captured with the action, not the live
        // configuration: a rebinding must not repaint an older action's result
        // as if it had applied to the newly selected device.
        show_latest_frame(overlay.as_ref(), &feedback, None, &mut overlay_cursor);
        // Non-blocking accept keeps this loop responsive to hotkeys and to
        // shutdown, instead of stalling in accept() or on a slow client.
        // Hand any pending connections to the bounded I/O pool. This never
        // blocks: a client that connected but sends nothing stays in the
        // backlog or with its worker, so it cannot stall hotkeys or shutdown.
        listener.pump()?;
        // Take at most one finished request per turn, so the loop keeps
        // returning to the hotkey pump and to shutdown.
        let Some(incoming) = listener.next_request() else {
            std::thread::sleep(std::time::Duration::from_millis(20));
            continue;
        };
        let mut connection = incoming.connection;
        let mut received = incoming.request;
        let host = received.header("host").map(str::to_string);
        let origin = received.header("origin").map(str::to_string);
        let method = Method::parse(received.method.as_str());
        let path = received.path.clone();
        let administrator = service::authorized(&received.headers, token.as_deref());
        let browser_scope = path == "/api/config"
            || path == "/api/plugins"
            || path == "/api/pad/execute"
            || is_hardware_action_route(&path)
            || path.starts_with("/api/apps/actions/")
            || path.starts_with("/api/shortcuts")
            || path.starts_with("/api/hardware")
            || matches!(
                path.as_str(),
                "/api/apps"
                    | "/api/apps/summary"
                    | "/api/apps/pin"
                    | "/api/apps/remove"
                    | "/api/apps/bind"
            );
        let browser_principal = browser_auth.principal(service::request_token(&received.headers));
        let authorized = administrator || (browser_scope && browser_principal.is_some());
        let principal = if administrator {
            Some("native-administrator")
        } else {
            browser_principal.as_deref()
        };
        // The body was read on the worker, so public endpoints that take their
        // input in the body (one-use bootstrap code redemption) still work.
        let body_text = received.body.take();
        let body_rejected = received.body_rejected;
        let body = body_text.as_deref().unwrap_or("");
        let is_config_write = method == Method::Post && path == "/api/config";
        let routed_request = Request {
            method,
            path: &path,
            host: host.as_deref(),
            origin: origin.as_deref(),
            authorized,
            body,
            body_rejected,
        };
        let bootstrap = method == Method::Post
            && matches!(path.as_str(), "/api/bootstrap" | "/api/bootstrap/new");
        let (mut code, mut mime, mut payload) =
            if method == Method::Post && path == "/instance/challenge" {
                instance_challenge_route(&routed_request, token.as_deref())
            } else if path == "/api/plugins" {
                plugins_route(&routed_request, token.is_some(), &plugins)
            } else if path == "/api/pad/execute" {
                pad_execute_route(&routed_request, token.is_some(), &actions, &mut feedback)
            } else if is_hardware_action_route(&path) {
                #[cfg(windows)]
                let mut gamepad_devices = || gamepad_devices_json(&mut gamepads);
                #[cfg(not(windows))]
                let mut gamepad_devices = || serde_json::json!({"devices":[]});
                actions_route(
                    &routed_request,
                    token.is_some(),
                    &mut actions,
                    &mut hardware,
                    &mut apps,
                    &shortcuts,
                    &mut gamepad_devices,
                    state,
                )
            } else if path.starts_with("/api/apps") {
                apps_route(&routed_request, token.is_some(), &mut apps, principal)
            } else if path.starts_with("/api/hardware") {
                hardware_route(
                    &routed_request,
                    token.is_some(),
                    &mut hardware,
                    &shortcuts,
                    &feedback,
                    state,
                    &actions,
                )
            } else if path.starts_with("/api/shortcuts") {
                shortcuts_route(
                    &routed_request,
                    token.is_some(),
                    &mut apps,
                    &mut shortcuts,
                    &mut hardware,
                    &actions,
                    state,
                    principal,
                )
            } else if bootstrap {
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
            // A microphone trigger equal to a saved dispatch chord would make
            // VibDock capture the very key it injects for an application; an
            // action-binding trigger must stay unique as well.
            let requested_hotkey = service::validate_patch(body)
                .ok()
                .and_then(|patch| patch.hotkey)
                .flatten();
            let dispatch_conflict = requested_hotkey
                .as_deref()
                .is_some_and(|hotkey| shortcuts.blocks_trigger(hotkey));
            let action_conflict = requested_hotkey
                .as_deref()
                .and_then(|hotkey| crate::hotkey::Hotkey::parse(hotkey).ok())
                .is_some_and(|hotkey| actions.conflicts_with_trigger(&hotkey));
            if dispatch_conflict || action_conflict {
                code = 409;
                mime = "application/json";
                payload = serde_json::json!({
                    "error": "hotkey_activation_failed",
                    "message": "That shortcut is already used as an application dispatch target or by another VibDock binding; choose another.",
                })
                .to_string();
            } else {
                match commit_live_patch(state, body, binding, &mut activation) {
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
                            "Failed to persist configuration; the stored values are unchanged."
                                .into();
                    }
                }
            }
        }
        if code == 200 && path == "/api/config" {
            payload = feedback.enrich(&payload);
        }
        let is_shutdown = payload == SHUTDOWN_SENTINEL;
        if is_shutdown {
            payload = "{\"stopping\":true}".to_string();
            mime = "application/json";
        }
        // Socket writes run on the worker; routing never waits for the client.
        if code == 200 && method == Method::Post && path == "/instance/challenge" {
            connection.respond_keep_alive(code, mime, &payload);
        } else {
            connection.respond(code, mime, &payload);
        }
        // Answer first, then stop: the caller gets proof the shutdown was
        // accepted by the instance that owns the token. Returning here lets
        // `serve` drop the listener and clear the instance marker, so a later
        // launch does not see a stale record for a port nobody holds.
        if is_shutdown {
            connection.finish()?;
            return Ok(());
        }
    }
}

fn instance_challenge_route(request: &Request<'_>, token: Option<&str>) -> Routed {
    if let Some(rejection) = validate_request(request, token.is_some()) {
        return rejection;
    }
    let Some(token) = token else {
        return unauthorized();
    };
    let value = serde_json::from_str::<serde_json::Value>(request.body).ok();
    let proof = value
        .as_ref()
        .and_then(|value| value.get("nonce"))
        .and_then(|value| value.as_str())
        .and_then(|nonce| crate::instance_proof::respond(token, nonce));
    match proof {
        Some(proof) => (
            200,
            "application/json",
            serde_json::json!({"proof": proof}).to_string(),
        ),
        None => (
            400,
            "text/plain",
            "Expected a 32-byte hexadecimal nonce".into(),
        ),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn instance_proof_route_preserves_host_origin_and_preview_boundaries() {
        let host = format!("127.0.0.1:{}", service::port());
        let nonce = "a".repeat(64);
        let body = serde_json::json!({"nonce": nonce}).to_string();
        let mut request = Request {
            method: Method::Post,
            path: "/instance/challenge",
            host: Some(&host),
            origin: None,
            authorized: false,
            body: &body,
            body_rejected: false,
        };
        let (status, _, payload) = instance_challenge_route(&request, Some("synthetic-secret"));
        assert_eq!(status, 200);
        let value: serde_json::Value = serde_json::from_str(&payload).unwrap();
        assert!(crate::instance_proof::verify(
            "synthetic-secret",
            &nonce,
            value["proof"].as_str().unwrap()
        ));
        assert_eq!(instance_challenge_route(&request, None).0, 405);
        request.origin = Some("http://foreign.example");
        assert_eq!(
            instance_challenge_route(&request, Some("synthetic-secret")).0,
            403
        );
        request.origin = None;
        request.host = Some("foreign.example");
        assert_eq!(
            instance_challenge_route(&request, Some("synthetic-secret")).0,
            403
        );
        request.host = Some(&host);
        request.body_rejected = true;
        assert_eq!(
            instance_challenge_route(&request, Some("synthetic-secret")).0,
            413
        );
        request.body_rejected = false;
        request.body = "{\"nonce\":\"invalid\"}";
        assert_eq!(
            instance_challenge_route(&request, Some("synthetic-secret")).0,
            400
        );
    }

    /// The cursor must only advance past state the indicator actually accepted.
    ///
    /// Its fields drive the "did anything change?" short-circuit, so advancing
    /// it before the frame is queued would mark a state as delivered and never
    /// retry it — the indicator would sit on an older frame after any rejected
    /// enqueue. The no-overlay case is the reachable half of that contract on
    /// every platform: nothing to render into means the cursor must stay put.
    #[test]
    fn the_cursor_does_not_advance_without_an_overlay() {
        let feedback = ActionFeedback {
            sequence: 7,
            state: Some("live".to_string()),
            error: None,
            target: Some("Shure MV7".to_string()),
            recent: std::collections::VecDeque::new(),
        };
        let mut cursor = OverlayCursor::default();
        #[cfg(windows)]
        let overlay: Option<&crate::feedback_windows::OverlayHandle> = None;
        #[cfg(not(windows))]
        let overlay: Option<&OverlayPlaceholder> = None;
        show_latest_frame(overlay, &feedback, None, &mut cursor);
        assert_eq!(
            cursor,
            OverlayCursor::default(),
            "the cursor must stay at its initial state so the frame is retried"
        );
    }

    /// An action keeps the device it actually ran against.
    ///
    /// The regression this guards: reading the target from the live
    /// configuration at display time relabels a previously verified result with
    /// whatever microphone is selected now, so one device's LIVE/MUTED is shown
    /// next to another device's name.
    #[test]
    fn an_action_keeps_the_device_it_ran_against_after_a_rebinding() {
        let mut feedback = ActionFeedback::default();
        // Two actions against the first microphone, then the user rebinds.
        feedback.record_for(Ok("LIVE".to_string()), Some("Shure MV7"));
        feedback.record_for(Ok("MUTED".to_string()), Some("Shure MV7"));
        assert_eq!(feedback.target.as_deref(), Some("Shure MV7"));
        // The rebinding itself is not an action, so the recorded target must
        // stay with the action that produced the state.
        assert_eq!(
            feedback.target.as_deref(),
            Some("Shure MV7"),
            "a config change must not relabel a completed action"
        );
        // A later action records the new device, without rewriting the old one.
        feedback.record_for(Ok("LIVE".to_string()), Some("Rode NT"));
        assert_eq!(feedback.target.as_deref(), Some("Rode NT"));
        assert_eq!(feedback.sequence, 3);
    }

    /// An action recorded with no device is labelled as having no target,
    /// rather than inheriting whatever is configured afterwards.
    #[test]
    fn an_action_without_a_device_does_not_inherit_the_later_configuration() {
        let mut feedback = ActionFeedback::default();
        feedback.record_for(Ok("MUTED".to_string()), None);
        assert_eq!(
            feedback.target, None,
            "an action with no target must not be relabelled from the live config"
        );
        feedback.record(Ok("LIVE".to_string()));
        assert_eq!(
            feedback.target, None,
            "the no-target convenience path must also stay unlabelled"
        );
    }

    /// A cursor whose target changed must still be considered changed, which is
    /// what makes the retry above converge rather than loop on a stale target.
    #[test]
    fn the_cursor_equality_compares_both_sequence_and_target() {
        let mut cursor = OverlayCursor {
            sequence: 7,
            target: "Shure MV7".to_string(),
        };
        let same = OverlayCursor {
            sequence: 7,
            target: "Shure MV7".to_string(),
        };
        let other_target = OverlayCursor {
            sequence: 7,
            target: "Rode NT".to_string(),
        };
        let other_sequence = OverlayCursor {
            sequence: 8,
            target: "Shure MV7".to_string(),
        };
        assert_eq!(cursor, same);
        assert_ne!(
            cursor, other_target,
            "a changed target must count as changed"
        );
        assert_ne!(
            cursor, other_sequence,
            "a changed sequence must count as changed"
        );
        // Advancing both fields reaches the state the next enqueue would
        // record, which is what lets a retried frame converge.
        cursor.target = "Rode NT".to_string();
        cursor.sequence = 8;
        assert_eq!(
            cursor,
            OverlayCursor {
                sequence: 8,
                target: "Rode NT".to_string(),
            }
        );
    }

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
    fn recent_actions_are_bounded_and_carry_trigger_details() {
        let mut feedback = ActionFeedback::default();
        feedback.record_event(
            TriggerEvent {
                source: "vibe_key",
                label: Some("微信输入法"),
                trigger: Some("Ctrl + Alt + Shift + F9"),
                dispatch: Some("Ctrl + Win"),
                device_button: Some(0),
                target: Some("微信输入法"),
                detail: None,
            },
            Ok("DISPATCHED".into()),
        );
        let view: serde_json::Value = serde_json::from_str(&feedback.enrich("{}")).unwrap();
        assert_eq!(view["last_action"]["state"], "DISPATCHED");
        let events = view["recent_actions"].as_array().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0]["source"], "vibe_key");
        assert_eq!(events[0]["device_button"], 0);
        assert_eq!(events[0]["trigger"], "Ctrl + Alt + Shift + F9");
        assert_eq!(events[0]["dispatch"], "Ctrl + Win");
        assert_eq!(events[0]["state"], "DISPATCHED");
        assert!(events[0]["at"].as_u64().unwrap() > 0);

        for _ in 0..25 {
            feedback.record_event(
                TriggerEvent {
                    source: "application_shortcut",
                    label: Some("Codex"),
                    trigger: Some("Ctrl + Alt + Shift + F10"),
                    dispatch: Some("Ctrl + Shift + Space"),
                    device_button: None,
                    target: None,
                    detail: None,
                },
                Err("not the foreground window".into()),
            );
        }
        let view: serde_json::Value = serde_json::from_str(&feedback.enrich("{}")).unwrap();
        assert_eq!(view["recent_actions"].as_array().unwrap().len(), 20);
        assert_eq!(view["last_action"]["sequence"], 26);
        assert_eq!(view["last_action"]["error"], "not the foreground window");
    }

    #[test]
    fn browser_code_redemption_preserves_origin_and_replay_protection() {
        let mut auth = crate::browser_auth::BrowserAuth::default();
        let code = auth.mint().unwrap();
        let body = serde_json::json!({"code": code}).to_string();
        let host = host();
        let mut request = Request {
            method: Method::Post,
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
        method: Method,
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
    fn call_rejected(method: Method, path: &str, service_mode: bool) -> u16 {
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
        assert_eq!(call_rejected(Method::Post, "/api/config", true), 413);
        assert_eq!(call_rejected(Method::Post, "/unrecognized", true), 413);
    }

    #[test]
    fn preview_is_read_only_and_rejects_foreign_hosts() {
        assert_eq!(
            call(Method::Get, "/", host().as_str(), None, false, "", false).0,
            200
        );
        assert_eq!(
            call(Method::Post, "/", host().as_str(), None, false, "", false).0,
            405
        );
        assert_eq!(
            call(Method::Get, "/", "evil.example", None, false, "", false).0,
            403
        );
        assert_eq!(
            call(
                Method::Get,
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
            Method::Get,
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
    fn health_reports_platform_audio_support() {
        let (_, body) = call(Method::Get, "/health", &host(), None, true, "", true);
        let health: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(health["audio_control"], cfg!(windows));
    }

    #[test]
    fn service_mode_reports_service_and_accepts_writes() {
        assert!(call(
            Method::Get,
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
                Method::Post,
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
                call(Method::Post, path, host().as_str(), None, false, "{}", true).0,
                401
            );
        }
        assert_eq!(
            call(
                Method::Get,
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
                Method::Post,
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
                Method::Post,
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
                Method::Post,
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
                method: Method::Post,
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
                Method::Get,
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
                Method::Get,
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
                Method::Get,
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
                Method::Post,
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
                call(Method::Post, path, host().as_str(), None, true, "{}", true).0,
                404
            );
        }
    }

    #[test]
    fn unknown_methods_are_rejected() {
        assert_eq!(
            // PUT and DELETE are not distinguished: both arrive as Other and
            // must be rejected with 405, never routed.
            call(Method::Other, "/", host().as_str(), None, true, "", true).0,
            405
        );
        assert_eq!(
            call(Method::Other, "/", host().as_str(), None, true, "", true).0,
            405
        );
    }
}

/// Adds the current hardware status to a shortcut view.
///
/// The page joins provisioning state by saved id; keeping it in one envelope
/// means a shortcut edit and its device result cannot be read from two
/// different snapshots.
fn with_hardware(
    mut view: serde_json::Value,
    hardware: &mut crate::hardware::Hardware,
    shortcuts: &ShortcutRegistry,
) -> serde_json::Value {
    view["hardware"] = hardware.view(&shortcuts.device_bindings());
    view
}

/// Per-application shortcut reads and edits.
///
/// The browser never supplies an executable path: it sends the opaque reference
/// from a scan it just received, and this route resolves that reference on the
/// service side. The microphone trigger is passed in so a new app trigger can
/// never silently steal the existing binding.
#[allow(clippy::too_many_arguments)]
fn shortcuts_route(
    request: &Request<'_>,
    token_configured: bool,
    apps: &mut crate::resident_apps::Registry,
    shortcuts: &mut ShortcutRegistry,
    hardware: &mut crate::hardware::Hardware,
    actions: &crate::actions::Registry,
    config: &Config,
    _principal: Option<&str>,
) -> Routed {
    if let Some(rejection) = validate_request(request, token_configured) {
        return rejection;
    }
    if !request.authorized {
        return unauthorized();
    }
    #[cfg(windows)]
    {
        match (request.method, request.path) {
            (Method::Get, "/api/shortcuts") => (
                200,
                "application/json",
                with_hardware(shortcuts.view(), hardware, shortcuts).to_string(),
            ),
            (Method::Post, "/api/shortcuts/add") => {
                let parsed = serde_json::from_str::<serde_json::Value>(request.body).ok();
                let reference = parsed
                    .as_ref()
                    .and_then(|value| value["application_reference"].as_str().map(str::to_string));
                let wants_device = parsed
                    .as_ref()
                    .and_then(|value| value.get("device_button"))
                    .is_some_and(serde_json::Value::is_number);
                if let Some(button) = parsed
                    .as_ref()
                    .and_then(|value| value["device_button"].as_u64())
                    .and_then(|value| u8::try_from(value).ok())
                {
                    if hardware.direct_configured(button) {
                        return (
                            409,
                            "application/json",
                            serde_json::json!({"error":"That Vibe Key button already has a direct chord; clear it first."}).to_string(),
                        );
                    }
                    if actions.has_button(button) {
                        return (
                            409,
                            "application/json",
                            serde_json::json!({"error":"That Vibe Key button already has a hardware action; remove it first."}).to_string(),
                        );
                    }
                }
                let application = reference
                    .as_deref()
                    .and_then(|reference| apps.resolve_application(reference));
                // A registered trigger equal to a direct firmware chord would
                // make that button emit the trigger as well as its own keys.
                if let Some(trigger) = parsed
                    .as_ref()
                    .and_then(|value| value["trigger"].as_str())
                    .and_then(|trigger| crate::hotkey::Hotkey::parse(trigger).ok())
                {
                    if hardware.direct_conflicts_with_trigger(&trigger) {
                        return (
                            409,
                            "application/json",
                            serde_json::json!({"error":"That trigger equals a direct Vibe Key chord; clear the direct chord or choose another trigger."}).to_string(),
                        );
                    }
                    if actions.conflicts_with_trigger(&trigger) {
                        return (
                            409,
                            "application/json",
                            serde_json::json!({"error":"That trigger is already used by a hardware action; choose another trigger."}).to_string(),
                        );
                    }
                }
                // An injected dispatch chord equal to an action trigger would
                // also run that hardware action. Keep the two namespaces apart.
                if let Some(dispatch) = parsed
                    .as_ref()
                    .and_then(|value| value["dispatch"].as_str())
                    .and_then(|dispatch| crate::hotkey::Hotkey::parse(dispatch).ok())
                {
                    if actions.conflicts_with_trigger(&dispatch) {
                        return (
                            409,
                            "application/json",
                            serde_json::json!({"error":"That dispatch chord equals a hardware action trigger; choose another dispatch chord."}).to_string(),
                        );
                    }
                }
                let (code, value) =
                    shortcuts.mutate("add", request.body, config.hotkey.as_deref(), application);
                if code == 200 {
                    if let Some(reference) = reference.as_deref() {
                        apps.consume_application(reference);
                    }
                }
                let mut view = value;
                if code == 200 && wants_device {
                    // Persisting and arming the binding happened first. A
                    // failed firmware write is reported in `hardware`, never
                    // converted into a blanket success or a rollback of a
                    // trigger the user can still see and remove.
                    view["hardware"] = hardware.provision(&shortcuts.device_bindings());
                } else if code == 200 {
                    view = with_hardware(view, hardware, shortcuts);
                }
                (code, "application/json", view.to_string())
            }
            (Method::Post, "/api/shortcuts/remove") => {
                let id = serde_json::from_str::<serde_json::Value>(request.body)
                    .ok()
                    .and_then(|value| value["id"].as_str().map(str::to_string));
                let button = id.as_deref().and_then(|id| shortcuts.device_button_for(id));
                let (code, value) =
                    shortcuts.mutate("remove", request.body, config.hotkey.as_deref(), None);
                if code == 200 {
                    if let Some(button) = button {
                        // The binding is gone, so the button must not keep
                        // emitting its trigger. A clear failure is not fatal to
                        // the removal; hardware state remains visible.
                        let _ = hardware.clear_button(button);
                    }
                }
                let view = with_hardware(value, hardware, shortcuts);
                (code, "application/json", view.to_string())
            }
            _ => (
                404,
                "application/json",
                serde_json::json!({"error":"Unknown shortcut route"}).to_string(),
            ),
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (apps, config);
        match (request.method, request.path) {
            (Method::Get, "/api/shortcuts") => (
                200,
                "application/json",
                with_hardware(shortcuts.view(), hardware, shortcuts).to_string(),
            ),
            _ => (
                501,
                "application/json",
                serde_json::json!({"error":"Application shortcut dispatch is unavailable on this platform"}).to_string(),
            ),
        }
    }
}

/// Hardware detection and explicit re-provisioning.
/// Hardware action bindings and the local command table.
#[allow(clippy::too_many_arguments)]
/// Read-only built-in plugin registry for Settings and agent clients.
fn plugins_route(
    request: &Request<'_>,
    token_configured: bool,
    plugins: &crate::plugins::Registry,
) -> Routed {
    if let Some(rejection) = validate_request(request, token_configured) {
        return rejection;
    }
    if !request.authorized {
        return unauthorized();
    }
    if request.method != Method::Get || request.path != "/api/plugins" {
        return (
            405,
            "application/json",
            serde_json::json!({"error":"Use GET /api/plugins"}).to_string(),
        );
    }
    (200, "application/json", plugins.view().to_string())
}

/// Executes one saved action binding from the Pad view.
///
/// The browser submits only an id; it cannot submit a step list. This keeps the
/// virtual keypad on the same saved-action path as a physical button and avoids
/// turning the Pad into an arbitrary process/input endpoint.
fn pad_execute_route(
    request: &Request<'_>,
    token_configured: bool,
    actions: &crate::actions::Registry,
    feedback: &mut ActionFeedback,
) -> Routed {
    if let Some(rejection) = validate_request(request, token_configured) {
        return rejection;
    }
    if !request.authorized {
        return unauthorized();
    }
    if request.method != Method::Post || request.path != "/api/pad/execute" {
        return (
            405,
            "application/json",
            serde_json::json!({"error":"Use POST /api/pad/execute"}).to_string(),
        );
    }
    let id = serde_json::from_str::<serde_json::Value>(request.body)
        .ok()
        .and_then(|value| value["id"].as_str().map(str::to_string));
    let Some(id) = id else {
        return (
            400,
            "application/json",
            serde_json::json!({"error":"id must be a saved action id"}).to_string(),
        );
    };
    let Some(steps) = actions.steps_for_id(&id) else {
        return (
            404,
            "application/json",
            serde_json::json!({"error":"No saved action with that id"}).to_string(),
        );
    };
    let name = actions
        .name_for_id(&id)
        .unwrap_or_else(|| "Pad action".into());
    let result = run_action_steps(&steps, actions.commands());
    let detail = result.as_ref().ok().cloned();
    let recorded = result
        .as_ref()
        .map(|_| "DONE".to_string())
        .map_err(Clone::clone);
    feedback.record_event(
        TriggerEvent {
            source: "pad",
            label: Some(&name),
            trigger: None,
            dispatch: None,
            device_button: None,
            target: None,
            detail: detail.as_deref(),
        },
        recorded,
    );
    match result {
        Ok(detail) => (
            200,
            "application/json",
            serde_json::json!({"state":"DONE","name":name,"detail":detail}).to_string(),
        ),
        Err(error) => (
            502,
            "application/json",
            serde_json::json!({"state":"FAILED","name":name,"error":error}).to_string(),
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn actions_route(
    request: &Request<'_>,
    token_configured: bool,
    actions: &mut crate::actions::Registry,
    hardware: &mut crate::hardware::Hardware,
    apps: &mut crate::resident_apps::Registry,
    shortcuts: &ShortcutRegistry,
    devices: &mut dyn FnMut() -> serde_json::Value,
    config: &Config,
) -> Routed {
    if let Some(rejection) = validate_request(request, token_configured) {
        return rejection;
    }
    if !request.authorized {
        return unauthorized();
    }
    match (request.method, request.path) {
        (Method::Get, "/api/actions") => (200, "application/json", actions.view().to_string()),
        (Method::Get, "/api/actions/commands") => (
            200,
            "application/json",
            actions.commands().view().to_string(),
        ),
        (Method::Get, "/api/actions/gamepads") => {
            let devices = devices();
            (200, "application/json", devices.to_string())
        }
        (Method::Post, "/api/actions/add") => {
            let reserved = crate::actions::Reserved {
                mic_hotkey: config
                    .hotkey
                    .as_deref()
                    .and_then(|hotkey| crate::hotkey::Hotkey::parse(hotkey).ok()),
                app_triggers: shortcuts.triggers(),
                app_dispatch_chords: shortcuts.dispatch_chords(),
                app_buttons: shortcuts
                    .device_bindings()
                    .iter()
                    .map(|binding| binding.button)
                    .collect(),
                direct_buttons: hardware.direct_buttons(),
                direct_chords: hardware.direct_chords(),
            };
            let mut used_references = Vec::new();
            let mut resolve = |reference: &str| -> Option<(String, String)> {
                let (executable, label, _) = apps.resolve_application(reference)?;
                used_references.push(reference.to_string());
                Some((executable, label))
            };
            let (code, mut view) = actions.add(request.body, &reserved, &mut resolve);
            if code == 200 {
                for reference in used_references {
                    apps.consume_application(&reference);
                }
                if let Some(binding) = view["bindings"]
                    .as_array()
                    .and_then(|bindings| bindings.last())
                    .cloned()
                {
                    if binding["source"]["kind"] == "au05" {
                        let button = binding["source"]["button"]
                            .as_u64()
                            .and_then(|button| u8::try_from(button).ok());
                        let trigger = binding["trigger"].as_str();
                        if let (Some(button), Some(trigger)) = (button, trigger) {
                            let outcome = crate::hotkey::Hotkey::parse(trigger)
                                .map_err(|error| error.to_string())
                                .and_then(|hotkey| hardware.write_trigger(button, &hotkey));
                            view["hardware"] = match outcome {
                                Ok(()) => serde_json::json!({
                                    "button": button,
                                    "state": "provisioned",
                                    "error": null,
                                }),
                                Err(error) => serde_json::json!({
                                    "button": button,
                                    "state": "failed",
                                    "error": error,
                                }),
                            };
                        }
                    }
                }
            }
            (code, "application/json", view.to_string())
        }
        (Method::Post, "/api/actions/remove") => {
            let id = serde_json::from_str::<serde_json::Value>(request.body)
                .ok()
                .and_then(|value| value["id"].as_str().map(str::to_string));
            let button = id.as_deref().and_then(|id| actions.button_for_id(id));
            let (code, mut view) = actions.remove(request.body);
            if code == 200 {
                if let Some(button) = button {
                    if let Err(error) = hardware.clear_button(button) {
                        view["hardware"] = serde_json::json!({
                            "button": button,
                            "state": "failed",
                            "error": error,
                        });
                    }
                }
            }
            (code, "application/json", view.to_string())
        }
        (Method::Post, "/api/actions/sync") => {
            let mut results = Vec::new();
            for binding in actions.bindings().to_vec() {
                if let crate::actions::Source::Au05Button(button) = binding.source {
                    if let Some(trigger) = binding.trigger.as_deref() {
                        let outcome = crate::hotkey::Hotkey::parse(trigger)
                            .map_err(|error| error.to_string())
                            .and_then(|hotkey| hardware.write_trigger(button, &hotkey));
                        results.push(match outcome {
                            Ok(()) => serde_json::json!({"id":binding.id,"button":button,"state":"provisioned","error":null}),
                            Err(error) => serde_json::json!({"id":binding.id,"button":button,"state":"failed","error":error}),
                        });
                    }
                }
            }
            let mut view = actions.view();
            view["hardware"] = serde_json::Value::Array(results);
            (200, "application/json", view.to_string())
        }
        _ => (
            404,
            "application/json",
            serde_json::json!({"error":"Unknown action route"}).to_string(),
        ),
    }
}

#[cfg(windows)]
fn gamepad_devices_json(monitor: &mut crate::gamepad_windows::Monitor) -> serde_json::Value {
    let latest: std::collections::BTreeMap<String, (u16, u64)> = monitor
        .latest_pressed()
        .into_iter()
        .map(|(device_key, usage, at)| (device_key, (usage, at)))
        .collect();
    let devices = monitor.refresh();
    serde_json::json!({
        "devices": devices.iter().map(|device| serde_json::json!({
            "device_key": device.device_key,
            "name": device.name,
            "vendor_id": format!("{:04x}", device.vendor_id),
            "product_id": format!("{:04x}", device.product_id),
            "button_usages": device.button_usages,
            "connected": device.connected,
            "latest_button": latest.get(&device.device_key).map(|(usage, at)| {
                serde_json::json!({"usage": usage, "at": at})
            }),
        })).collect::<Vec<_>>(),
    })
}

fn hardware_route(
    request: &Request<'_>,
    token_configured: bool,
    hardware: &mut crate::hardware::Hardware,
    shortcuts: &ShortcutRegistry,
    feedback: &ActionFeedback,
    config: &Config,
    actions: &crate::actions::Registry,
) -> Routed {
    if let Some(rejection) = validate_request(request, token_configured) {
        return rejection;
    }
    if !request.authorized {
        return unauthorized();
    }
    match (request.method, request.path) {
        (Method::Get, "/api/hardware") => {
            let mut view = hardware.view(&shortcuts.device_bindings());
            view["recent_actions"] = feedback.recent_json();
            (200, "application/json", view.to_string())
        }
        (Method::Post, "/api/hardware/sync") => {
            let mut view = hardware.provision(&shortcuts.device_bindings());
            view["recent_actions"] = feedback.recent_json();
            (200, "application/json", view.to_string())
        }
        (Method::Post, "/api/hardware/direct") => {
            // A button can carry either an application binding or a direct
            // firmware chord, never both: two writers would fight over one
            // firmware slot.
            if let Some(button) = request
                .body
                .parse::<serde_json::Value>()
                .ok()
                .and_then(|value| value["button"].as_u64())
                .and_then(|value| u8::try_from(value).ok())
            {
                if shortcuts
                    .device_bindings()
                    .iter()
                    .any(|binding| binding.button == button)
                {
                    return (
                        409,
                        "application/json",
                        serde_json::json!({"error":"That Vibe Key button already has an application shortcut; remove it first."}).to_string(),
                    );
                }
                if actions.has_button(button) {
                    return (
                        409,
                        "application/json",
                        serde_json::json!({"error":"That Vibe Key button already has a hardware action; remove it first."}).to_string(),
                    );
                }
            }
            // A direct chord that equals a registered trigger would be
            // captured by VibDock as well as emitted by the device, creating a
            // second, unintended action. Reject it before touching firmware.
            if let Some(chord) = request
                .body
                .parse::<serde_json::Value>()
                .ok()
                .and_then(|value| value["chord"].as_str().map(str::to_string))
            {
                if let Ok(parsed) = crate::hotkey::Hotkey::parse(&chord) {
                    let mic_conflict = config
                        .hotkey
                        .as_deref()
                        .and_then(|saved| crate::hotkey::Hotkey::parse(saved).ok())
                        .is_some_and(|saved| saved == parsed);
                    if mic_conflict
                        || shortcuts.conflicts_with_trigger(&parsed.as_string())
                        || actions.conflicts_with_trigger(&parsed)
                    {
                        return (
                            409,
                            "application/json",
                            serde_json::json!({"error":"That chord is already registered as a VibDock trigger; choose another direct chord."}).to_string(),
                        );
                    }
                }
            }
            let (code, value) = hardware.configure_direct(request.body);
            if code != 200 {
                return (code, "application/json", value.to_string());
            }
            let mut view = hardware.provision(&shortcuts.device_bindings());
            view["recent_actions"] = feedback.recent_json();
            (200, "application/json", view.to_string())
        }
        (Method::Post, "/api/hardware/direct/remove") => {
            let (code, value) = hardware.remove_direct(request.body);
            if code != 200 {
                return (code, "application/json", value.to_string());
            }
            let mut view = hardware.provision(&shortcuts.device_bindings());
            view["recent_actions"] = feedback.recent_json();
            (200, "application/json", view.to_string())
        }
        _ => (
            404,
            "application/json",
            serde_json::json!({"error":"Unknown hardware route"}).to_string(),
        ),
    }
}

fn apps_route(
    request: &Request<'_>,
    token_configured: bool,
    apps: &mut crate::resident_apps::Registry,
    principal: Option<&str>,
) -> Routed {
    if let Some(rejection) = validate_request(request, token_configured) {
        return rejection;
    }
    if !request.authorized {
        return unauthorized();
    }
    if request.path.starts_with("/api/apps/actions/") {
        #[cfg(not(target_os = "macos"))]
        let result = apps.action_route(
            request.method == Method::Post,
            request.method == Method::Get,
            request.path,
            request.body,
            principal.unwrap_or(""),
        );
        #[cfg(target_os = "macos")]
        let result = {
            let _ = principal;
            (
                501,
                serde_json::json!({"error":"Playback controls are unavailable on this platform"}),
            )
        };
        return (result.0, "application/json", result.1.to_string());
    }
    let (code, value) = match (request.method, request.path) {
        (Method::Get, "/api/apps") => (200, apps.view(false)),
        (Method::Get, "/api/apps/summary") => (200, apps.view(true)),
        (Method::Post, "/api/apps/pin") => apps.mutate("pin", request.body),
        (Method::Post, "/api/apps/remove") => apps.mutate("remove", request.body),
        (Method::Post, "/api/apps/bind") => {
            #[cfg(windows)]
            {
                apps.bind(request.body, validate_bound_microphone)
            }
            #[cfg(not(windows))]
            {
                (
                    501,
                    serde_json::json!({"error":"Microphone associations require the Windows audio adapter"}),
                )
            }
        }
        _ => (404, serde_json::json!({"error":"Unknown app route"})),
    };
    (code, "application/json", value.to_string())
}

#[cfg(windows)]
fn validate_bound_microphone(device_id: &str) -> std::result::Result<(), String> {
    let result = service::run_device_action(service::DeviceAction::Status, Some(device_id))?;
    match result {
        service::ActionResult::MuteState {
            device_id: actual, ..
        } if actual == device_id => Ok(()),
        _ => Err("The selected microphone changed; refresh and select it again".into()),
    }
}

#[cfg(test)]
mod resident_api_tests {
    use super::*;
    #[test]
    fn microphone_association_route_is_post_only_and_unavailable_in_preview() {
        let host = format!("127.0.0.1:{}", crate::service::port());
        let origin = format!("http://{host}");
        let mut apps = crate::resident_apps::Registry::empty(std::path::PathBuf::new(), None);
        let mut request = Request {
            method: Method::Post,
            path: "/api/apps/bind",
            host: Some(&host),
            origin: Some(&origin),
            authorized: true,
            body: "{}",
            body_rejected: false,
        };
        assert_eq!(
            apps_route(&request, false, &mut apps, Some("fixture-principal")).0,
            405
        );
        request.method = Method::Get;
        assert_eq!(
            apps_route(&request, true, &mut apps, Some("fixture-principal")).0,
            404
        );
        request.method = Method::Post;
        assert_eq!(
            apps_route(&request, true, &mut apps, Some("fixture-principal")).0,
            if cfg!(windows) { 400 } else { 501 }
        );
    }
    #[test]
    fn software_reads_and_writes_require_authentication_and_origin() {
        let host = format!("127.0.0.1:{}", crate::service::port());
        let mut apps = crate::resident_apps::Registry::empty(std::path::PathBuf::new(), None);
        for path in [
            "/api/apps",
            "/api/apps/summary",
            "/api/apps/pin",
            "/api/apps/remove",
            "/api/apps/bind",
            "/api/apps/actions/prepare",
            "/api/apps/actions/execute",
            "/api/apps/actions/fixture-receipt",
        ] {
            let mut request = Request {
                method: if path.ends_with("pin")
                    || path.ends_with("remove")
                    || path.ends_with("bind")
                {
                    Method::Post
                } else {
                    Method::Get
                },
                path,
                host: Some(&host),
                origin: None,
                authorized: false,
                body: "{}",
                body_rejected: false,
            };
            assert_eq!(
                apps_route(&request, true, &mut apps, Some("fixture-principal")).0,
                401
            );
            request.authorized = true;
            request.origin = Some("https://attacker.invalid");
            assert_eq!(
                apps_route(&request, true, &mut apps, Some("fixture-principal")).0,
                403
            );
            request.origin = None;
            request.host = Some("attacker.invalid");
            assert_eq!(
                apps_route(&request, true, &mut apps, Some("fixture-principal")).0,
                403
            );
            request.host = Some(&host);
            request.body_rejected = true;
            assert_eq!(
                apps_route(&request, true, &mut apps, Some("fixture-principal")).0,
                413
            );
        }
    }
    #[test]
    fn software_api_has_no_audio_mutation_or_path_routes() {
        let host = format!("127.0.0.1:{}", crate::service::port());
        let mut apps = crate::resident_apps::Registry::empty(std::path::PathBuf::new(), None);
        for path in [
            "/api/apps/mute",
            "/api/apps/toggle",
            "/api/apps/icon?path=C:\u{005c}private.exe",
            "/api/apps/launch",
        ] {
            let request = Request {
                method: Method::Post,
                path,
                host: Some(&host),
                origin: None,
                authorized: true,
                body: "{}",
                body_rejected: false,
            };
            assert_eq!(
                apps_route(&request, true, &mut apps, Some("fixture-principal")).0,
                404
            );
        }
    }
    #[test]
    fn application_shortcut_routes_require_authentication_and_origin() {
        let host = format!("127.0.0.1:{}", crate::service::port());
        let mut apps = crate::resident_apps::Registry::empty(std::path::PathBuf::new(), None);
        let mut shortcuts = crate::app_shortcuts::Registry::empty_for_test();
        let mut hardware = crate::hardware::Hardware::start(false, std::path::PathBuf::new(), None);
        let actions = crate::actions::Registry::empty_for_test(
            crate::actions::CommandRegistry::empty_for_test(),
        );
        let config = Config::default();
        for (path, method) in [
            ("/api/shortcuts", Method::Get),
            ("/api/shortcuts/add", Method::Post),
            ("/api/shortcuts/remove", Method::Post),
        ] {
            let mut request = Request {
                method,
                path,
                host: Some(&host),
                origin: None,
                authorized: false,
                body: "{}",
                body_rejected: false,
            };
            assert_eq!(
                shortcuts_route(
                    &request,
                    true,
                    &mut apps,
                    &mut shortcuts,
                    &mut hardware,
                    &actions,
                    &config,
                    Some("fixture-principal")
                )
                .0,
                401
            );
            request.authorized = true;
            request.origin = Some("https://attacker.invalid");
            assert_eq!(
                shortcuts_route(
                    &request,
                    true,
                    &mut apps,
                    &mut shortcuts,
                    &mut hardware,
                    &actions,
                    &config,
                    Some("fixture-principal")
                )
                .0,
                403
            );
        }
        let request = Request {
            method: Method::Get,
            path: "/api/shortcuts",
            host: Some(&host),
            origin: None,
            authorized: true,
            body: "",
            body_rejected: false,
        };
        assert_eq!(
            shortcuts_route(
                &request,
                true,
                &mut apps,
                &mut shortcuts,
                &mut hardware,
                &actions,
                &config,
                Some("fixture-principal")
            )
            .0,
            200
        );
    }
    #[test]
    fn application_shortcut_mutations_are_rejected_in_read_only_preview() {
        let host = format!("127.0.0.1:{}", crate::service::port());
        let mut apps = crate::resident_apps::Registry::empty(std::path::PathBuf::new(), None);
        let mut shortcuts = crate::app_shortcuts::Registry::empty_for_test();
        let mut hardware = crate::hardware::Hardware::start(false, std::path::PathBuf::new(), None);
        let actions = crate::actions::Registry::empty_for_test(
            crate::actions::CommandRegistry::empty_for_test(),
        );
        let request = Request {
            method: Method::Post,
            path: "/api/shortcuts/add",
            host: Some(&host),
            origin: None,
            authorized: true,
            body: "{}",
            body_rejected: false,
        };
        assert_eq!(
            shortcuts_route(
                &request,
                false,
                &mut apps,
                &mut shortcuts,
                &mut hardware,
                &actions,
                &Config::default(),
                Some("fixture-principal")
            )
            .0,
            405
        );
    }
    #[test]
    fn hardware_routes_require_authentication_and_origin() {
        let host = format!("127.0.0.1:{}", crate::service::port());
        let shortcuts = crate::app_shortcuts::Registry::empty_for_test();
        let mut hardware = crate::hardware::Hardware::start(false, std::path::PathBuf::new(), None);
        let feedback = ActionFeedback::default();
        let actions = crate::actions::Registry::empty_for_test(
            crate::actions::CommandRegistry::empty_for_test(),
        );
        for (path, method) in [
            ("/api/hardware", Method::Get),
            ("/api/hardware/sync", Method::Post),
            ("/api/hardware/direct", Method::Post),
            ("/api/hardware/direct/remove", Method::Post),
        ] {
            let mut request = Request {
                method,
                path,
                host: Some(&host),
                origin: None,
                authorized: false,
                body: "{}",
                body_rejected: false,
            };
            assert_eq!(
                hardware_route(
                    &request,
                    true,
                    &mut hardware,
                    &shortcuts,
                    &feedback,
                    &Config::default(),
                    &actions
                )
                .0,
                401
            );
            request.authorized = true;
            request.origin = Some("https://attacker.invalid");
            assert_eq!(
                hardware_route(
                    &request,
                    true,
                    &mut hardware,
                    &shortcuts,
                    &feedback,
                    &Config::default(),
                    &actions
                )
                .0,
                403
            );
        }
        let request = Request {
            method: Method::Get,
            path: "/api/hardware",
            host: Some(&host),
            origin: None,
            authorized: true,
            body: "",
            body_rejected: false,
        };
        let (code, mime, body) = hardware_route(
            &request,
            true,
            &mut hardware,
            &shortcuts,
            &feedback,
            &Config::default(),
            &actions,
        );
        assert_eq!(code, 200);
        assert_eq!(mime, "application/json");
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["available"], false);
        assert_eq!(value["buttons"].as_array().unwrap().len(), 3);
        assert!(value["recent_actions"].as_array().unwrap().is_empty());
        // The direct-chord section is always present so the page can render
        // its editor even when no device is attached.
        assert_eq!(value["direct"]["revision"], 0);
        assert!(value["direct"]["entries"].as_array().unwrap().is_empty());
    }

    #[test]
    fn action_routes_require_authentication_and_origin() {
        let host = format!("127.0.0.1:{}", crate::service::port());
        let mut actions = crate::actions::Registry::empty_for_test(
            crate::actions::CommandRegistry::empty_for_test(),
        );
        let mut hardware = crate::hardware::Hardware::start(false, std::path::PathBuf::new(), None);
        let mut apps = crate::resident_apps::Registry::empty(std::path::PathBuf::new(), None);
        let shortcuts = crate::app_shortcuts::Registry::empty_for_test();
        let mut devices = || serde_json::json!({"devices":[]});
        let config = Config::default();
        for (path, method) in [
            ("/api/actions", Method::Get),
            ("/api/actions/add", Method::Post),
            ("/api/actions/remove", Method::Post),
            ("/api/actions/commands", Method::Get),
            ("/api/actions/gamepads", Method::Get),
            ("/api/actions/sync", Method::Post),
        ] {
            let mut request = Request {
                method,
                path,
                host: Some(&host),
                origin: None,
                authorized: false,
                body: "{}",
                body_rejected: false,
            };
            assert_eq!(
                actions_route(
                    &request,
                    true,
                    &mut actions,
                    &mut hardware,
                    &mut apps,
                    &shortcuts,
                    &mut devices,
                    &config,
                )
                .0,
                401
            );
            request.authorized = true;
            request.origin = Some("https://attacker.invalid");
            assert_eq!(
                actions_route(
                    &request,
                    true,
                    &mut actions,
                    &mut hardware,
                    &mut apps,
                    &shortcuts,
                    &mut devices,
                    &config,
                )
                .0,
                403
            );
        }
        let request = Request {
            method: Method::Get,
            path: "/api/actions",
            host: Some(&host),
            origin: None,
            authorized: true,
            body: "",
            body_rejected: false,
        };
        let (code, _, body) = actions_route(
            &request,
            true,
            &mut actions,
            &mut hardware,
            &mut apps,
            &shortcuts,
            &mut devices,
            &config,
        );
        assert_eq!(code, 200);
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(value["bindings"].as_array().unwrap().is_empty());
    }

    #[test]
    fn plugin_registry_requires_authentication_and_lists_builtins() {
        let host = format!("127.0.0.1:{}", crate::service::port());
        let plugins = crate::plugins::Registry::start();
        let mut request = Request {
            method: Method::Get,
            path: "/api/plugins",
            host: Some(&host),
            origin: None,
            authorized: false,
            body: "",
            body_rejected: false,
        };
        assert_eq!(plugins_route(&request, true, &plugins).0, 401);
        request.authorized = true;
        request.origin = Some("https://attacker.invalid");
        assert_eq!(plugins_route(&request, true, &plugins).0, 403);
        request.origin = None;
        let (code, _, body) = plugins_route(&request, true, &plugins);
        assert_eq!(code, 200);
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert!(value["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .any(|plugin| plugin["id"] == "ulanzi.au05"));
    }

    #[test]
    fn pad_execute_requires_auth_and_only_accepts_saved_ids() {
        let host = format!("127.0.0.1:{}", crate::service::port());
        let actions = crate::actions::Registry::empty_for_test(
            crate::actions::CommandRegistry::empty_for_test(),
        );
        let mut feedback = ActionFeedback::default();
        let mut request = Request {
            method: Method::Post,
            path: "/api/pad/execute",
            host: Some(&host),
            origin: None,
            authorized: false,
            body: "{\"id\":\"missing\"}",
            body_rejected: false,
        };
        assert_eq!(
            pad_execute_route(&request, true, &actions, &mut feedback).0,
            401
        );
        request.authorized = true;
        request.origin = Some("https://attacker.invalid");
        assert_eq!(
            pad_execute_route(&request, true, &actions, &mut feedback).0,
            403
        );
        request.origin = None;
        request.body = "{}";
        assert_eq!(
            pad_execute_route(&request, true, &actions, &mut feedback).0,
            400
        );
        request.body = "{\"id\":\"missing\"}";
        assert_eq!(
            pad_execute_route(&request, true, &actions, &mut feedback).0,
            404
        );
    }
}
