//! Real-process lifecycle coverage using an isolated state directory and port.
//! No audio endpoint is selected and no device action is executed.

use std::{
    fs,
    net::TcpListener,
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

struct Fixture {
    root: PathBuf,
    port: u16,
    service: Option<Child>,
}

impl Fixture {
    fn command(&self, action: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_vibdock"));
        command
            .arg(action)
            .env("LOCALAPPDATA", &self.root)
            .env("HOME", &self.root)
            .env("VIBDOCK_PORT", self.port.to_string())
            .env("VIBDOCK_OVERLAY", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn run_short(&self, action: &str) -> Output {
        let mut child = self.command(action).spawn().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while child.try_wait().unwrap().is_none() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("isolated {action} exceeded its deadline");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        child.wait_with_output().unwrap()
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
fn a_real_service_is_discovered_reused_and_stopped_with_a_fresh_proof() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let root = std::env::temp_dir().join(format!(
        "vibdock-identity-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&root).unwrap();
    let mut fixture = Fixture {
        root,
        port,
        service: None,
    };
    drop(listener);
    fixture.service = Some(fixture.command("service-start").spawn().unwrap());
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(fixture
            .service
            .as_mut()
            .unwrap()
            .try_wait()
            .unwrap()
            .is_none());
        let status = fixture.run_short("service-status");
        if String::from_utf8_lossy(&status.stdout).contains("Service running") {
            assert!(status.status.success());
            break;
        }
        assert!(
            Instant::now() < deadline,
            "isolated service was not discovered"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let reused = fixture.run_short("service-start");
    assert!(reused.status.success());
    assert!(String::from_utf8_lossy(&reused.stdout).contains("Reusing running service"));
    assert!(fixture
        .service
        .as_mut()
        .unwrap()
        .try_wait()
        .unwrap()
        .is_none());
    let stopped = fixture.run_short("service-stop");
    assert!(stopped.status.success());
    assert!(String::from_utf8_lossy(&stopped.stdout).contains("Service stopped"));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = fixture.service.as_mut().unwrap().try_wait().unwrap() {
            assert!(
                status.success(),
                "service must exit naturally after shutdown"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "service did not exit after accepting shutdown"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(!fixture.root.join("VibDock/service.json").exists());
}
