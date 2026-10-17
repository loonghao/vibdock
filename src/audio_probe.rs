//! Pure validation for bounded, exact-device microphone observations and tests.
use serde_json::{json, Value};

#[cfg(any(windows, test))]
pub(crate) const MAX_REQUEST_BYTES: usize = 2048;

#[cfg(any(windows, test))]
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Request {
    pub device_id: Option<String>,
    pub test: bool,
    pub operation: Option<String>,
}

fn plain(value: &str, max: usize) -> bool {
    !value.trim().is_empty() && value.len() <= max && !value.chars().any(char::is_control)
}

#[cfg(any(windows, test))]
pub(crate) fn decode_request(bytes: &[u8]) -> Result<Request, &'static str> {
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err("Microphone probe request is too large");
    }
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| "Expected a microphone probe JSON object")?;
    let object = value
        .as_object()
        .ok_or("Expected a microphone probe JSON object")?;
    if object.len() != 2
        || object
            .keys()
            .any(|key| !matches!(key.as_str(), "device_id" | "test" | "operation"))
        || field_count(bytes) != 2
    {
        return Err("Expected device_id and exactly one probe or operation field");
    }
    let operation = match value.get("operation") {
        Some(Value::String(operation))
            if matches!(operation.as_str(), "mute" | "unmute" | "toggle") =>
        {
            Some(operation.clone())
        }
        Some(_) => return Err("Unknown microphone operation"),
        None => None,
    };
    let test = if operation.is_some() {
        false
    } else {
        value["test"].as_bool().ok_or("test must be a boolean")?
    };
    let device_id = match &value["device_id"] {
        Value::Null if !test && operation.is_none() => None,
        Value::String(id) if plain(id, 512) => Some(id.clone()),
        _ => return Err("An explicit, plain microphone identity is required"),
    };
    Ok(Request {
        device_id,
        test,
        operation,
    })
}

// Value keeps only the last duplicate key; count separators in the validated JSON too.
#[cfg(any(windows, test))]
fn field_count(bytes: &[u8]) -> usize {
    let (mut quoted, mut escaped, mut count) = (false, false, 0);
    for byte in bytes {
        if quoted {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                quoted = false;
            }
        } else if *byte == b'"' {
            quoted = true;
        } else if *byte == b':' {
            count += 1;
        }
    }
    count
}

#[cfg(any(windows, test))]
pub(crate) fn encode_request(device_id: Option<&str>, test: bool) -> Result<Vec<u8>, &'static str> {
    let bytes = json!({"device_id":device_id,"test":test})
        .to_string()
        .into_bytes();
    decode_request(&bytes)?;
    Ok(bytes)
}

#[cfg(any(windows, test))]
pub(crate) fn encode_action(device_id: &str, operation: &str) -> Result<Vec<u8>, &'static str> {
    let bytes = json!({"device_id":device_id,"operation":operation})
        .to_string()
        .into_bytes();
    decode_request(&bytes)?;
    Ok(bytes)
}

#[cfg(any(windows, test))]
pub(crate) fn action_failure(
    device_id: &str,
    operation: &str,
    attempted: bool,
    error: &str,
) -> Value {
    json!({"device_id":device_id,"operation":operation,"state":if attempted {"outcome_unconfirmed"}else{"rejected_before_write"},"muted":null,"before_muted":null,"write_attempted":attempted,"error":error})
}

#[cfg(any(windows, test))]
pub(crate) fn valid_action(value: &Value, device_id: &str, operation: &str) -> bool {
    if !value.as_object().is_some_and(|object| {
        object.len() == 7
            && object.keys().all(|key| {
                matches!(
                    key.as_str(),
                    "device_id"
                        | "operation"
                        | "state"
                        | "muted"
                        | "before_muted"
                        | "write_attempted"
                        | "error"
                )
            })
    }) || value["device_id"].as_str() != Some(device_id)
        || !plain(device_id, 512)
        || value["operation"].as_str() != Some(operation)
        || !matches!(operation, "mute" | "unmute" | "toggle")
    {
        return false;
    }
    let Some(attempted) = value["write_attempted"].as_bool() else {
        return false;
    };
    let optional_bool = |key: &str| matches!(value.get(key), Some(Value::Null | Value::Bool(_)));
    if !optional_bool("muted") || !optional_bool("before_muted") {
        return false;
    }
    match value["state"].as_str() {
        Some("verified") => value["before_muted"].as_bool().is_some_and(|before| {
            value["muted"].as_bool()
                == Some(match operation {
                    "mute" => true,
                    "unmute" => false,
                    _ => !before,
                })
                && value["error"].is_null()
        }),
        Some("rejected_before_write") => {
            !attempted && value["error"].as_str().is_some_and(|e| plain(e, 2048))
        }
        Some("outcome_unconfirmed") => {
            attempted && value["error"].as_str().is_some_and(|e| plain(e, 2048))
        }
        _ => false,
    }
}

pub(crate) fn unknown(device_id: Option<&str>, error: &str) -> Value {
    json!({"device_id":device_id,"name":null,"state":"unknown","muted":null,"age_ms":null,"error":error,"peak":null,"meter_source":"unknown","level_state":"unknown"})
}

#[cfg(any(windows, test))]
pub(crate) fn finite_peak(peak: f32) -> Option<f64> {
    (peak.is_finite() && (0.0..=1.0).contains(&peak)).then_some(f64::from(peak))
}

#[cfg(any(windows, test))]
pub(crate) fn passive_peak(peak: f32, hardware: bool) -> Option<f64> {
    finite_peak(peak).filter(|peak| hardware || *peak > 0.0)
}

pub(crate) fn valid_microphone(value: &Value, expected: &str) -> bool {
    if !value.as_object().is_some_and(|object| {
        object.keys().all(|key| {
            matches!(
                key.as_str(),
                "device_id"
                    | "name"
                    | "state"
                    | "age_ms"
                    | "error"
                    | "peak"
                    | "meter_source"
                    | "level_state"
                    | "muted"
                    | "sample_count"
                    | "captured_frames"
                    | "signal_detected"
                    | "duration_ms"
                    | "elapsed_ms"
            )
        })
    }) || !plain(expected, 512)
        || value["device_id"].as_str() != Some(expected)
        || !matches!(value.get("name"), Some(Value::Null))
            && !value["name"].as_str().is_some_and(|name| plain(name, 160))
        || !matches!(
            value["meter_source"].as_str(),
            Some("hardware" | "software" | "unknown")
        )
    {
        return false;
    }
    let available = match value["level_state"].as_str() {
        Some("available") => {
            if !value["peak"]
                .as_f64()
                .is_some_and(|peak| peak.is_finite() && (0.0..=1.0).contains(&peak))
            {
                return false;
            }
            true
        }
        Some("unknown") if value.get("peak") == Some(&Value::Null) => false,
        _ => return false,
    };
    match value["state"].as_str() {
        Some("muted" | "unmuted") => {
            value["muted"].as_bool() == Some(value["state"] == "muted")
                && value.get("error") == Some(&Value::Null)
                && value["age_ms"].as_u64() == Some(0)
        }
        Some("unknown" | "disconnected") => {
            !available
                && value["error"]
                    .as_str()
                    .is_some_and(|error| plain(error, 2048))
                && matches!(
                    value.get("muted"),
                    None | Some(Value::Null | Value::Bool(_))
                )
                && (value.get("age_ms") == Some(&Value::Null)
                    || value["age_ms"].as_u64() == Some(0))
        }
        _ => false,
    }
}

#[cfg(any(windows, test))]
pub(crate) fn valid_test(value: &Value, expected: &str) -> bool {
    if !valid_microphone(value, expected)
        || value["sample_count"].as_u64().is_none()
        || value["captured_frames"].as_u64().is_none()
        || value["duration_ms"].as_u64() != Some(1500)
        || !value["elapsed_ms"]
            .as_u64()
            .is_some_and(|elapsed| elapsed <= 3000)
    {
        return false;
    }
    if matches!(value["state"].as_str(), Some("muted" | "unmuted")) {
        value["sample_count"]
            .as_u64()
            .is_some_and(|count| count > 0)
            && value["captured_frames"]
                .as_u64()
                .is_some_and(|frames| frames > 0)
            && value["level_state"] == "available"
            && value["elapsed_ms"]
                .as_u64()
                .is_some_and(|elapsed| elapsed >= 1500)
            && value["peak"]
                .as_f64()
                .is_some_and(|peak| value["signal_detected"].as_bool() == Some(peak > 0.001))
    } else {
        value.get("signal_detected") == Some(&Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn microphone() -> Value {
        json!({"device_id":"chosen","name":"Microphone","state":"unmuted","muted":false,"age_ms":0,"error":null,"peak":0.25,"meter_source":"hardware","level_state":"available"})
    }

    fn completed_test() -> Value {
        let mut value = microphone();
        value["sample_count"] = json!(20);
        value["captured_frames"] = json!(128);
        value["duration_ms"] = json!(1500);
        value["elapsed_ms"] = json!(1500);
        value["signal_detected"] = json!(true);
        value
    }

    #[test]
    fn request_roundtrip_preserves_exact_identity_and_observation_null() {
        for (device_id, test) in [
            (None, false),
            (Some(" chosen "), false),
            (Some("chosen"), true),
        ] {
            let decoded = decode_request(&encode_request(device_id, test).unwrap()).unwrap();
            assert_eq!(decoded.device_id.as_deref(), device_id);
            assert_eq!(decoded.test, test);
        }
    }

    #[test]
    fn request_rejects_missing_extra_malformed_and_oversize_fields() {
        for bytes in [
            &b"{}"[..],
            &b"[]"[..],
            &b"null"[..],
            &b"{\"device_id\":null}"[..],
            &b"{\"test\":false}"[..],
            &b"{\"device_id\":null,\"test\":false,\"extra\":1}"[..],
            &b"{\"device_id\":\"other\",\"device_id\":\"chosen\",\"test\":false}"[..],
            &b"{\"device_id\":\"chosen\",\"test\":true,\"test\":false}"[..],
            &b"{\"device_id\":null,\"test\":0}"[..],
            &b"{\"device_id\":7,\"test\":false}"[..],
            &b"{\"device_id\":null,\"test\":true}"[..],
            &b"{\"device_id\":\"chosen\",\"test\":true} trailing"[..],
            &[0xff][..],
        ] {
            assert!(decode_request(bytes).is_err(), "accepted {bytes:?}");
        }
        assert!(decode_request(&vec![b' '; MAX_REQUEST_BYTES + 1]).is_err());
    }

    #[test]
    fn invalid_device_ids_are_never_encoded() {
        for id in ["", " \t ", "\nchosen", "chosen\u{7f}", &"a".repeat(513)] {
            assert!(encode_request(Some(id), false).is_err());
            assert!(encode_request(Some(id), true).is_err());
        }
        assert!(encode_request(None, true).is_err());
        assert!(encode_request(Some(&"é".repeat(256)), true).is_ok());
        assert!(encode_request(Some(&"é".repeat(257)), true).is_err());
        assert!(encode_request(Some("device:\"quoted\"\\path"), true).is_ok());
    }

    #[test]
    fn finite_peak_rejects_nonfinite_and_out_of_range_without_clamping() {
        for peak in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.001, 1.001] {
            assert_eq!(finite_peak(peak), None);
        }
        for peak in [0.0, 0.25, 1.0] {
            assert_eq!(finite_peak(peak), Some(f64::from(peak)));
        }
    }

    #[test]
    fn passive_peak_does_not_claim_silence_from_a_software_zero() {
        assert_eq!(passive_peak(0.0, false), None);
        assert_eq!(passive_peak(0.0, true), Some(0.0));
        assert_eq!(passive_peak(0.25, false), Some(0.25));
        for hardware in [false, true] {
            for peak in [f32::NAN, f32::INFINITY, -0.1, 1.1] {
                assert_eq!(passive_peak(peak, hardware), None);
            }
        }
    }

    #[test]
    fn microphone_requires_identity_consistent_mute_and_bounded_fields() {
        assert!(valid_microphone(&microphone(), "chosen"));
        assert!(!valid_microphone(&microphone(), "another"));
        for (key, invalid) in [
            ("muted", json!(true)),
            ("error", json!("readback failed")),
            ("age_ms", json!(1)),
            ("name", json!("a".repeat(161))),
            ("name", json!("bad\nname")),
            ("peak", json!(-0.1)),
            ("peak", json!(1.01)),
            ("peak", Value::Null),
            ("meter_source", json!("invented")),
            ("level_state", json!("unknown")),
        ] {
            let mut value = microphone();
            value[key] = invalid;
            assert!(!valid_microphone(&value, "chosen"), "accepted {value}");
        }
        let mut value = microphone();
        value["state"] = json!("muted");
        value["muted"] = json!(true);
        value["peak"] = Value::Null;
        value["level_state"] = json!("unknown");
        value["meter_source"] = json!("software");
        assert!(valid_microphone(&value, "chosen"));
    }

    #[test]
    fn failed_observations_do_not_claim_a_level_or_replace_identity() {
        let mut value = unknown(Some("chosen"), "Endpoint is unavailable");
        assert!(valid_microphone(&value, "chosen"));
        assert!(!valid_microphone(&value, "another"));
        value["state"] = json!("disconnected");
        value["age_ms"] = json!(0);
        value["muted"] = json!(false);
        assert!(valid_microphone(&value, "chosen"));
        value["peak"] = json!(0.0);
        assert!(!valid_microphone(&value, "chosen"));
        value["level_state"] = json!("available");
        assert!(!valid_microphone(&value, "chosen"));
        value["peak"] = Value::Null;
        value["level_state"] = json!("unknown");
        value["error"] = Value::Null;
        assert!(!valid_microphone(&value, "chosen"));
        assert_eq!(
            unknown(None, "No selected endpoint")["device_id"],
            Value::Null
        );
    }

    #[test]
    fn completed_test_accepts_silence_and_requires_measured_duration_and_signal() {
        assert!(valid_test(&completed_test(), "chosen"));
        let mut silence = completed_test();
        silence["peak"] = json!(0.0);
        silence["signal_detected"] = json!(false);
        assert!(valid_test(&silence, "chosen"));
        silence["peak"] = json!(0.001);
        assert!(valid_test(&silence, "chosen"));
        silence["peak"] = json!(0.0011);
        assert!(!valid_test(&silence, "chosen"));
        for (key, invalid) in [
            ("sample_count", json!(0)),
            ("sample_count", json!(1.5)),
            ("captured_frames", json!(0)),
            ("captured_frames", json!(-1)),
            ("duration_ms", json!(1499)),
            ("elapsed_ms", json!(1499)),
            ("elapsed_ms", json!(3001)),
            ("signal_detected", Value::Null),
        ] {
            let mut value = completed_test();
            value[key] = invalid;
            assert!(!valid_test(&value, "chosen"), "accepted {value}");
        }
    }

    #[test]
    fn failed_test_allows_partial_counts_without_inventing_signal() {
        let mut value = unknown(Some("chosen"), "Sampling failed");
        value["sample_count"] = json!(3);
        value["captured_frames"] = json!(128);
        value["duration_ms"] = json!(1500);
        value["elapsed_ms"] = json!(125);
        value["signal_detected"] = Value::Null;
        assert!(valid_test(&value, "chosen"));
        for (key, invalid) in [
            ("signal_detected", json!(false)),
            ("peak", json!(0.0)),
            ("error", json!("")),
            ("elapsed_ms", json!(3001)),
        ] {
            let mut invalid_value = value.clone();
            invalid_value[key] = invalid;
            assert!(!valid_test(&invalid_value, "chosen"));
        }
    }

    #[test]
    fn responses_reject_raw_samples_and_unexpected_sensitive_fields() {
        for (key, field) in [
            ("samples", json!([0.1, 0.2])),
            ("credentials", json!("never forward")),
        ] {
            let mut value = completed_test();
            value[key] = field;
            assert!(!valid_microphone(&value, "chosen"));
            assert!(!valid_test(&value, "chosen"));
        }
    }

    #[test]
    fn action_requests_require_exact_endpoint_and_closed_operation() {
        for operation in ["mute", "unmute", "toggle"] {
            let request = decode_request(&encode_action("chosen", operation).unwrap()).unwrap();
            assert_eq!(request.operation.as_deref(), Some(operation));
            assert_eq!(request.device_id.as_deref(), Some("chosen"));
            assert!(!request.test);
        }
        for bytes in [
            r#"{"device_id":null,"operation":"mute"}"#,
            r#"{"device_id":"chosen","operation":"default"}"#,
            r#"{"device_id":"chosen","operation":"mute","test":false}"#,
            r#"{"device_id":"chosen","operation":"mute","operation":"toggle"}"#,
        ] {
            assert!(decode_request(bytes.as_bytes()).is_err());
        }
    }

    #[test]
    fn action_results_require_exact_consistent_readback_and_honest_uncertainty() {
        let mut value = action_failure("chosen", "toggle", false, "before write failed");
        assert!(valid_action(&value, "chosen", "toggle"));
        value["write_attempted"] = json!(true);
        assert!(!valid_action(&value, "chosen", "toggle"));
        value["state"] = json!("outcome_unconfirmed");
        assert!(valid_action(&value, "chosen", "toggle"));
        value["before_muted"] = json!(false);
        value["muted"] = json!(true);
        value["error"] = Value::Null;
        value["state"] = json!("verified");
        assert!(valid_action(&value, "chosen", "toggle"));
        assert!(!valid_action(&value, "other", "toggle"));
        value["muted"] = json!(false);
        assert!(!valid_action(&value, "chosen", "toggle"));
    }
}
