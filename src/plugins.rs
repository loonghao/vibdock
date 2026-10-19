//! Built-in plugin manifests and the host-side plugin registry view.
//!
//! This is the first host integration of `vibdock-plugin-api`: it does not yet
//! load third-party packages, but it makes the built-in capabilities visible
//! through one authenticated endpoint and gives the UI/agent a stable list
//! instead of device-specific branches.

use crate::plugin_api::{
    ActionDescriptor, Capability, Contributions, ControlKind, DeviceControl, DeviceView,
    PluginKind, PluginManifest, ProfilePreset, Requirements,
};
use serde_json::{json, Value};

struct Entry {
    manifest: PluginManifest,
    enabled: bool,
    error: Option<String>,
}

pub(crate) struct Registry {
    entries: Vec<Entry>,
}

impl Registry {
    pub(crate) fn start() -> Self {
        let mut entries = Vec::new();
        for manifest in builtins() {
            let error = manifest.validate().err().map(|error| format!("{error:?}"));
            entries.push(Entry {
                enabled: error.is_none(),
                error,
                manifest,
            });
        }
        Self { entries }
    }

    /// Authenticated read-only view for the Settings plugin panel and agents.
    pub(crate) fn view(&self) -> Value {
        json!({
            "schema_version": 1,
            "plugins": self.entries.iter().map(|entry| {
                let manifest = &entry.manifest;
                json!({
                    "id": manifest.id,
                    "name": manifest.name,
                    "version": manifest.version,
                    "vendor": manifest.vendor,
                    "description": manifest.description,
                    "kind": manifest.kind,
                    "capabilities": manifest.capabilities,
                    "permissions": manifest.permissions,
                    "enabled": entry.enabled,
                    "error": entry.error,
                    "controls": manifest.contributes.device_view.as_ref()
                        .map(|view| view.controls.iter().map(|control| json!({
                            "id": control.id,
                            "label": control.label,
                            "kind": control.kind,
                            "icon": control.icon,
                            "row": control.row,
                            "column": control.column,
                        })).collect::<Vec<_>>())
                        .unwrap_or_default(),
                    "actions": manifest.contributes.actions.iter().map(|action| json!({
                        "id": action.id,
                        "label": action.label,
                        "parameters": action.parameters.iter().map(|parameter| json!({
                            "name": parameter.name,
                            "label": parameter.label,
                            "required": parameter.required,
                            "max_length": parameter.max_length,
                        })).collect::<Vec<_>>(),
                    })).collect::<Vec<_>>(),
                })
            }).collect::<Vec<_>>(),
        })
    }
}

#[allow(clippy::too_many_arguments)]
fn manifest(
    id: &str,
    name: &str,
    version: &str,
    vendor: &str,
    description: &str,
    kind: Vec<PluginKind>,
    capabilities: Vec<Capability>,
    permissions: Vec<&str>,
    contributes: Contributions,
) -> PluginManifest {
    PluginManifest {
        schema_version: crate::plugin_api::SCHEMA_VERSION,
        id: id.into(),
        name: name.into(),
        version: version.into(),
        vendor: vendor.into(),
        description: description.into(),
        kind,
        capabilities,
        permissions: permissions.into_iter().map(str::to_string).collect(),
        contributes,
        requires: Requirements::default(),
    }
}

fn control(id: &str, label: &str, kind: ControlKind, icon: Option<&str>, row: u8) -> DeviceControl {
    DeviceControl {
        id: id.into(),
        label: label.into(),
        kind,
        icon: icon.map(str::to_string),
        row,
        column: 0,
    }
}

fn builtins() -> Vec<PluginManifest> {
    let au05 = manifest(
        "ulanzi.au05",
        "Ulanzi Vibe Key",
        env!("CARGO_PKG_VERSION"),
        "Ulanzi",
        "3 buttons, a knob and an omnidirectional microphone over a 2.4G receiver.",
        vec![PluginKind::Device],
        vec![
            Capability::DeviceEnumerate,
            Capability::DeviceHidVendor,
            Capability::FirmwareReadback,
            Capability::MicrophoneControl,
        ],
        vec!["hid:fff1:00dd"],
        Contributions {
            device_view: Some(DeviceView {
                controls: vec![
                    control("button1", "语音键", ControlKind::Button, Some("mic"), 0),
                    control("button2", "确认键", ControlKind::Button, Some("check"), 1),
                    control("button3", "取消键", ControlKind::Button, Some("close"), 2),
                    control("knob", "旋钮", ControlKind::Knob, None, 3),
                ],
            }),
            ..Contributions::default()
        },
    );

    let gamepad = manifest(
        "sony.dualsense",
        "Sony DualSense",
        "0.1.0",
        "Sony",
        "Generic HID gamepad buttons and sticks. A controller appears here when Windows enumerates it.",
        vec![PluginKind::Device],
        vec![Capability::DeviceEnumerate],
        vec!["hid:054c"],
        Contributions {
            device_view: Some(DeviceView {
                controls: Vec::new(),
            }),
            ..Contributions::default()
        },
    );

    let codex = manifest(
        "openai.codex",
        "Codex",
        "0.1.0",
        "OpenAI",
        "Launch or focus Codex and send its voice or command shortcut.",
        vec![PluginKind::Integration, PluginKind::ActionProvider],
        vec![
            Capability::AppDiscover,
            Capability::AppLaunch,
            Capability::InputInject,
            Capability::WindowForeground,
        ],
        vec![],
        Contributions {
            actions: vec![
                ActionDescriptor {
                    id: "voice".into(),
                    label: "切换 Codex 语音".into(),
                    parameters: Vec::new(),
                },
                ActionDescriptor {
                    id: "commands".into(),
                    label: "打开命令面板".into(),
                    parameters: Vec::new(),
                },
            ],
            profiles: vec![ProfilePreset {
                id: "codex.voice".into(),
                name: "Codex 语音".into(),
                plugin: "openai.codex".into(),
                bindings: Vec::new(),
            }],
            ..Contributions::default()
        },
    );

    let dsh = manifest(
        "deepseek.harness",
        "DeepSeek Harness",
        "0.1.0",
        "DeepSeek",
        "Focus the local DeepSeek Harness workspace and start its voice or command action.",
        vec![PluginKind::Integration, PluginKind::ActionProvider],
        vec![
            Capability::AppDiscover,
            Capability::AppLaunch,
            Capability::InputInject,
            Capability::WindowForeground,
        ],
        vec![],
        Contributions {
            actions: vec![ActionDescriptor {
                id: "workspace".into(),
                label: "打开 DSH 工作台".into(),
                parameters: Vec::new(),
            }],
            ..Contributions::default()
        },
    );

    let wechat = manifest(
        "tencent.wechat-input",
        "微信输入法",
        "0.1.0",
        "Tencent",
        "Open the WeChat Input voice panel with its firmware shortcut.",
        vec![PluginKind::Integration, PluginKind::ActionProvider],
        vec![Capability::AppDiscover, Capability::InputInject],
        vec![],
        Contributions {
            actions: vec![ActionDescriptor {
                id: "voice".into(),
                label: "微信语音".into(),
                parameters: Vec::new(),
            }],
            ..Contributions::default()
        },
    );

    let workbuddy = manifest(
        "workbuddy.desktop",
        "WorkBuddy",
        "0.1.0",
        "WorkBuddy",
        "Launch WorkBuddy and start its voice session.",
        vec![PluginKind::Integration, PluginKind::ActionProvider],
        vec![Capability::AppDiscover, Capability::AppLaunch],
        vec![],
        Contributions {
            actions: vec![ActionDescriptor {
                id: "voice".into(),
                label: "开始 WorkBuddy 语音".into(),
                parameters: Vec::new(),
            }],
            ..Contributions::default()
        },
    );

    let commands = manifest(
        "vibdock.commands",
        "脚本与命令",
        env!("CARGO_PKG_VERSION"),
        "VibDock",
        "Structured local commands with declared parameters; executable and argv only.",
        vec![PluginKind::ActionProvider],
        vec![Capability::CommandRun],
        vec![],
        Contributions::default(),
    );

    vec![au05, gamepad, codex, dsh, wechat, workbuddy, commands]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_are_valid_and_au05_has_a_device_view() {
        let registry = Registry::start();
        assert_eq!(registry.entries.len(), 7);
        assert!(registry.entries.iter().all(|entry| entry.enabled));
        let view = registry.view();
        let plugins = view["plugins"].as_array().unwrap();
        let au05 = plugins
            .iter()
            .find(|plugin| plugin["id"] == "ulanzi.au05")
            .unwrap();
        assert_eq!(au05["kind"][0], "device");
        assert_eq!(au05["controls"].as_array().unwrap().len(), 4);
        assert_eq!(au05["permissions"][0], "hid:fff1:00dd");
    }

    #[test]
    fn action_providers_round_trip_through_the_view() {
        let registry = Registry::start();
        let view = registry.view();
        let codex = view["plugins"]
            .as_array()
            .unwrap()
            .iter()
            .find(|plugin| plugin["id"] == "openai.codex")
            .unwrap();
        assert_eq!(codex["actions"][0]["id"], "voice");
        assert_eq!(
            codex["actions"][0]["parameters"].as_array().unwrap().len(),
            0
        );
    }
}
