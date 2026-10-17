//! Own exactly one disposable native helper; no in-service COM or reader thread.
use crate::resident_action_protocol as action;
use crate::resident_apps::audio_probe;
use serde_json::{json, Value};
use std::{
    ffi::c_void,
    fs::{File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::windows::{
        fs::OpenOptionsExt,
        io::{AsRawHandle, FromRawHandle},
        process::CommandExt,
    },
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

#[link(name = "kernel32")]
extern "system" {
    fn CreateNamedPipeW(
        name: *const u16,
        access: u32,
        mode: u32,
        instances: u32,
        output: u32,
        input: u32,
        timeout: u32,
        security: *const c_void,
    ) -> *mut c_void;
    fn ConnectNamedPipe(pipe: *mut c_void, overlapped: *mut c_void) -> i32;
    fn GetNamedPipeClientProcessId(pipe: *mut c_void, process_id: *mut u32) -> i32;
}

/// Both ends are opened by this process before spawning. Only the read end is
/// inherited as stdin. A nonblocking writer keeps activation inside the same
/// cancellation/deadline loop even if the helper never reads a single byte.
struct Input {
    writer: File,
    reader: File,
}
impl Input {
    fn new() -> std::io::Result<Self> {
        let name = format!(
            r"\\.\pipe\vibdock-action-{}",
            crate::service::generate_token()
                .map_err(|error| std::io::Error::other(error.to_string()))?
        );
        let wide: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let raw = unsafe {
            CreateNamedPipeW(
                wide.as_ptr(),
                0x0000_0002 | 0x0008_0000, // PIPE_ACCESS_OUTBOUND | FILE_FLAG_FIRST_PIPE_INSTANCE
                0x0000_0001 | 0x0000_0008, // PIPE_NOWAIT | PIPE_REJECT_REMOTE_CLIENTS; byte type
                1,
                action::MAX_JOB_BYTES as u32 + 1,
                0,
                0,
                std::ptr::null(),
            )
        };
        if raw == -1isize as *mut c_void {
            return Err(std::io::Error::last_os_error());
        }
        let writer = unsafe { File::from_raw_handle(raw) };
        // Default process ACLs, no security-setting change. One local client
        // slot is consumed here; no listener or pipe name is passed to a child.
        let reader = OpenOptions::new().read(true).open(&name)?;
        // PIPE_NOWAIT reports a completed client connection as ERROR_PIPE_CONNECTED.
        if unsafe { ConnectNamedPipe(writer.as_raw_handle(), std::ptr::null_mut()) } != 0
            || std::io::Error::last_os_error().raw_os_error() != Some(535)
        {
            return Err(std::io::Error::other(
                "Private helper pipe is not connected",
            ));
        }
        let mut pid = 0;
        if unsafe { GetNamedPipeClientProcessId(writer.as_raw_handle(), &mut pid) } == 0
            || pid != std::process::id()
        {
            return Err(std::io::Error::other(
                "Private helper pipe owner is unconfirmed",
            ));
        }
        Ok(Self { writer, reader })
    }
}
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

pub(crate) fn collect(
    executable: &Path,
    stop: &AtomicBool,
    device_id: Option<&str>,
) -> (Value, bool) {
    let payload = match audio_probe::encode_request(device_id, false) {
        Ok(payload) => payload,
        Err(error) => return (failure(error), true),
    };
    let (mut value, cleaned) = collect_request(executable, stop, &payload);
    if !value.is_object() {
        value = failure("Native observation returned an invalid object");
    }
    if value.get("microphone").is_none() {
        value["microphone"] = audio_probe::unknown(
            device_id,
            value["error"]
                .as_str()
                .unwrap_or("Microphone observation is unavailable"),
        );
    }
    (value, cleaned)
}

pub(crate) fn probe_microphone(
    executable: &Path,
    stop: &AtomicBool,
    device_id: &str,
) -> (Value, bool) {
    let failed = |error: &str| {
        let mut value = audio_probe::unknown(Some(device_id), error);
        for (key, field) in [
            ("duration_ms", json!(1500)),
            ("elapsed_ms", json!(0)),
            ("sample_count", json!(0)),
            ("captured_frames", json!(0)),
            ("signal_detected", Value::Null),
        ] {
            value[key] = field;
        }
        value
    };
    let payload = match audio_probe::encode_request(Some(device_id), true) {
        Ok(payload) => payload,
        Err(error) => return (failed(error), true),
    };
    let (value, cleaned) = collect_request(executable, stop, &payload);
    if audio_probe::valid_test(&value, device_id) {
        (value, cleaned)
    } else {
        (
            failed(
                value["error"]
                    .as_str()
                    .unwrap_or("Microphone test output is invalid or changed"),
            ),
            cleaned,
        )
    }
}

pub(crate) fn microphone_action(
    executable: &Path,
    stop: &AtomicBool,
    device_id: &str,
    operation: &str,
) -> (Value, bool) {
    let payload = match audio_probe::encode_action(device_id, operation) {
        Ok(payload) => payload,
        Err(error) => {
            return (
                audio_probe::action_failure(device_id, operation, false, error),
                true,
            )
        }
    };
    let mut activated = false;
    let (value, cleaned) = contained_request(executable, stop, &payload, &mut activated);
    if audio_probe::valid_action(&value, device_id, operation) {
        (value, cleaned)
    } else {
        (
            audio_probe::action_failure(
                device_id,
                operation,
                activated,
                value["error"]
                    .as_str()
                    .unwrap_or("microphone_action_output_invalid"),
            ),
            cleaned,
        )
    }
}

pub(crate) fn preferred_route(
    executable: &Path,
    stop: &AtomicBool,
    value: &Value,
    revision: &std::sync::atomic::AtomicU64,
    expected: u64,
) -> (Value, bool) {
    let payload = value.to_string().into_bytes();
    let job = match crate::resident_apps::app_route::parse_job(&payload) {
        Ok(job) => job,
        Err(error) => return (json!({"error":error}), true),
    };
    let mut activated = false;
    let (value, cleaned) = guarded_request(executable, &payload, &mut activated, || {
        stop.load(Ordering::Relaxed) || revision.load(Ordering::Acquire) != expected
    });
    if crate::resident_apps::app_route::valid_result(&value, &job) {
        (value, cleaned)
    } else {
        (
            crate::resident_apps::app_route::failure(
                &job,
                activated,
                value["error"].as_str().unwrap_or("route_result_invalid"),
            ),
            cleaned,
        )
    }
}

fn collect_request(executable: &Path, stop: &AtomicBool, payload: &[u8]) -> (Value, bool) {
    contained_request(executable, stop, payload, &mut false)
}

fn contained_request(
    executable: &Path,
    stop: &AtomicBool,
    payload: &[u8],
    activated: &mut bool,
) -> (Value, bool) {
    guarded_request(executable, payload, activated, || {
        stop.load(Ordering::Relaxed)
    })
}

fn guarded_request(
    executable: &Path,
    payload: &[u8],
    activated: &mut bool,
    cancelled: impl Fn() -> bool,
) -> (Value, bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    if cancelled() {
        return (failure("Native observation cancelled"), true);
    }
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
    let Input { writer, reader } = match Input::new() {
        Ok(input) => input,
        Err(_) => return (failure("Native helper input is unavailable"), true),
    };
    let mut child = match Command::new(executable)
        .arg("_resident_snapshot")
        .stdin(Stdio::from(reader))
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
    let mut input = Some(writer);
    let mut bytes = Vec::with_capacity(payload.len() + 1);
    bytes.push(0x56);
    bytes.extend_from_slice(payload);
    let mut sent = 0;
    let mut reason = "Native observation helper failed";
    let success = if !assigned {
        reason = "Native helper containment or activation failed";
        false
    } else {
        loop {
            if cancelled() {
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
            if let Some(writer) = input.as_mut() {
                // Once any gate/payload write is attempted the parent cannot
                // prove that a mutating helper did not reach its native write.
                *activated = true;
                match writer.write(&bytes[sent..]) {
                    Ok(count) => sent += count,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(_) => {
                        reason = "Native helper activation failed";
                        break false;
                    }
                }
                if sent == bytes.len() {
                    drop(input.take());
                }
            }
            match child.try_wait() {
                Ok(Some(status)) => break status.success() && sent == bytes.len(),
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
    drop(input);
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

/// The caller serializes scans and actions on the one resident owner thread.
/// A false cleanup flag is terminal: it must not start another helper.
pub(crate) fn execute(executable: &Path, stop: &AtomicBool, value: &Value) -> (Value, bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    let parsed = match action::parse_job(value) {
        Ok(parsed) => parsed,
        Err(error) => return (action::failure(value, false, error), true),
    };
    let payload = match serde_json::to_vec(value) {
        Ok(bytes) if bytes.len() <= action::MAX_JOB_BYTES => bytes,
        _ => {
            return (
                action::failure(value, false, "action_job_budget_exceeded"),
                true,
            )
        }
    };
    let fail = |attempted, error| action::failure(value, attempted, error);
    if stop.load(Ordering::Relaxed) {
        return (fail(false, "action_cancelled"), true);
    }
    let job = match Job::new() {
        Ok(job) => job,
        Err(_) => return (fail(false, "helper_containment_unavailable"), true),
    };
    let mut output = match Output::new() {
        Ok(output) => output,
        Err(_) => return (fail(false, "helper_output_unavailable"), true),
    };
    let stdout = match output.file.try_clone() {
        Ok(file) => file,
        Err(_) => return (fail(false, "helper_output_unavailable"), true),
    };
    let Input { writer, reader } = match Input::new() {
        Ok(input) => input,
        Err(_) => return (fail(false, "helper_input_unavailable"), true),
    };
    if stop.load(Ordering::Relaxed) {
        return (fail(false, "action_cancelled"), true);
    }
    if Instant::now() >= deadline {
        return (fail(false, "action_timed_out"), true);
    }
    let mut child = match Command::new(executable)
        .arg("_resident_action")
        .stdin(Stdio::from(reader))
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::null())
        .creation_flags(0x08000000)
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return (fail(false, "helper_start_failed"), true),
    };
    let assigned =
        unsafe { AssignProcessToJobObject(job.0, HANDLE(child.as_raw_handle())).is_ok() };
    let mut input = Some(writer);
    let mut bytes = Vec::with_capacity(payload.len() + 1);
    bytes.push(0x56);
    bytes.extend_from_slice(&payload);
    let mut sent = 0;
    let mut attempted = false;
    let mut reason = "helper_failed";
    let success = if !assigned {
        reason = "helper_containment_failed";
        false
    } else {
        loop {
            if stop.load(Ordering::Relaxed) {
                reason = "action_cancelled";
                break false;
            }
            if Instant::now() >= deadline {
                reason = "action_timed_out";
                break false;
            }
            if output.file.metadata().map_or(true, |metadata| {
                metadata.len() > action::MAX_RESULT_BYTES as u64
            }) {
                reason = "action_output_budget_exceeded";
                break false;
            }
            if let Some(writer) = input.as_mut() {
                // Conservatively uncertain from the first activation write
                // attempt, including partial/failed writes. No synchronous
                // write_all, flush, or pipe-reader thread can escape deadline.
                attempted = true;
                match writer.write(&bytes[sent..]) {
                    Ok(count) => sent += count,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(_) => {
                        reason = "helper_activation_failed";
                        break false;
                    }
                }
                if sent == bytes.len() {
                    drop(input.take()); // EOF is required before the helper parses/executes.
                }
            }
            match child.try_wait() {
                Ok(Some(status)) => break status.success() && sent == bytes.len(),
                Ok(None) => {}
                Err(_) => break false,
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    };
    if !success {
        let _ = child.kill();
    }
    drop(input);
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
    drop(job);
    if !cleaned {
        return (fail(attempted, "helper_cleanup_unconfirmed"), false);
    }
    if !success {
        return (fail(attempted, reason), true);
    }
    let mut bytes = Vec::new();
    if output.file.seek(SeekFrom::Start(0)).is_err()
        || (&mut output.file)
            .take(action::MAX_RESULT_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .is_err()
    {
        return (fail(true, "action_output_unreadable"), true);
    }
    if bytes.len() > action::MAX_RESULT_BYTES {
        return (fail(true, "action_output_budget_exceeded"), true);
    }
    match action::decode_result(&bytes, &parsed) {
        Some(result) => (result, true),
        None => (fail(true, "action_output_invalid"), true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_route_revision_never_activates_a_helper() {
        let value = json!({"kind":"preferred_endpoint_route","operation_id":"a".repeat(64),"target":{"process_id":42,"process_started":"123","executable_path":"C:\\Fixture\\App.exe"},"flow":"capture","device_id":"chosen","roles":[0,1,2]});
        let (result, cleaned) = preferred_route(
            Path::new("must-never-launch.exe"),
            &AtomicBool::new(false),
            &value,
            &std::sync::atomic::AtomicU64::new(2),
            1,
        );
        assert!(cleaned);
        assert_eq!(result["state"], "rejected_before_write");
        assert_eq!(result["write_attempted"], false);
        assert!(crate::resident_apps::app_route::valid_result(
            &result,
            &crate::resident_apps::app_route::parse_job(value.to_string().as_bytes()).unwrap()
        ));
    }

    #[test]
    fn cancelled_microphone_action_is_rejected_before_activation() {
        let (value, cleaned) = microphone_action(
            Path::new("must-never-launch.exe"),
            &AtomicBool::new(true),
            "explicit-mic",
            "toggle",
        );
        assert!(cleaned);
        assert_eq!(value["state"], "rejected_before_write");
        assert_eq!(value["write_attempted"], false);
        assert!(audio_probe::valid_action(&value, "explicit-mic", "toggle"));
    }

    #[test]
    fn cancelled_microphone_probe_never_spawns_or_selects_another_endpoint() {
        let (value, cleaned) = probe_microphone(
            Path::new("nonexistent-helper-must-never-launch.exe"),
            &AtomicBool::new(true),
            "explicit-fixture-microphone",
        );
        assert!(cleaned);
        assert_eq!(value["device_id"], "explicit-fixture-microphone");
        assert_eq!(value["state"], "unknown");
        assert_eq!(value["sample_count"], 0);
        assert_eq!(value["peak"], Value::Null);
        assert_eq!(value["signal_detected"], Value::Null);
        assert!(audio_probe::valid_test(
            &value,
            "explicit-fixture-microphone"
        ));
    }

    #[test]
    #[ignore = "requires a freshly built owned CLI and an isolated LOCALAPPDATA; CI runs it explicitly"]
    fn contained_action_helper_rejects_impossible_identity() {
        let executable = PathBuf::from(
            std::env::var_os("VIBDOCK_ACTION_HELPER_EXE").expect("fresh CLI path required"),
        );
        assert!(executable.is_file());
        let job = json!({"operation_id":"a".repeat(64),"requested_muted":true,"target":{
            "endpoint_id":"nonexistent-fixture-output","session_instance_id":"nonexistent-fixture-session",
            "process_id":std::process::id(),"process_started":"1","executable_path":"C:\\VibDockFixture\\NeverExecuted.exe"}});
        let (result, cleaned) = execute(&executable, &AtomicBool::new(false), &job);
        assert!(cleaned, "Owned action helper exit was not confirmed");
        assert_eq!(
            result["state"], "rejected_before_write",
            "Contained helper failed before returning its native rejection: {result}"
        );
        assert!(matches!(
            result["error"].as_str(),
            Some(
                "process_identity_changed"
                    | "process_unavailable"
                    | "com_unavailable"
                    | "playback_unavailable"
            )
        ));
        assert!(result["observed_muted"].is_null());
        assert!(action::valid_result(
            &result,
            &action::parse_job(&job).unwrap()
        ));
        let (cancelled, cleaned) = execute(&executable, &AtomicBool::new(true), &job);
        assert!(cleaned);
        assert_eq!(cancelled["state"], "rejected_before_write");
        assert_eq!(cancelled["error"], "action_cancelled");
        let directory = crate::service::state_dir().unwrap();
        assert!(std::fs::read_dir(directory).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".observer-")));
    }

    #[test]
    fn private_stdin_preserves_the_gate_and_json_until_eof() {
        let Input {
            mut writer,
            mut reader,
        } = Input::new().unwrap();
        let bytes = b"V{\"private\":true}";
        assert_eq!(writer.write(bytes).unwrap(), bytes.len());
        drop(writer);
        let mut read = Vec::new();
        (&mut reader).take(128).read_to_end(&mut read).unwrap();
        assert_eq!(read, bytes);
    }

    #[test]
    fn full_private_stdin_does_not_block_the_owner() {
        let Input {
            mut writer,
            reader: _reader,
        } = Input::new().unwrap();
        let bytes = [0u8; action::MAX_JOB_BYTES + 1];
        for _ in 0..128 {
            match writer.write(&bytes) {
                Ok(0) => return,
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return,
                Err(error) => panic!("Unexpected private pipe error: {error}"),
            }
        }
        panic!("Private stdin did not enforce its bounded buffer");
    }
}
