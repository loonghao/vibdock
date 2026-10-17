//! Closed, revisioned binding configuration, independent of platform activation.
use crate::hotkey::{Chord, Hotkey};
use serde_json::{json, Value};
use std::{io::Read, path::PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Operation {
    Mute,
    Unmute,
    Toggle,
}
impl Operation {
    fn parse(value: &Value) -> Result<Self, String> {
        match value.as_str() {
            Some("mute") => Ok(Self::Mute),
            Some("unmute") => Ok(Self::Unmute),
            Some("toggle") => Ok(Self::Toggle),
            _ => Err("Unsupported audio operation".into()),
        }
    }
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Mute => "mute",
            Self::Unmute => "unmute",
            Self::Toggle => "toggle",
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Action {
    Microphone {
        device_id: String,
        operation: Operation,
    },
    Playback {
        entry_id: String,
        operation: Operation,
    },
    Keys {
        chord: Chord,
        delay_ms: u64,
    },
}
impl Action {
    fn parse(value: &Value) -> Result<Self, String> {
        match value["type"].as_str() {
            Some("microphone") => {
                fields(value, &["type", "device_id", "operation"])?;
                Ok(Self::Microphone {
                    device_id: text(value, "device_id", 512)?,
                    operation: Operation::parse(&value["operation"])?,
                })
            }
            Some("playback") => {
                fields(value, &["type", "entry_id", "operation"])?;
                let entry_id = text(value, "entry_id", 64)?;
                if entry_id.len() != 64 || !entry_id.bytes().all(|c| c.is_ascii_hexdigit()) {
                    return Err("Playback entry_id must be a saved entry reference".into());
                }
                Ok(Self::Playback {
                    entry_id,
                    operation: Operation::parse(&value["operation"])?,
                })
            }
            Some("keys") => {
                fields(value, &["type", "chord", "delay_ms"])?;
                let chord = Chord::parse(&text(value, "chord", 128)?).map_err(|e| e.to_string())?;
                let delay_ms = value["delay_ms"]
                    .as_u64()
                    .filter(|v| *v <= 2000)
                    .ok_or("Key delay must be 0..2000 milliseconds")?;
                Ok(Self::Keys { chord, delay_ms })
            }
            _ => Err("Unsupported binding action type".into()),
        }
    }
    fn json(&self) -> Value {
        match self {
            Self::Microphone {
                device_id,
                operation,
            } => json!({"type":"microphone","device_id":device_id,"operation":operation.as_str()}),
            Self::Playback {
                entry_id,
                operation,
            } => json!({"type":"playback","entry_id":entry_id,"operation":operation.as_str()}),
            Self::Keys { chord, delay_ms } => {
                json!({"type":"keys","chord":chord.as_string(),"delay_ms":delay_ms})
            }
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Binding {
    pub(crate) id: String,
    pub(crate) label: String,
    pub(crate) enabled: bool,
    pub(crate) trigger: Hotkey,
    pub(crate) actions: Vec<Action>,
}
impl Binding {
    fn parse(value: &Value) -> Result<Self, String> {
        fields(value, &["id", "label", "enabled", "trigger", "actions"])?;
        let id = text(value, "id", 64)?;
        if id == "legacy-microphone" {
            return Err("That binding id is reserved for the legacy shortcut".into());
        }
        if !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
        {
            return Err(
                "Binding id must contain ASCII letters, digits, underscores or hyphens".into(),
            );
        }
        let label = text(value, "label", 100)?;
        let enabled = value["enabled"]
            .as_bool()
            .ok_or("enabled must be a boolean")?;
        let trigger =
            Hotkey::parse_trigger(&text(value, "trigger", 128)?).map_err(|e| e.to_string())?;
        let raw = value["actions"]
            .as_array()
            .filter(|a| !a.is_empty() && a.len() <= 8)
            .ok_or("A binding requires 1..8 actions")?;
        let actions = raw
            .iter()
            .map(Action::parse)
            .collect::<Result<Vec<_>, _>>()?;
        let total: u64 = actions
            .iter()
            .map(|a| match a {
                Action::Keys { delay_ms, .. } => *delay_ms,
                _ => 0,
            })
            .sum();
        if total > 10000 {
            return Err("Total key delay exceeds 10000 milliseconds".into());
        }
        Ok(Self {
            id,
            label,
            enabled,
            trigger,
            actions,
        })
    }
    pub(crate) fn json(&self) -> Value {
        json!({"id":self.id,"label":self.label,"enabled":self.enabled,"trigger":self.trigger.as_string(),"actions":self.actions.iter().map(Action::json).collect::<Vec<_>>()})
    }
}
fn text(value: &Value, key: &str, limit: usize) -> Result<String, String> {
    value[key]
        .as_str()
        .filter(|v| !v.trim().is_empty() && v.len() <= limit && !v.chars().any(char::is_control))
        .map(str::to_owned)
        .ok_or_else(|| format!("{key} must be a non-empty plain string of at most {limit} bytes"))
}
fn fields(value: &Value, names: &[&str]) -> Result<(), String> {
    let object = value.as_object().ok_or("Expected a JSON object")?;
    if object.len() != names.len() || object.keys().any(|key| !names.contains(&key.as_str())) {
        return Err("Unexpected or missing fields; only typed binding actions are accepted".into());
    }
    Ok(())
}
fn parse_list(value: &Value) -> Result<Vec<Binding>, String> {
    let raw = value
        .as_array()
        .filter(|v| v.len() <= 8)
        .ok_or("At most eight bindings are accepted")?;
    let mut bindings: Vec<Binding> = Vec::new();
    for value in raw {
        let next = Binding::parse(value)?;
        if bindings.iter().any(|b| b.id == next.id) {
            return Err("Duplicate binding id".into());
        }
        if next.enabled
            && bindings
                .iter()
                .any(|b| b.enabled && b.trigger == next.trigger)
        {
            return Err("Enabled binding triggers must be unique".into());
        }
        bindings.push(next);
    }
    Ok(bindings)
}
pub(crate) fn strict_json(raw: &str) -> Result<Value, String> {
    let value: Value = serde_json::from_str(raw).map_err(|_| "Expected valid JSON".to_string())?;
    let (mut quoted, mut escaped, mut fields) = (false, false, 0usize);
    for byte in raw.bytes() {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else if byte == b'"' {
            quoted = true;
        } else if byte == b':' {
            fields += 1;
        }
    }
    fn count(value: &Value) -> usize {
        match value {
            Value::Object(map) => map.len() + map.values().map(count).sum::<usize>(),
            Value::Array(list) => list.iter().map(count).sum(),
            _ => 0,
        }
    }
    if fields != count(&value) {
        return Err("Duplicate JSON fields are rejected".into());
    }
    Ok(value)
}
pub(crate) struct Store {
    path: PathBuf,
    pub(crate) revision: u64,
    pub(crate) bindings: Vec<Binding>,
    pub(crate) error: Option<String>,
}
impl Store {
    pub(crate) fn load(path: PathBuf) -> Self {
        let mut store = Self {
            path,
            revision: 0,
            bindings: vec![],
            error: None,
        };
        let result = (|| -> Result<(u64, Vec<Binding>), String> {
            let file = match std::fs::File::open(&store.path) {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok((0, vec![])),
                Err(_) => {
                    return Err("Binding storage cannot be read; original data is preserved".into())
                }
            };
            let mut bytes = vec![];
            file.take(65537)
                .read_to_end(&mut bytes)
                .map_err(|_| "Binding storage cannot be read")?;
            if bytes.len() > 65536 {
                return Err("Binding storage exceeds its size budget".into());
            }
            let value = strict_json(
                std::str::from_utf8(&bytes).map_err(|_| "Binding storage is not UTF-8")?,
            )?;
            fields(&value, &["schema_version", "revision", "bindings"])?;
            if value["schema_version"] != 1 {
                return Err("Unsupported binding schema".into());
            }
            Ok((
                value["revision"]
                    .as_u64()
                    .ok_or("Invalid stored revision")?,
                parse_list(&value["bindings"])?,
            ))
        })();
        match result {
            Ok((revision, bindings)) => {
                store.revision = revision;
                store.bindings = bindings;
            }
            Err(error) => store.error = Some(error),
        }
        store
    }
    pub(crate) fn candidate(&self, body: &str) -> Result<Vec<Binding>, (u16, String)> {
        if self.error.is_some() {
            return Err((
                503,
                "Binding storage is unavailable; original data is preserved".into(),
            ));
        }
        if body.len() > 65536 {
            return Err((413, "Binding request exceeds its size budget".into()));
        }
        let value = strict_json(body).map_err(|error| (400, error))?;
        fields(&value, &["revision", "bindings"]).map_err(|e| (400, e))?;
        let revision = value["revision"]
            .as_u64()
            .ok_or((400, "revision must be an unsigned integer".into()))?;
        if revision != self.revision {
            return Err((409, "Bindings changed; refresh before saving".into()));
        }
        parse_list(&value["bindings"]).map_err(|e| (400, e))
    }
    pub(crate) fn commit(&mut self, bindings: Vec<Binding>) -> Result<(), String> {
        let revision = self
            .revision
            .checked_add(1)
            .ok_or("Binding revision is exhausted")?;
        let value = json!({"schema_version":1,"revision":revision,"bindings":bindings.iter().map(Binding::json).collect::<Vec<_>>()});
        crate::service::atomic_write(&self.path, value.to_string().as_bytes())
            .map_err(|_| "Could not persist bindings; previous data is unchanged")?;
        self.revision = revision;
        self.bindings = bindings;
        Ok(())
    }
    pub(crate) fn view(&self) -> Value {
        json!({"schema_version":1,"revision":self.revision,"bindings":self.bindings.iter().map(Binding::json).collect::<Vec<_>>(),"storage_error":self.error})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> Value {
        json!({"id":"fixture","label":"Fixture only","enabled":true,"trigger":"Ctrl+Alt+F9","actions":[{"type":"keys","chord":"Enter","delay_ms":0}]})
    }
    #[test]
    fn strict_actions_limits_and_canonical_metadata() {
        let mut value = fixture();
        assert_eq!(
            Binding::parse(&value).unwrap().json()["trigger"],
            "Ctrl + Alt + F9"
        );
        value["actions"][0]["shell"] = json!("forbidden");
        assert!(Binding::parse(&value).is_err());
        value = fixture();
        value["actions"][0]["delay_ms"] = json!(2001);
        assert!(Binding::parse(&value).is_err());
        value = fixture();
        value["trigger"] = json!("F12");
        assert!(Binding::parse(&value).is_err());
        assert!(parse_list(&json!([fixture(), fixture()])).is_err());
        assert!(parse_list(&Value::Array(vec![fixture(); 9])).is_err());
        assert!(strict_json("{\"revision\":0,\"revision\":0,\"bindings\":[]}").is_err());
        assert!(strict_json("{\"name\":\"a:\\\"b\",\"nested\":{\"x\":1}}").is_ok());
    }
    #[test]
    fn revision_persistence_and_corrupt_storage_preserve_bytes() {
        let directory = std::env::temp_dir().join(format!(
            "vibdock-bindings-{}",
            crate::service::generate_token().unwrap()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("bindings.json");
        let mut store = Store::load(path.clone());
        let candidate = store
            .candidate(&json!({"revision":0,"bindings":[fixture()]}).to_string())
            .unwrap();
        store.commit(candidate).unwrap();
        assert_eq!(Store::load(path.clone()).revision, 1);
        assert_eq!(
            store
                .candidate("{\"revision\":0,\"bindings\":[]}")
                .unwrap_err()
                .0,
            409
        );
        std::fs::write(&path, b"bad-json").unwrap();
        let broken = Store::load(path.clone());
        assert!(broken.error.is_some());
        assert_eq!(broken.candidate("{}").unwrap_err().0, 503);
        assert_eq!(std::fs::read(path).unwrap(), b"bad-json");
        std::fs::remove_dir_all(directory).unwrap();
    }
}
