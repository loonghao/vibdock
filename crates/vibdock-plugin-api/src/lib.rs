//! Stable contracts for VibDock plugins.
//!
//! A plugin package is data first: `plugin.json` declares identity, version,
//! kind, capabilities and the UI/action/profile contributions it owns. The host
//! validates that manifest before enabling anything. Built-in plugins are Rust
//! crates compiled into the host; third-party plugins are expected to run out of
//! process behind the same JSON contract rather than linking against an unstable
//! Rust ABI.
//!
//! This crate intentionally depends only on `serde`/`serde_json`. It must not
//! depend on the Windows input, HID, audio or service crates.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_ID_LEN: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PluginKind {
    /// Owns physical controls and contributes a device view.
    Device,
    /// Owns application discovery, launch/input actions or voice integration.
    Integration,
    /// Contributes local structured commands.
    ActionProvider,
    /// Contributes a sandboxed configuration page.
    Ui,
}

/// Capabilities the host is willing to expose. The host maps each capability to
/// an audited implementation; a manifest cannot grant itself OS access.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    DeviceEnumerate,
    /// The concrete VID/PID is declared separately in `permissions`.
    DeviceHidVendor,
    FirmwareReadback,
    AppDiscover,
    AppLaunch,
    InputInject,
    WindowForeground,
    CommandRun,
    MicrophoneControl,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PluginManifest {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    pub vendor: String,
    #[serde(default)]
    pub description: String,
    pub kind: Vec<PluginKind>,
    #[serde(default)]
    pub capabilities: Vec<Capability>,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default)]
    pub contributes: Contributions,
    #[serde(default)]
    pub requires: Requirements,
}

const fn default_schema_version() -> u32 {
    SCHEMA_VERSION
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Requirements {
    #[serde(default)]
    pub core: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Contributions {
    /// Declarative device layout. The host draws it and owns the hit testing.
    #[serde(default)]
    pub device_view: Option<DeviceView>,
    /// JSON Schema subset rendered by the host form renderer.
    #[serde(default)]
    pub config_schema: Option<Value>,
    #[serde(default)]
    pub profiles: Vec<ProfilePreset>,
    #[serde(default)]
    pub actions: Vec<ActionDescriptor>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceView {
    pub controls: Vec<DeviceControl>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceControl {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub kind: ControlKind,
    #[serde(default)]
    pub icon: Option<String>,
    /// Logical grid position. The host maps this to its own drawing.
    #[serde(default)]
    pub row: u8,
    #[serde(default)]
    pub column: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlKind {
    #[default]
    Button,
    Knob,
    Dial,
    Microphone,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GestureKind {
    Single,
    Double,
    Long,
    /// Distinct controls tapped in order-insensitive quick succession.
    Sequence,
    /// Distinct controls held together past the hold threshold.
    Hold,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProfilePreset {
    pub id: String,
    pub name: String,
    pub plugin: String,
    #[serde(default)]
    pub bindings: Vec<PresetBinding>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PresetBinding {
    /// One control for a single-key gesture, two or three for a combo.
    pub controls: Vec<String>,
    #[serde(default)]
    pub gesture: Option<GestureKind>,
    /// Steps use the host's action schema; plugins do not execute them.
    #[serde(default)]
    pub steps: Vec<Value>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionDescriptor {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub parameters: Vec<ActionParameter>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionParameter {
    pub name: String,
    pub label: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default = "default_parameter_length")]
    pub max_length: usize,
}

const fn default_parameter_length() -> usize {
    200
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogLevel {
    Info,
    Warn,
    Error,
}

/// Host services a plugin may use after it has been enabled.
pub trait HostServices {
    fn state_dir(&self) -> &Path;
    fn log(&self, level: LogLevel, message: &str);
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceEvent {
    pub device_key: String,
    pub control: String,
    pub pressed: bool,
    /// Monotonic milliseconds supplied by the host.
    pub at_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginError {
    pub code: String,
    pub message: String,
}

impl PluginError {
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}

/// Declarative plugin contract. Built-in plugins implement this directly;
/// third-party plugins are adapted from the same JSON manifest.
pub trait Plugin: Send + Sync {
    fn manifest(&self) -> &PluginManifest;
}

/// A plugin that owns physical controls and reports press/release edges.
pub trait DevicePlugin: Plugin {
    fn poll(&mut self, host: &mut dyn HostServices) -> Result<Vec<DeviceEvent>, PluginError>;
}

/// A plugin that contributes named actions with typed parameters.
pub trait ActionProvider: Plugin {
    fn actions(&self) -> &[ActionDescriptor];
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ManifestError {
    UnsupportedSchema(u32),
    EmptyField(&'static str),
    InvalidId,
    NoKind,
    DuplicateCapability(Capability),
    NoDeviceViewForDevicePlugin,
    BadControl(&'static str),
}

impl PluginManifest {
    /// Rejects malformed manifests before any capability is exposed.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(ManifestError::UnsupportedSchema(self.schema_version));
        }
        for (field, value) in [
            ("id", &self.id),
            ("name", &self.name),
            ("version", &self.version),
            ("vendor", &self.vendor),
        ] {
            if value.trim().is_empty() {
                return Err(ManifestError::EmptyField(field));
            }
        }
        if self.id.len() > MAX_ID_LEN
            || !self
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        {
            return Err(ManifestError::InvalidId);
        }
        if self.kind.is_empty() {
            return Err(ManifestError::NoKind);
        }
        for (index, capability) in self.capabilities.iter().enumerate() {
            if self.capabilities[..index].contains(capability) {
                return Err(ManifestError::DuplicateCapability(*capability));
            }
        }
        let has_device_kind = self.kind.contains(&PluginKind::Device);
        if has_device_kind && self.contributes.device_view.is_none() {
            return Err(ManifestError::NoDeviceViewForDevicePlugin);
        }
        if let Some(view) = &self.contributes.device_view {
            let mut seen = Vec::new();
            for control in &view.controls {
                if control.id.trim().is_empty() || control.label.trim().is_empty() {
                    return Err(ManifestError::BadControl("empty id or label"));
                }
                if seen.contains(&control.id) {
                    return Err(ManifestError::BadControl("duplicate control id"));
                }
                seen.push(control.id.clone());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device_manifest() -> PluginManifest {
        PluginManifest {
            schema_version: SCHEMA_VERSION,
            id: "ulanzi.au05".into(),
            name: "Ulanzi Vibe Key".into(),
            version: "1.4.0".into(),
            vendor: "Ulanzi".into(),
            description: "3 buttons and a knob".into(),
            kind: vec![PluginKind::Device],
            capabilities: vec![Capability::DeviceEnumerate, Capability::FirmwareReadback],
            permissions: vec!["hid:fff1:00dd".into()],
            contributes: Contributions {
                device_view: Some(DeviceView {
                    controls: vec![DeviceControl {
                        id: "button1".into(),
                        label: "语音键".into(),
                        kind: ControlKind::Button,
                        icon: Some("mic".into()),
                        row: 0,
                        column: 0,
                    }],
                }),
                ..Contributions::default()
            },
            requires: Requirements::default(),
        }
    }

    #[test]
    fn a_device_manifest_with_a_view_is_valid() {
        assert_eq!(device_manifest().validate(), Ok(()));
    }

    #[test]
    fn a_device_plugin_must_contribute_a_view() {
        let mut manifest = device_manifest();
        manifest.contributes.device_view = None;
        assert_eq!(
            manifest.validate(),
            Err(ManifestError::NoDeviceViewForDevicePlugin)
        );
    }

    #[test]
    fn ids_and_versions_are_checked() {
        let mut manifest = device_manifest();
        manifest.id = "bad id".into();
        assert_eq!(manifest.validate(), Err(ManifestError::InvalidId));
        manifest = device_manifest();
        manifest.version.clear();
        assert_eq!(
            manifest.validate(),
            Err(ManifestError::EmptyField("version"))
        );
    }

    #[test]
    fn duplicate_controls_are_rejected() {
        let mut manifest = device_manifest();
        let view = manifest.contributes.device_view.as_mut().unwrap();
        view.controls.push(view.controls[0].clone());
        assert_eq!(
            manifest.validate(),
            Err(ManifestError::BadControl("duplicate control id"))
        );
    }

    #[test]
    fn unknown_schema_is_rejected() {
        let mut manifest = device_manifest();
        manifest.schema_version = 99;
        assert_eq!(
            manifest.validate(),
            Err(ManifestError::UnsupportedSchema(99))
        );
    }

    #[test]
    fn a_gamepad_manifest_round_trips_through_json() {
        let manifest = PluginManifest {
            id: "sony.dualsense".into(),
            name: "Sony DualSense".into(),
            version: "1.0.0".into(),
            vendor: "Sony".into(),
            kind: vec![PluginKind::Device],
            capabilities: vec![Capability::DeviceEnumerate],
            permissions: vec!["hid:054c".into()],
            contributes: Contributions {
                device_view: Some(DeviceView {
                    controls: vec![DeviceControl {
                        id: "cross".into(),
                        label: "Cross".into(),
                        kind: ControlKind::Button,
                        icon: None,
                        row: 0,
                        column: 0,
                    }],
                }),
                ..Contributions::default()
            },
            ..device_manifest()
        };
        let encoded = serde_json::to_string(&manifest).unwrap();
        let decoded: PluginManifest = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, manifest);
        assert_eq!(decoded.validate(), Ok(()));
    }
}
