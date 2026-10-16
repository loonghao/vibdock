//! Own exactly one disposable observer; no in-service COM or pipe reader thread.
use serde_json::{json, Value};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::windows::{fs::OpenOptionsExt, io::AsRawHandle, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};
use windows::Win32::{
    Foundation::{CloseHandle, HANDLE},
    System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    },
};
const OUTPUT_LIMIT: usize = 512 * 1024;
struct Job(HANDLE);
impl Job {
    fn new() -> std::io::Result<Self> {
        unsafe {
            // NULL security attributes: this private unnamed handle is not inherited.
            let job = Self(CreateJobObjectW(None, None).map_err(std::io::Error::other)?);
            let mut info = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            SetInformationJobObject(
                job.0,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of_val(&info) as u32,
            )
            .map_err(std::io::Error::other)?;
            Ok(job)
        }
    }
}
impl Drop for Job {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}
struct Output {
    file: File,
    path: PathBuf,
}
impl Output {
    fn new() -> std::io::Result<Self> {
        let directory = crate::service::state_dir()
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        std::fs::create_dir_all(&directory)?;
        let path = directory.join(format!(
            ".observer-{}.tmp",
            crate::service::generate_token()
                .map_err(|error| std::io::Error::other(error.to_string()))?
        ));
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .custom_flags(0x0400_0000) // FILE_FLAG_DELETE_ON_CLOSE; inherited ACLs unchanged.
            .open(&path)?;
        Ok(Self { file, path })
    }
}
impl Drop for Output {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}
fn failure(message: &str) -> Value {
    json!({"available":false,"complete":false,"endpoints":[],"sessions":[],"skipped":0,"error":message})
}

pub(crate) fn collect(executable: &Path, stop: &AtomicBool) -> (Value, bool) {
    let job = match Job::new() {
        Ok(value) => value,
        Err(_) => return (failure("Native helper containment is unavailable"), true),
    };
    let mut output = match Output::new() {
        Ok(value) => value,
        Err(_) => return (failure("Native helper output storage is unavailable"), true),
    };
    let stdout = match output.file.try_clone() {
        Ok(file) => file,
        Err(_) => return (failure("Native helper output storage is unavailable"), true),
    };
    let mut child = match Command::new(executable)
        .arg("_resident_snapshot")
        .stdin(Stdio::piped())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::null())
        .creation_flags(0x08000000)
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return (failure("Native observation helper could not start"), true),
    };
    // Until this succeeds, the helper only waits on its stdin gate. It cannot
    // initialize COM or scan software images before containment is established.
    let assigned =
        unsafe { AssignProcessToJobObject(job.0, HANDLE(child.as_raw_handle())).is_ok() };
    let activated = assigned
        && child
            .stdin
            .take()
            .is_some_and(|mut input| input.write_all(&[0x56]).is_ok());
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut reason = "Native observation helper failed";
    let success = if !activated {
        reason = "Native helper containment or activation failed";
        false
    } else {
        loop {
            if stop.load(Ordering::Relaxed) {
                reason = "Native observation cancelled";
                break false;
            }
            if Instant::now() >= deadline {
                reason = "Native observation timed out";
                break false;
            }
            if output
                .file
                .metadata()
                .map_or(true, |metadata| metadata.len() > OUTPUT_LIMIT as u64)
            {
                reason = "Native observation exceeded its output budget";
                break false;
            }
            match child.try_wait() {
                Ok(Some(status)) => break status.success(),
                Ok(None) => {}
                Err(_) => break false,
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    };
    // No unconditional wait or pipe-reader join. Windows termination is an
    // asynchronous request; distinguish a confirmed exit from pending cleanup.
    if !success {
        let _ = child.kill();
    }
    drop(child.stdin.take());
    let cleanup_deadline = Instant::now() + Duration::from_millis(400);
    let cleaned = loop {
        match child.try_wait() {
            Ok(Some(_)) => break true,
            Ok(None) => {}
            Err(_) => break false,
        }
        if Instant::now() >= cleanup_deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    drop(job); // Requests termination of only this owned helper if still alive.
    if !cleaned {
        return (
            failure("Native helper cleanup is unconfirmed; observation stopped"),
            false,
        );
    }
    if !success {
        return (failure(reason), true);
    }
    let mut bytes = Vec::new();
    if output.file.seek(SeekFrom::Start(0)).is_err()
        || (&mut output.file)
            .take(OUTPUT_LIMIT as u64 + 1)
            .read_to_end(&mut bytes)
            .is_err()
    {
        return (failure("Native observation output could not be read"), true);
    }
    if bytes.len() > OUTPUT_LIMIT {
        return (
            failure("Native observation exceeded its output budget"),
            true,
        );
    }
    (
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| failure("Native observation returned invalid or truncated data")),
        true,
    )
}
