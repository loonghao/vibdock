# Hardware action sequences

Status: implemented in the current Windows source. Live evidence on 2026-10-19
is limited to one AU05 action binding (add → generated trigger → firmware write
and readback → remove, with the default direct layout restored). A physical
action press running the steps, live command execution, and any gamepad input are
**not** verified; no gamepad was detected by Windows on the acceptance host. The
dated record is the [hardware action acceptance](../testing/hardware-actions-acceptance.md).

## Model

A hardware action maps one physical control to an ordered list of 1-8 steps:

- **source** — an AU05 button (`0`-`2`) or a HID gamepad button (`device_key` +
  button usage);
- **steps** — `launch`, `send_keys`, `toggle_microphone`, `command`, `wait`;
- **trigger** — generated, registered and written into firmware for an AU05
  source; absent for a gamepad source, which the HID monitor feeds directly.

Up to 16 actions are saved. Actions are stored in `actions.json` (schema 1,
revision, bindings). Step execution happens only in the authenticated service
when the physical control fires; there is no HTTP endpoint that runs a saved
action.

The layer deliberately does not add AI application voice start/stop. A voice
workflow is composed from `launch` + `wait` + `send_keys` (optionally
`toggle_microphone`); application `start_voice`, `end_voice` and `input_routing`
remain `not_implemented`. A successful step is a process spawn or injected
input or a microphone readback, never proof that an application did what the user
wanted.

## Sources

### AU05 button

Saving an action for button `n`:

1. The service picks the first unused chord in
   `Ctrl + Alt + Shift + F1`-`F12` (canonical text). Used chords are the
   microphone trigger, application-binding triggers, direct firmware chords, and
   already saved action triggers. If all twelve are used, the save fails with
   `409`.
2. The trigger is registered with the Windows global hotkey table first, then
   the binding (including its trigger text) is persisted to `actions.json`. A
   registration failure returns `409` and nothing is saved; a persist failure
   returns `500` and restores the previous registrations.
3. The service then writes the trigger into the button's firmware with the
   normal heartbeat / fixed-function clear / shortcut write / readback sequence.
   The add response carries a top-level `hardware` object
   `{button, state, error}`. A write failure reports `state: "failed"` but the
   action remains saved and armed; a later `POST /api/actions/sync` retries.
4. Removing the binding unregisters the trigger and asks the device to clear the
   button slot. A clear failure is reported but does not roll back the removal.

The generated trigger is not checked against application *dispatch* chords. A
dispatch chord equal to an action trigger would be injected by the application
binding and also captured by VibDock's action registration, running the action.
Choose values that do not collide; this is an open conflict-check gap.

The hardware status object (`GET /api/hardware`) does **not** describe action
bindings. Per-button firmware state for actions is returned only by the action
add/remove/sync responses and the `armed` field in `GET /api/actions`.

### HID gamepad button

A gamepad binding stores `device_key`, button `usage`, and a display `label`. It
has no trigger and no firmware write; the HID monitor dispatches on a press edge.

Identity rules:

- `vvvv:pppp:serial` (lowercase hex) when the device exposes a non-empty serial;
  otherwise `vvvv:pppp:interface-path`.
- Long keys are truncated and tagged with a hash of the full value, so distinct
  interfaces do not collapse to the same text.
- Identical units that no serial distinguishes get a path-derived collision
  suffix. A key can change when a duplicate unit is attached or removed, so a
  binding for a serial-less device may not follow a replaced unit.

Button usages normally come from the HID Button page (`0x09`). The Generic
Desktop page (`0x01`) is accepted only as a fallback for descriptors that declare
button caps there; a device never mixes pages. The top-level collection must be
Generic Desktop page `0x01` with usage `0x04` (joystick), `0x05` (game pad), or
`0x08` (multi-axis controller).

The monitor opens up to 8 collections read-only with shared access, enumerates
at most 128 HID interfaces, keeps at most 128 declared button usages per device
(256 button caps), and polls with a 20 ms total budget, 10 ms per read, 32
reports per poll, and 8 reports per device. `refresh` runs about every two
seconds on the accept-loop thread, and each `/api/actions/gamepads` request also
refreshes the list. There is no background thread and no synthetic device: with
no controller attached the result is an empty list.

`latest_button` is per-device, timestamped in milliseconds since the Unix epoch,
and exists only for a UI "press a button to learn it" flow. It is not authority:
the binding is saved from the learned `device_key` + `usage`, and the page polls
`/api/actions/gamepads` only while the action editor has a gamepad source
selected.

## Steps

| Type | Request fields | Stored/returned fields | Execution | Success means |
| --- | --- | --- | --- | --- |
| `launch` | `application_reference` (opaque, resolved server-side), optional `label`, optional `args` | stored with `executable`; view returns `label` + `args` only | `std::process::Command::new(executable).args(args).spawn()` | the process started; `detail` includes its pid. The application is not verified as ready or focused |
| `send_keys` | `keys` | canonical chord text | parsed with the shared hotkey grammar and injected into the current foreground application | Windows accepted the input events; the application may ignore the chord |
| `toggle_microphone` | `device_id` | endpoint id | toggles that exact endpoint through the Windows audio adapter and reads the mute state back | a verified mute readback; affects every application using the endpoint |
| `command` | `command_id`, `parameters` object of strings | command id + supplied values | expands the local definition to executable + argv and spawns it | the process started; no shell is involved |
| `wait` | `ms` | `0`-`10000` | sleeps on the accept-loop thread | the wait elapsed |

A `launch` reference is resolved server-side from a fresh application scan; the
browser never supplies an executable path. The API additionally accepts optional
`args` (up to 16 entries, each at most 512 bytes, no control characters), which
are stored in `actions.json` and shown in the view; the Settings editor sends an
empty list. `send_keys` has no target-application check: it injects into
whatever is foreground, like the `running` application-shortcut scope.

`toggle_microphone` requires an explicit endpoint. Save-time validation only
checks that the id is a bounded string; a stale or unknown endpoint fails at
execution, when the action tries to read its mute state. There is no default
microphone fallback.

One action can compose a workflow, for example `launch Codex` → `wait 1000` →
`toggle_microphone` → `send_keys Ctrl + Shift + Space` → `launch WorkBuddy`.
That is still not voice integration: the chord goes to whatever is foreground,
the target's response is unverified, and the microphone toggle affects every
application using the endpoint. Steps are bounded to eight, so a composed
workflow must fit that budget.

## Execution semantics

- Steps run in order. The first failure stops the sequence and returns
  `step i/n (kind) failed: <reason>`; later steps do not run.
- Earlier steps are **not rolled back**. A failed launch after a successful
  microphone toggle leaves the microphone toggled.
- On success the result is the joined per-step `detail` strings (for example
  `launched Codex (pid 1234); waited 1000 ms`), which the page shows in
  最近触发 with `state: "DONE"`.
- Execution runs on the service accept-loop thread. `wait` blocks that thread,
  so up to eight waits can delay hotkey handling and API responses for up to 80
  seconds; launches and commands spawn and return. Gamepad polling shares the
  same thread.
- There is no execute endpoint and no per-step API; results are observable only
  through the recent-trigger log returned by `GET /api/hardware` and
  `GET /api/config`.

Recent-trigger events for this layer use `source: "action_binding"` for an AU05
button and `source: "gamepad"` for a gamepad button. `label` is the action name,
`trigger` is the generated trigger for an AU05 source and `null` for a gamepad
source, `dispatch` is `null`, `device_button` is the AU05 button or `null`, and
`detail` is the success detail. A successful run sets `state: "DONE"` with that
`detail`; on failure `state` is `null`, `error` carries the step failure, and
`detail` is `null`. Direct firmware chords still produce no log entry because no
host hotkey is involved.

## Local command definitions (`commands.json`)

Commands are local configuration, not request text. `commands.json` is read at
service start from the VibDock state directory; there is no API to create,
modify, or execute a command string. `GET /api/actions/commands` returns the
sanitized view: id, label, and parameter declarations. Executable paths and fixed
argv stay server-side.

Schema 1 shape:

```json
{
  "schema_version": 1,
  "revision": 0,
  "commands": [
    {
      "id": "notify",
      "label": "发送通知",
      "executable": "C:\\Tools\\vibdock\\notify.exe",
      "args": ["--title", "{title}", "--body", "{body}"],
      "parameters": [
        {"name": "title", "label": "标题", "required": true, "max_length": 60},
        {"name": "body", "label": "正文", "required": false, "max_length": 200}
      ]
    }
  ]
}
```

Bounds and rules:

- at most 32 commands, at most 16 argv entries per command, at most 512 bytes
  per fixed argv entry, no control characters;
- ids are 1-40 ASCII alphanumeric/`-`/`_`; labels are at most 80 bytes;
- at most 8 parameters; parameter names start with an ASCII letter and contain
  only letters, digits, or `_`; labels are at most 80 bytes; `max_length`
  defaults to 120 and is clamped to 1-256; `required` defaults to false;
- the executable must be a local path (`local_executable_path`);
- every `{name}` placeholder inside a fixed argv entry must name a declared
  parameter, otherwise the whole file is rejected and command execution is
  disabled.

At execution the service builds a fresh argv from the fixed entries, replacing
`{name}` with the supplied string value in each entry. Missing required values,
unknown supplied parameters, values over `max_length`, control characters, or an
unknown command id fail the step. Parameters are strings; there are no numeric or
boolean parameter types.

The executable is spawned directly with argv, never through a shell command
line. Pointing a command definition at a shell interpreter (`cmd.exe`,
`powershell.exe`, …) is possible only by editing the local file, and is then the
user's local configuration choice: that file is a trust boundary, not an
arbitrary-shell API exposed to the browser or the network.

If a command step references a command that later disappears or whose definition
becomes invalid, `actions.json` no longer loads: its original bytes are preserved
and action edits are disabled until the command table or the action file is
fixed. The same fail-closed behavior applies to a corrupt or unsupported
`commands.json`.

## Persistence, identity, and conflicts

`actions.json` (schema 1, revision, at most 16 bindings) stores, per binding:

- random 64-hex `id`, `name` (at most 80 bytes);
- `source` (`au05` button `0`-`2`, or `gamepad` `device_key` + `usage` +
  `label`);
- `trigger` (required for AU05, forbidden for gamepad);
- 1-8 stored steps (a launch step stores the resolved executable path; the API
  view redacts it).

Stale revisions return `409`, unknown fields `400`, unavailable storage `503`,
and malformed files preserve their bytes and disable edits. Registration happens
before persistence; a failed registration or persistence leaves the previous set
active.

Button ownership is exclusive across three modes:

- one AU05 button carries an application shortcut, a direct firmware chord, or a
  hardware action, never more than one;
- the action route refuses an app-bound or direct-chord button and reports
  `409`; adding an application binding or direct chord on an action button is
  refused by those routes;
- a gamepad button is intended to carry at most one action (same `device_key` +
  `usage`); see the duplicate-check gap below.

Action triggers are unique among the microphone trigger, application triggers,
direct chords, and other action triggers. Application *dispatch* chords are not
in that set (see the open gap above).

The gamepad duplicate check compares the whole stored source record, including
its display label. A hand-crafted API request that names the same `device_key`
and `usage` with a different label is therefore not rejected; the page prevents
that state, and execution resolves the first matching binding.

## API and UI

Authenticated routes, same bearer/`Host`/`Origin` rules as the other `/api`
routes:

| Route | Purpose |
| --- | --- |
| `GET /api/actions` | `schema_version`, `platform`, `revision`, `bindings`, `limit` (16), `dispatch_available`, `storage_writable`, `error` |
| `GET /api/actions/commands` | sanitized command definitions and `storage_writable`/`error` |
| `GET /api/actions/gamepads` | `{devices:[{device_key,name,vendor_id,product_id,button_usages,connected,latest_button}]}` |
| `POST /api/actions/add` | `{revision,name,source,steps}`; AU05 response adds `hardware:{button,state,error}` |
| `POST /api/actions/remove` | `{revision,id}`; a firmware-clear failure is reported in `hardware` |
| `POST /api/actions/sync` | rewrites every AU05 action trigger; returns `hardware:[{id,button,state,error}]` |

A binding view is `{id, name, source, trigger, steps, armed, error}`. `armed`
means the AU05 trigger is currently registered; gamepad bindings report armed
while the monitor runs. Steps are returned as `launch{type,label,args}`,
`send_keys{type,keys}`, `toggle_microphone{type,device_id}`,
`command{type,command_id,parameters}`, or `wait{type,ms}`.

The Settings card **硬件动作（按键 → 动作序列）** lists saved actions with a
step summary, opens an editor that chooses the source (AU05 button or learned
gamepad button), adds/reorders/deletes steps, selects a command and fills its
declared parameters, and saves/removes/syncs. A hardware write failure after save
is shown as a warning with a sync retry. The card is hidden in the read-only
design preview. There is no UI or API to create or edit command definitions.

## Failure and recovery

| Situation | What happens | What the user can do |
| --- | --- | --- |
| No unused generated trigger | add returns `409` "No unused Ctrl+Alt+Shift+F1-F12 trigger remains" | Remove another AU05 action binding or use a gamepad source |
| Trigger registration fails | add returns `409` with the binding name and reason; nothing is saved | Choose another trigger or free the conflicting registration |
| Firmware write fails after save | The action is saved and armed; add response has `hardware.state = "failed"` | Press an AU05 key and retry with 同步到设备 / `POST /api/actions/sync` |
| Device absent on add/sync | Binding persists; hardware result is `failed` | Reconnect the device, wake it, sync |
| Action removed, device absent | The binding is removed; the firmware slot may not be cleared | Sync with the device present, or write a different chord |
| `actions.json` or `commands.json` invalid | Original bytes preserved; action edits return `503`; a broken command reference disables actions until fixed | Fix the file while the service is stopped, or remove it to start empty |
| Step launch/command spawn fails | Execution stops and reports the failing step and OS error | Fix the executable/path or the command definition, then retry |
| `send_keys` focus changes | Execution stops with a "foreground window changed" error after the chord may already have been sent | Check the foreground application; no rollback |
| `toggle_microphone` id is stale | Execution stops; the reported error says the endpoint could not be used | Re-select the microphone in the action |
| `wait` is used heavily | The service loop pauses for the waits, delaying hotkeys and API responses | Keep waits short; avoid long chains |
| Button claimed by another mode | Add returns `409` naming the conflict | Clear the application shortcut or direct chord first |

## Security and privacy

- Steps are typed and validated. There is no shell-string field, no API that
  submits command text, and no HTTP route that executes a binding.
- `launch` uses an opaque application reference resolved server-side; the browser
  never supplies an executable path. Optional launch `args` are client-supplied
  argv entries for that resolved executable and are visible in the saved view.
- `command` definitions come only from the local `commands.json`. The service
  never concatenates a command line: fixed argv entries and substituted values
  are separate `Command` arguments. Editing that file is the local trust
  boundary; a user can point an entry at a shell interpreter.
- `toggle_microphone` requires an explicit saved endpoint and verifies the mute
  readback; there is no default-device fallback.
- All `/api/actions` routes require the bearer token and pass the same `Host`/
  `Origin` checks as the other `/api` routes. The page hides the action card in
  the read-only preview.
- The gamepad adapter opens HID collections read-only with shared access and
  never starts a thread or fabricates a device.
- Recent action events include the action name, generated trigger, success
  detail (including launch pids), and error text; they are memory-only, bounded,
  and cleared on service restart. Direct firmware chords remain invisible.
- `commands.json` contains local executable paths and must not be attached to a
  public bug report. `actions.json` stores resolved executable paths for launch
  steps and is also personal data.

## Limits and open acceptance

Verified on 2026-10-19:

- one AU05 action binding with a `wait` step was added through the authenticated
  API, received `Ctrl + Alt + Shift + F1`, was written into the firmware and read
  back (`provisioned`), then removed; the default direct layout was restored;
- a physical press of that action-bound button executed the step and appeared in
  `recent_actions` as `source: "action_binding"`, `state: "DONE"`, and
  `detail: "waited 120 ms"`. The first attempts produced no host keys until the
  2.4G receiver was replugged; the vendor configuration channel had stayed
  responsive, which matches the known keyboard-forward-channel freeze on this
  dongle;
- the action UI scripts pass `node --check`, the existing DOM test, and a DOM
  harness for action flows (not a physical pointer run).

Not verified:

- the full launch → send keys → toggle microphone chain on hardware and its
  stop-at-first-failure behavior;
- any live gamepad input, learning, or gamepad action (Windows detected no
  gamepad on the acceptance host; the adapter has compilation and unit-test
  coverage only);
- live command execution with substituted arguments;
- wait timing and service-loop blocking under a real press;
- sleep/wake recovery, Ulanzi Studio coexistence, and the default-layout
  interaction when an action binding already exists;
- hosted CI with hardware: CI has no AU05 and no gamepad, so only protocol,
  validation, persistence, and page code are covered.

Also out of scope: AI application voice start/stop, application input routing,
arbitrary command or shell APIs, the AU05 knob/lighting/standby settings,
macOS, and Linux. The action layer never claims that a launched application is
ready, that a chord was accepted, or that a microphone toggle achieved a
particular voice state.

## References

- [Hardware action acceptance](../testing/hardware-actions-acceptance.md)
- [AU05 direct bindings](au05-direct-bindings.md)
- [Resident software contract](../resident-software.md)
- [Interaction design overview](README.md)

