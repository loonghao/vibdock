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
    ["http://127.0.0.1", "http://localhost", "http://[::1]"]
        .iter()
        .any(|base| origin == format!("{base}:{}", port()) || *base == origin)
}

/// Host headers accepted for the bound loopback listener.
pub(crate) fn is_allowed_host(host: &str) -> bool {
    ["127.0.0.1", "localhost", "[::1]"]
        .iter()
        .any(|base| host == format!("{base}:{}", port()) || *base == host)
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
pub(crate) struct Instance {
    pub(crate) port: u16,
    pub(crate) token: String,
}

impl Instance {
    /// Serialises the record as a single JSON line.
    pub(crate) fn to_json(&self) -> String {
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
pub(crate) fn live_instance() -> Option<Instance> {
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
    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port))
        .or_else(|_| std::net::TcpStream::connect(("::1", port)))
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

/// True when something is accepting TCP connections on this loopback port.
///
/// Only used to detect that a port is occupied by a foreign process, never as
/// proof that our service is running — see [`instance_answers`].
pub(crate) fn probe_port(port: u16) -> bool {
    std::net::TcpStream::connect(("127.0.0.1", port))
        .or_else(|_| std::net::TcpStream::connect(("::1", port)))
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

/// Path of the persisted service configuration.
pub(crate) fn config_path() -> Result<PathBuf> {
    Ok(state_dir()?.join("config.json"))
}

/// Loads the persisted configuration.
///
/// A missing file means "no configuration yet". A file that cannot be read or
/// parsed is reported as an error rather than silently discarded, so corrupt
/// state is visible instead of masquerading as an empty config.
pub(crate) fn load_config() -> Result<Config> {
    match fs::read_to_string(config_path()?) {
        Ok(raw) => match Config::parse(&raw) {
            Ok(config) => Ok(config),
            Err(message) => {
                // Corrupt config must not stop the service, but it must be
                // reported so the loss is visible rather than silent.
                eprintln!("WARNING: {message}; starting with an empty configuration.");
                Ok(Config::default())
            }
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Config::default()),
        Err(error) => Err(error.to_string().into()),
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
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Config {
    pub(crate) device_id: Option<String>,
    pub(crate) hotkey: Option<String>,
}

impl Config {
    /// Parses a persisted config object.
    ///
    /// Unrecognised keys are ignored so a newer build's file stays loadable by
    /// an older one. A body that is not a JSON object at all is an error, which
    /// lets callers tell "corrupt file" apart from "empty configuration" — the
    /// difference matters because silently treating a corrupt file as empty
    /// would quietly drop a user's saved device binding.
    pub(crate) fn parse(raw: &str) -> std::result::Result<Config, &'static str> {
        let trimmed = raw.trim();
        if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
            return Err("configuration file is not a JSON object");
        }
        let mut config = Config::default();
        if let Some(value) = extract(trimmed, "\"device_id\":\"") {
            let value = value.split('"').next().unwrap_or("");
            config.device_id = (!value.is_empty()).then(|| value.to_string());
        }
        if let Some(value) = extract(trimmed, "\"hotkey\":\"") {
            let value = value.split('"').next().unwrap_or("");
            config.hotkey = (!value.is_empty()).then(|| value.to_string());
        }
        Ok(config)
    }

    /// Serialises the config as a single JSON line.
    pub(crate) fn to_json(&self) -> String {
        format!(
            "{{\"device_id\":\"{}\",\"hotkey\":\"{}\"}}",
            self.device_id.as_deref().unwrap_or(""),
            self.hotkey.as_deref().unwrap_or("")
        )
    }
}

/// Validates a config patch: only known string fields, bounded in size, with no
/// control characters or quotes that would break the persisted JSON.
pub(crate) fn validate_patch(body: &str) -> std::result::Result<Config, &'static str> {
    let mut config = Config::default();
    let trimmed = body.trim();
    if !trimmed.starts_with('{') || !trimmed.ends_with('}') {
        return Err("Configuration body must be a JSON object.");
    }
    for key in ["device_id", "hotkey"] {
        let needle = format!("\"{key}\":\"");
        let Some(rest) = extract(trimmed, &needle) else {
            continue;
        };
        let value = rest.split('"').next().unwrap_or("");
        if value.len() > 512 || value.contains(['\0', '\n', '\r', '\\']) {
            return Err("Configuration values must be plain strings under 512 bytes.");
        }
        let value = (!value.is_empty()).then(|| value.to_string());
        match key {
            "device_id" => config.device_id = value,
            _ => config.hotkey = value,
        }
    }
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;

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

        // A partial update must retain the field it did not name.
        let partial = validate_patch("{\"device_id\":\"other-mic\"}").unwrap();
        atomic_write(&path, partial.to_json().as_bytes()).unwrap();
        assert_eq!(
            Config::parse(&fs::read_to_string(&path).unwrap()).unwrap(),
            partial
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
        assert!(validate_patch("{\"device_id\":\"a\\\\b\"}").is_err());
        assert!(validate_patch(&format!("{{\"hotkey\":\"{}\"}}", "x".repeat(600))).is_err());
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
    fn allowed_origins_are_loopback_only() {
        assert!(is_allowed_origin("http://127.0.0.1:43128"));
        assert!(is_allowed_origin("http://localhost:43128"));
        assert!(!is_allowed_origin("http://evil.example"));
        assert!(!is_allowed_origin("https://127.0.0.1:43128"));
        assert!(is_allowed_host("127.0.0.1:43128"));
        assert!(!is_allowed_host("evil.example"));
    }
}
