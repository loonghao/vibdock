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
pub(crate) const ADDRESS: &str = "127.0.0.1:43128";
/// Upper bound on a request body; larger payloads are truncated, never parsed.
const MAX_BODY: usize = 8192;

/// Outcome of a routed request: status, content type, body, and whether the
/// client may cache it.
type Routed = (u16, &'static str, String);

/// Everything that varies per request, grouped so the routing table stays a
/// pure function of its inputs.
struct Request<'a> {
    method: &'a Method,
    path: &'a str,
    host: Option<&'a str>,
    origin: Option<&'a str>,
    authorized: bool,
    body: &'a str,
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
            match service::validate_patch(request.body) {
                Ok(config) => (200, "application/json", config.to_json()),
                Err(message) => (400, "text/plain; charset=utf-8", message.into()),
            }
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
    let server = Server::http(ADDRESS).map_err(|e| e.to_string())?;
    let token = instance.as_ref().map(|i| i.token.clone());
    if let Some(instance) = &instance {
        service::write_instance(instance)?;
        println!("VibDock service listening on http://{ADDRESS}");
    } else {
        println!("VibDock design preview: http://{ADDRESS} (read-only; Ctrl+C to stop)");
    }
    let mut state = Config::default();
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
        if request.method() == &Method::Post {
            let _ = request.as_reader().read_to_string(&mut body);
            if body.len() > MAX_BODY {
                body.truncate(MAX_BODY);
            }
        }
        let is_config_write = request.method() == &Method::Post && request.url() == "/api/config";
        let (code, mime, payload) = route(
            &Request {
                method: request.method(),
                path: request.url(),
                host,
                origin,
                authorized,
                body: &body,
            },
            state,
            token.is_some(),
        );
        if is_config_write && code == 200 {
            if let Ok(config) = service::validate_patch(&body) {
                *state = config;
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

    const HOST: Option<&str> = Some("127.0.0.1:43128");

    /// Builds a request and routes it, returning the status code and body.
    fn call(
        method: &Method,
        path: &str,
        host: Option<&str>,
        origin: Option<&str>,
        authorized: bool,
        body: &str,
        service_mode: bool,
    ) -> (u16, String) {
        let state = Config::default();
        let (code, _, payload) = route(
            &Request {
                method,
                path,
                host,
                origin,
                authorized,
                body,
            },
            &state,
            service_mode,
        );
        (code, payload)
    }

    #[test]
    fn preview_is_read_only_and_rejects_foreign_hosts() {
        assert_eq!(call(&Method::Get, "/", HOST, None, false, "", false).0, 200);
        assert_eq!(
            call(&Method::Post, "/", HOST, None, false, "", false).0,
            405
        );
        assert_eq!(
            call(
                &Method::Get,
                "/",
                Some("evil.example"),
                None,
                false,
                "",
                false
            )
            .0,
            403
        );
        assert_eq!(
            call(&Method::Get, "/missing", HOST, None, false, "", false).0,
            404
        );
        assert!(call(&Method::Get, "/health", HOST, None, false, "", false)
            .1
            .contains("design-preview"));
    }

    #[test]
    fn service_mode_reports_service_and_accepts_writes() {
        assert!(call(&Method::Get, "/health", HOST, None, true, "", true)
            .1
            .contains("\"mode\":\"service\""));
        assert_eq!(
            call(
                &Method::Post,
                "/api/config",
                HOST,
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
                call(&Method::Post, path, HOST, None, false, "{}", true).0,
                401
            );
        }
        assert_eq!(
            call(&Method::Get, "/api/config", HOST, None, false, "", true).0,
            401
        );
    }

    #[test]
    fn foreign_origin_is_rejected() {
        assert_eq!(
            call(
                &Method::Post,
                "/api/config",
                HOST,
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
                HOST,
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
                HOST,
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
    fn no_shell_execution_endpoint_exists() {
        for path in [
            "/api/actions/exec",
            "/api/actions/shell",
            "/api/actions/run",
            "/api/actions/command",
        ] {
            assert_eq!(
                call(&Method::Post, path, HOST, None, true, "{}", true).0,
                404
            );
        }
    }

    #[test]
    fn unknown_methods_are_rejected() {
        assert_eq!(call(&Method::Put, "/", HOST, None, true, "", true).0, 405);
        assert_eq!(
            call(&Method::Delete, "/", HOST, None, true, "", true).0,
            405
        );
    }
}
