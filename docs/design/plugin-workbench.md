# VibDock plugin workbench

Status: design proposal, 2026-10-19. The clickable prototype is
[`prototype-workbench.html`](prototype-workbench.html); it is a throwaway UI
exploration, not a production page.

## Why change

The current Settings page grew feature by feature: microphone shortcuts,
application sessions, per-application chords, AU05 vendor writes, actions and
gamepad bindings each added a section, an API route, a form, and a set of
conflict rules. That works for three built-in integrations, but every new app
(Codex, DeepSeek Harness, WorkBuddy, another keypad, a gamepad) repeats the same
work and makes the page longer and less coherent.

The user-facing problem is the same in miniature: a new user opens a page full
of forms and does not know which button to press first. The device is physical,
spatial and memorable; the settings page is a vertical document.

The proposal is to make **the device the navigation**, **the profile the unit of
configuration**, and **the integration a plugin**.

## Decision: borrow the DSH plugin shape, not its runtime

DeepSeek Harness/Cordis is a good reference for the plugin contract:

- a plugin is a package with a manifest, a version, declared dependencies and a
  lifecycle;
- the host owns services (config, logs, events, UI slots) and injects only what
  the plugin declares;
- install/enable/disable/config is a first-class operation;
- the UI is composed from plugin-contributed pages/slots rather than one
  monolith.

VibDock should borrow those ideas, but **not embed the DSH/Cordis JavaScript
runtime**. VibDock is a native Rust resident service that owns HID, global
hotkeys, audio sessions and process launching. Adding a Node runtime would add
startup cost, a second security boundary and a large dependency for no gain.
Instead:

- manifests and configuration are JSON;
- plugin logic in v1 is native Rust behind a small `Plugin` trait (built-in
  plugins), plus a declarative manifest for generated UI;
- third-party plugins can later run in a sandboxed host (WASM or a separate
  process) if demand exists;
- a future `dsh.bridge` plugin can talk to DSH over its local HTTP/MCP surface
  without putting DSH inside VibDock.

This keeps one privileged process and one set of audited capabilities.

## Host / plugin contract

```
┌──────────────────────────── VibDock host ────────────────────────────┐
│ auth · config store · profile engine · action executor · UI shell     │
│ device manager · hotkey manager · event log · plugin registry         │
└───────────────┬───────────────────────────────┬───────────────────────┘
                │ manifest + capabilities       │ device/config/action APIs
      ┌─────────▼─────────┐           ┌─────────▼─────────┐
      │ ulanzi.au05       │           │ codex             │
      │ device plugin     │           │ integration       │
      └───────────────────┘           └───────────────────┘
      ┌───────────────────┐           ┌───────────────────┐
      │ gamepad.hid       │           │ dsh / workbuddy   │
      └───────────────────┘           └───────────────────┘
      ┌───────────────────┐
      │ scripts / commands│
      └───────────────────┘
```

### Manifest

A plugin directory contains `plugin.json` plus optional icons/docs:

```json
{
  "schema_version": 1,
  "id": "ulanzi.au05",
  "name": "Ulanzi Vibe Key",
  "version": "1.4.0",
  "kind": ["device"],
  "vendor": "Ulanzi",
  "description": "3 buttons, knob and microphone over the 2.4G receiver.",
  "capabilities": [
    "device.enumerate",
    "device.hid.vendor:fff1:00dd",
    "input.button-map",
    "firmware.readback"
  ],
  "requires": { "core": "^1.0" },
  "contributes": {
    "device_views": ["ui/device.json"],
    "profiles": ["ui/profiles.json"],
    "config_schema": "ui/config.schema.json"
  },
  "permissions": []
}
```

An integration plugin declares actions, not shell commands:

```json
{
  "id": "codex",
  "kind": ["integration", "action-provider"],
  "capabilities": ["app.discover", "app.launch", "input.inject", "window.foreground"],
  "contributes": {
    "config_schema": "ui/codex.schema.json",
    "actions": [
      { "id": "voice.toggle", "label": "切换 Codex 语音", "params": [] },
      { "id": "command.palette", "label": "打开命令面板", "params": [] }
    ],
    "profiles": [
      {
        "id": "codex.voice",
        "name": "Codex 语音",
        "steps": [
          { "action": "app.launch", "target": "codex" },
          { "action": "wait", "ms": 400 },
          { "action": "microphone.toggle" },
          { "action": "codex.voice.toggle" }
        ]
      }
    ]
  }
}
```

The host validates the manifest, resolves only known capability names, and
rejects unknown fields rather than silently ignoring them.

### Capability and trust model

Capabilities are the security boundary; a manifest alone cannot launch a process
or inject input. The host maps capabilities to audited implementations:

| Capability | Who can have it | What it allows |
| --- | --- | --- |
| `device.enumerate` | built-in / signed device plugins | list HID interfaces |
| `device.hid.vendor:vid:pid` | AU05-like device plugins | open the vendor collection |
| `firmware.readback` | device plugins | write and verify device slots |
| `app.discover` | integration plugins | read local application inventory |
| `app.launch` | integration plugins | launch a server-resolved executable |
| `input.inject` | integration plugins | send a chord to the foreground app |
| `window.foreground` | integration plugins | read the foreground window identity |
| `command.run` | action-provider plugins | run a declared local command |

The first plugin release should be built-in and shipped with the app. A local
plugin folder is an advanced option. Remote marketplace installation needs
manifest signing, a review process and a sandbox; do not expose arbitrary shell
or arbitrary UI HTML before that exists. Browser configuration remains
authenticated and origin-checked, exactly as today.

### Device support today

- **Ulanzi AU05** goes through its vendor HID collection and keeps the current
  direct-chord/action behavior. It is the reference device view.
- **Sony white controller** needs no bespoke integration: the standard HID
  gamepad collection carries its buttons and sticks, so the existing generic
  gamepad plugin can list it once Windows enumerates it (DualSense and
  DualShock 4 both expose a standard gamepad collection). On the acceptance
  host Windows currently sees only a Sony WH-1000XM5 headset (`054C:0DF0`), not
  a controller; the workbench therefore shows the Sony card as
  `未连接 / 等待 USB 或蓝牙`, not as a broken plugin. Connecting the controller
  is a device-state question, not an install question.
- A future second keypad contributes a new device view file; it does not add a
  new settings section.

### Profiles

A profile is the user-facing unit and the only thing most users edit:

```json
{
  "id": "codex.voice",
  "name": "Codex 语音",
  "plugin": "codex",
  "bindings": [
    {
      "input": { "device": "ulanzi.au05", "control": "button2" },
      "steps": [
        { "action": "app.launch", "target": "codex" },
        { "action": "wait", "ms": 400 },
        { "action": "microphone.toggle" },
        { "action": "input.send", "keys": "Ctrl + Shift + Space" }
      ]
    }
  ]
}
```

- A profile can be assigned to one or more physical controls.
- A plugin can ship default profiles; the user can clone and edit them.
- Profiles export/import as JSON and are portable between devices.
- The same profile can be shown as a list item, a card, or a patch cable; the
  data model does not depend on the layout.

### UI composition

The host provides primitives; plugins contribute data, not untrusted HTML:

- **Device view**: a JSON description of controls (`id`, label, icon, geometry
  or named slots), so the host can render the device illustration and clickable
  hotspots consistently. The AU05 reference lives in the host as a stylized
  drawing; a second keypad adds a different view file, not a new page.
- **Config schema**: JSON Schema subset rendered by the host form renderer
  (text, select, toggle, path picker, shortcut capture). No plugin-authored
  JavaScript in the privileged page.
- **Actions**: named actions with typed parameters, displayed as step choices.
- **Profiles**: default presets contributed by plugins.
- **Health**: the plugin reports connected/needs-attention, and the device card
  shows one status sentence instead of many raw fields.

If a future plugin needs a custom visual, it can contribute a sandboxed iframe
page with a narrow postMessage API (read config, propose profile). That is a
later phase and never runs in the privileged origin.

## The redesigned workbench

Three directions were prototyped. They share data and components, but disagree
about hierarchy:

| Variant | Focal point | Best at |
| --- | --- | --- |
| **A · 设备画布** | Large device with clickable keys | first-time setup, spatial memory, showing what is physically available |
| **B · 信号链** | Key → plugin → action chain | explaining what a plugin contributes, composable actions |
| **C · Profile 面板** | Profile cards | managing many profiles, switching context, seeing assignments at a glance |

Recommendation: **A as the default**, with C's profile deck and B's chain as the
inspector for a selected key. Concretely:

1. **Device-first home.** The AU05 is drawn to scale with three keys, knob and
   mic grille. Clicking a key selects it; clicking the knob switches the active
   profile. No separate "hardware" and "actions" cards.
2. **Profile deck.** Under the device, a horizontal deck of profile cards. Click
   a card to assign it to the selected key; the card shows the plugin vendor and
   a compact step summary. This replaces the empty "new binding" form.
3. **Inspector, not another page.** The right panel shows the selected control:
   plugin, profile, ordered steps, test, save. Advanced settings (firmware
   direct chord, raw HID config) are secondary tabs in the same inspector.
4. **Auto-setup first.** On connect, detect running applications and installed
   plugins and offer a one-click layout: "语音键 → 微信语音, 确认键 → Codex,
   取消键 → DSH". The user confirms instead of starting from a blank form.
5. **Plugin directory.** A drawer lists device and application plugins with
   one-line capability summaries and install/disable. A plugin's detail page
   shows exactly what it can read or do, plus its provided profiles.
6. **One state language.** Connected / needs attention / not installed /
   readback verified are consistent tags across devices, plugins and profiles.
   Never show a green "done" for a write that was not read back.

The prototype also covers the gamepad empty state ("Windows 当前未检测到设备"),
so the layout does not pretend a plugin is working when no device is present.

## Migration plan

**Phase 0 — prototype (this change).** Static, no persistence; choose variants.

**Phase 1 — host shell + AU05 plugin.** Introduce `plugin.json` for the built-in
AU05 support, a profile store, and the workbench shell. Keep existing APIs for
compatibility; the new UI reads the plugin/profile view.

**Phase 2 — application plugins.** Move Codex, DeepSeek Harness, WorkBuddy and
WeChat Input to manifests that declare launch/edit/voice actions and default
profiles. Command definitions become an action-provider plugin.

**Phase 3 — gamepad plugin.** The existing HID monitor becomes a device plugin
with a device view and learnable buttons.

**Phase 4 — third-party plugins.** Local plugin folder, manifest signing,
sandboxed runtime, marketplace metadata. Only after the capability boundary and
review process have tests.

## Non-goals

- No remote code execution, no arbitrary shell strings, no plugin-authored
  privileged JavaScript.
- No silent fallback to another microphone or another device.
- No claim that a dispatched chord started an application or a voice session.
- No new release claim from this design or its prototype.

## Acceptance for phase 1

- The same AU05 features (default layout, direct chords, app bindings, actions)
  work through the workbench shell and the legacy APIs.
- A profile assignment writes atomically, reports firmware readback separately,
  and survives restart.
- Plugin manifest validation rejects unknown capabilities and missing fields.
- The device view is keyboard-operable; every control has a focus state and an
  accessible name.
- The page remains usable with no device, no gamepad, no plugins installed, and
  in read-only preview mode (no authenticated calls).
