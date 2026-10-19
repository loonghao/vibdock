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

### Gesture layer

Three physical keys should not mean three actions. Each input has up to three
gesture slots, so the same AU05 exposes nine logical controls:

| Gesture | Default timing | Semantics |
| --- | --- | --- |
| 单击 `single` | after release, after the double-click window | one press and release |
| 双击 `double` | second press within 300 ms | two presses, one action |
| 长按 `long` | held for 550 ms | fires at the threshold, not on release |

Timings are per profile or global user settings and are bounded (for example
double window 150–600 ms, long press 350–900 ms). The state machine is:

```
Idle ──press──▶ Held ──release before long──▶ AwaitingDouble ──timeout──▶ Single
                  │                              └──second press──▶ Double
                  └──held past long threshold──▶ Long (release is ignored)
```

Only one gesture is emitted per press sequence. Repeated key-down reports are
ignored; a second press after the double window becomes the start of a new
sequence. If a button has no double or long mapping, the host can fire `single`
immediately on release instead of waiting, so the feature does not add latency
to simple buttons.

**Physical detection.** The AU05 firmware stores one shortcut entry per button
and cannot classify gestures itself. The host therefore writes a unique
**sentinel** key (for example bare `F13`, `F14`, `F15`; the AU05 accepts them in
direct mode and normal applications ignore them) into a button that is in
gesture mode, and observes keyboard down/up events. A low-level keyboard hook
(`WH_KEYBOARD_LL`) or Raw Input provides press/release timing; the hook consumes
the sentinel so it is not typed into the foreground application.
`RegisterHotKey` is not sufficient: it reports a press, not a release, so it
cannot measure long-press or double-press reliably.

This is the same shape as OpenVibeKey's macOS "sentinel + delayed single click"
design, adapted to Windows input monitoring. The honest tradeoffs:

- **Single click is delayed** by the double-click window unless double is not
  configured. The UI must show this, and the timing control must be visible.
- **A button in gesture mode cannot also be a firmware direct chord.** The
  vendor slot holds the sentinel instead of `Ctrl + Win`, `Enter`, etc.
- **The WeChat voice default needs care.** We measured that an injected
  `Ctrl + Win` did not open WeChat Input voice while a firmware-emitted chord
  did. Moving button 1 to gesture mode would therefore trade away the reliable
  single-click voice path. The workbench should keep button 1 in direct mode by
  default and offer gestures on buttons 2/3, or let the user choose an explicit
  action for the single gesture (for example launch WeChat Input, then send a
  different chord).
- **Service availability.** Gesture detection requires the resident service. If
  it is not running, a sentinel button does nothing; the UI must not promise a
  fallback it cannot deliver.
- **Gamepad is simpler.** The existing HID monitor already yields press and
  release events, so the same classifier can run without sentinels or firmware
  writes; only the double-window delay applies.

The profile schema gains a gesture key:

```json
{
  "bindings": [
    {
      "input": { "device": "ulanzi.au05", "control": "button2", "gesture": "double" },
      "steps": [
        { "action": "app.launch", "target": "dsh" },
        { "action": "wait", "ms": 200 }
      ]
    }
  ]
}
```

A source may have at most one binding per gesture (three per physical control).
The host rejects a second binding for the same `(control, gesture)`, and a
button cannot mix gesture mode with a direct firmware chord. Switching a button
into gesture mode writes and verifies the sentinel; switching back restores the
direct chord and removes the sentinel registration.

The prototype shows this as three tabs (单击 / 双击 / 长按) in the inspector, a
`3/3` count on each key callout, timing sliders, and "9 个动作位" in the canvas
header. Variant C tags each profile with the exact key+gesture assignments.

### Multi-button combinations

Three buttons also form chords. The pattern set may contain any of these shapes:

| Pattern | Example | Recognized when |
| --- | --- | --- |
| `Sequence([1, 2])` | 1 then 2 quickly | both tapped inside the 250 ms chord window |
| `Sequence([1, 2, 3])` | 1, 2, 3 quickly | all three tapped inside the window |
| `Sequence([2, 3])` | 2 and 3 quickly | both tapped, either order |
| `Hold([1, 2])` | 1+2 held | both held together past 550 ms |
| `Hold([2, 3])` | 2+3 held | both held together past 550 ms |

Combinations are **unordered sets** for sequence purposes: `2→3` and `3→2`
both match `2+3`. Repeated presses of a control already in the current sequence
do not duplicate it. A sequence needs at least two distinct controls; a maximum
of three is enforced in the first release.

Resolution rules:

1. **Longer sequence wins.** If `1+2` and `1+2+3` both exist, releasing `1,2`
   waits for the chord window after the last press. Adding `3` fires `1+2+3`;
   otherwise the shorter `1+2` fires at the deadline.
2. **Hold beats tap for the same set.** If `1+2` is both a sequence and a hold,
   holding past the threshold fires the hold and the later releases are ignored;
   releasing before the threshold fires the sequence.
3. **A first tap waits only if it can still become a combo.** Pressing only `1`
   while `1+2` exists buffers until the chord window ends. If no combo starts
   with `1`, the single-key gesture can proceed without this extra wait. This is
   why the engine needs the configured pattern set, not just the raw events.
4. **One action per sequence.** After a sequence or hold fires, the remaining
   releases in that sequence are consumed. A new sequence starts only after all
   controls are released and the next press begins.
5. **Timeouts are bounded.** Chord window 80–1000 ms (default 250 ms), hold
   200–3000 ms (default 550 ms). A sequence press older than the window is
   discarded rather than matched late.

The AU05 sentinel approach extends directly: each active button is written with
a different sentinel key (`F13`, `F14`, `F15`), so a keyboard hook can tell
which button went down and when. The same hook feeds the single-key gesture
classifier and the multi-key matcher; the hook must not mistake the sentinels for
ordinary typing. Holding `1+2` means observing both sentinels held; tapping
`1,2,3` means three down/up pairs within the window. No firmware change is
needed beyond writing the three sentinels once.

If the service is not running, sentinel buttons produce nothing. Direct firmware
chords remain the fallback for users who do not need gestures, and the voice
button tradeoff described above still applies.

The prototype adds a 多键组合 deck with the exact patterns `1+2`, `1+2+3`,
`2+3`, and `1+2` hold. Each card shows the ordered key sequence, the pattern
kind (快按 / 长按), and the profile it executes.

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

The first three variants are intentionally exploratory; the dark instrument
panel, coloured plugin badges and card decks read as over-designed. The
**clean direction** strips those away and is the recommended visual target:
[`prototype-clean.html`](prototype-clean.html).

It uses a warm paper background, black type, one burnt-orange accent and no
gradients or glows. The device is a line drawing, gestures are three plain rows,
combos are a table, and the editor is a drawer that only appears when a row is
clicked. Plugins are a modal, not a permanently visible card list. There are no
fake vendor/version chips and no decoration that does not carry state.

Concretely:

1. **Device-first home, restrained.** A line-drawn AU05 with three clickable
   keys, a knob and mic grille. No glowing stage and no oversized illustration.
2. **Gesture rows, not cards.** Clicking 单击 / 双击 / 长按 opens the editor for
   that assignment. The row shows only the profile name and a one-line hint.
3. **Combo table.** `1+2`, `1+2+3`, `2+3`, `1+2 长按` are rows with sequence,
   kind and profile. No per-combo card.
4. **Drawer editor.** Profile select, the resulting steps, timing details and
   save. Everything else stays out of the way.
5. **Plugins as a modal.** Device and application plugins with connection state
   and settings. The main page is not a plugin catalogue.
6. **One accent, one state language.** Connected, unconfigured and unsupported
   are plain text/green/grey; only the selected key and primary action use the
   accent. A dispatched chord is never reported as verified.

The earlier A/B/C explorations remain in
[`prototype-workbench.html`](prototype-workbench.html) as information-architecture
references, not as the visual target. The clean prototype also shows the Sony
controller as an unconnected device instead of pretending it works.

## Implementation status

The platform-independent gesture classifier has landed as `src/gesture.rs` with
11 unit tests covering single, double, long, repeated down reports, window expiry,
reset and unusable timings. The multi-button matcher has landed as `src/combo.rs`
with 10 unit tests covering sequences, order independence, longer-sequence
precedence, holds, quick release, repeated presses and bounded timings. They are
not wired to the service yet: the Windows keyboard hook/sentinel path, persisted
gesture/combo bindings, profile UI and gamepad integration remain Phase 1 work.

## Migration plan

**Phase 0 — prototype (this change).** Static, no persistence; choose variants.

**Phase 1 — host shell + AU05 plugin + gestures.** Introduce `plugin.json` for
the built-in AU05 support, a profile store, the workbench shell, and the gesture
classifier + sentinel-key detection for AU05. Keep existing APIs for
compatibility; the new UI reads the plugin/profile view.

**Phase 2 — application plugins.** Move Codex, DeepSeek Harness, WorkBuddy and
WeChat Input to manifests that declare launch/edit/voice actions and default
profiles. Command definitions become an action-provider plugin.

**Phase 3 — gamepad plugin + gestures.** The existing HID monitor becomes a
device plugin with a device view and learnable buttons; its press/release events
feed the same gesture classifier, without sentinels or firmware writes.

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
- The gesture classifier is unit-tested for single, double, long, repeat
  suppression, double-window expiry, and long-then-release; the sentinel hook
  suppresses its own keys and a missing service never fabricates a gesture.
- The page remains usable with no device, no gamepad, no plugins installed, and
  in read-only preview mode (no authenticated calls).


