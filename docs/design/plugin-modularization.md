# Plugin modularization

Status: architecture proposal plus the first extraction, 2026-10-19. The tree
already builds these crates:

- `vibdock-input` — hotkey parsing, gesture classifier, multi-button matcher;
- `vibdock-au05` — pure Ulanzi AU05 wire protocol and keymap;
- `vibdock-plugin-api` — manifest, capability and plugin trait contracts.

The Windows adapter, action executor, web routes and profiles still live in the
root crate. This document is explicit about what is done and what is not.

## The problem

The repository is currently one library crate with 30+ modules. Device identity
is still hard-coded in places:

- `src/hardware.rs` knows `au05::VENDOR_ID`, the report layout, the three button
  slots, the HID interface lookup, and the direct-chord/action conflict rules;
- `src/web.rs` has AU05-specific routes and response fields (`buttons`,
  `direct_chord`, `hardware`), plus gamepad-specific routes;
- `src/actions.rs` gives AU05 a generated hotkey and writes firmware;
- `src/app_shortcuts.rs` asks `crate::au05::hotkey_entries` whether a device
  button can carry a trigger.

Adding a second keypad or a new integration therefore touches the same files,
the same UI and the same tests. A plugin model needs boundaries that exist in
the type system, not only in naming.

## Target crate graph

```
vibdock-cli / vibdock-launcher          thin binaries
        │
        ▼
vibdock-core (host kernel)              service lifecycle, auth, config store,
  ├─ vibdock-plugin-api                 profile engine, action executor,
  ├─ vibdock-input                      plugin registry, API routes
  ├─ vibdock-device-au05                AU05 HID + firmware adapter
  ├─ vibdock-device-gamepad             generic HID gamepad adapter
  ├─ vibdock-integration-codex          launch / focus / voice actions
  ├─ vibdock-integration-dsh            DSH discovery + voice actions
  ├─ vibdock-integration-wechat         WeChat Input voice action
  ├─ vibdock-integration-workbuddy      WorkBuddy actions
  └─ vibdock-action-commands            local structured commands
        │
        ▼
vibdock-web                            authenticated loopback HTTP + static UI
vibdock-platform-windows               HID, hotkey hook, SendInput, audio
```

Rules:

1. Pure crates (`vibdock-input`, `vibdock-au05`, `vibdock-plugin-api`) have no
   Windows, HID, audio, process or web dependencies and run their tests on every
   build host.
2. Device crates own one protocol/device family and expose press/release events
   plus a firmware/configuration surface. They do not know about profiles or the
   web UI.
3. Integration crates expose declarative actions and discovery. They may use
   capability-scoped host services (launch, inject, read foreground window) but
   never the raw service/config APIs.
4. Only `vibdock-core` owns authentication, configuration mutation, the action
   executor and the plugin registry.
5. `vibdock-web` renders the host UI and schema-driven plugin forms. It does not
   contain device-specific branches.

## Crate responsibilities

| Crate | Owns | Does not own | Status |
| --- | --- | --- | --- |
| `vibdock-input` | hotkey parse/normalize, single/double/long classifier, 2–3 button sequence/hold matcher | HID, hotkey registration, actions | extracted |
| `vibdock-au05` | vendor frame format, TEA, shortcut/fixed-function payloads, keymap, readback parsing | opening HID, persistence, defaults | extracted |
| `vibdock-plugin-api` | manifest schema, capability enum, device view, profile preset, action descriptor, plugin/host traits | implementation, OS access | new |
| `vibdock-device-au05` | HID enumeration/open, vendor frame I/O, provision/readback, device identity | profiles, web, actions | next |
| `vibdock-device-gamepad` | generic gamepad enumeration and button edges | profiles, web | next |
| `vibdock-integration-*` | app discovery/launch, default profiles, plugin-specific actions | config store, auth | next |
| `vibdock-core` | plugin registry, profile store, action executor, capabilities, service | device protocols, UI layout | next |
| `vibdock-web` | HTTP routes, auth boundary, schema form renderer, workbench shell | HID, firmware | next |
| `vibdock-cli` / `vibdock-launcher` | argument parsing, tray/launcher wiring | business logic | later |

The root package stays as a compatibility shell while the crates are extracted;
its binaries keep working and the existing public paths are re-exported. The
goal is to move code, not to rewrite behavior in one step.

## Plugin contract

`vibdock-plugin-api` is data-first. A package contains `plugin.json`:

```json
{
  "schema_version": 1,
  "id": "ulanzi.au05",
  "name": "Ulanzi Vibe Key",
  "version": "1.4.0",
  "vendor": "Ulanzi",
  "kind": ["device"],
  "capabilities": ["device_enumerate", "device_hid_vendor", "firmware_readback"],
  "permissions": ["hid:fff1:00dd"],
  "contributes": {
    "device_view": {
      "controls": [
        { "id": "button1", "label": "语音键", "kind": "button", "icon": "mic", "row": 0, "column": 0 },
        { "id": "knob", "label": "旋钮", "kind": "knob", "row": 1, "column": 0 }
      ]
    },
    "config_schema": { "type": "object", "properties": {} },
    "profiles": [],
    "actions": []
  }
}
```

The host validates `schema_version`, id, non-empty fields, kinds, duplicate
capabilities, device-view controls and device-plugin/view consistency before
enabling anything. A capability is a closed enum; `permissions` carry the
device-specific details (`hid:fff1:00dd`). Unknown capabilities are rejected,
not ignored.

The runtime trait surface is intentionally small:

```rust
pub trait Plugin: Send + Sync {
    fn manifest(&self) -> &PluginManifest;
}

pub trait DevicePlugin: Plugin {
    fn poll(&mut self, host: &mut dyn HostServices) -> Result<Vec<DeviceEvent>, PluginError>;
}

pub trait ActionProvider: Plugin {
    fn actions(&self) -> &[ActionDescriptor];
}
```

`DeviceEvent` carries `device_key`, `control`, `pressed` and a monotonic
timestamp. That is exactly what `vibdock-input` needs, so a new device plugin
does not reimplement gesture/combo timing.

## Built-in versus third-party

**Built-in plugins** are workspace crates compiled into the host. They can use
the internal `vibdock-platform-windows` crate and are reviewed with the core.
This is how AU05, the gamepad, Codex, DSH, WeChat Input and WorkBuddy should
ship.

**Third-party plugins** should run **out of process**, not be loaded as Rust
dynamic libraries:

- Rust has no stable ABI; `libloading` + `dyn Trait` breaks across compiler and
  dependency versions.
- A sidecar can be written in any language, crashes without taking down the
  resident service, and is easier to sandbox.
- The contract is the same `plugin.json` plus JSON-RPC over stdio or a
  loopback-only authenticated local socket.
- The host owns capabilities and executes actions; the plugin proposes them and
  reports device events. It never receives the auth token or the raw config
  store.

A future WASM runtime is an alternative for pure logic, but it cannot directly
open HID. Device plugins need the sidecar/native route.

## Migration plan

**Step 1 — pure protocol and input (done).**

- Extracted `src/hotkey.rs`, `src/gesture.rs`, `src/combo.rs` into
  `crates/vibdock-input`.
- Extracted `src/au05.rs` into `crates/vibdock-au05`.
- The root crate re-exports `vibdock_input::hotkey` and aliases
  `vibdock_au05 as au05`, so existing `crate::hotkey` / `crate::au05` call sites
  did not need a flag-day rewrite.
- Added `crates/vibdock-plugin-api` with manifest validation and traits, and
  made it a host dependency.

**Step 2 — AU05 device crate.**

Move `src/hardware.rs`'s Windows `windows_impl` and persistence split into
`vibdock-device-au05`; move only the generic `DevicePlugin`/firmware interface
into the plugin API. The host keeps the binding tables and conflict rules.

**Step 3 — host kernel and registry.**

Extract `vibdock-core` (service, config, profiles, action executor, plugin
registry) and `vibdock-web` (routes, auth, form renderer) from the root. Add
`GET /api/plugins`, enable/disable and per-plugin config routes. Keep the
existing `/api/hardware` and `/api/actions` routes as compatibility adapters
during the migration.

**Step 4 — built-in integration plugins.**

Move Codex, DSH, WeChat Input and WorkBuddy into their own crates/manifests with
declared actions and default profiles. The commands layer becomes
`vibdock-action-commands`.

**Step 5 — gamepad and third-party sidecars.**

Extract `vibdock-device-gamepad` from `src/gamepad_windows.rs`; add the sidecar
protocol and a local plugin folder with manifest signing. Only then advertise a
plugin marketplace.

## Acceptance for the first extraction

- `cargo test --workspace --locked --all-targets` passes; the pure crates run
  their own tests without a Windows device.
- The root binaries and all existing integration tests behave exactly as before;
  no route, config file or acceptance claim changes.
- `cargo clippy --workspace --locked --all-targets -- -D warnings` passes.
- The AU05 protocol crate has no dependency on the host, service, web, audio or
  Windows crates.
- A manifest with an unknown capability, missing device view or duplicate
  control is rejected with a specific error.
- The plugin API crate has no dependency on the host or Windows crates.

## Non-goals for now

- No dynamic Rust plugin loading.
- No arbitrary HTML/JavaScript from a plugin in the privileged page.
- No plugin-authored shell strings; command steps remain structured argv.
- No claim that extracting crates makes third-party plugins safe. The sidecar,
  capability and signing work is a separate phase.
