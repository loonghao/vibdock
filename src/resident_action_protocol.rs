//! Private, closed protocol for one server-captured playback-session action.
//! This is not a browser request format and never resolves a client pathname.

use serde_json::{json, Value};

pub(crate) const MAX_JOB_BYTES: usize = 16 * 1024;
pub(crate) const MAX_RESULT_BYTES: usize = 4096;

#[derive(Debug)]
pub(crate) struct ActionTarget<'a> {
    pub endpoint_id: &'a str,
    pub session_instance_id: &'a str,
    pub process_id: u32,
    pub process_started: u64,
    pub executable_path: &'a str,
}

impl ActionTarget<'_> {
    pub(crate) fn matches_process(&self, pid: u32, started: u64, image: &str) -> bool {
        pid == self.process_id && started == self.process_started && image == self.executable_path
    }
}

#[derive(Debug)]
pub(crate) struct ActionJob<'a> {
    pub operation_id: &'a str,
    pub requested_muted: bool,
    pub target: ActionTarget<'a>,
}

/// GetProcessId's positive AUDCLNT_S_NO_SINGLE_PROCESS is not ownership proof.
/// IsSystemSoundsSession must independently return exactly S_FALSE.
pub(crate) fn single_process_owner(status: i32, system_status: i32, pid: u32) -> Option<u32> {
    (status == 0 && system_status == 1 && pid != 0).then_some(pid)
}

fn closed(value: &Value, keys: &[&str]) -> bool {
    value.as_object().is_some_and(|object| {
        object.len() == keys.len() && keys.iter().all(|key| object.contains_key(*key))
    })
}

fn text<'a>(value: &'a Value, key: &str, limit: usize) -> Option<&'a str> {
    value.get(key)?.as_str().filter(|text| {
        !text.is_empty() && text.len() <= limit && !text.chars().any(char::is_control)
    })
}

pub(crate) fn operation_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(crate) fn local_executable_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() > 7
        && bytes.len() <= 4096
        && !path.chars().any(char::is_control)
        && bytes[0].is_ascii_alphabetic()
        && bytes[1..3] == *b":\\"
        && !path[3..].contains(['/', ':', '*', '?', '"', '<', '>', '|'])
        && path[3..].split('\\').all(|part| {
            !part.is_empty() && part != "." && part != ".." && !part.ends_with(['.', ' '])
        })
        && path
            .rsplit('.')
            .next()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
}

pub(crate) fn parse_job(value: &Value) -> Result<ActionJob<'_>, &'static str> {
    if !closed(value, &["operation_id", "requested_muted", "target"]) {
        return Err("invalid_action_job");
    }
    let id = text(value, "operation_id", 64)
        .filter(|id| operation_id(id))
        .ok_or("invalid_operation_id")?;
    let requested_muted = value["requested_muted"]
        .as_bool()
        .ok_or("invalid_requested_state")?;
    let target = &value["target"];
    if !closed(
        target,
        &[
            "endpoint_id",
            "session_instance_id",
            "process_id",
            "process_started",
            "executable_path",
        ],
    ) {
        return Err("invalid_action_target");
    }
    let endpoint_id = text(target, "endpoint_id", 1024).ok_or("invalid_endpoint")?;
    let session_instance_id =
        text(target, "session_instance_id", 1024).ok_or("invalid_session_instance")?;
    let process_id = target["process_id"]
        .as_u64()
        .and_then(|id| u32::try_from(id).ok())
        .filter(|id| *id != 0)
        .ok_or("invalid_process_id")?;
    let started = text(target, "process_started", 20).ok_or("invalid_process_started")?;
    let process_started = started
        .parse::<u64>()
        .ok()
        .filter(|stamp| *stamp != 0 && stamp.to_string() == started)
        .ok_or("invalid_process_started")?;
    let executable_path = text(target, "executable_path", 4096)
        .filter(|path| local_executable_path(path))
        .ok_or("invalid_executable_path")?;
    Ok(ActionJob {
        operation_id: id,
        requested_muted,
        target: ActionTarget {
            endpoint_id,
            session_instance_id,
            process_id,
            process_started,
            executable_path,
        },
    })
}

/// Only the owned worker writes this input, using serde_json::to_vec. Requiring
/// its canonical encoding also rejects duplicate keys before native execution.
pub(crate) fn decode_job(bytes: &[u8]) -> Result<Value, &'static str> {
    if bytes.is_empty() || bytes.len() > MAX_JOB_BYTES {
        return Err("action_job_budget_exceeded");
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| "invalid_action_json")?;
    parse_job(&value)?;
    if serde_json::to_vec(&value).map_err(|_| "invalid_action_json")? != bytes {
        return Err("noncanonical_action_json");
    }
    Ok(value)
}

pub(crate) fn failure(job: &Value, attempted: bool, error: &'static str) -> Value {
    let id = text(job, "operation_id", 64).filter(|id| operation_id(id));
    json!({
        "operation_id":id, "requested_muted":job["requested_muted"].as_bool(),
        "state":if attempted { "outcome_unconfirmed" } else { "rejected_before_write" },
        "observed_muted":null, "error":error
    })
}

/// A helper cannot claim success with a missing/mismatched operation, unknown
/// mute state, extra private data, or a state inconsistent with its readback.
pub(crate) fn valid_result(value: &Value, job: &ActionJob<'_>) -> bool {
    if !closed(
        value,
        &[
            "operation_id",
            "requested_muted",
            "state",
            "observed_muted",
            "error",
        ],
    ) || value["operation_id"].as_str() != Some(job.operation_id)
        || value["requested_muted"].as_bool() != Some(job.requested_muted)
    {
        return false;
    }
    let observed = value["observed_muted"].as_bool();
    let error = text(value, "error", 128).is_some_and(|error| {
        error
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
    });
    match value["state"].as_str() {
        Some("verified") => observed == Some(job.requested_muted) && value["error"].is_null(),
        Some("verified_mismatch") => observed == Some(!job.requested_muted) && error,
        Some("rejected_before_write" | "outcome_unconfirmed") => {
            value["observed_muted"].is_null() && error
        }
        _ => false,
    }
}

pub(crate) fn decode_result(bytes: &[u8], job: &ActionJob<'_>) -> Option<Value> {
    if bytes.is_empty() || bytes.len() > MAX_RESULT_BYTES {
        return None;
    }
    let value: Value = serde_json::from_slice(bytes).ok()?;
    if !valid_result(&value, job) {
        return None;
    }
    // println! may append one newline. Reject duplicate-key output as well as
    // extra frames rather than letting JSON object construction erase evidence.
    let bytes = bytes
        .strip_suffix(b"\r\n")
        .or_else(|| bytes.strip_suffix(b"\n"))
        .unwrap_or(bytes);
    (serde_json::to_vec(&value).ok()? == bytes).then_some(value)
}

/// The native implementation holds the same read-only process handle for the
/// entire transaction. Synthetic tests exercise the real transaction ordering.
pub(crate) trait ExactSessionAction {
    fn verify_target(&mut self) -> Result<(), &'static str>;
    fn set_mute(&mut self, requested: bool) -> Result<(), &'static str>;
    fn read_mute(&mut self) -> Result<bool, &'static str>;
}

pub(crate) fn run_action(value: &Value, backend: &mut impl ExactSessionAction) -> Value {
    let job = match parse_job(value) {
        Ok(job) => job,
        Err(error) => return failure(value, false, error),
    };
    if let Err(error) = backend.verify_target() {
        return failure(value, false, error);
    }
    // Calling SetMute is the uncertainty boundary, including a failed HRESULT.
    if let Err(error) = backend.set_mute(job.requested_muted) {
        return failure(value, true, error);
    }
    if let Err(error) = backend.verify_target() {
        return failure(value, true, error);
    }
    let observed = match backend.read_mute() {
        Ok(observed) => observed,
        Err(error) => return failure(value, true, error),
    };
    if let Err(error) = backend.verify_target() {
        return failure(value, true, error);
    }
    let matches = observed == job.requested_muted;
    json!({
        "operation_id":job.operation_id, "requested_muted":job.requested_muted,
        "state":if matches { "verified" } else { "verified_mismatch" },
        "observed_muted":observed, "error":if matches { None } else { Some("readback_mismatch") }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job() -> Value {
        json!({"operation_id":"a".repeat(64),"requested_muted":true,"target":{
            "endpoint_id":"speaker","session_instance_id":"one-instance","process_id":42,
            "process_started":"123456","executable_path":"C:\\Example\\App.exe"}})
    }

    #[test]
    fn closed_job_rejects_missing_unknown_and_wrong_typed_fields() {
        let valid = job();
        let parsed = parse_job(&valid).unwrap();
        assert_eq!(parsed.target.endpoint_id, "speaker");
        assert_eq!(parsed.target.session_instance_id, "one-instance");
        assert_eq!(parsed.target.process_id, 42);
        assert_eq!(parsed.target.process_started, 123456);
        assert_eq!(parsed.target.executable_path, r"C:\Example\App.exe");
        for pointer in [
            "/operation_id",
            "/requested_muted",
            "/target",
            "/target/endpoint_id",
            "/target/session_instance_id",
            "/target/process_id",
            "/target/process_started",
            "/target/executable_path",
        ] {
            let mut value = job();
            *value.pointer_mut(pointer).unwrap() = Value::Null;
            assert!(parse_job(&value).is_err(), "{pointer}");
        }
        for pointer in ["", "/target"] {
            let mut value = job();
            value
                .pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert("command".into(), json!("unwanted"));
            assert!(parse_job(&value).is_err());
        }
        for id in [
            json!(0),
            json!(-1),
            json!(u64::from(u32::MAX) + 1),
            json!("42"),
            json!(42.0),
        ] {
            let mut value = job();
            value["target"]["process_id"] = id;
            assert!(parse_job(&value).is_err());
        }
        for stamp in ["0", "0123", "+123", "-1", "18446744073709551616", " 123"] {
            let mut value = job();
            value["target"]["process_started"] = json!(stamp);
            assert!(parse_job(&value).is_err());
        }
        for id in [
            "A".repeat(64),
            "a".repeat(63),
            "a".repeat(65),
            "g".repeat(64),
        ] {
            let mut value = job();
            value["operation_id"] = json!(id);
            assert!(parse_job(&value).is_err());
        }
    }

    #[test]
    fn positive_hresult_multi_process_system_and_unknown_owners_are_never_selected() {
        assert_eq!(single_process_owner(0, 1, 42), Some(42));
        for (status, system, pid) in [
            (1, 1, 42),
            (0x0889_000d, 1, 42),
            (-1, 1, 42),
            (0, 0, 42),
            (0, -1, 42),
            (0, 2, 42),
            (0, 1, 0),
        ] {
            assert_eq!(single_process_owner(status, system, pid), None);
        }
    }

    #[test]
    fn captured_process_identity_is_never_adopted_or_normalized() {
        let value = job();
        let parsed = parse_job(&value).unwrap();
        let target = parsed.target;
        assert!(target.matches_process(42, 123456, r"C:\Example\App.exe"));
        for (pid, started, path) in [
            (43, 123456, r"C:\Example\App.exe"),
            (42, 123457, r"C:\Example\App.exe"),
            (42, 123456, r"C:\Other\App.exe"),
            (42, 123456, r"C:\Example\app.exe"),
            (0, 123456, r"C:\Example\App.exe"),
            (42, 0, r"C:\Example\App.exe"),
        ] {
            assert!(!target.matches_process(pid, started, path));
        }
    }

    #[test]
    fn canonical_stdin_rejects_duplicate_keys_trailing_data_and_excess_budget() {
        let bytes = serde_json::to_vec(&job()).unwrap();
        assert_eq!(decode_job(&bytes).unwrap(), job());
        let raw = String::from_utf8(bytes.clone()).unwrap();
        let duplicate = raw.replacen("{", "{\"requested_muted\":false,", 1);
        assert!(decode_job(duplicate.as_bytes()).is_err());
        assert!(decode_job(format!("{raw}\n").as_bytes()).is_err());
        assert!(decode_job(format!("{raw}{{}}").as_bytes()).is_err());
        assert!(decode_job(&vec![b' '; MAX_JOB_BYTES + 1]).is_err());
        assert!(decode_job(&[0xff]).is_err());
    }

    #[test]
    fn paths_and_identifiers_are_bounded_literal_local_locations() {
        for path in [
            r"\\server\share\app.exe",
            r"\\?\C:\app.exe",
            r"C:app.exe",
            r"C:\..\app.exe",
            r"C:\app.exe:stream",
            r"C:\folder.\app.exe",
            r"C:\folder \app.exe",
            r"C:\\app.exe",
            r"C:/app.exe",
            r"C:\app.dll",
            "C:\\bad\n\\app.exe",
        ] {
            let mut value = job();
            value["target"]["executable_path"] = json!(path);
            assert!(parse_job(&value).is_err(), "{path}");
        }
        for key in ["endpoint_id", "session_instance_id"] {
            for text in [String::new(), "x".repeat(1025), "bad\nidentity".into()] {
                let mut value = job();
                value["target"][key] = json!(text);
                assert!(parse_job(&value).is_err());
            }
        }
    }

    #[derive(Default)]
    struct Fake {
        checks: usize,
        fail_check: usize,
        writes: usize,
        reads: usize,
        write_fails: bool,
        read_fails: bool,
        observed: bool,
    }
    impl ExactSessionAction for Fake {
        fn verify_target(&mut self) -> Result<(), &'static str> {
            self.checks += 1;
            if self.checks == self.fail_check {
                Err("target_changed")
            } else {
                Ok(())
            }
        }
        fn set_mute(&mut self, _: bool) -> Result<(), &'static str> {
            self.writes += 1;
            if self.write_fails {
                Err("write_failed")
            } else {
                Ok(())
            }
        }
        fn read_mute(&mut self) -> Result<bool, &'static str> {
            self.reads += 1;
            if self.read_fails {
                Err("readback_failed")
            } else {
                Ok(self.observed)
            }
        }
    }

    #[test]
    fn stale_or_wrong_identity_never_writes_and_later_changes_never_succeed() {
        for fail_check in 1..=3 {
            let mut backend = Fake {
                fail_check,
                observed: true,
                ..Fake::default()
            };
            let result = run_action(&job(), &mut backend);
            assert_eq!(backend.writes, usize::from(fail_check != 1));
            assert_eq!(
                result["state"],
                if fail_check == 1 {
                    "rejected_before_write"
                } else {
                    "outcome_unconfirmed"
                }
            );
            assert!(result["observed_muted"].is_null());
        }
    }

    #[test]
    fn failed_write_or_readback_never_claims_rejection_or_success() {
        for write_fails in [true, false] {
            let mut backend = Fake {
                write_fails,
                read_fails: !write_fails,
                ..Fake::default()
            };
            let result = run_action(&job(), &mut backend);
            assert_eq!(result["state"], "outcome_unconfirmed");
            assert_eq!(backend.writes, 1);
            assert!(result["observed_muted"].is_null());
        }
    }

    #[test]
    fn verified_states_require_boolean_readback_and_both_postwrite_checks() {
        let value = job();
        let parsed = parse_job(&value).unwrap();
        for observed in [true, false] {
            let mut backend = Fake {
                observed,
                ..Fake::default()
            };
            let result = run_action(&value, &mut backend);
            assert_eq!((backend.checks, backend.writes, backend.reads), (3, 1, 1));
            assert_eq!(
                result["state"],
                if observed {
                    "verified"
                } else {
                    "verified_mismatch"
                }
            );
            assert!(valid_result(&result, &parsed));
            let bytes = serde_json::to_vec(&result).unwrap();
            assert!(decode_result(&bytes, &parsed).is_some());
            assert!(decode_result(
                format!("{}\n", String::from_utf8(bytes).unwrap()).as_bytes(),
                &parsed
            )
            .is_some());
            for key in ["operation_id", "requested_muted", "observed_muted", "state"] {
                let mut invalid = result.clone();
                invalid[key] = Value::Null;
                assert!(!valid_result(&invalid, &parsed));
            }
            let mut invalid = result;
            invalid["executable_path"] = json!("private");
            assert!(!valid_result(&invalid, &parsed));
        }
        assert!(valid_result(
            &failure(&value, false, "target_changed"),
            &parsed
        ));
        assert!(valid_result(
            &failure(&value, true, "helper_timed_out"),
            &parsed
        ));
        assert!(decode_result(&vec![b' '; MAX_RESULT_BYTES + 1], &parsed).is_none());
        let raw = failure(&value, true, "helper_timed_out").to_string();
        let duplicate = raw.replacen("{", "{\"state\":\"verified\",", 1);
        assert!(decode_result(duplicate.as_bytes(), &parsed).is_none());
    }
}
