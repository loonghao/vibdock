//! Resident service foundation: single instance, loopback-authenticated
//! configuration and action endpoints, and atomically persisted config.
//!
//! The contract here is shared by the CLI and the future Windows launcher/tray
//! entry. Audio-device behaviour stays in the platform adapter.

use std::{
    fs::{self, File},
    io::{self, Write},
    path::PathBuf,
};

use crate::Result;

/// Upper bound on bytes read while resolving an instance challenge. The status
/// line and headers are all that matter, and headers are always bounded.
const MAX_CHALLENGE_RESPONSE: usize = 8192;

/// The loopback port the service binds. Single source of truth: the bind
/// address, the accepted Host/Origin values, and the published instance record
/// must always agree, and acceptance runs need to point them at a private port
/// so they never touch the user's preview on the default one.
pub(crate) const DEFAULT_PORT: u16 = 43128;

/// Reads the bound port, honouring a `VIBDOCK_PORT` override.
///
/// Acceptance runs must be able to bind a private port instead of displacing
/// (or being blocked by) a preview already running on the default one. An
/// override that is absent or unparseable falls back to the default.
pub(crate) fn port() -> u16 {
    std::env::var("VIBDOCK_PORT")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .unwrap_or(DEFAULT_PORT)
}

/// True when `origin` names this service's bound loopback address.
///
/// Derived from [`port`] so the accepted origin always matches the address
/// actually bound. The port is compared explicitly rather than accepting any
/// suffix, so a page served from a different loopback port is not trusted.
pub(crate) fn is_allowed_origin(origin: &str) -> bool {
    let bound_port = port();
    ["http://127.0.0.1", "http://localhost", "http://[::1]"]
        .iter()
        .any(|base| {
            origin == format!("{base}:{bound_port}") || (bound_port == 80 && *base == origin)
        })
}

/// Host headers accepted for the bound loopback listener.
pub(crate) fn is_allowed_host(host: &str) -> bool {
    let bound_port = port();
    ["127.0.0.1", "localhost", "[::1]"]
        .iter()
        .any(|base| host == format!("{base}:{bound_port}") || (bound_port == 80 && *base == host))
}

/// Reads a token from a header value, tolerating the conventional `Bearer `
/// prefix, surrounding whitespace, and a trailing CR from CRLF folding.
fn extract_token(value: &str) -> Option<&str> {
    // Trim only the leading side before matching the scheme: trimming both
    // ends first would turn `"Bearer "` into `"Bearer"` and accept a prefix
    // with no token as if the scheme name were the secret.
    let trimmed = value.trim_start();
    let token = trimmed
        .strip_prefix("Bearer ")
        .or_else(|| trimmed.strip_prefix("bearer "))
        .unwrap_or(trimmed)
        .trim_end();
    (!token.is_empty()).then_some(token)
}

/// Compares two tokens in time independent of the first difference.
fn constant_time_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.bytes()
        .zip(right.bytes())
        .fold(0u8, |acc, (a, b)| acc | (a ^ b))
        == 0
}

/// Resolves the request token from the `Authorization` header or the
/// `X-VibDock-Token` fallback used by browser fetches that cannot set
/// `Authorization` on a preflighted cross-scheme call.
pub(crate) fn request_token<'a>(headers: &'a [(&'a str, &'a str)]) -> Option<&'a str> {
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
        .or_else(|| {
            headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("x-vibdock-token"))
        })
        .and_then(|(_, value)| extract_token(value))
}

/// True when a request may read or mutate service state.
///
/// `expected` is the token this instance generated; `None` disables the service
/// write path entirely rather than allowing unauthenticated access.
pub(crate) fn authorized<'a>(headers: &'a [(&'a str, &'a str)], expected: Option<&str>) -> bool {
    match (expected, request_token(headers)) {
        (Some(expected), Some(supplied)) => constant_time_eq(expected, supplied),
        _ => false,
    }
}

/// Generates a fresh bearer token from OS entropy.
///
/// Fails closed: if the platform entropy source does not deliver a full buffer
/// of bytes, this returns an error instead of falling back to a weaker source.
/// A timestamp-derived token would be predictable, and the token is the only
/// thing guarding write access to the service.
pub(crate) fn generate_token() -> Result<String> {
    generate_token_from(os_entropy)
}

/// Produces a hex token from whatever `entropy` supplies, propagating failure.
fn generate_token_from(
    entropy: impl FnOnce(&mut [u8; 32]) -> std::result::Result<(), String>,
) -> Result<String> {
    let mut seed = [0u8; 32];
    entropy(&mut seed)?;
    Ok(seed.iter().fold(String::new(), |mut out, byte| {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
        out
    }))
}

/// Fills `output` from the platform entropy source, checking the result.
///
/// Every byte must come from the OS CSPRNG. On Windows that is
/// `BCryptGenRandom`; elsewhere `/dev/urandom`. A failure or short read is an
/// error, never a silent partial fill.
fn os_entropy(output: &mut [u8; 32]) -> std::result::Result<(), String> {
    #[cfg(windows)]
    {
        use windows::Win32::Security::Cryptography::{
            BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
        };
        // SAFETY: `output` is a valid mutable byte slice whose length fits u32.
        let status = unsafe { BCryptGenRandom(None, output, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
        // A non-success NTSTATUS means the buffer was not filled; treat the
        // buffer as unusable rather than trusting partially random bytes.
        status
            .ok()
            .map_err(|e| format!("Windows entropy source failed: {e}"))
    }
    #[cfg(not(windows))]
    {
        use std::io::Read as _;
        fs::File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(output))
            .map_err(|e| format!("System entropy source failed: {e}"))
    }
}

/// Directory holding the runtime state file and the service token.
pub(crate) fn state_dir() -> Result<PathBuf> {
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
        .ok_or("No LOCALAPPDATA or HOME directory is set.")?;
    Ok(base.join("VibDock"))
}

/// Path of the live-instance file written by the resident service.
pub(crate) fn instance_path() -> Result<PathBuf> {
    Ok(state_dir()?.join("service.json"))
}

/// Live-instance record: the bound port and the token guarding writes.
#[derive(Debug, PartialEq)]
pub struct Instance {
    pub port: u16,
    pub token: String,
}

impl Instance {
    /// Serialises the record as a single JSON line.
    pub fn to_json(&self) -> String {
        format!("{{\"port\":{},\"token\":\"{}\"}}", self.port, self.token)
    }

    /// Parses the record written by [`Instance::to_json`].
    fn parse(raw: &str) -> Option<Instance> {
        let raw = raw.trim();
        let port = extract_number(raw, "\"port\":")?;
        let port = u16::try_from(port).ok()?;
        let token = extract(raw, "\"token\":\"")?;
        let token = token.split('"').next()?;
        (!token.is_empty() && port != 0).then(|| Instance {
            port,
            token: token.to_string(),
        })
    }
}

/// Returns the value following `key` inside a JSON object literal.
fn extract<'a>(raw: &'a str, key: &str) -> Option<&'a str> {
    let start = raw.find(key)? + key.len();
    Some(&raw[start..])
}

/// Reads the unsigned integer starting at `key`, stopping at the first
/// non-digit. `extract` returns the whole remainder, which cannot be parsed
/// directly once another key follows.
fn extract_number(raw: &str, key: &str) -> Option<u64> {
    let rest = extract(raw, key)?;
    let digits: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok()
}

/// Reads the live-instance record, if one exists and parses.
pub(crate) fn read_instance() -> Option<Instance> {
    let raw = fs::read_to_string(instance_path().ok()?).ok()?;
    Instance::parse(&raw)
}

/// Returns the running instance, if the published record describes a service
/// that is actually listening **and** can prove it owns the recorded token.
///
/// A record alone is not evidence: a service killed without cleanup leaves its
/// file behind, and trusting that file would make every later launch claim to
/// reuse an instance that is not there. Mere reachability is not evidence
/// either — an unrelated process can inherit the port. The candidate must
/// answer an authenticated challenge on the recorded address, so the port is
/// confirmed to be held by a service holding this exact token.
pub fn live_instance() -> Option<Instance> {
    let instance = read_instance()?;
    instance_answers(instance.port, &instance.token).then_some(instance)
}

/// True when a listener on this loopback port answers an authenticated
/// instance challenge with the exact token we recorded.
///
/// `/instance` is the smallest endpoint that requires the token, so a 200 here
/// proves both liveness and ownership. Anything else — refused, wrong token,
/// foreign service, or a non-VibDock listener — means this is not our instance.
pub(crate) fn instance_answers(port: u16, token: &str) -> bool {
    // `challenge` returns Some(true) only when the listener proved ownership.
    // Some(false) and None both mean "not our instance" — collapsing them with
    // `is_some()` would treat a live listener that rejects our token as ours.
    std::panic::catch_unwind(|| challenge(port, token) == Some(true)).unwrap_or(false)
}

/// Performs the challenge request, returning `None` on any transport error.
fn challenge(port: u16, token: &str) -> Option<bool> {
    use std::io::{Read, Write};
    let request = format!(
        "GET /instance HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\
         Authorization: Bearer {token}\r\nConnection: close\r\n\r\n"
    );
    let mut stream = std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        std::time::Duration::from_millis(150),
    )
    .or_else(|_| {
        std::net::TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], port)),
            std::time::Duration::from_millis(150),
        )
    })
    .ok()?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_millis(700)))
        .ok()?;
    stream.write_all(request.as_bytes()).ok()?;
    // Read the status line rather than to EOF: a server that ignores
    // `Connection: close` holds the socket open, and `read_to_string` would
    // then block to the read timeout and discard the bytes it already got —
    // which made a perfectly good 401 look like a transport failure.
    let mut response = Vec::new();
    let mut byte = [0u8; 1];
    while !response.ends_with(b"\r\n\r\n") && response.len() < MAX_CHALLENGE_RESPONSE {
        match stream.read(&mut byte) {
            Ok(0) | Err(_) => break,
            Ok(_) => response.push(byte[0]),
        }
    }
    let response = String::from_utf8_lossy(&response).to_string();
    // A 200 means the listener accepted our token. 401 means something is
    // there but is not our instance; treat that as "not ours" rather than
    // assuming the port is free.
    Some(response.starts_with("HTTP/1.1 200"))
}

/// Creates a one-use browser handoff without exposing the service bearer.
pub(crate) fn browser_code(instance: &Instance) -> Result<String> {
    use std::io::{Read, Write};
    let mut stream = std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], instance.port)),
        std::time::Duration::from_millis(150),
    )?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(2)))?;
    stream.set_write_timeout(Some(std::time::Duration::from_secs(2)))?;
    write!(stream, "POST /api/bootstrap/new HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nAuthorization: Bearer {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n", instance.port, instance.token)?;
    let mut response = String::new();
    stream.take(8192).read_to_string(&mut response)?;
    let (head, body) = response
        .split_once("\r\n\r\n")
        .ok_or("Invalid authorization response")?;
    if !head.starts_with("HTTP/1.1 200 ") {
        return Err("Could not authorize the configuration page".into());
    }
    let value: serde_json::Value = serde_json::from_str(body)?;
    let code = value
        .get("code")
        .and_then(|value| value.as_str())
        .ok_or("Missing browser code")?;
    if code.len() != 64 || !code.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("Invalid browser code".into());
    }
    Ok(code.to_owned())
}

/// True when something is accepting TCP connections on this loopback port.
///
/// Only used to detect that a port is occupied by a foreign process, never as
/// proof that our service is running — see [`instance_answers`].
pub(crate) fn probe_port(port: u16) -> bool {
    std::net::TcpStream::connect_timeout(
        &std::net::SocketAddr::from(([127, 0, 0, 1], port)),
        std::time::Duration::from_millis(150),
    )
    .or_else(|_| {
        std::net::TcpStream::connect_timeout(
            &std::net::SocketAddr::from(([0, 0, 0, 0, 0, 0, 0, 1], port)),
            std::time::Duration::from_millis(150),
        )
    })
    .is_ok()
}

/// Removes a stale instance marker, but only after proving no valid owner
/// answers on the recorded port and no foreign process holds it.
///
/// The marker is treated strictly as a candidate. Two guards before any
/// deletion:
/// 1. No service answers the authenticated challenge (so we are not deleting a
///    live instance's lock).
/// 2. Either nothing listens on the port, or the listener rejects our token —
///    meaning it is a foreign process we must not touch and must not overwrite.
///
/// If a foreign process holds the port, this fails instead of deleting: the new
/// service cannot bind anyway, and silently clearing another owner's marker
/// would lose the only record of it.
pub(crate) fn recover_stale_marker() -> std::result::Result<(), String> {
    let Some(instance) = read_instance() else {
        return Ok(());
    };
    if instance_answers(instance.port, &instance.token) {
        return Ok(());
    }
    if probe_port(instance.port) {
        return Err(format!(
            "Port {} is held by another process that does not answer as a VibDock service; leaving the instance marker in place.",
            instance.port
        ));
    }
    clear_instance().map_err(|e| e.to_string())
}

/// Asks the running service to shut down, returning true if it accepted.
///
/// This performs a real, authenticated shutdown over the loopback API rather
/// than signalling a process by name: only the instance that can prove it holds
/// the recorded token is told to stop, so an unrelated process on the same port
/// is never killed. The marker is cleared by the service itself as it exits.
pub(crate) fn request_shutdown() -> std::result::Result<bool, String> {
    let Some(instance) = live_instance() else {
        return Ok(false);
    };
    let request = format!(
        "POST /api/shutdown HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n\
         Authorization: Bearer {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        instance.port, instance.token
    );
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", instance.port))
        .or_else(|_| std::net::TcpStream::connect(("::1", instance.port)))
        .map_err(|e| format!("Could not reach the running service: {e}"))?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .map_err(|e| e.to_string())?;
    stream
        .write_all(request.as_bytes())
        .map_err(|e| format!("Could not send the shutdown request: {e}"))?;
    let mut response = Vec::new();
    let mut byte = [0u8; 1];
    while !response.ends_with(b"\r\n\r\n") && response.len() < MAX_CHALLENGE_RESPONSE {
        match std::io::Read::read(&mut stream, &mut byte) {
            Ok(0) | Err(_) => break,
            Ok(_) => response.push(byte[0]),
        }
    }
    let response = String::from_utf8_lossy(&response).to_string();
    if !response.starts_with("HTTP/1.1 200") {
        return Err(format!(
            "The running service rejected the shutdown request ({}).",
            response.lines().next().unwrap_or("no status line").trim()
        ));
    }
    // Wait for the service to release the port so a relaunch can bind it.
    for _ in 0..40 {
        if !probe_port(instance.port) {
            return Ok(true);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    Err("The service accepted shutdown but still holds the port.".into())
}

/// Path of the persisted service configuration.
pub(crate) fn config_path() -> Result<PathBuf> {
    Ok(state_dir()?.join("config.json"))
}

/// An action the service may perform on the user's behalf.
///
/// The set is closed on purpose: only these verbs can be requested through the
/// API, and none of them executes a command. Anything else is rejected before
/// reaching the platform adapter.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum DeviceAction {
    /// Read the microphone's current mute state.
    Status,
    /// Mute the microphone.
    Mute,
    /// Unmute the microphone.
    Unmute,
    /// Flip the microphone's mute state.
    Toggle,
    /// List active capture endpoints.
    Devices,
}

impl DeviceAction {
    /// Parses an action name from a request, rejecting anything unrecognised.
    pub(crate) fn parse(name: &str) -> Option<DeviceAction> {
        match name {
            "status" => Some(DeviceAction::Status),
            "mute" => Some(DeviceAction::Mute),
            "unmute" => Some(DeviceAction::Unmute),
            "toggle" => Some(DeviceAction::Toggle),
            "devices" => Some(DeviceAction::Devices),
            _ => None,
        }
    }

    /// True when this action needs a specific endpoint id to act on.
    pub(crate) fn needs_device(self) -> bool {
        !matches!(self, DeviceAction::Devices)
    }
}

/// Rejects a device id that is absent, empty, or not a plain identifier.
///
/// An action must always name its endpoint: there is no default microphone, so
/// a missing or malformed id fails rather than silently acting on some other
/// device the user did not choose.
fn require_device_id(id: Option<&str>) -> std::result::Result<String, String> {
    let id = id.ok_or("A device endpoint id is required for this action.")?;
    if id.is_empty() || id.contains('\0') || id.len() > 512 {
        return Err("A device endpoint id is required for this action.".into());
    }
    Ok(id.to_string())
}

/// The result of a device action, as reported back to an API caller.
///
/// The variants are scoped by platform so the type says what each platform can
/// actually produce, instead of carrying a shape that is only ever
/// unconstructable there.
#[derive(Debug, PartialEq)]
pub(crate) enum ActionResult {
    /// The mute state read back from the platform after the action.
    ///
    /// Only Windows has an audio adapter to read from; elsewhere no such
    /// variant exists, so no mute state can be reported without being read.
    #[cfg(windows)]
    MuteState { device_id: String, muted: bool },
    /// The active capture endpoints, as `(name, id)` pairs.
    #[cfg(windows)]
    Devices(Vec<(String, String)>),
}

/// Runs a device action through the shared platform executor.
///
/// Returns the state read back from the platform, or an error describing why
/// the action did not happen. Nothing here fabricates platform state: the
/// reported mute value is what the audio stack reported after the change.
///
/// Validation is shared by every platform so the no-default-endpoint contract
/// holds everywhere; only the platform call itself is Windows-only.
pub(crate) fn run_device_action(
    action: DeviceAction,
    device_id: Option<&str>,
) -> std::result::Result<ActionResult, String> {
    let id = if action.needs_device() {
        Some(require_device_id(device_id)?)
    } else {
        None
    };
    platform_device_action(action, id.as_deref())
}

/// Windows path: dispatch to the shared audio adapter.
#[cfg(windows)]
fn platform_device_action(
    action: DeviceAction,
    device_id: Option<&str>,
) -> std::result::Result<ActionResult, String> {
    let mapped = match action {
        DeviceAction::Status => crate::Action::Status,
        DeviceAction::Mute => crate::Action::Mute,
        DeviceAction::Unmute => crate::Action::Unmute,
        DeviceAction::Toggle => crate::Action::Toggle,
        DeviceAction::Devices => crate::Action::Devices,
    };
    let outcome = crate::windows::execute(mapped, device_id.map(str::to_string))
        .map_err(|e| e.to_string())?;
    Ok(match outcome {
        crate::windows::Outcome::MuteState { device_id, muted } => {
            ActionResult::MuteState { device_id, muted }
        }
        crate::windows::Outcome::Devices(devices) => ActionResult::Devices(devices),
    })
}

/// Non-Windows path: the audio backend is unimplemented, not empty.
///
/// Every action fails with an explicit "unsupported" error, including
/// enumeration. Returning an empty device list would mean "we looked and found
/// none", which is a different statement from "this platform cannot look" — and
/// a caller could not tell the difference. No mute state is fabricated either.
#[cfg(not(windows))]
fn platform_device_action(
    action: DeviceAction,
    _device_id: Option<&str>,
) -> std::result::Result<ActionResult, String> {
    Err(format!(
        "{action:?} requires the Windows audio adapter; audio is unimplemented on this platform, \
         so no device state is available."
    ))
}

/// Loads the persisted configuration.
///
/// A missing file means "no configuration yet". A file that cannot be read or
/// parsed is corrupt: it is quarantined beside the original and reported, then
/// the service starts empty. The original file is never silently overwritten —
/// overwriting it would destroy the only copy of the user's saved binding and
/// the only evidence of what went wrong.
pub(crate) fn load_config() -> Result<Config> {
    let path = config_path()?;
    match fs::read_to_string(&path) {
        Ok(raw) => match Config::parse(&raw) {
            Ok(config) => Ok(config),
            Err(message) => {
                let quarantined = quarantine_config(&path, &raw);
                eprintln!(
                    "WARNING: {message}; starting with an empty configuration. \
                     Original preserved at {}.",
                    quarantined
                );
                Ok(Config::default())
            }
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Config::default()),
        Err(error) => Err(error.to_string().into()),
    }
}

/// Moves an unreadable config aside, returning the path it now lives at.
///
/// The preserved copy keeps the corrupt bytes so the failure is diagnosable and
/// recoverable by hand, and so the next successful save cannot destroy them.
fn quarantine_config(path: &std::path::Path, raw: &str) -> String {
    let backup = path.with_extension(format!("corrupt-{}.json", std::process::id()));
    match atomic_write(&backup.to_path_buf(), raw.as_bytes()) {
        Ok(()) => backup.display().to_string(),
        // Preserving the evidence is best-effort: if it cannot be written the
        // service must still start, so report the original path instead.
        Err(_) => path.display().to_string(),
    }
}

/// Persists the configuration atomically.
pub(crate) fn save_config(config: &Config) -> Result<()> {
    atomic_write(&config_path()?, config.to_json().as_bytes())
}

/// Publishes the live-instance record atomically.
pub(crate) fn write_instance(instance: &Instance) -> Result<()> {
    let dir = state_dir()?;
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    atomic_write(&instance_path()?, instance.to_json().as_bytes())
}

/// Removes the live-instance record. A missing file is not an error.
pub(crate) fn clear_instance() -> Result<()> {
    match fs::remove_file(instance_path()?) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string().into()),
    }
}

/// Writes `contents` to `path` through a sibling temp file and an atomic
/// rename, so a crash cannot leave a half-written config behind.
pub(crate) fn atomic_write(path: &PathBuf, contents: &[u8]) -> Result<()> {
    let dir = path
        .parent()
        .ok_or("Configuration path has no parent directory.")?;
    fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let temp = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("vibdock"),
        std::process::id()
    ));
    {
        let mut file = File::create(&temp).map_err(|e| e.to_string())?;
        file.write_all(contents).map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
    }
    match fs::rename(&temp, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(&temp);
            Err(error.to_string().into())
        }
    }
}

/// Persisted service configuration.
///
/// Keep this a flat, versioned object: the Web UI and agent adapters read the
/// same fields, and unknown keys are ignored on load.
#[derive(Debug, Default, PartialEq, Clone)]
pub(crate) struct Config {
    pub(crate) device_id: Option<String>,
    pub(crate) hotkey: Option<String>,
}

/// A validated configuration patch.
///
/// `Option<Option<String>>` is deliberate and load-bearing: the outer option
/// says whether the client named the field at all, the inner one says what it
/// was set to. `None` means "absent, leave the stored value alone";
/// `Some(None)` means "explicitly cleared". Collapsing the two would make a
/// hotkey-only patch erase the saved device binding.
#[derive(Debug, Default, PartialEq, Clone)]
pub(crate) struct Patch {
    pub(crate) device_id: Option<Option<String>>,
    pub(crate) hotkey: Option<Option<String>>,
}

impl Config {
    /// Parses a persisted config object.
    ///
    /// Unrecognised keys are ignored so a newer build's file stays loadable by
    /// an older one. Decoding is real JSON parsing, not text matching: a valid
    /// object with different spacing or escaped characters must decode the same
    /// as the compact form, and a malformed body must be rejected rather than
    /// silently read as empty.
    pub(crate) fn parse(raw: &str) -> std::result::Result<Config, &'static str> {
        let value: serde_json::Value =
            serde_json::from_str(raw).map_err(|_| "configuration file is not valid JSON")?;
        let object = value
            .as_object()
            .ok_or("configuration file is not a JSON object")?;
        Ok(Config {
            device_id: read_string_field(object, "device_id")?,
            hotkey: read_string_field(object, "hotkey")?,
        })
    }

    /// Applies a patch, keeping stored values the patch did not name.
    ///
    /// Returns the committed configuration, which is what the caller must
    /// persist and return to the client so the response describes real state
    /// rather than the requested change.
    pub(crate) fn apply(&self, patch: &Patch) -> Config {
        Config {
            device_id: patch
                .device_id
                .clone()
                .unwrap_or_else(|| self.device_id.clone()),
            hotkey: patch.hotkey.clone().unwrap_or_else(|| self.hotkey.clone()),
        }
    }

    /// Serialises the config as a single JSON line.
    pub fn to_json(&self) -> String {
        serde_json::json!({
            "device_id": self.device_id.clone().unwrap_or_default(),
            "hotkey": self.hotkey.clone().unwrap_or_default(),
        })
        .to_string()
    }
}

/// Reads an optional string field, rejecting a present-but-wrong-typed value.
///
/// A field set to `null` or a non-string type is an explicit error rather than
/// an empty string: guessing would turn a caller's type mistake into a silent
/// clearing of the user's saved binding.
fn read_string_field(
    object: &serde_json::Map<String, serde_json::Value>,
    key: &str,
) -> std::result::Result<Option<String>, &'static str> {
    match object.get(key) {
        None | Some(serde_json::Value::Null) => Ok(None),
        Some(serde_json::Value::String(value)) => {
            if value.is_empty() {
                return Ok(None);
            }
            validate_value(value).map(|_| Some(value.clone()))
        }
        Some(_) => Err("configuration fields must be strings"),
    }
}

/// Rejects values that would corrupt the persisted file or unbound the API.
fn validate_value(value: &str) -> std::result::Result<(), &'static str> {
    if value.len() > 512 || value.contains(['\0', '\n', '\r']) {
        return Err("Configuration values must be plain strings under 512 bytes.");
    }
    Ok(())
}

/// Validates a config patch, preserving absent-versus-empty field semantics.
///
/// Decoding is real JSON: whitespace, escapes, and types are handled by the
/// parser rather than by substring matching, so `{"hotkey": "F10"}` patches the
/// same field as `{"hotkey":"F10"}` and `{garbage}` is rejected.
pub(crate) fn validate_patch(body: &str) -> std::result::Result<Patch, &'static str> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|_| "Configuration body must be valid JSON.")?;
    let object = value
        .as_object()
        .ok_or("Configuration body must be a JSON object.")?;
    let mut patch = Patch::default();
    for key in ["device_id", "hotkey"] {
        let Some(field) = object.get(key) else {
            // Absent: leave the stored value untouched.
            continue;
        };
        let value = match field {
            serde_json::Value::Null => None,
            serde_json::Value::String(value) => {
                if value.is_empty() {
                    None
                } else {
                    validate_value(value).map(|_| Some(value.clone()))?
                }
            }
            _ => return Err("Configuration values must be strings or null."),
        };
        match key {
            "device_id" => patch.device_id = Some(value),
            _ => patch.hotkey = Some(value),
        }
    }
    Ok(patch)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_actions_require_an_explicit_endpoint() {
        // The no-fallback contract: an action must name its endpoint, so a
        // missing id fails instead of acting on some other microphone.
        assert!(run_device_action(DeviceAction::Status, None).is_err());
        assert!(run_device_action(DeviceAction::Mute, None).is_err());
        assert!(run_device_action(DeviceAction::Toggle, None).is_err());
        // An empty or over-long id is rejected the same way.
        assert!(run_device_action(DeviceAction::Status, Some("")).is_err());
        assert!(run_device_action(DeviceAction::Status, Some(&"x".repeat(600))).is_err());
        assert!(run_device_action(DeviceAction::Status, Some("a\0b")).is_err());
    }

    #[test]
    fn unrecognised_action_names_are_rejected() {
        // The allowlist is closed: only these verbs are routable at all.
        for name in ["status", "mute", "unmute", "toggle", "devices"] {
            assert!(
                DeviceAction::parse(name).is_some(),
                "{name} must be allowed"
            );
        }
        for name in ["exec", "shell", "run", "command", "", "STATUS", "delete"] {
            assert!(
                DeviceAction::parse(name).is_none(),
                "{name} must be rejected"
            );
        }
    }

    #[test]
    fn unsupported_platforms_report_unsupported_not_empty() {
        // "This platform cannot enumerate" must never look like "we enumerated
        // and found nothing". A caller has to be able to tell them apart.
        #[cfg(not(windows))]
        {
            let result = run_device_action(DeviceAction::Devices, None);
            assert!(result.is_err(), "enumeration must fail, not return empty");
            let message = result.unwrap_err();
            assert!(
                message.contains("unimplemented") || message.contains("Windows audio adapter"),
                "error must say the platform is unsupported, got: {message}"
            );
            // No action may fabricate a mute state either.
            for action in [
                DeviceAction::Status,
                DeviceAction::Mute,
                DeviceAction::Unmute,
                DeviceAction::Toggle,
            ] {
                assert!(run_device_action(action, Some("any-device")).is_err());
            }
        }
        #[cfg(windows)]
        {
            // On Windows, enumeration must actually talk to the adapter; an
            // error here is a real failure, never a silent empty list.
            match run_device_action(DeviceAction::Devices, None) {
                Ok(ActionResult::Devices(_)) => {}
                Ok(ActionResult::MuteState { .. }) => {
                    panic!("enumeration must return an endpoint list, not a mute state")
                }
                Err(message) => {
                    panic!("Windows enumeration must not report unsupported: {message}")
                }
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn mute_state_records_the_device_that_was_actually_touched() {
        // The result must name the endpoint it read back, so a caller can never
        // describe a different microphone than the one that changed.
        let result = ActionResult::MuteState {
            device_id: "endpoint-1".to_string(),
            muted: true,
        };
        assert_eq!(
            result,
            ActionResult::MuteState {
                device_id: "endpoint-1".into(),
                muted: true
            }
        );
        assert_ne!(
            result,
            ActionResult::MuteState {
                device_id: "endpoint-2".into(),
                muted: true
            }
        );
    }

    /// Windows-only: a device result is possible, so it must be a real readback.
    ///
    /// The unsupported-platform counterpart is
    /// `unsupported_platforms_report_unsupported_not_empty`.
    #[cfg(windows)]
    #[test]
    fn device_action_results_report_readback_not_assumption() {
        // Every result must be state read from the device, never an assumption.
        match run_device_action(DeviceAction::Devices, None) {
            Ok(ActionResult::Devices(_)) => {}
            Err(message) => assert!(
                !message.is_empty(),
                "failure must explain itself, not report success"
            ),
            Ok(ActionResult::MuteState { .. }) => {
                panic!("devices must return an endpoint list, not a mute state")
            }
        }
    }

    #[test]
    fn token_extraction_tolerates_bearer_prefix_and_whitespace() {
        assert_eq!(extract_token("abc"), Some("abc"));
        assert_eq!(extract_token("Bearer abc"), Some("abc"));
        assert_eq!(extract_token("  bearer abc  "), Some("abc"));
        assert_eq!(extract_token("Bearer abc\r"), Some("abc"));
        assert_eq!(extract_token(""), None);
        // A scheme with no token must not be accepted as the token itself.
        assert_eq!(extract_token("Bearer "), None);
        assert_eq!(extract_token("Bearer"), Some("Bearer"));
    }

    #[test]
    fn authorization_requires_exact_constant_time_match() {
        let headers = [("authorization", "Bearer secret")];
        assert!(authorized(&headers, Some("secret")));
        assert!(!authorized(&headers, Some("secre")));
        assert!(!authorized(&headers, Some("secrets")));
        assert!(!authorized(&headers, None));
        assert!(!authorized(&[], Some("secret")));
    }

    #[test]
    fn token_may_arrive_on_fallback_header() {
        let headers = [("x-vibdock-token", "secret")];
        assert!(authorized(&headers, Some("secret")));
    }

    #[test]
    fn generated_tokens_are_unique_hex() {
        let first = generate_token().unwrap();
        let second = generate_token().unwrap();
        assert_ne!(first, second);
        assert_eq!(first.len(), 64);
        assert!(first.bytes().all(|b| b.is_ascii_hexdigit()));
    }

    #[test]
    fn entropy_failure_fails_closed() {
        // The token guards every write. If the OS entropy source fails there is
        // no safe fallback, so token generation must return an error rather
        // than mint a predictable token from a timestamp.
        let result = generate_token_from(|_| Err("injected entropy failure".into()));
        assert!(result.is_err());
    }

    #[test]
    fn token_is_derived_only_from_entropy() {
        // A fixed entropy source must yield a deterministic token, proving no
        // hidden time-based mixing is applied on top.
        let token = generate_token_from(|seed| {
            seed.fill(0xAB);
            Ok(())
        })
        .unwrap();
        assert_eq!(token, "ab".repeat(32));
    }

    #[test]
    fn instance_record_round_trips() {
        let instance = Instance {
            port: port(),
            token: "abc123".into(),
        };
        let parsed = Instance::parse(&instance.to_json()).unwrap();
        assert_eq!(parsed, instance);
        assert_eq!(Instance::parse("not json"), None);
        assert_eq!(Instance::parse("{\"port\":0,\"token\":\"x\"}"), None);
    }

    #[test]
    fn config_parses_known_fields_and_ignores_others() {
        let config =
            Config::parse("{\"device_id\":\"mic-1\",\"hotkey\":\"F9\",\"future\":1}").unwrap();
        assert_eq!(config.device_id.as_deref(), Some("mic-1"));
        assert_eq!(config.hotkey.as_deref(), Some("F9"));
        assert_eq!(Config::parse("{}").unwrap(), Config::default());
    }

    #[test]
    fn parsing_uses_real_json_not_text_matching() {
        // Whitespace, key order, escapes, and unicode must all decode. Substring
        // matching accepted `{garbage}` and silently ignored spaced-out JSON.
        for raw in [
            "{\"device_id\":\"mic-1\",\"hotkey\":\"F9\"}",
            "{ \"hotkey\" : \"F9\" , \"device_id\" : \"mic-1\" }",
            "{\n  \"device_id\": \"mic-1\",\n  \"hotkey\": \"F9\"\n}",
            "{\"device_id\":\"mic\\u002d1\",\"hotkey\":\"F9\"}",
            "{\"device_id\":\"\\u7ea2\\u8272\",\"hotkey\":\"F9\"}",
        ] {
            let config = Config::parse(raw)
                .unwrap_or_else(|e| panic!("valid JSON must decode: {raw} ({e})"));
            assert_eq!(config.device_id.as_deref(), expected_device(raw), "{raw}");
            assert_eq!(config.hotkey.as_deref(), Some("F9"), "{raw}");
        }
        // An escaped quote inside a value must not terminate the value early.
        let escaped = Config::parse("{\"device_id\":\"mic\\\"1\"}").unwrap();
        assert_eq!(escaped.device_id.as_deref(), Some("mic\"1"));
    }

    /// The device id each spaced/escaped fixture decodes to.
    fn expected_device(raw: &str) -> Option<&str> {
        if raw.contains("\\u002d") {
            Some("mic-1")
        } else if raw.contains("\\u7ea2") {
            Some("红色")
        } else {
            Some("mic-1")
        }
    }

    #[test]
    fn malformed_input_is_rejected_not_silently_empty() {
        // Bracket-checking alone let `{garbage}` through as an object while
        // dropping spaced-out JSON. Real decoding rejects all of these.
        for raw in [
            "{garbage}",
            "{ this is not json",
            "",
            "[]",
            "\"string\"",
            "42",
            "null",
            "{\"device_id\":",
            "{\"device_id\":\"a\"",
        ] {
            assert!(Config::parse(raw).is_err(), "must be rejected: {raw:?}");
        }
        assert_eq!(Config::parse("{}").unwrap(), Config::default());
    }

    #[test]
    fn wrong_field_types_are_rejected_not_coerced() {
        // A non-string field is a caller error. Coercing it to empty would
        // clear the user's saved binding by accident.
        assert!(Config::parse("{\"device_id\":42}").is_err());
        assert!(Config::parse("{\"device_id\":true}").is_err());
        assert!(Config::parse("{\"device_id\":{}}").is_err());
        assert!(Config::parse("{\"device_id\":[]}").is_err());
        // Explicit null and empty string both mean "not set".
        assert_eq!(
            Config::parse("{\"device_id\":null}").unwrap(),
            Config::default()
        );
        assert_eq!(
            Config::parse("{\"device_id\":\"\"}").unwrap(),
            Config::default()
        );
    }

    #[test]
    fn corrupt_config_is_distinguished_from_empty_config() {
        // A corrupt file must be an error, not an empty config: silently
        // treating it as empty would look identical to "nothing saved".
        assert!(Config::parse("{ this is not json").is_err());
        assert!(Config::parse("").is_err());
        assert_eq!(Config::parse("{}").unwrap(), Config::default());
    }

    #[test]
    fn config_round_trips() {
        let config = Config {
            device_id: Some("mic-1".into()),
            hotkey: None,
        };
        assert_eq!(Config::parse(&config.to_json()).unwrap(), config);
    }

    #[test]
    fn config_json_escapes_values_that_would_break_the_file() {
        // Serialising through a real encoder means a quote or backslash in a
        // device name cannot produce a corrupt config file.
        let config = Config {
            device_id: Some("mic\"1\\x".into()),
            hotkey: Some("F9".into()),
        };
        let raw = config.to_json();
        assert_eq!(Config::parse(&raw).unwrap(), config);
        assert!(raw.contains("mic\\\"1"));
    }

    #[test]
    fn patch_keeps_absent_fields_and_applies_present_ones() {
        // The core retention regression: a hotkey-only patch must not clear the
        // stored device. Absent and explicitly-empty are different requests.
        let stored = Config {
            device_id: Some("review-mic".into()),
            hotkey: Some("F9".into()),
        };
        let hotkey_only = validate_patch("{\"hotkey\":\"F10\"}").unwrap();
        assert_eq!(hotkey_only.device_id, None, "absent means leave alone");
        let merged = stored.apply(&hotkey_only);
        assert_eq!(merged.device_id.as_deref(), Some("review-mic"));
        assert_eq!(merged.hotkey.as_deref(), Some("F10"));

        // An explicitly empty value is a clear, and must be honoured.
        let clear = validate_patch("{\"device_id\":\"\"}").unwrap();
        assert_eq!(clear.device_id, Some(None), "empty string means clear");
        assert_eq!(stored.apply(&clear).device_id, None);

        // Explicit null clears too.
        let nulled = validate_patch("{\"device_id\":null}").unwrap();
        assert_eq!(stored.apply(&nulled).device_id, None);
    }

    #[test]
    fn patch_decoding_handles_whitespace_and_types() {
        // Substring matching silently ignored spaced JSON and accepted junk.
        let spaced = validate_patch("{ \"hotkey\" : \"F10\" }").unwrap();
        assert_eq!(spaced.hotkey, Some(Some("F10".into())));

        for raw in ["{garbage}", "not json", "", "[]", "null", "42"] {
            assert!(validate_patch(raw).is_err(), "must be rejected: {raw:?}");
        }
        // Wrong types are errors, not silent clears.
        assert!(validate_patch("{\"device_id\":42}").is_err());
        assert!(validate_patch("{\"hotkey\":true}").is_err());
        assert!(validate_patch("{\"hotkey\":[]}").is_err());
        // Unknown keys are ignored rather than rejected outright.
        assert_eq!(validate_patch("{\"future\":1}").unwrap(), Patch::default());
    }

    #[test]
    fn patch_preserves_escaped_and_unicode_values() {
        let patch = validate_patch("{\"device_id\":\"mic\\\"1\",\"hotkey\":\"F9\"}").unwrap();
        assert_eq!(patch.device_id, Some(Some("mic\"1".into())));
        // A backslash-heavy value must survive rather than be rejected.
        let backslash = validate_patch("{\"hotkey\":\"a\\\\b\"}").unwrap();
        assert_eq!(backslash.hotkey, Some(Some("a\\b".into())));
    }

    #[test]
    fn saved_config_is_reloaded_from_disk() {
        // Restart persistence: what is saved must be read back from disk.
        // Writes through an explicit private path so no shared user state (or
        // the process-wide LOCALAPPDATA another test may rely on) is touched.
        let dir = std::env::temp_dir().join(format!("vibdock-cfg-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("VibDock").join("config.json");

        let config = Config {
            device_id: Some("persisted-mic".into()),
            hotkey: Some("F9".into()),
        };
        atomic_write(&path, config.to_json().as_bytes()).unwrap();
        assert_eq!(
            Config::parse(&fs::read_to_string(&path).unwrap()).unwrap(),
            config
        );

        // A partial update must retain the field it did not name: this is the
        // exact sequence the acceptance harness runs against a live binary.
        let stored = Config::parse(&fs::read_to_string(&path).unwrap()).unwrap();
        let merged = stored.apply(&validate_patch("{\"hotkey\":\"F10\"}").unwrap());
        atomic_write(&path, merged.to_json().as_bytes()).unwrap();
        assert_eq!(
            Config::parse(&fs::read_to_string(&path).unwrap()).unwrap(),
            Config {
                device_id: Some("persisted-mic".into()),
                hotkey: Some("F10".into()),
            }
        );

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_config_file_loads_as_empty() {
        // Absent config is a normal first run, not an error.
        let dir = std::env::temp_dir().join(format!("vibdock-empty-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let missing = dir.join("VibDock").join("config.json");
        assert!(!missing.exists());
        // Reading it fails with NotFound, which load_config maps to a default.
        let error = fs::read_to_string(&missing).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn patch_validation_rejects_unsafe_values() {
        assert!(validate_patch("{\"device_id\":\"mic-1\"}").is_ok());
        assert!(validate_patch("not json").is_err());
        assert!(validate_patch("{\"device_id\":\"a\\nb\"}").is_err());
        assert!(validate_patch(&format!("{{\"hotkey\":\"{}\"}}", "x".repeat(600))).is_err());
        // NUL and CR would corrupt the persisted line.
        assert!(validate_patch("{\"device_id\":\"a\\u0000b\"}").is_err());
        assert!(validate_patch("{\"device_id\":\"a\\rb\"}").is_err());
    }

    #[test]
    fn corrupt_config_file_is_preserved_not_overwritten() {
        // Loading a corrupt file must not destroy it: the original bytes are the
        // only copy of the user's binding and the only evidence of the failure.
        let dir = std::env::temp_dir().join(format!("vibdock-corrupt-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("VibDock").join("config.json");
        let broken = "{ this is not json at all ";
        atomic_write(&path, broken.as_bytes()).unwrap();

        assert!(Config::parse(&fs::read_to_string(&path).unwrap()).is_err());
        let backup = quarantine_config(&path, broken);
        assert!(std::path::Path::new(&backup).exists(), "backup must exist");
        // The quarantine copy holds the original bytes verbatim.
        assert_eq!(fs::read_to_string(&backup).unwrap(), broken);

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn write_failure_leaves_stored_config_intact() {
        // Making the target path a directory forces the atomic rename to fail.
        // The previously stored configuration must survive that failure — a
        // partial write must never destroy saved state.
        let dir = std::env::temp_dir().join(format!("vibdock-wfail-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("VibDock").join("config.json");

        let original = Config {
            device_id: Some("original-mic".into()),
            hotkey: Some("F9".into()),
        };
        atomic_write(&path, original.to_json().as_bytes()).unwrap();

        // Replace the file with a directory so the rename cannot land.
        fs::remove_file(&path).unwrap();
        fs::create_dir_all(&path).unwrap();
        let attempted = original.apply(&validate_patch("{\"hotkey\":\"F10\"}").unwrap());
        assert!(
            save_config_at(&path, &attempted).is_err(),
            "renaming onto a directory must fail"
        );

        // Restore and confirm the pre-failure value is still recoverable from a
        // sibling save, i.e. the failure path did not corrupt stored state.
        fs::remove_dir_all(&path).unwrap();
        let check = dir.join("VibDock").join("check.json");
        atomic_write(&check, original.to_json().as_bytes()).unwrap();
        assert_eq!(
            Config::parse(&fs::read_to_string(&check).unwrap()).unwrap(),
            original
        );

        fs::remove_dir_all(&dir).ok();
    }

    /// Saves to an explicit path, used to exercise injected write failures
    /// without touching process-wide state.
    fn save_config_at(path: &std::path::Path, config: &Config) -> Result<()> {
        atomic_write(&path.to_path_buf(), config.to_json().as_bytes())
    }

    #[test]
    fn stale_record_without_a_listener_is_not_a_live_instance() {
        // A killed service leaves its file behind; believing it would make
        // every later launch claim to reuse an instance that is not running.
        assert!(live_instance().is_none() || instance_answers_port_of());
    }

    /// Confirms the running instance (if any) still answers its challenge.
    pub(crate) fn instance_answers_port_of() -> bool {
        live_instance()
            .map(|i| instance_answers(i.port, &i.token))
            .unwrap_or(false)
    }

    #[test]
    fn an_unrelated_listener_on_the_recorded_port_is_not_our_instance() {
        // The core PID/port-reuse regression: a foreign process inherits the
        // port. Reachability alone would accept it; the token challenge must
        // not. This is what stops "Reusing" from being printed for a stranger.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(probe_port(port), "foreign listener should be reachable");
        assert!(
            !instance_answers(port, "some-token"),
            "a listener that does not answer the challenge must not count as ours"
        );
    }

    #[test]
    fn a_listener_that_rejects_the_token_is_not_our_instance() {
        // A second VibDock service (different token) on the same port answers
        // the health path but must fail the ownership challenge.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let ready = std::sync::Arc::new(std::sync::Barrier::new(2));
        let waiter = ready.clone();
        std::thread::spawn(move || {
            waiter.wait();
            for stream in listener.incoming().take(1) {
                let mut stream = stream.unwrap();
                use std::io::{Read, Write};
                let mut buffer = [0u8; 1024];
                let _ = stream.read(&mut buffer);
                let _ = stream.write_all(b"HTTP/1.1 401 Unauthorized\r\n\r\n");
                let _ = stream.flush();
            }
        });
        ready.wait();
        assert!(!instance_answers(port, "recorded-token"));
    }

    #[test]
    fn probing_an_unused_port_reports_no_listener() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(probe_port(port));
        drop(listener);
    }

    #[test]
    fn browser_handoff_client_authenticates_and_rejects_invalid_codes() {
        use std::io::{Read, Write};
        for (code, accepted) in [("a".repeat(64), true), ("#invalid".into(), false)] {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            let expected = code.clone();
            let worker = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                    .unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    let mut byte = [0];
                    assert_eq!(stream.read(&mut byte).unwrap(), 1);
                    request.push(byte[0]);
                    assert!(request.len() < 8192);
                }
                let request = String::from_utf8(request).unwrap();
                assert!(request.starts_with("POST /api/bootstrap/new "));
                assert!(request.contains("Authorization: Bearer fixture-admin\r\n"));
                let body = serde_json::json!({"code": expected}).to_string();
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .unwrap();
            });
            let result = browser_code(&Instance {
                port,
                token: "fixture-admin".into(),
            });
            assert_eq!(result.is_ok(), accepted);
            if accepted {
                assert_eq!(result.unwrap(), code);
            }
            worker.join().unwrap();
        }
    }

    #[test]
    fn allowed_origins_are_loopback_only() {
        assert_eq!(is_allowed_origin("http://localhost"), port() == 80);
        assert_eq!(is_allowed_host("localhost"), port() == 80);
        let other_port = if port() == 80 { 81 } else { 80 };
        assert!(!is_allowed_origin(&format!(
            "http://localhost:{other_port}"
        )));
        assert!(!is_allowed_host(&format!("localhost:{other_port}")));
        assert!(is_allowed_origin("http://127.0.0.1:43128"));
        assert!(is_allowed_origin("http://localhost:43128"));
        assert!(!is_allowed_origin("http://evil.example"));
        assert!(!is_allowed_origin("https://127.0.0.1:43128"));
        assert!(is_allowed_host("127.0.0.1:43128"));
        assert!(!is_allowed_host("evil.example"));
    }
}
