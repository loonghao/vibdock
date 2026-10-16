//! macOS launcher lifecycle, separate from AppKit and the Windows tray.
//!
//! An advisory file lock is held for the whole native event loop. It is never
//! unlinked: deleting a locked inode would let a second process lock a new one.
use crate::{service, Result};
use std::{
    fs::{self, File, OpenOptions, TryLockError},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

/// Starts/reuses the proved service and owns the one native desktop entry.
pub fn run(acceptance: bool) -> Result<()> {
    if acceptance {
        validate_acceptance_environment()?;
    }
    let lock_path = service::state_dir()?.join("macos-launcher.lock");
    let mut lock = match acquire_lock(&lock_path)? {
        Some(lock) => lock,
        None if acceptance => return Err("Native acceptance requires an isolated launcher.".into()),
        None => return send_reopen(&lock_path),
    };
    let (listener, reopen) = ReopenListener::start(&mut lock)?;
    *REOPEN_LISTENER
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some(listener);
    let _reopen_cleanup = ReopenCleanup;
    let mut child = ensure_service(acceptance)?;
    let result = if acceptance {
        crate::macos_resident::self_acceptance(reopen).and_then(|report| {
            println!("{report}");
            if report["passed"] == true {
                Ok(())
            } else {
                Err("Native acceptance failed; see redacted report.".into())
            }
        })
    } else {
        crate::macos_resident::run(reopen)
    };
    // Reap only our child if it has already exited. Never kill a service by PID
    // or name, and never wait indefinitely when native startup itself failed.
    if let Some(child) = child.as_mut() {
        if result.is_err() {
            stop_owned_child(child)?;
        }
        let deadline = Instant::now() + Duration::from_millis(500);
        while child.try_wait()?.is_none() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    result
}

fn acquire_lock(path: &Path) -> Result<Option<File>> {
    fs::create_dir_all(path.parent().ok_or("Launcher lock has no parent")?)?;
    // Darwin O_NOFOLLOW, from Apple's sys/fcntl.h. Fail closed on a symlink;
    // this is an app-owned empty lock file, not a user-selected executable.
    const O_NOFOLLOW: i32 = 0x0100;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(O_NOFOLLOW)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err("The launcher lock is not a regular file.".into());
    }
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(TryLockError::WouldBlock) => Ok(None),
        Err(TryLockError::Error(error)) => Err(error.into()),
    }
}

// AppKit's system terminate exits without unwinding the outer Rust stack.
// The delegate explicitly drains this owner before replying to that request.
static REOPEN_LISTENER: std::sync::Mutex<Option<ReopenListener>> = std::sync::Mutex::new(None);
struct ReopenCleanup;
impl Drop for ReopenCleanup {
    fn drop(&mut self) {
        cleanup_reopen_before_system_exit();
    }
}
pub(crate) fn cleanup_reopen_before_system_exit() {
    let listener = REOPEN_LISTENER
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take();
    drop(listener);
}

struct ReopenListener {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    path: PathBuf,
    identity: (u64, u64),
}
impl ReopenListener {
    fn start(lock: &mut File) -> Result<(Self, std::sync::mpsc::Receiver<()>)> {
        use std::{
            io::{Seek, Write},
            os::unix::{fs::MetadataExt, net::UnixDatagram},
            sync::{
                atomic::{AtomicBool, Ordering},
                mpsc, Arc,
            },
        };
        let nonce = service::generate_token()?;
        let name = service::generate_token()?;
        // A fixed system temporary root keeps Darwin's 104-byte Unix socket
        // path bound independent of a long HOME/TMPDIR. Bind is exclusive.
        let path = Path::new("/tmp").join(format!("vibdock-ui-{name}.sock"));
        let socket = UnixDatagram::bind(&path)?;
        socket.set_read_timeout(Some(Duration::from_millis(100)))?;
        let metadata = fs::symlink_metadata(&path)?;
        let identity = (metadata.dev(), metadata.ino());
        let (sender, receiver) = mpsc::sync_channel(1);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = Arc::clone(&stop);
        let expected = nonce.clone();
        let thread = std::thread::Builder::new()
            .name("macos-reopen".into())
            .spawn(move || {
                let mut bytes = [0u8; 65];
                while !stopping.load(Ordering::Relaxed) {
                    match socket.recv(&mut bytes) {
                        Ok(64)
                            if bytes[..64]
                                .iter()
                                .zip(expected.bytes())
                                .fold(0u8, |difference, (a, b)| difference | (a ^ b))
                                == 0 =>
                        {
                            let _ = sender.try_send(());
                        }
                        Ok(_) => {}
                        Err(error)
                            if matches!(
                                error.kind(),
                                std::io::ErrorKind::WouldBlock
                                    | std::io::ErrorKind::TimedOut
                                    | std::io::ErrorKind::Interrupted
                            ) => {}
                        Err(_) => break,
                    }
                }
            })?;
        let listener = Self {
            stop,
            thread: Some(thread),
            path,
            identity,
        };
        // This nonce grants only Show on this launcher, never service access.
        // Publish after the owned receiver is ready; no paths or PIDs to execute.
        lock.rewind()?;
        lock.set_len(0)?;
        lock.write_all(
            serde_json::json!({"nonce": nonce, "socket": name})
                .to_string()
                .as_bytes(),
        )?;
        lock.sync_all()?;
        Ok((listener, receiver))
    }
}
impl Drop for ReopenListener {
    fn drop(&mut self) {
        use std::{os::unix::fs::MetadataExt, sync::atomic::Ordering};
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let deadline = Instant::now() + Duration::from_millis(500);
            while !thread.is_finished() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            if thread.is_finished() {
                let _ = thread.join();
            }
        }
        if let Ok(metadata) = fs::symlink_metadata(&self.path) {
            if (metadata.dev(), metadata.ino()) == self.identity {
                let _ = fs::remove_file(&self.path);
            }
        }
    }
}

fn send_reopen(path: &Path) -> Result<()> {
    use std::{io::Read, os::unix::net::UnixDatagram};
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let result = (|| -> Result<()> {
            let file = OpenOptions::new()
                .read(true)
                .custom_flags(0x0100 | 0x0004) // O_NOFOLLOW | O_NONBLOCK
                .open(path)?;
            if !file.metadata()?.is_file() {
                return Err("Invalid launcher reopen record type".into());
            }
            let mut bytes = String::new();
            file.take(2049).read_to_string(&mut bytes)?;
            if bytes.len() > 2048 {
                return Err("Invalid launcher reopen record".into());
            }
            let record: serde_json::Value = serde_json::from_str(&bytes)?;
            let nonce = record["nonce"].as_str().ok_or("Missing reopen nonce")?;
            let name = record["socket"].as_str().ok_or("Missing reopen address")?;
            for value in [nonce, name] {
                if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err("Invalid launcher reopen record".into());
                }
            }
            let socket = UnixDatagram::unbound()?;
            socket.set_write_timeout(Some(Duration::from_millis(100)))?;
            socket.send_to(
                nonce.as_bytes(),
                Path::new("/tmp").join(format!("vibdock-ui-{name}.sock")),
            )?;
            Ok(())
        })();
        if result.is_ok() {
            return result;
        }
        if Instant::now() >= deadline {
            return Err("The existing VibDock panel did not accept a reopen request. Try again after startup completes.".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn stop_owned_child(child: &mut Child) -> Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    child.kill()?;
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err("Owned startup child termination remains unconfirmed; state was retained.".into())
}

fn ensure_service(acceptance: bool) -> Result<Option<Child>> {
    if service::live_instance().is_some() {
        if acceptance {
            return Err("Native acceptance must start its own service.".into());
        }
        return Ok(None);
    }
    let cli = std::env::current_exe()?.with_file_name("vibdock");
    if !cli.is_file() {
        return Err("The bundled VibDock service executable is missing.".into());
    }
    let mut child = Command::new(cli)
        .arg("service-start")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if let Some(instance) = service::live_instance() {
            if acceptance && instance.port != service::port() {
                stop_owned_child(&mut child)?;
                return Err("Native acceptance found the wrong service port.".into());
            }
            return Ok(Some(child));
        }
        if child.try_wait()?.is_some() {
            return Err("The VibDock service exited before proving its identity.".into());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    stop_owned_child(&mut child)?;
    Err("The resident service did not prove its identity before the startup deadline.".into())
}

fn validate_acceptance_environment() -> Result<()> {
    let enabled = std::env::var("VIBDOCK_NATIVE_ACCEPTANCE").as_deref() == Ok("1");
    let state = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or("Native acceptance requires an isolated state directory")?
        .canonicalize()?;
    let temporary = std::env::temp_dir().canonicalize()?;
    let isolated = state.starts_with(&temporary)
        && state
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|s| s.starts_with("vibdock-macos-desktop-"));
    if !enabled || !isolated || service::port() == service::DEFAULT_PORT || service::port() == 0 {
        return Err("Native acceptance requires explicit opt-in, a private temporary state directory, and a nondefault port.".into());
    }
    validate_acceptance_state(&state)
}

fn validate_acceptance_state(state: &Path) -> Result<()> {
    let child = state.join("VibDock");
    if fs::symlink_metadata(&child).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        return Err("Native acceptance must not follow a linked state directory.".into());
    }
    if fs::symlink_metadata(child.join("service.json")).is_ok() {
        return Err("Native acceptance refuses any preexisting service record.".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_reopen_is_independent_of_executable_copy_and_rejects_bad_nonce() {
        let dir = std::env::temp_dir().join(format!(
            "vibdock-reopen-test-{}",
            service::generate_token().unwrap()
        ));
        let path = dir.join("launcher.lock");
        let mut lock = acquire_lock(&path).unwrap().unwrap();
        let (listener, messages) = ReopenListener::start(&mut lock).unwrap();
        // Caller does not supply a bundle path or executable; both copies target
        // the exact socket held by the lock owner.
        send_reopen(&path).unwrap();
        messages.recv_timeout(Duration::from_secs(1)).unwrap();
        use std::os::unix::net::UnixDatagram;
        UnixDatagram::unbound()
            .unwrap()
            .send_to(&[b'0'; 64], &listener.path)
            .unwrap();
        assert!(messages.recv_timeout(Duration::from_millis(150)).is_err());
        let socket = listener.path.clone();
        drop(listener);
        assert!(!socket.exists());
        drop(lock);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn acceptance_rejects_copied_marker_and_linked_state_without_network() {
        let dir = std::env::temp_dir().join(format!(
            "vibdock-state-test-{}",
            service::generate_token().unwrap()
        ));
        let child = dir.join("VibDock");
        fs::create_dir_all(&child).unwrap();
        assert!(validate_acceptance_state(&dir).is_ok());
        fs::write(
            child.join("service.json"),
            r#"{"port":43128,"token":"synthetic-not-a-real-token"}"#,
        )
        .unwrap();
        assert!(validate_acceptance_state(&dir).is_err());
        fs::remove_dir_all(&child).unwrap();
        let other = dir.join("other");
        fs::create_dir_all(&other).unwrap();
        std::os::unix::fs::symlink(&other, &child).unwrap();
        assert!(validate_acceptance_state(&dir).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn singleton_is_exclusive_and_recovers_without_unlink() {
        let dir = std::env::temp_dir().join(format!(
            "vibdock-lock-test-{}",
            service::generate_token().unwrap()
        ));
        let path = dir.join("launcher.lock");
        let first = acquire_lock(&path).unwrap().unwrap();
        assert!(acquire_lock(&path).unwrap().is_none());
        drop(first);
        assert!(path.exists());
        let next = acquire_lock(&path).unwrap().unwrap();
        drop(next);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn singleton_rejects_symlink_without_touching_target() {
        let dir = std::env::temp_dir().join(format!(
            "vibdock-lock-symlink-{}",
            service::generate_token().unwrap()
        ));
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("untouched");
        fs::write(&target, "original").unwrap();
        let path = dir.join("launcher.lock");
        std::os::unix::fs::symlink(&target, &path).unwrap();
        assert!(acquire_lock(&path).is_err());
        assert_eq!(fs::read_to_string(target).unwrap(), "original");
        fs::remove_dir_all(dir).unwrap();
    }
}
