# VibDock agent skill

Use this skill when an agent needs to inspect or configure the local VibDock
service: the Ulanzi AU05 Vibe Key, saved actions, profiles, gamepads, or the
Virtual Pad.

VibDock is a local-first Windows service with an authenticated loopback API. The
physical device, global hotkeys and process actions are executed by the Rust
service; the browser and agent never inject input directly.

## Safety rules

- Never print, log or commit the service bearer token.
- Do not expose the loopback service on a network interface without an explicit
  user request and a pairing/security review.
- Do not create arbitrary shell commands. Saved actions use structured steps;
  command steps use a local `commands.json` definition and separate argv values.
- A successful API response does not prove the target application executed an
  action. Report `DONE`/`DISPATCHED` as VibDock state, not application state.
- Ask before mutating device firmware, deleting bindings, or writing a direct
  chord. These can change what the physical buttons do.
- If a write fails, do not silently fall back to another microphone or device.

## Discovery

Read the last service state from:

```powershell
$state = Get-Content -Raw (Join-Path $env:LOCALAPPDATA 'VibDock\service.json') | ConvertFrom-Json
$base = "http://127.0.0.1:$($state.port)"
$headers = @{ Authorization = "Bearer $($state.token)" }
```

If the file is missing, start the service first:

```powershell
vibdock service-start
```

Use the helper script for read-only JSON calls:

```powershell
.\scripts\vibdock-agent.ps1 plugins
.\scripts\vibdock-agent.ps1 actions
.\scripts\vibdock-agent.ps1 action-commands
.\scripts\vibdock-agent.ps1 hardware
.\scripts\vibdock-agent.ps1 pad-execute -Id <saved-action-id>
```

## CLI

The console CLI is appropriate for setup and health checks:

```powershell
vibdock service-status
vibdock hardware-status --json
vibdock shortcut-apps --json
vibdock shortcut-list --json
vibdock shortcut-add --app "<app name>" --trigger "Ctrl + Alt + Shift + F10" --dispatch "Ctrl + Shift + Space" --device-button 2
vibdock hardware-bind --button 3 --chord "Escape" --label "取消"
vibdock hardware-unbind --button 3
```

`hardware-bind` writes firmware and verifies the slot. It requires the device to
be connected; a missing device is an error, not a request to use another
keyboard.

## API

All routes require the bearer token and same-origin checks. The helper script
adds both the token and the loopback host; use `Invoke-RestMethod` directly only
when the helper does not cover the route.

| Route | Use |
| --- | --- |
| `GET /api/plugins` | built-in plugin manifests, capabilities, controls and actions |
| `GET /api/actions` | saved action bindings and revisions |
| `GET /api/actions/commands` | sanitized local command definitions |
| `POST /api/actions/add` | create a saved action from structured steps |
| `POST /api/actions/remove` | remove a saved action by id |
| `POST /api/actions/sync` | rewrite AU05 action triggers |
| `POST /api/pad/execute` | execute one **saved** action id from a Pad/virtual key |

`POST /api/pad/execute` accepts only `{"id":"<saved id>"}`. It cannot create an
action or submit a step list. Use the normal action API to create or edit.

## Workflows

### Inspect what is available

1. `vibdock hardware-status --json` or `.\scripts\vibdock-agent.ps1 hardware`.
2. `.\scripts\vibdock-agent.ps1 plugins`.
3. Report device connection, firmware state, controls and plugin actions.
4. Do not claim a physical press occurred unless a recent event exists.

### Bind a physical button directly

1. Confirm the user wants to overwrite that button's current firmware behavior.
2. Use `vibdock hardware-bind --button N --chord "<chord>" --label "<label>"`.
3. Read back with `vibdock hardware-status --json`; report `provisioned` and the
   exact readback, or the error.
4. Remember that a direct chord is emitted by firmware; VibDock may not see a
   recent event for it.

### Run a saved action from a virtual key

1. List saved actions with `.\scripts\vibdock-agent.ps1 actions`.
2. Pick a saved id; never invent steps.
3. Call `.\scripts\vibdock-agent.ps1 pad-execute -Id <id>`.
4. Report the returned `DONE`/`FAILED` and `detail`, and verify the recent event
   if needed.

### Change a saved action

Use the authenticated action API or Settings UI. Before adding:
- resolve applications from `vibdock shortcut-apps`, do not guess paths;
- use structured step types (`launch`, `send_keys`, `toggle_microphone`,
  `command`, `wait`);
- keep 1–8 steps and prefer explicit device ids;
- expect 409 on a button/trigger/action conflict and surface the error.

## Current limitations

- Gesture/multi-button recognition classifiers exist, but the Windows
  sentinel-key monitor and persisted gesture bindings are not wired yet.
- The gamepad adapter is present; no live gamepad was detected on the acceptance
  host, so do not claim gamepad acceptance without a device.
- `/pad` page, pairing and profile projection are not implemented; only
  `/api/pad/execute` and the design prototype exist.
- Plugin extraction is partial: `vibdock-input`, `vibdock-au05`,
  `vibdock-device-au05`, `vibdock-device-gamepad` and `vibdock-plugin-api` are
  crates; the host/web/action modules are still in the root crate.
- The service binds loopback by default. A remote tablet requires an explicit
  network listener and pairing, which is not implemented.

## Verification

After an agent change:

```powershell
cargo fmt --all --check
cargo clippy --workspace --locked --all-targets -- -D warnings
cargo test --workspace --locked --all-targets
node scripts/test-binding-ui.cjs
```

For hardware changes, run the relevant checklist in
`docs/testing/au05-acceptance.md` or
`docs/testing/hardware-actions-acceptance.md`, and report what was actually
observed.

