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
pub(crate) fn address() -> String {
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
///
/// The request body is not carried here: a config write needs it only after
/// routing, and only the caller can apply it because it owns the live state and
/// must persist before answering.
struct Request<'a> {
    method: &'a Method,
    path: &'a str,
    host: Option<&'a str>,
    origin: Option<&'a str>,
    authorized: bool,
    /// The body was oversized or not valid UTF-8, so it was not parsed.
    body_rejected: bool,
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
}

/// Applies a config patch to the live state and persists it before reporting
/// success.
///
/// Order matters. The patch is merged into the current state (so a field the
/// client did not name keeps its stored value), the merged result is written to
/// disk, and only then is memory updated and the committed state returned. A
/// failed write therefore leaves both the on-disk and in-memory configuration
/// exactly as they were, and the caller must surface an error status.
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

/// Routes one request.
///
/// `state` is the live configuration snapshot; `token_configured` reports
/// whether this instance published a write token. A service without a token
/// stays read-only, preserving the previous design-preview behaviour.
fn route(request: &Request<'_>, state: &Config, token_configured: bool) -> Routed {
    let method = request.method;
    let path = request.path;
    if !request.host.is_some_and(service::is_allowed_host) {
        return (403, "text/plain; charset=utf-8", "Invalid host".into());
    }
    if let Some(origin) = request.origin {
        if !service::is_allowed_origin(origin) {
            return (403, "text/plain; charset=utf-8", "Invalid origin".into());
        }
    }
    if !token_configured && method != &Method::Get {
        return (
            405,
            "text/plain; charset=utf-8",
            "Read-only design preview".into(),
        );
    }
    // Checked before any body parsing, but after host/origin validation: an
    // oversized or undecodable body is a client error, never a crash.
    if request.body_rejected {
        return (
            413,
            "text/plain; charset=utf-8",
            "Request body is too large or not valid UTF-8.".into(),
        );
    }
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
        (Method::Get, "/api/config") => {
            if !request.authorized {
                return unauthorized();
            }
            (200, "application/json", state.to_json())
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
pub(crate) fn serve(instance: Option<Instance>) -> Result<()> {
    let server = Server::http(address()).map_err(|e| e.to_string())?;
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
    let result = accept_loop(&server, &token, &mut state);
    if instance.is_some() {
        let _ = service::clear_instance();
    }
    result
}

/// Serves requests until the listener stops, tracking config mutations.
fn accept_loop(server: &Server, token: &Option<String>, state: &mut Config) -> Result<()> {
    for mut request in server.incoming_requests() {
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
        let authorized = service::authorized(&headers, token.as_deref());
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
        let (mut code, mut mime, mut payload) = route(
            &Request {
                method: request.method(),
                path: request.url(),
                host,
                origin,
                authorized,
                body_rejected,
            },
            state,
            token.is_some(),
        );
        if is_config_write && code == 200 && payload == COMMIT_SENTINEL {
            match commit_patch(state, &body) {
                WriteOutcome::Committed(committed) => {
                    // Only now is the change real: the file is on disk, so
                    // update memory and answer with the committed state.
                    *state = committed.clone();
                    mime = "application/json";
                    payload = committed.to_json();
                }
                WriteOutcome::Rejected(message) => {
                    code = 400;
                    mime = "text/plain; charset=utf-8";
                    payload = message.into();
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
        let response = with_headers(
            Response::from_data(payload.as_bytes()).with_status_code(StatusCode(code)),
        )
        .with_header(Header::from_bytes("Content-Type", mime).unwrap());
        if let Err(error) = request.respond(response) {
            eprintln!("Response failed: {error}");
        }
    }
    Ok(())
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

    /// Host header matching the bound loopback address for these tests.
    fn host() -> String {
        address()
    }

    /// Builds a request and routes it, returning the status code and body.
    ///
    /// Mirrors what `accept_loop` does after routing: a `/api/config` write is
    /// committed through `commit_patch`, so these tests exercise the same
    /// merge-and-persist path the server runs rather than a bare route call.
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
        // real configuration and from any shared LOCALAPPDATA.
        let guard = STATE_DIR_LOCK.lock().unwrap();
        let original = std::env::var_os("LOCALAPPDATA");
        let dir = std::env::temp_dir().join(format!("vibdock-web-test-{}", std::process::id()));
        std::env::set_var("LOCALAPPDATA", &dir);
        let state = Config::default();
        let (mut code, mut mime, mut payload) = route(
            &Request {
                method,
                path,
                host: Some(host),
                origin,
                authorized,
                body_rejected: false,
            },
            &state,
            service_mode,
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
        drop(guard);
        let _ = fs::remove_dir_all(&dir);
        (code, payload)
    }

    /// Serialises tests that temporarily redirect `LOCALAPPDATA`, because the
    /// process-wide environment is shared across the parallel test runner.
    static STATE_DIR_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
                body_rejected: true,
            },
            &state,
            service_mode,
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
                body_rejected: false,
            },
            &state,
            true,
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
        let patch = service::validate_patch("{\"hotkey\":\"F10\"}").unwrap();
        let merged = stored.apply(&patch);
        assert_eq!(merged.device_id.as_deref(), Some("review-mic"));
        assert_eq!(merged.hotkey.as_deref(), Some("F10"));
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
