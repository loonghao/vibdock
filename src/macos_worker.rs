//! One disposable, bounded native application observer. AppKit runs only in
//! the helper's main thread; service shutdown never waits on AppKit itself.
use serde_json::{json, Value};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
const OUTPUT_LIMIT: usize = 512 * 1024;
extern "C" {
    fn _exit(status: i32) -> !;
}
fn hard_exit() -> ! {
    // The disposable child owns no persistent state. Avoid C/AppKit exit
    // handlers, which could otherwise defeat the watchdog's termination bound.
    unsafe { _exit(125) }
}
fn failure(message: &str) -> Value {
    json!({"available":false,"complete":false,"applications":[],"skipped":0,"error":message})
}
fn private_output() -> std::io::Result<File> {
    let directory =
        crate::service::state_dir().map_err(|error| std::io::Error::other(error.to_string()))?;
    std::fs::create_dir_all(&directory)?;
    let path = directory.join(format!(
        ".mac-observer-{}.tmp",
        crate::service::generate_token().map_err(std::io::Error::other)?
    ));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    // POSIX keeps the anonymous inode open through stdout, without an on-disk
    // file containing application paths after a crash or abrupt parent exit.
    std::fs::remove_file(path)?;
    Ok(file)
}
fn stop_child(child: &mut Child) -> bool {
    if matches!(child.try_wait(), Ok(Some(_))) {
        return true;
    }
    let _ = child.kill(); // Only this exact unreaped child, never a name/PID search.
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => return false,
        }
    }
    false
}
pub(crate) fn collect(executable: &Path, stop: &AtomicBool) -> (Value, bool) {
    if stop.load(Ordering::Relaxed) {
        return (failure("Native application observation cancelled"), true);
    }
    let mut output = match private_output() {
        Ok(file) => file,
        Err(_) => {
            return (
                failure("Native application observation storage is unavailable"),
                true,
            )
        }
    };
    let stdout = match output.try_clone() {
        Ok(file) => file,
        Err(_) => {
            return (
                failure("Native application observation storage is unavailable"),
                true,
            )
        }
    };
    let mut child = match Command::new(executable)
        .arg("_resident_snapshot")
        .stdin(Stdio::piped())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return (failure("Native application helper could not start"), true),
    };
    // Keep this pipe open for the lifetime of the helper. Its EOF watchdog
    // exits when the parent disappears; an independent four-second watchdog
    // also bounds stuck native calls, even if the parent is abruptly killed.
    let mut input = child.stdin.take();
    let activated = input
        .as_mut()
        .is_some_and(|pipe| pipe.write_all(&[0x56]).is_ok());
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut reason = "Native application observation failed";
    let success = activated
        && loop {
            if stop.load(Ordering::Relaxed) {
                reason = "Native application observation cancelled";
                break false;
            }
            if Instant::now() >= deadline {
                reason = "Native application observation timed out";
                break false;
            }
            if output
                .metadata()
                .map_or(true, |m| m.len() > OUTPUT_LIMIT as u64)
            {
                reason = "Native application observation exceeded its output budget";
                break false;
            }
            match child.try_wait() {
                Ok(Some(status)) => break status.success(),
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
                Err(_) => break false,
            }
        };
    if !success {
        drop(input);
        let clean = stop_child(&mut child);
        return (
            failure(if clean {
                reason
            } else {
                "Native helper cleanup remains unconfirmed; observation stopped"
            }),
            clean,
        );
    }
    drop(input);
    let value = (|| -> Option<Value> {
        output.seek(SeekFrom::Start(0)).ok()?;
        bounded_json(output)
    })()
    .unwrap_or_else(|| failure("Native application observation was invalid"));
    (value, true)
}

fn bounded_json(reader: impl Read) -> Option<Value> {
    let mut bytes = Vec::new();
    reader
        .take((OUTPUT_LIMIT + 1) as u64)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > OUTPUT_LIMIT {
        return None;
    }
    serde_json::from_slice(&bytes).ok()
}

/// Internal main-thread helper entry. Only VibDock's own executable is started;
/// no client-provided path, command, bundle or program is accepted here.
pub(crate) fn print_snapshot() {
    let deadline = std::thread::Builder::new()
        .name("mac-observer-deadline".into())
        .spawn(|| {
            std::thread::sleep(Duration::from_secs(4));
            hard_exit();
        });
    if deadline.is_err() {
        println!("{}", failure("Native helper watchdog could not start"));
        return;
    }
    let mut gate = [0u8];
    if std::io::stdin().read_exact(&mut gate).is_err() || gate != [0x56] {
        println!("{}", failure("Native application helper was not activated"));
        return;
    }
    if std::thread::Builder::new()
        .name("mac-observer-parent".into())
        .spawn(|| {
            let mut byte = [0u8];
            // EOF, unexpected data and read errors all end this disposable helper.
            let _ = std::io::stdin().read(&mut byte);
            hard_exit();
        })
        .is_err()
    {
        println!(
            "{}",
            failure("Native helper parent watchdog could not start")
        );
        return;
    }
    let value = crate::macos_observer::snapshot();
    let encoded = value.to_string();
    let encoded = if encoded.len() <= OUTPUT_LIMIT {
        encoded
    } else {
        failure("Native application observation exceeded its output budget").to_string()
    };
    let mut stdout = std::io::stdout().lock();
    let _ = writeln!(stdout, "{encoded}");
    let _ = stdout.flush();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn invalid_helper_output_never_claims_complete() {
        let v = failure("test fixture");
        assert_eq!(v["available"], false);
        assert_eq!(v["complete"], false);
        assert_eq!(v["applications"], json!([]));
    }
    #[test]
    fn output_is_bounded_and_must_be_valid_json() {
        assert!(bounded_json(&b"not json"[..]).is_none());
        assert!(bounded_json(std::io::repeat(b' ').take((OUTPUT_LIMIT + 1) as u64)).is_none());
        assert_eq!(
            bounded_json(&b"{\"available\":false}"[..]).unwrap()["available"],
            false
        );
    }
    #[test]
    fn cancellation_does_not_spawn_or_touch_storage() {
        let (value, clean) = collect(Path::new("/invalid/never-execute"), &AtomicBool::new(true));
        assert!(clean);
        assert_eq!(value["available"], false);
        assert!(value["error"].as_str().unwrap().contains("cancelled"));
    }
    #[test]
    fn cleanup_reaps_only_the_owned_child() {
        let mut owned = Command::new("/bin/sleep").arg("10").spawn().unwrap();
        assert!(stop_child(&mut owned));
        assert!(owned.try_wait().unwrap().is_some());
    }
}
