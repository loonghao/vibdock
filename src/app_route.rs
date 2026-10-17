//! Closed preferred-endpoint contracts. A policy readback does not prove an app's stream moved.
use serde_json::{json, Value};

pub(crate) const MAX_BYTES: usize = 8192;
#[cfg(any(windows, test))]
const PREFIX: &str = r"\\?\SWD#MMDEVAPI#";
#[cfg(any(windows, test))]
const RENDER: &str = "#{e6327cad-dcec-4949-ae8a-991e976a79d2}";
#[cfg(any(windows, test))]
const CAPTURE: &str = "#{2eef81be-33fa-4800-9670-1cd474972c3f}";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Request {
    pub revision: u64,
    pub selection_id: String,
    pub flow: String,
    pub device_id: String,
    pub roles: Vec<u32>,
}
#[derive(Clone, Debug)]
pub(crate) struct Job {
    pub operation_id: String,
    pub process_id: u32,
    pub process_started: u64,
    pub executable_path: String,
    pub flow: String,
    pub device_id: String,
    pub roles: Vec<u32>,
}
impl Job {
    pub(crate) fn json(&self) -> Value {
        json!({"kind":"preferred_endpoint_route","operation_id":self.operation_id,"target":{"process_id":self.process_id,"process_started":self.process_started.to_string(),"executable_path":self.executable_path},"flow":self.flow,"device_id":self.device_id,"roles":self.roles})
    }
}
fn plain(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}
fn token(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn fields(bytes: &[u8]) -> usize {
    let (mut quote, mut escape, mut count) = (false, false, 0);
    for byte in bytes {
        if quote {
            if escape {
                escape = false;
            } else if *byte == b'\\' {
                escape = true;
            } else if *byte == b'"' {
                quote = false;
            }
        } else if *byte == b'"' {
            quote = true;
        } else if *byte == b':' {
            count += 1;
        }
    }
    count
}
fn object(bytes: &[u8], allowed: &[&str], count: usize) -> Result<Value, &'static str> {
    if bytes.len() > MAX_BYTES {
        return Err("route_request_too_large");
    }
    let value: Value = serde_json::from_slice(bytes).map_err(|_| "expected_route_json")?;
    if !value.as_object().is_some_and(|o| {
        o.len() == allowed.len() && o.keys().all(|key| allowed.contains(&key.as_str()))
    }) || fields(bytes) != count
    {
        return Err("unexpected_or_duplicate_route_field");
    }
    Ok(value)
}
fn parse_values(value: &Value) -> Result<(String, String, Vec<u32>), &'static str> {
    let flow = value["flow"]
        .as_str()
        .filter(|flow| matches!(*flow, "capture" | "render"))
        .ok_or("route_flow_invalid")?
        .to_owned();
    let device = value["device_id"]
        .as_str()
        .filter(|id| plain(id, 512))
        .ok_or("explicit_route_device_required")?
        .to_owned();
    let raw = value["roles"]
        .as_array()
        .filter(|roles| !roles.is_empty() && roles.len() <= 3)
        .ok_or("route_roles_invalid")?;
    let mut roles = Vec::new();
    for role in raw {
        let role = role
            .as_u64()
            .filter(|role| *role <= 2)
            .ok_or("route_role_invalid")? as u32;
        if roles.contains(&role) {
            return Err("duplicate_route_role");
        }
        roles.push(role);
    }
    Ok((flow, device, roles))
}
pub(crate) fn parse_request(bytes: &[u8]) -> Result<Request, &'static str> {
    let value = object(
        bytes,
        &["revision", "selection_id", "flow", "device_id", "roles"],
        5,
    )?;
    let revision = value["revision"]
        .as_u64()
        .ok_or("route_revision_required")?;
    let selection_id = value["selection_id"]
        .as_str()
        .filter(|id| token(id))
        .ok_or("route_selection_required")?
        .to_owned();
    let (flow, device_id, roles) = parse_values(&value)?;
    Ok(Request {
        revision,
        selection_id,
        flow,
        device_id,
        roles,
    })
}
pub(crate) fn parse_job(bytes: &[u8]) -> Result<Job, &'static str> {
    let value = object(
        bytes,
        &[
            "kind",
            "operation_id",
            "target",
            "flow",
            "device_id",
            "roles",
        ],
        9,
    )?;
    if value["kind"] != "preferred_endpoint_route"
        || !value["target"].as_object().is_some_and(|o| {
            o.len() == 3
                && o.keys().all(|key| {
                    matches!(
                        key.as_str(),
                        "process_id" | "process_started" | "executable_path"
                    )
                })
        })
    {
        return Err("route_target_invalid");
    }
    let operation_id = value["operation_id"]
        .as_str()
        .filter(|id| token(id))
        .ok_or("route_operation_invalid")?
        .to_owned();
    let process_id = value["target"]["process_id"]
        .as_u64()
        .filter(|pid| *pid > 4 && *pid <= u32::MAX as u64)
        .ok_or("route_process_invalid")? as u32;
    let started = value["target"]["process_started"]
        .as_str()
        .filter(|stamp| !stamp.is_empty() && stamp.bytes().all(|b| b.is_ascii_digit()))
        .ok_or("route_creation_required")?;
    let process_started = started
        .parse::<u64>()
        .ok()
        .filter(|stamp| *stamp > 0)
        .ok_or("route_creation_invalid")?;
    let executable_path = value["target"]["executable_path"]
        .as_str()
        .filter(|path| path.len() <= 1024 && super::local_executable(path))
        .ok_or("route_executable_invalid")?
        .to_owned();
    let (flow, device_id, roles) = parse_values(&value)?;
    Ok(Job {
        operation_id,
        process_id,
        process_started,
        executable_path,
        flow,
        device_id,
        roles,
    })
}
#[cfg(any(windows, test))]
pub(crate) fn pack(device_id: &str, flow: &str) -> Option<String> {
    if !plain(device_id, 512) {
        return None;
    }
    Some(format!(
        "{PREFIX}{device_id}{}",
        match flow {
            "capture" => CAPTURE,
            "render" => RENDER,
            _ => return None,
        }
    ))
}
#[cfg(any(windows, test))]
pub(crate) fn unpack(value: &str, flow: &str) -> Option<String> {
    let suffix = match flow {
        "capture" => CAPTURE,
        "render" => RENDER,
        _ => return None,
    };
    let id = value.strip_prefix(PREFIX)?.strip_suffix(suffix)?;
    plain(id, 512).then(|| id.to_owned())
}

#[cfg(any(windows, test))]
pub(crate) trait Adapter {
    fn guard(&mut self) -> Result<(), String>;
    fn get(&mut self, role: u32) -> Result<Option<String>, String>;
    fn set(&mut self, role: u32, device: Option<&str>) -> Result<(), String>;
}
pub(crate) fn failure(job: &Job, attempted: bool, error: &str) -> Value {
    json!({"operation_id":job.operation_id,"process_id":job.process_id,"flow":job.flow,"device_id":job.device_id,"state":if attempted {"outcome_unconfirmed"}else{"rejected_before_write"},"write_attempted":attempted,"rollback_attempted":false,"rollback_verified":false,"roles":[],"stream_state":"unknown","error":error})
}

/// Snapshot every selected enum role before any write. Failed Set calls may have
/// mutated, so include them in rollback and verify every old value explicitly.
#[cfg(any(windows, test))]
pub(crate) fn apply(adapter: &mut impl Adapter, job: &Job) -> Value {
    let desired = pack(&job.device_id, &job.flow).expect("validated route job");
    let mut result = failure(job, false, "route_unavailable");
    let mut old = Vec::new();
    for role in &job.roles {
        let value = match adapter
            .guard()
            .and_then(|_| adapter.get(*role))
            .and_then(|value| adapter.guard().map(|_| value))
        {
            Ok(value)
                if value
                    .as_deref()
                    .is_none_or(|value| unpack(value, &job.flow).is_some()) =>
            {
                value
            }
            Ok(_) => {
                result["error"] = json!("route_previous_value_invalid");
                return result;
            }
            Err(error) => {
                result["error"] = json!(error);
                return result;
            }
        };
        old.push((*role, value));
    }
    let mut attempted = Vec::new();
    let execute = (|| -> Result<(), String> {
        for (role, previous) in &old {
            adapter.guard()?;
            if previous.as_deref() != Some(desired.as_str()) {
                attempted.push(*role);
                adapter.set(*role, Some(&desired))?;
            }
        }
        let mut roles = Vec::new();
        for role in &job.roles {
            adapter.guard()?;
            let readback = adapter.get(*role)?;
            adapter.guard()?;
            if readback.as_deref() != Some(desired.as_str()) {
                return Err("route_readback_mismatch".into());
            }
            roles.push(json!({"role":role,"device_id":job.device_id,"verified":true}));
        }
        result["roles"] = json!(roles);
        Ok(())
    })();
    result["write_attempted"] = json!(!attempted.is_empty());
    match execute {
        Ok(()) => {
            result["state"] = json!("verified");
            result["error"] = Value::Null;
        }
        Err(error) if attempted.is_empty() => {
            result["error"] = json!(error);
        }
        Err(error) => {
            result["error"] = json!(error);
            result["state"] = json!("outcome_unconfirmed");
            result["rollback_attempted"] = json!(true);
            let mut restored = true;
            for role in attempted.iter().rev() {
                let previous = old
                    .iter()
                    .find(|(old, _)| old == role)
                    .unwrap()
                    .1
                    .as_deref();
                if adapter
                    .guard()
                    .and_then(|_| adapter.set(*role, previous))
                    .is_err()
                {
                    restored = false;
                }
            }
            for (role, previous) in &old {
                match adapter
                    .guard()
                    .and_then(|_| adapter.get(*role))
                    .and_then(|value| adapter.guard().map(|_| value))
                {
                    Ok(value) if &value == previous => {}
                    _ => restored = false,
                }
            }
            result["rollback_verified"] = json!(restored);
            if restored {
                result["state"] = json!("rolled_back");
            }
        }
    }
    result
}
pub(crate) fn valid_result(value: &Value, job: &Job) -> bool {
    if !value.as_object().is_some_and(|o| {
        o.len() == 11
            && o.keys().all(|key| {
                matches!(
                    key.as_str(),
                    "operation_id"
                        | "process_id"
                        | "flow"
                        | "device_id"
                        | "state"
                        | "write_attempted"
                        | "rollback_attempted"
                        | "rollback_verified"
                        | "roles"
                        | "stream_state"
                        | "error"
                )
            })
    }) || value["operation_id"] != job.operation_id
        || value["process_id"] != job.process_id
        || value["flow"] != job.flow
        || value["device_id"] != job.device_id
        || value["stream_state"] != "unknown"
    {
        return false;
    }
    let (Some(attempted), Some(rollback), Some(restored), Some(roles)) = (
        value["write_attempted"].as_bool(),
        value["rollback_attempted"].as_bool(),
        value["rollback_verified"].as_bool(),
        value["roles"].as_array(),
    ) else {
        return false;
    };
    match value["state"].as_str() {
        Some("verified") => {
            !rollback
                && !restored
                && value["error"].is_null()
                && roles.len() == job.roles.len()
                && roles.iter().zip(&job.roles).all(|(row, role)| {
                    row.as_object().is_some_and(|o| o.len() == 3)
                        && row["role"] == *role
                        && row["device_id"] == job.device_id
                        && row["verified"] == true
                })
        }
        Some("rejected_before_write" | "outcome_unconfirmed" | "rolled_back") => {
            roles.is_empty()
                && value["error"].as_str().is_some_and(|e| plain(e, 2048))
                && match value["state"].as_str().unwrap() {
                    "rejected_before_write" => !attempted && !rollback && !restored,
                    "rolled_back" => attempted && rollback && restored,
                    _ => attempted && !restored,
                }
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    fn job() -> Job {
        Job {
            operation_id: "a".repeat(64),
            process_id: 42,
            process_started: 123,
            executable_path: r"C:\Fixture\App.exe".into(),
            flow: "capture".into(),
            device_id: "chosen".into(),
            roles: vec![0, 1, 2],
        }
    }
    struct Fake {
        values: BTreeMap<u32, Option<String>>,
        sets: Vec<(u32, Option<String>)>,
        fail_role: Option<u32>,
        guard_fails: bool,
        readback_wrong: bool,
        gets: usize,
        lose_identity_after_get: Option<usize>,
    }
    impl Adapter for Fake {
        fn guard(&mut self) -> Result<(), String> {
            if self.guard_fails {
                Err("identity_changed".into())
            } else {
                Ok(())
            }
        }
        fn get(&mut self, role: u32) -> Result<Option<String>, String> {
            self.gets += 1;
            if self.lose_identity_after_get == Some(self.gets) {
                self.guard_fails = true;
            }
            if self.readback_wrong && !self.sets.is_empty() {
                Ok(None)
            } else {
                Ok(self.values.get(&role).cloned().flatten())
            }
        }
        fn set(&mut self, role: u32, value: Option<&str>) -> Result<(), String> {
            self.sets.push((role, value.map(str::to_owned)));
            self.values.insert(role, value.map(str::to_owned));
            if self.fail_role == Some(role) {
                self.fail_role = None;
                Err("partial_write".into())
            } else {
                Ok(())
            }
        }
    }
    fn fake() -> Fake {
        Fake {
            values: BTreeMap::new(),
            sets: vec![],
            fail_role: None,
            guard_fails: false,
            readback_wrong: false,
            gets: 0,
            lose_identity_after_get: None,
        }
    }
    #[test]
    fn packing_preserves_exact_id_and_direction() {
        for flow in ["capture", "render"] {
            let packed = pack(" chosen ", flow).unwrap();
            assert_eq!(unpack(&packed, flow).as_deref(), Some(" chosen "));
            assert_eq!(
                unpack(
                    &packed,
                    if flow == "capture" {
                        "render"
                    } else {
                        "capture"
                    }
                ),
                None
            );
        }
        assert_eq!(pack("", "capture"), None);
        assert_eq!(pack("chosen", "all"), None);
    }
    #[test]
    fn request_requires_unique_enum_roles_and_strict_fields() {
        let mut value = json!({"revision":0,"selection_id":"a".repeat(64),"flow":"capture","device_id":"chosen","roles":[0,1,2]});
        assert_eq!(
            parse_request(value.to_string().as_bytes()).unwrap().roles,
            vec![0, 1, 2]
        );
        for roles in [
            json!([]),
            json!([0, 0]),
            json!([3]),
            json!([1.5]),
            json!(["1"]),
        ] {
            value["roles"] = roles;
            assert!(parse_request(value.to_string().as_bytes()).is_err());
        }
        value["roles"] = json!([0]);
        value["device_id"] = Value::Null;
        assert!(parse_request(value.to_string().as_bytes()).is_err());
        assert!(parse_request(format!(r#"{{"revision":0,"revision":1,"selection_id":"{}","flow":"render","device_id":"d","roles":[0]}}"#,"a".repeat(64)).as_bytes()).is_err());
    }
    #[test]
    fn every_role_get_is_explicit_and_verified() {
        let mut fake = fake();
        let job = job();
        let value = apply(&mut fake, &job);
        assert_eq!(
            fake.sets.iter().map(|(role, _)| *role).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(value["state"], "verified");
        assert!(valid_result(&value, &job));
    }
    #[test]
    fn partial_failure_restores_including_failed_role_and_null_old_value() {
        let mut fake = fake();
        fake.values.insert(0, pack("old", "capture"));
        fake.fail_role = Some(1);
        let job = job();
        let value = apply(&mut fake, &job);
        assert_eq!(value["state"], "rolled_back");
        assert_eq!(fake.values[&0], pack("old", "capture"));
        assert_eq!(fake.values[&1], None);
        assert_eq!(
            fake.sets.iter().map(|(role, _)| *role).collect::<Vec<_>>(),
            vec![0, 1, 1, 0]
        );
        assert!(valid_result(&value, &job));
    }
    #[test]
    fn identity_and_invalid_old_values_prevent_all_writes() {
        let job = job();
        for invalid_old in [false, true] {
            let mut fake = fake();
            fake.guard_fails = !invalid_old;
            if invalid_old {
                fake.values.insert(0, Some("malformed".into()));
            }
            let value = apply(&mut fake, &job);
            assert_eq!(value["state"], "rejected_before_write");
            assert!(fake.sets.is_empty());
            assert!(valid_result(&value, &job));
        }
    }
    #[test]
    fn unconfirmed_rollback_and_malformed_results_cannot_claim_success() {
        let mut fake = fake();
        fake.readback_wrong = true;
        let job = job();
        let mut value = apply(&mut fake, &job);
        assert_eq!(value["state"], "rolled_back");
        value["state"] = json!("verified");
        assert!(!valid_result(&value, &job));
        value = failure(&job, true, "timed_out");
        assert!(valid_result(&value, &job));
        value["device_id"] = json!("different");
        assert!(!valid_result(&value, &job));
    }

    #[test]
    fn identity_lost_during_get_is_fenced_before_write_and_after_readback() {
        let job = job();
        for get in [1, 4] {
            let mut fake = fake();
            fake.lose_identity_after_get = Some(get);
            let result = apply(&mut fake, &job);
            assert_eq!(
                result["state"],
                if get == 1 {
                    "rejected_before_write"
                } else {
                    "outcome_unconfirmed"
                }
            );
            assert_eq!(fake.sets.is_empty(), get == 1);
            assert!(valid_result(&result, &job));
        }
    }
    #[test]
    fn failed_rollback_readback_retains_uncertainty() {
        let job = job();
        let mut fake = fake();
        fake.values.insert(0, pack("old", "capture"));
        fake.readback_wrong = true;
        let result = apply(&mut fake, &job);
        assert_eq!(result["state"], "outcome_unconfirmed");
        assert_eq!(result["rollback_verified"], false);
        assert!(valid_result(&result, &job));
    }
}
