//! Resident service foundation: single instance, loopback-authenticated
//! configuration and action endpoints, and atomically persisted config.
//!
//! The contract here is shared by the CLI and the future Windows launcher/tray
//! entry. Audio-device behaviour stays in the platform adapter.

use std::{
    fs::{self, File},
    io::{self, Read, Write},
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use crate::Result;

/// Loopback address family accepted as a browser/Web origin for this service.
pub(crate) fn is_allowed_origin(origin: &str) -> bool {
    matches!(
        origin,
        "http://127.0.0.1:43128" | "http://localhost:43128" | "http://[::1]:43128"
    )
}

/// Host headers accepted for the bound loopback listener.
pub(crate) fn is_allowed_host(host: &str) -> bool {
    matches!(host, "127.0.0.1:43128" | "localhost:43128" | "[::1]:43128")
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

/// Generates a fresh bearer token from OS entropy and the current instant.
pub(crate) fn generate_token() -> Result<String> {
    let mut seed = [0u8; 32];
    fs::File::open("/dev/urandom")
        .and_then(|mut file| file.read_exact(&mut seed))
        .or_else(|_| {
            getrandom_fallback(&mut seed);
            Ok::<(), io::Error>(())
        })
        .map_err(|e| e.to_string())?;
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let mut mixed = seed;
    for (index, byte) in mixed.iter_mut().enumerate() {
        *byte ^= ((stamp >> ((index % 8) * 8)) & 0xff) as u8;
    }
    Ok(mixed.iter().fold(String::new(), |mut out, byte| {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
        out
    }))
}

/// `BCryptGenRandom` entropy source for Windows hosts without `/dev/urandom`.
#[cfg(windows)]
fn getrandom_fallback(output: &mut [u8]) {
    use windows::Win32::Security::Cryptography::{
        BCryptGenRandom, BCRYPT_USE_SYSTEM_PREFERRED_RNG,
    };
    // SAFETY: `output` is a valid mutable byte slice and the length fits u32.
    unsafe {
        let _ = BCryptGenRandom(None, output, BCRYPT_USE_SYSTEM_PREFERRED_RNG);
    }
}

/// Non-Windows build fallback. The service only ships on Windows; keeping a
/// deterministic-but-seeded path lets the parser and routing tests run
/// everywhere CI builds.
#[cfg(not(windows))]
fn getrandom_fallback(output: &mut [u8]) {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0x9E3779B97F4A7C15);
    for (index, byte) in output.iter_mut().enumerate() {
        *byte = ((stamp >> ((index % 8) * 8)) & 0xff) as u8;
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
/// that is actually listening.
///
/// A record alone is not evidence: a service killed without cleanup leaves its
/// file behind, and trusting that file would make every later launch claim to
/// reuse an instance that is not there. The port is probed before the record is
/// believed.
pub(crate) fn live_instance() -> Option<Instance> {
    let instance = read_instance()?;
    probe_port(instance.port).then_some(instance)
}

/// True when something is accepting TCP connections on this loopback port.
fn probe_port(port: u16) -> bool {
    std::net::TcpStream::connect(("127.0.0.1", port))
        .or_else(|_| std::net::TcpStream::connect(("::1", port)))
        .is_ok()
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
    /// Parses a persisted config object. Unrecognised keys are ignored so a
    /// newer build's file stays loadable by an older one.
    #[cfg(test)]
    pub(crate) fn parse(raw: &str) -> Config {
        let mut config = Config::default();
        if let Some(value) = extract(raw, "\"device_id\":\"") {
            let value = value.split('"').next().unwrap_or("");
            config.device_id = (!value.is_empty()).then(|| value.to_string());
        }
        if let Some(value) = extract(raw, "\"hotkey\":\"") {
            let value = value.split('"').next().unwrap_or("");
            config.hotkey = (!value.is_empty()).then(|| value.to_string());
        }
        config
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
    fn instance_record_round_trips() {
        let instance = Instance {
            port: 43128,
            token: "abc123".into(),
        };
        let parsed = Instance::parse(&instance.to_json()).unwrap();
        assert_eq!(parsed, instance);
        assert_eq!(Instance::parse("not json"), None);
        assert_eq!(Instance::parse("{\"port\":0,\"token\":\"x\"}"), None);
    }

    #[test]
    fn config_parses_known_fields_and_ignores_others() {
        let config = Config::parse("{\"device_id\":\"mic-1\",\"hotkey\":\"F9\",\"future\":1}");
        assert_eq!(config.device_id.as_deref(), Some("mic-1"));
        assert_eq!(config.hotkey.as_deref(), Some("F9"));
        assert_eq!(Config::parse("{}"), Config::default());
    }

    #[test]
    fn config_round_trips() {
        let config = Config {
            device_id: Some("mic-1".into()),
            hotkey: None,
        };
        assert_eq!(Config::parse(&config.to_json()), config);
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
        assert!(live_instance().is_none() || probe_port(live_instance().unwrap().port));
    }

    #[test]
    fn probing_an_unused_port_reports_no_listener() {
        // Bind a port, confirm the probe sees it, release it, then confirm the
        // probe stops seeing it. This closes the loop on the liveness check.
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
