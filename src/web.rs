use crate::Result;
use tiny_http::{Header, Method, Response, Server, StatusCode};

const PAGE: &str = include_str!("../docs/design/index.html");
const ADDRESS: &str = "127.0.0.1:43128";
/// The page probes `/health` to show whether the local service is running, so
/// `connect-src` must permit same-origin requests while everything else stays locked down.
const CSP: &str = "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; form-action 'none'; base-uri 'none'";

fn route(method: &Method, path: &str, host: Option<&str>) -> (u16, &'static str, &'static str) {
    if !matches!(host, Some("127.0.0.1:43128") | Some("localhost:43128")) {
        return (403, "text/plain; charset=utf-8", "Invalid host");
    }
    if method != &Method::Get {
        return (405, "text/plain; charset=utf-8", "Read-only design preview");
    }
    match path {
        "/" => (200, "text/html; charset=utf-8", PAGE),
        "/health" => (
            200,
            "application/json",
            "{\"mode\":\"design-preview\",\"audio_control\":false}",
        ),
        _ => (404, "text/plain; charset=utf-8", "Not found"),
    }
}

pub(crate) fn serve() -> Result<()> {
    let server = Server::http(ADDRESS).map_err(|e| e.to_string())?;
    println!("VibDock design preview: http://{ADDRESS} (read-only; Ctrl+C to stop)");
    for request in server.incoming_requests() {
        let host = request
            .headers()
            .iter()
            .find(|h| h.field.equiv("Host"))
            .map(|h| h.value.as_str());
        let (code, mime, body) = route(request.method(), request.url(), host);
        let response = Response::from_string(body)
            .with_status_code(StatusCode(code))
            .with_header(Header::from_bytes("Content-Type", mime).unwrap())
            .with_header(Header::from_bytes("X-Content-Type-Options", "nosniff").unwrap())
            .with_header(Header::from_bytes("Content-Security-Policy", CSP).unwrap());
        if let Err(error) = request.respond(response) {
            eprintln!("Response failed: {error}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preview_is_read_only_and_rejects_foreign_hosts() {
        assert_eq!(route(&Method::Get, "/", Some(ADDRESS)).0, 200);
        assert_eq!(route(&Method::Post, "/", Some(ADDRESS)).0, 405);
        assert_eq!(route(&Method::Get, "/", Some("evil.example")).0, 403);
        assert_eq!(route(&Method::Get, "/missing", Some(ADDRESS)).0, 404);
        assert!(route(&Method::Get, "/health", Some(ADDRESS))
            .2
            .contains("design-preview"));
    }

    #[test]
    fn page_allows_loopback_health_probe() {
        assert!(
            CSP.contains("connect-src 'self'"),
            "the page probes /health, so connect-src must allow same-origin"
        );
        assert!(
            !CSP.contains("connect-src 'none'"),
            "connect-src 'none' would break the service-state badge"
        );
        assert!(
            CSP.contains("default-src 'none'") && CSP.contains("frame-ancestors 'none'"),
            "tightening connect-src must not loosen the rest of the policy"
        );
    }
}
