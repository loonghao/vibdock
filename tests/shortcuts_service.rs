//! Real-service coverage for application shortcuts and the loop guard.
//!
//! The service is started in an isolated state directory on a private port.
//! No microphone action is executed and no application is launched. The seeded
//! shortcut only proves file loading, authenticated routing and dispatch-chord
//! conflict rejection; a CI runner without an interactive desktop may report
//! the trigger as unregistered, which is an expected partial state.
#![cfg(windows)]

use std::{
    fs,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const ENTRY_ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SEEDED_TRIGGER: &str = "Ctrl + Alt + Shift + F23";
const SEEDED_DISPATCH: &str = "Ctrl + Shift + F23";

struct Fixture {
    root: PathBuf,
    port: u16,
    service: Option<Child>,
}

impl Fixture {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let root = std::env::temp_dir().join(format!(
            "vibdock-shortcuts-e2e-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&root).unwrap();
        Self {
            root,
            port,
            service: None,
        }
    }

    fn state(&self) -> PathBuf {
        self.root.join("VibDock")
    }

    fn seed_shortcut(&self) {
        fs::create_dir_all(self.state()).unwrap();
        let stored = serde_json::json!({
            "schema_version": 1,
            "revision": 0,
            "entries": [{
                "id": ENTRY_ID,
                "label": "Fixture app",
                "executable_path": r"C:\Fixture\Codex.exe",
                "trigger": SEEDED_TRIGGER,
                "dispatch": SEEDED_DISPATCH,
                "scope": "foreground"
            }]
        });
        fs::write(
            self.state().join("shortcuts.json"),
            stored.to_string().as_bytes(),
        )
        .unwrap();
    }

    fn start(&mut self) {
        let mut command = Command::new(env!("CARGO_BIN_EXE_vibdock"));
        command
            .arg("service-start")
            .env("LOCALAPPDATA", &self.root)
            .env("HOME", &self.root)
            .env("VIBDOCK_PORT", self.port.to_string())
            .env("VIBDOCK_OVERLAY", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        self.service = Some(command.spawn().unwrap());
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if self.state().join("service.json").exists()
                && TcpStream::connect(("127.0.0.1", self.port)).is_ok()
            {
                return;
            }
            assert!(
                self.service.as_mut().unwrap().try_wait().unwrap().is_none(),
                "service exited before publishing its instance record"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("service did not become reachable");
    }

    fn token(&self) -> String {
        let raw = fs::read_to_string(self.state().join("service.json")).unwrap();
        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
        value["token"].as_str().unwrap().to_string()
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        token: Option<&str>,
        origin: Option<&str>,
        body: Option<&str>,
    ) -> (u16, String) {
        let body = body.unwrap_or("");
        let mut request = format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n",
            self.port
        );
        if let Some(token) = token {
            request.push_str(&format!("Authorization: Bearer {token}\r\n"));
        }
        if let Some(origin) = origin {
            request.push_str(&format!("Origin: {origin}\r\n"));
        }
        if !body.is_empty() {
            request.push_str(&format!(
                "Content-Type: application/json\r\nContent-Length: {}\r\n",
                body.len()
            ));
        }
        request.push_str("\r\n");
        request.push_str(body);
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).unwrap();
        let text = String::from_utf8(response).unwrap();
        let status = text
            .split_whitespace()
            .nth(1)
            .and_then(|code| code.parse().ok())
            .unwrap_or(0);
        let body = text
            .split_once("\r\n\r\n")
            .map(|(_, body)| body.to_string())
            .unwrap_or_default();
        (status, body)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(child) = &mut self.service {
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
            }
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn real_service_loads_shortcuts_enforces_auth_and_rejects_dispatch_trigger_loops() {
    let mut fixture = Fixture::new();
    fixture.seed_shortcut();
    let original = fs::read(fixture.state().join("shortcuts.json")).unwrap();
    fixture.start();
    let token = fixture.token();

    let (status, body) = fixture.request("GET", "/api/shortcuts", Some(&token), None, None);
    assert_eq!(status, 200, "{body}");
    let view: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(view["entries"].as_array().unwrap().len(), 1);
    assert_eq!(view["entries"][0]["label"], "Fixture app");
    assert_eq!(view["entries"][0]["trigger"], SEEDED_TRIGGER);
    assert_eq!(view["entries"][0]["dispatch"], SEEDED_DISPATCH);
    assert_eq!(view["entries"][0]["scope"], "foreground");
    assert_eq!(view["storage_writable"], true);
    assert!(
        !body.contains("Fixture\\Codex.exe"),
        "public shortcut view leaked an executable path"
    );

    let (status, body) = fixture.request("GET", "/api/apps", Some(&token), None, None);
    assert_eq!(status, 200, "{body}");
    let apps: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(apps["applications"].is_array());
    assert!(apps["applications_complete"].is_boolean());
    assert!(
        !body.contains("Fixture\\Codex.exe"),
        "software view leaked a seeded path"
    );

    for (method, path, body) in [
        ("GET", "/api/shortcuts", None),
        ("POST", "/api/shortcuts/add", Some("{}")),
        ("POST", "/api/shortcuts/remove", Some("{}")),
    ] {
        assert_eq!(
            fixture.request(method, path, None, None, body).0,
            401,
            "{method} {path} accepted an unauthenticated request"
        );
    }
    assert_eq!(
        fixture
            .request(
                "GET",
                "/api/shortcuts",
                Some(&token),
                Some("https://foreign.example"),
                None
            )
            .0,
        403
    );

    let invalid = serde_json::json!({
        "revision": 0,
        "application_reference": "0".repeat(64),
        "trigger": "Ctrl + Alt + Shift + F24",
        "dispatch": "Ctrl + Shift + F24"
    })
    .to_string();
    let (status, body) = fixture.request(
        "POST",
        "/api/shortcuts/add",
        Some(&token),
        None,
        Some(&invalid),
    );
    assert_eq!(status, 409, "{body}");
    assert!(!body.contains("Fixture\\Codex.exe"));
    assert_eq!(
        fs::read(fixture.state().join("shortcuts.json")).unwrap(),
        original,
        "a rejected add rewrote the shortcut file"
    );

    // The microphone trigger may not equal a saved dispatch chord: Windows
    // would feed the injected key back into the global registration.
    let conflict = serde_json::json!({ "hotkey": SEEDED_DISPATCH }).to_string();
    let (status, body) =
        fixture.request("POST", "/api/config", Some(&token), None, Some(&conflict));
    assert_eq!(status, 409, "{body}");
    assert!(body.contains("dispatch target"), "{body}");
    assert!(
        !fixture.state().join("config.json").exists(),
        "a rejected microphone trigger was persisted"
    );
    assert_eq!(
        fs::read(fixture.state().join("shortcuts.json")).unwrap(),
        original
    );

    let remove = serde_json::json!({"revision": 0, "id": ENTRY_ID}).to_string();
    let (status, body) = fixture.request(
        "POST",
        "/api/shortcuts/remove",
        Some(&token),
        None,
        Some(&remove),
    );
    assert_eq!(status, 200, "{body}");
    let view: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(view["entries"].as_array().unwrap().len(), 0);
    assert_eq!(view["revision"], 1);
    let stored: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.state().join("shortcuts.json")).unwrap()).unwrap();
    assert_eq!(stored["revision"], 1);
    assert_eq!(stored["entries"].as_array().unwrap().len(), 0);

    let (status, body) = fixture.request("GET", "/api/shortcuts", Some(&token), None, None);
    assert_eq!(status, 200, "{body}");
    let view: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(view["entries"].as_array().unwrap().len(), 0);
}
