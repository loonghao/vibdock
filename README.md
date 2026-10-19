<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/brand/vibdock-symbol-light.svg">
  <img src="assets/brand/vibdock-symbol.svg" alt="VibDock logo: diamond with a negative-space voice channel" width="88" height="88">
</picture>

# VibDock

A local-first bridge between hardware controls, voice workflows, and personal AI agents.

Use a microphone button to talk to an agent, silence an input device, and get back to a meeting without hunting through windows. VibDock is being built for the way people move between coding assistants, personal agents, and collaboration apps.

**Early preview:** Published v0.1.0 provides Windows microphone commands and a design preview. Current source adds authenticated Web configuration, one resident microphone shortcut, a Windows tray that opens Settings directly, saved Windows software observations with microphone associations and explicit native single-session playback controls, a macOS native menu-bar/Dock/panel with saved application-presence observations, and native desktop packaging. Current Windows source also recognises running window applications independently of their audio sessions and can save up to eight per-application shortcut bindings that dispatch a user-supplied chord only when the bound executable owns the foreground window. The same source can write chords directly into the three Ulanzi AU05 Vibe Key buttons over the device's vendor HID interface, so per-application buttons no longer depend on Ulanzi Studio. A first-run default layout writes firmware-emitted `Ctrl + Win` (WeChat voice), `Enter`, and `Escape` to the three buttons, and the same direct-chord mode can carry other chords and bare keys that VibDock cannot register itself; a recent-trigger log shows captured application-bound presses. The same source can also map one AU05 button or HID gamepad button to an ordered **hardware action** sequence (launch, send keys, toggle microphone, local command, wait); the AU05 write path for that layer has a live add/write/readback/remove check plus a physical wait-step execution; gamepad hardware remains unverified. The first-run defaults, the three default buttons' host behaviour, and a physical application-bound press were checked on a real AU05 on 2026-10-19; Studio coexistence, device replacement, power-cycle persistence, and sleep/wake recovery remain open. These source features are not a new published release. The remaining physical-device items, real default-browser launch, tray pointer interaction and DPI/focus behavior still require desktop acceptance. Application input routing and voice-session start/stop remain `not_implemented`; VibDock is independent of these vendors.

**Service acceptance:** Integrated baseline `3b44b770` passes [hosted Windows, Linux and macOS CI](https://github.com/loonghao/vibdock/actions/runs/38841945041), including the local service identity and authorization regressions. That baseline predates the desktop and microphone-association changes. New candidates must pass their exact-head CI and the native/browser evidence checks below. Cross-platform build success is not platform audio or physical-device acceptance.

**Desktop status feedback:** The corner status indicator is implemented in source as an opt-in Windows overlay, **off by default**. Enable it by setting `VIBDOCK_OVERLAY=1` (`on`, `true`, and `yes` also work) before starting the service; `VIBDOCK_OVERLAY_CORNER` selects a corner (`bottom-right` default, or `bottom-left`, `top-right`, `top-left`). The window is placed in the taskbar-free work area of the primary monitor; if that work area cannot be read it falls back to the full monitor rectangle, which is **not** taskbar-aware and may let the indicator overlap a taskbar band. It has unit tests for its status contracts and placement but has **not** been accepted against live Windows: no run has verified typing/focus behaviour, multi-monitor placement, or failed-action feedback on a real desktop. The optional breathing border is planned and not implemented.

## Download and run

Published v0.1.0 archives contain one Rust executable and this README. Current Windows source builds produce `vibdock.exe` (console CLI) and `vibdock-launcher.exe` (desktop launcher); the pending release pipeline packages both and checks their executable subsystems. No Node.js, Python, or separate Web server is required.

| Platform | Published v0.1.0 | Pending source |
| --- | --- | --- |
| Windows x64 | Microphone commands, fixed global mute shortcut, design preview | Authenticated configuration, resident configurable shortcut, tray-first Settings, persistent software/microphone associations, exact-session playback controls, direct AU05 provisioning with first-run defaults and direct firmware chords, AU05/gamepad hardware action sequences, dual-entry archive |
| Linux x64 | Design preview | Local configuration service; audio and global shortcuts unsupported |
| macOS Apple silicon | Design preview | Native .app, menu bar/Dock and floating panel, persistent application list/icons/running state, authenticated local configuration; audio and global shortcuts unsupported |

## macOS native candidate

Current source also packages `VibDock.app`: open it for the original-logo menu
bar, Dock entry and a small resident panel. Use Settings for the proved local
configuration page; closing the panel hides it, and Quit stops the verified
service. Repeated launches restore the same panel, including direct binary
launches from a second installed copy.

Candidates are not Developer ID signed or notarized. Hosted checks distinguish
real AppKit own-view rendering from a full desktop/pointer test. macOS software pins and native icons describe verified application presence only.
Audio, voice control, software actions and global shortcuts remain unavailable.
See [macOS native behavior and acceptance](docs/macos-desktop.md) for limits.

## Configure a hardware shortcut (pending Windows source)

This workflow requires a build containing both `vibdock.exe` and `vibdock-launcher.exe`; the published v0.1.0 download does not contain the launcher.

1. Extract both executables into the same directory. Start `vibdock-launcher.exe` to open authenticated local Settings and keep the notification-area icon running. Click the VibDock tray icon or launch again to reopen Settings.
2. Select the intended microphone from the service's device list. In the binding editor, record a combination such as **Ctrl+Alt+Shift+F9** and save it.
3. Check the separate **background binding** row. “Registered” means Windows accepted the shortcut; it does not mean an audio action already ran. A conflict leaves the previous configuration and registration in place.
4. In Ulanzi Studio, map the desired button to that exact keyboard combination using its shortcut action. Physical-device identification is not inferred from a mapped keyboard event.
5. Press the hardware button and verify the microphone's actual mute state. The page reports the last completed action or its error. Physical verification of this Studio-mapped microphone shortcut remains pending; a saved mapping alone is not proof of input delivery. The AU05 Vibe Key button defaults are covered separately below.
6. Close the browser to leave the service running. Reopen **Settings…** from the tray; use **Exit VibDock** or `vibdock.exe service-stop` to stop the service.

This workflow still maps the microphone shortcut through Ulanzi Studio: the
direct AU05 path provisions per-application button triggers or direct firmware
chords, and a trigger equal to the microphone shortcut is rejected.
Per-application Vibe Key buttons can be written directly without Studio; see
[Vibe Key (AU05) direct bindings](#vibe-key-au05-direct-bindings-windows-source).

Only one microphone/shortcut pair is currently active. Up to eight separate
per-application shortcut bindings can also be saved (see below). Do not run
legacy `listen` with the same chord: Windows allows only one registration. Input-device mute affects every application using that microphone. Per-agent input and voice-session start/stop are separate planned controls. Right-click a saved Windows software icon to mute or unmute one explicitly selected current playback session; its receipt requires a matching native readback. A dated two-session exact-session mute/unmute readback check is recorded in the [application recognition acceptance record](docs/testing/app-recognition-acceptance.md); meeting-level real-audio isolation remains pending. The existing explicit-session playback CLI is documented below.

Settings uses the service's saved configuration and real microphone list; service-mode saves do not write browser-local drafts. On the software page, associate each pinned Windows application with an exact microphone or clear that association. Its microphone controls address that saved endpoint and fail if it is unavailable. This is metadata plus endpoint control: it does not change the application's Windows input route, establish which microphone the application uses, or start/end a voice session. Endpoint mute affects every application using it.

The status page uses bounded, serial polling. When the service cannot answer, it shows unknown rather than preserving a successful state. “Last action” is a readback after a shortcut action, not continuous tracking of changes made by other applications. A service restart clears action history and requires a fresh browser session.

## Per-application shortcuts (Windows source)

Settings > **应用与麦克风** also recognises running applications that own a
visible top-level window, even when they have no playback session. This is how
an input method, a coding agent, or a meeting client can be selected before it
produces audio. Recognised names include Tencent Meeting (`wemeetapp.exe`),
WeChat Input (`wetype*.exe`), Codex/ChatGPT (`codex.exe`/`chatgpt.exe`),
DeepSeek Harness (`DeepSeek Harness.exe`, integration id `dsh`) and WorkBuddy
(`workbuddy.exe`). That match is a filename label only: it does not establish
that the client is installed correctly, connected, signed in, or able to accept
a shortcut.

For a recognised application, save:

- a **VibDock trigger** registered globally, such as `Ctrl + Alt + Shift + F10`;
- a **dispatch shortcut**, such as the voice shortcut configured inside the
  target application.

When the trigger fires, VibDock reads the current foreground window and, by
default, sends the dispatch chord only if that window belongs to the saved
executable. If a different application is in front, nothing is sent. For a
background input method or tray tool that never owns a foreground window, an
explicit **running** scope checks that the saved executable is running and then
sends the chord to whatever application is foreground. **The dispatch result is
not a verified application state**: the application may ignore the chord,
interpret it differently, or be unable to receive elevated input. A successful
dispatch is reported as `DISPATCHED`, never as "voice started" or "microphone
open".

Read the target's actual shortcut from its own settings. VibDock ships no
built-in WeChat, Codex, or Tencent Meeting shortcut. Up to eight bindings are
saved in `shortcuts.json`; a trigger must be unique, and a dispatch chord may
not equal any VibDock trigger (Windows would feed the injected chord back into
the global registration). A registered trigger cannot contain `Win` or be a
bare key, but a dispatch chord may contain `Win` (for example `Ctrl + Win`);
the AU05 direct firmware mode accepts both for a physical button. Bindings live
in `%LOCALAPPDATA%\VibDock\shortcuts.json`; executable paths in that file are
personal data.

The same bindings are available without the browser:

```powershell
.\vibdock.exe shortcut-apps
.\vibdock.exe shortcut-list --json
.\vibdock.exe shortcut-add --app "ChatGPT" --trigger "Ctrl+Alt+F10" --dispatch "Ctrl+Shift+Space" --device-button 1
.\vibdock.exe shortcut-add --application <reference> --trigger "Ctrl+Alt+F10" --dispatch "Ctrl+Shift+Space" --scope running
.\vibdock.exe shortcut-remove --id <saved-id>
```

`shortcut-apps` prints the opaque reference from a fresh scan; the CLI never
accepts an executable path. `--app` requires a unique case-insensitive name and
defaults a background application to `running`. The service must already be
running (`service-start`), and the command never dispatches the chord itself.

Recognition is deliberately bounded: only visible, non-cloaked, non-tool
windows are considered, shell/host processes are skipped, and a capped or
partially unreadable window scan reports state as unknown rather than "not
running". A closed set of known integration executables (`wetype*`, `wemeet*`,
`codex`/`chatgpt`, `DeepSeek Harness`, `workbuddy`) is also recognised when it
runs without a window, so WeChat Input can be pinned even when it has no
visible window. Other background services and tray-only helpers are not
listed.
The dated [application recognition acceptance record](docs/testing/app-recognition-acceptance.md)
lists the live detections, shortcut dispatch checks and the remaining UI limits.
Live foreground/focus and injected-input acceptance on a real desktop remains
pending; see [the software contract](docs/resident-software.md).

## Vibe Key (AU05) direct bindings (Windows source)

Current Windows source can provision the three round buttons of an Ulanzi AU05
Vibe Key directly, without installing or running Ulanzi Studio. The resident
service talks to the device's vendor HID collection (VID `0xFFF1`, PID `0x00DD`,
usage page `0xFFFC`, report ID `0x55`) and writes chords into the buttons'
firmware settings. There are three ways a managed button can be used:

- **Application binding** — a saved per-application shortcut names a Vibe Key
  button; its registered trigger is written into that button. Pressing the
  button emits the chord, VibDock captures it, and dispatch follows the same
  foreground/running rules as
  [per-application shortcuts](#per-application-shortcuts-windows-source).
- **Direct firmware chord** — a chord or single key is written straight into
  the button with no VibDock trigger and no dispatch. The device emits the keys
  itself and VibDock is not involved. This is how the default layout works and
  is needed for `Ctrl + Win` (which Windows will not let VibDock register) and
  for bare keys such as `Enter` and `Esc`.
- **Hardware action** — the service runs an ordered step sequence when the
  button is pressed: an AU05 action gets a generated, registered trigger that is
  written into the button; a gamepad action is fed directly by the HID monitor.
  See [hardware action sequences](docs/design/hardware-actions.md).

`provisioned` is only reported after the shortcut slot and the fixed-function
slot are read back and match, so it means the device returned exactly the
requested chord — not that the application ran, received anything, or started
voice. The protocol was cross-checked against the MIT-licensed
[OpenVibeKey](https://github.com/palaemonboy/OpenVibeKey) reference and against
live device readback; the first-run defaults and a physical application-bound
press have live acceptance evidence (below). The protocol design and full limits
are in [AU05 direct bindings](docs/design/au05-direct-bindings.md). The published
v0.1.0 download does not contain this feature.

### Default layout on first run

When no `hardware.json` exists, the authenticated service writes this built-in
layout once at startup and stores it in `hardware.json`:

| Vibe Key button | Chord | Label | What the device emits |
| --- | --- | --- | --- |
| 按钮 1 | `Ctrl + Win` | 微信语音 | left Ctrl + left Windows |
| 按钮 2 | `Enter` | 回车 | `Enter` |
| 按钮 3 | `Escape` | 取消 | `Escape` |

A button that already carries an application binding keeps that binding; its
default entry is skipped. On 2026-10-19 the default entries were written and
read back from a physical AU05 (firmware 4.4.0), and the user confirmed button 1
invokes WeChat Input voice, button 2 sends `Enter`, and button 3 sends `Escape`.
These are firmware-emitted physical keys, not VibDock dispatches.

If the device is absent on first run, the layout is still saved and can be
written later with **同步到设备** / `--sync`. A `hardware.json` that exists but
is empty does not re-seed the defaults.

### Setup

1. Build or use a current Windows source build. Keep `vibdock.exe` and
   `vibdock-launcher.exe` in the same directory, then start the launcher to
   open authenticated Settings. Keep the notification-area icon running.
2. Open **应用与麦克风**. The **Vibe Key（AU05）硬件按键** card should report
   已连接 and show the device and its microphone. If it reports 未检测到 AU05,
   plug in the 2.4G receiver, power the device on, press any AU05 key, and click
   刷新.
3. On first run, confirm the three buttons report 已写入设备并读回一致 and list
   their direct chords. If a button shows a device failure, press an AU05 key and
   click **同步到设备**.
4. Change a direct chord in **硬件直发快捷键**: choose the button, enter a
   chord such as `Ctrl + Shift + Win` or a single key such as `Enter`, add an
   optional label, and click 写入直发组合键. 清除该按键直发 removes it.
5. To bind an application instead, first clear that button's direct chord, then
   in **新增应用快捷键** choose the target application, a free **VibDock
   trigger**, the target's own dispatch chord, and the **Vibe Key 按钮**. Save.
6. Press the physical button. A direct chord produces its keys in whatever
   application is focused and does **not** appear in 最近触发. An
   application-bound button produces a 最近触发 entry and reports `DISPATCHED`
   or an error. **`DISPATCHED` is not a verified application state.**
7. Use **同步到设备** after a service restart, wake-up, or reconnect to
   re-detect the device and rewrite all saved buttons.

At most one saved item — an application binding, a direct chord, or a hardware
action — may name each button, and at most eight application bindings are saved
in total. A direct chord equal to a registered trigger is rejected, and a new
trigger equal to a direct chord is rejected, so a firmware-emitted chord can
never also feed the global hotkey table. Only the three round buttons (device
slots 0-2) are managed; the knob, the built-in
microphone, lighting, standby timers and every other device setting are never
written. A written chord lives in the device firmware, so the button keeps
emitting it while VibDock is not running; it then reaches the foreground
application like any other keyboard input.

### Layouts and the 最近触发 (recent trigger) log

`GET /api/hardware` and `GET /api/config` return `recent_actions`: a bounded,
in-memory log (newest last, at most 20) of completed triggers, and the page shows
the newest six in **最近触发** plus the latest one in the status line. Each entry
carries `sequence`, `at` (milliseconds since the Unix epoch), `source`
(`vibe_key`, `application_shortcut`, `action_binding`, `gamepad`, or
`microphone`), `label`, `trigger`, `dispatch`, `device_button`, `state`,
`error`, and `detail`. For application sources `label` is the saved binding
label; for `microphone` it is the configured endpoint id; for action and gamepad
sources it is the action name, the success `state` is `DONE`, and `detail`
describes the completed steps. It records that VibDock received a registered
trigger and what it sent; `DISPATCHED` still does not mean the application
acted. The log is cleared on service restart.

Direct firmware chords never appear in this log: the device emits them without a
host hotkey, so VibDock never sees the press. Confirm their effect in the target
application instead.

Two layouts are supported:

- **Out-of-the-box:** the three direct defaults above (WeChat voice / Enter /
  Escape).
- **One button per agent:** clear the direct chords, then bind each button to
  an application with its own trigger. The intended example is Codex/ChatGPT,
  DeepSeek Harness (`dsh`), and WorkBuddy.
- **One button per action sequence:** clear the button and save a hardware
  action instead, which can launch, send keys, toggle a microphone, run a local
  command, or wait. See the section below.

| Vibe Key button | Target | Recognised executable | Example trigger |
| --- | --- | --- | --- |
| 按钮 1 | Codex / ChatGPT | `codex.exe` / `chatgpt.exe` | `Ctrl + Alt + Shift + F10` |
| 按钮 2 | DeepSeek Harness (integration id `dsh`) | `DeepSeek Harness.exe` | `Ctrl + Alt + Shift + F11` |
| 按钮 3 | WorkBuddy | `workbuddy.exe` | `Ctrl + Alt + Shift + F12` |

These triggers are examples: pick chords no other application uses, and do not
reuse the microphone shortcut (the `listen` example uses
`Ctrl + Alt + Shift + F9`). A registered trigger must be AU05-expressible and
must not contain `Win`; a dispatch chord may contain `Win`. `F1`-`F12`, letters,
digits, common punctuation and named keys are accepted; `F13`-`F24` and
unsupported characters are rejected with `400`.

### Why default button 1 uses the direct path

`Ctrl + Win` is a common voice shortcut for input methods, but Windows does not
let VibDock register it as a global trigger, and `SendInput`-injected
`Ctrl + Win` produced a `DISPATCHED` receipt without invoking WeChat Input
during live acceptance. The firmware-emitted chord did invoke it. That negative
result is why the default uses direct firmware emission rather than an
application binding; it does not prove the application reacted in general.

### AU05 microphone

The AU05 microphone appears as a normal Windows capture endpoint whose friendly
name contains `AU05`. Select it in the target application or in VibDock's
existing **音频设备** view like any other microphone. This feature does not
select, open, or change an audio route, and the hardware status view uses the
name only as a label.

### CLI and API

The service must already be running (`service-start` or the launcher). The CLI
never talks to the device directly; it projects the authenticated API.

```powershell
.\vibdock.exe hardware-status
.\vibdock.exe hardware-status --json
.\vibdock.exe hardware-status --sync
.\vibdock.exe hardware-bind --button 1 --chord "Ctrl + Win" --label "微信语音"
.\vibdock.exe hardware-bind --button 2 --chord "Enter"
.\vibdock.exe hardware-unbind --button 1
.\vibdock.exe shortcut-add --app "DeepSeek Harness" --trigger "Ctrl+Alt+Shift+F11" --dispatch "Ctrl+Shift+Space" --device-button 2
.\vibdock.exe shortcut-remove --id <saved-id>
```

`--device-button` and `--button` are 1-3, matching the printed button labels;
the API fields are zero-based. `hardware-status` prints the buttons, their direct
chords, and the direct-chord list; `--json` prints the hardware status object;
`--sync` re-detects the device and rewrites every saved button.

The authenticated API uses the same bearer, `Host` and `Origin` rules as the
other `/api` routes:

```text
GET  /api/hardware                         (also returns recent_actions)
POST /api/hardware/sync
POST /api/hardware/direct                  {"revision":0,"button":0,"chord":"Ctrl + Win","label":"微信语音"}
POST /api/hardware/direct/remove           {"revision":1,"button":0}
GET  /api/shortcuts                        (each entry includes device_button; top-level hardware status)
POST /api/shortcuts/add
     {"revision":0,"application_reference":"<fresh reference from a scan>",
      "trigger":"Ctrl + Alt + Shift + F10","dispatch":"Ctrl + Shift + Space",
      "device_button":0}
GET  /api/config                           (includes last_action and recent_actions)
```

A direct chord accepts a chord or a bare key (except bare `Win`); `chord` and an
optional `label` are stored in `hardware.json`. `device_button` is optional and
accepts `0`, `1`, `2`, or `null`; an explicit `null` means no button. A button
cannot hold an application binding, a direct chord, and a hardware action at
once: the later save returns `409` until the other is cleared. The hardware
status object contains `schema_version`, `platform`, `available`, `enabled`,
`connected`, `device`, `buttons` (with `direct_chord`, `direct_label`,
`direct_state`, `direct_error`), `direct` (`revision`, `entries`, `limit`,
`storage_writable`, `error`), `error`, `studio_note`, and `recent_actions`. It
does not list hardware action bindings; their state comes from `/api/actions`.

### Provisioning states

The per-button `state` and, when a direct chord exists, `direct_state` use the
same labels; `direct_state` is `null` when no direct chord is configured.

- `provisioned` — this service instance wrote the chord and read both device
  slots back successfully. It is a firmware readback of the last completed
  write, not continuous monitoring and not an application result; unplugging the
  device does not retract it until a later attempt fails.
- `failed` — the latest write or readback failed; if the device is not connected
  and there is no recorded result for this binding, the not-found error is
  shown. A firmware failure does **not** roll back the saved binding or direct
  chord.
- `pending` — the item is saved and the device is connected, but this service
  instance has not written and verified it yet (for example after a service
  restart). The button may still work from its stored firmware chord; run a sync
  to re-verify it.
- `unbound` — no saved item names that button (combined `state` only; a
  `direct_state` is `null` rather than `unbound`).
- Unavailable — on a platform or mode where hardware control is disabled
  (the read-only preview, or a non-Windows build), the status reports
  `available: false`; the Settings card is hidden in the preview.

When both an application binding and a direct chord name one button, the
combined `state` reports `failed` with a conflict message and provisioning skips
that button. A hardware action binding is a third claimant: it is refused at
save time when the button already has an application shortcut or a direct
chord, and `/api/actions` reports its own `armed`/`error` state.

**Quit Ulanzi Studio while VibDock owns the device.** Studio may hold or talk on
the same vendor collection; VibDock does not detect Studio or coordinate with
it, and writes or readbacks can then time out. The device's vendor channel can
also sleep after its standby timeout: a write then times out, and the message
asks you to press any AU05 key, then retry the sync. Because
the chord lives in the device firmware, the button keeps working while VibDock
is closed, but VibDock cannot dispatch or capture anything then.

### Verified and not yet verified

Live acceptance on 2026-10-19 (AU05 firmware 4.4.0) verified the first-run
defaults (written and read back; entry bytes `83 01 83 08`, `02 28`, `02 29`),
the three default buttons producing WeChat voice / `Enter` / `Escape`, a
physical application-bound press appearing in 最近触发 with its
button/label/trigger/dispatch and `DISPATCHED`, and the negative `Ctrl + Win`
injection result above. Also on 2026-10-19 an AU05 hardware action with a `wait`
step was added through the authenticated API, received the generated trigger
`Ctrl + Alt + Shift + F1`, was written into the device firmware and read back
(`provisioned`), then removed and the default direct layout restored. A physical
press of the action-bound button executed the `wait` step and appeared in
`recent_actions` as `source: "action_binding"`, `state: "DONE"`, and
`detail: "waited 120 ms"`. The first physical attempts produced no host keys
until the 2.4G receiver was replugged, which restored the device's
keyboard-forward channel; the vendor configuration channel had stayed
responsive. The hardware action layer and its full limits are in
[hardware action acceptance](docs/testing/hardware-actions-acceptance.md).

Still unverified:

- Ulanzi Studio coexistence, device replacement identity, and power-cycle
  persistence.
- Sleep/wake recovery after the device's standby timeout.
- Clearing a firmware slot on `hardware-unbind`, especially after a service
  restart or with the device absent; confirm by pressing the button.
- A per-button matrix of application-binding writes and dispatches for all
  three buttons.
- Hosted CI with a device: CI has no AU05, so only protocol, validation,
  persistence, and page code are covered there.

The dated evidence and the full manual checklist are in the
[AU05 acceptance record](docs/testing/au05-acceptance.md).

## 硬件动作 / hardware action sequences (Windows source)

A saved **hardware action** maps one physical control to an ordered list of 1-8
steps. It is a separate layer above the two AU05 provisioning modes and works
with either an AU05 button or a HID gamepad button:

- an **AU05 button** (`0`-`2`): the service generates an otherwise-unused
  `Ctrl + Alt + Shift + F1`-`F12` trigger, registers it globally, and writes it
  into the button's firmware with the same readback verification; the press runs
  the steps.
- a **HID gamepad button**: identified by `device_key` + button usage, fed by
  the Windows HID monitor. Nothing is written to firmware and no trigger is
  registered.

Up to 16 actions are saved. One AU05 button can carry only one of application
shortcut, direct chord, or hardware action. Actions do **not** add AI
application voice start/stop; a voice workflow is composed from `launch` +
`wait` + `send_keys` (and optionally `toggle_microphone`). The service executes
steps in order and **stops at the first failure**; earlier steps are not rolled
back. There is no HTTP endpoint that executes a saved action: execution happens
only when the physical control fires.

### Setup

1. Start the launcher and open Settings > **应用与麦克风**. The **硬件动作（按键
   → 动作序列）** card is available in authorized service mode and hidden in the
   read-only preview.
2. Free the button first. A default AU05 button carries a direct chord and a
   previously used button may carry an application shortcut; clear that direct
   chord (清除该按键直发 / `hardware-unbind`) or remove the application binding,
   otherwise saving the action returns a conflict (`409`).
3. Click 新建硬件动作, name it, and choose the source: **Vibe Key 按钮** (按钮
   1-3) or **游戏手柄按键**. For a gamepad, connect it, press the button to learn
   `latest_button`, and click 使用这个按键. Gamepad bindings have no trigger.
4. Add 1-8 steps, reordering or deleting them as needed:
   - **启动应用** — only applications from the current recognition scan; the
     browser sends an opaque reference, never a path.
   - **发送快捷键** — a modifier-plus-key chord injected into the foreground
     application.
   - **切换麦克风** — an explicit microphone endpoint, verified by a mute
     readback.
   - **运行命令** — a command from local `commands.json` plus its declared
     parameters.
   - **等待** — 0-10000 ms.

   The action editor shows the command source and cannot create or edit command
   definitions.
5. Save. For an AU05 source the generated trigger is registered and written into
   the button; `provisioned` means both firmware slots read back identically. A
   firmware failure keeps the saved action and shows a warning; **同步到设备**
   retries. Gamepad actions save without any firmware write.
6. Press the control. Success and failure appear in **最近触发** as
   `source: action_binding` or `gamepad`. On success the entry's `state` is
   `DONE` and `detail` describes the completed steps; on failure `state` is
   `null`, `error` names the failing step, and `detail` is `null`. Direct
   firmware chords still never appear there.

### Step types

| Step | Fields | What it does | What success means |
| --- | --- | --- | --- |
| `launch` | `application_reference` (resolved server-side), optional `args` | spawns the executable with argv | the process started (pid in `detail`); the application is not verified as ready |
| `send_keys` | `keys` | parses the chord and injects it into the current foreground application | Windows accepted the input; the application may ignore it |
| `toggle_microphone` | `device_id` | toggles that exact microphone and reads the mute state back | a verified mute state; affects every application using the endpoint |
| `command` | `command_id`, `parameters` | expands the local command definition and spawns executable + argv | the process started; no shell is involved |
| `wait` | `ms` (0-10000) | sleeps | the wait elapsed; it blocks the service loop |

### Combined example

One button can start a multi-step workflow. For example, with button 1 freed:

| # | Step | Payload |
| --- | --- | --- |
| 1 | launch | Codex from the recognition scan |
| 2 | wait | 1000 ms |
| 3 | toggle_microphone | the saved microphone endpoint |
| 4 | send_keys | `Ctrl + Shift + Space` (Codex's own voice shortcut) |
| 5 | wait | 500 ms |
| 6 | launch | WorkBuddy |

This is a **workflow composition, not voice integration**: VibDock cannot verify
that Codex started voice or accepted the chord, and the microphone toggle affects
all applications using that endpoint. AI application `start_voice`, `end_voice`
and `input_routing` remain `not_implemented`.

### Local commands (`commands.json`)

Commands are structured local configuration, not request text. The page can only
select a command and fill its declared parameters; there is no API that creates
commands or submits a shell string. `GET /api/actions/commands` returns the
sanitized definitions (id, label, parameters) without executable paths or argv.

Example `commands.json` in `%LOCALAPPDATA%\VibDock\commands.json` (replace the
executable with an existing local program):

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

The service validates that the executable is a local path, that every `{name}`
placeholder is declared, and that values are strings no longer than
`max_length` (1-256, default 120) without control characters. At execution it
replaces placeholders per fixed argv entry and spawns the executable with argv —
never through a shell command line. A user who points a command at a shell
interpreter locally is making a deliberate local-configuration choice; that is a
`commands.json` trust boundary, not an arbitrary-shell API.

### API

The authenticated API uses the same bearer, `Host` and `Origin` rules as the
other `/api` routes:

```text
GET  /api/actions
GET  /api/actions/commands
GET  /api/actions/gamepads
POST /api/actions/add
     {"revision":0,"name":"打开 Codex 并静音",
      "source":{"kind":"au05","button":0},
      "steps":[{"type":"launch","application_reference":"<fresh reference>"},
               {"type":"wait","ms":1000},
               {"type":"send_keys","keys":"Ctrl + Shift + Space"},
               {"type":"toggle_microphone","device_id":"<endpoint id>"}]}
POST /api/actions/remove                    {"revision":1,"id":"<64 hex>"}
POST /api/actions/sync                      {}
```

`GET /api/actions` returns `revision`, `bindings` (id, name, source, trigger,
steps, armed, error), `limit` (16), `storage_writable`, and errors. A gamepad
source has no trigger. `GET /api/actions/gamepads` returns current HID gamepads
with `device_key`, name, vendor/product ids, `button_usages`, `connected`, and
the learning-only `latest_button`. `GET /api/actions/commands` returns the
sanitized command definitions. An AU05 add response may carry a top-level
`hardware` object `{button, state, error}`; `POST /api/actions/sync` rewrites
every AU05 action trigger and returns a `hardware` array of
`{id, button, state, error}`. There is no action-execute endpoint.

Generated AU05 action triggers avoid the microphone trigger, application
triggers, direct chords, and other action triggers; if all twelve chords are in
use the save fails with `409`. They are **not** checked against application
*dispatch* chords, so a dispatch chord equal to an action trigger would also be
captured by VibDock; choose triggers and dispatch chords accordingly.

### Gamepad identity and limits

The gamepad adapter enumerates HID top-level collections on the Generic Desktop
page with usage `0x04`, `0x05`, or `0x08`. Button usages are normally the HID
Button page `0x09`, with the Generic Desktop page `0x01` accepted as a fallback;
a device uses one consistent page. The monitor runs on the service accept-loop
thread and rescans about every two seconds, and each `/api/actions/gamepads`
request also refreshes the list; the page polls that endpoint only while the
action editor has a gamepad source selected. `latest_button` is for learning
only.

A gamepad binding's identity is `vvvv:pppp:serial` when the device exposes a
serial, otherwise `vvvv:pppp:interface-path`, with a collision suffix for
duplicate units. Devices without a serial may not follow a replaced unit.
**No physical gamepad was detected by Windows on the acceptance host**, so
this adapter has compilation and unit-test evidence only; the page shows the
empty state and waits for a pad.

### Verified and not verified

Live evidence on 2026-10-19: an AU05 action binding with a `wait` step was added
through the authenticated API, received trigger `Ctrl + Alt + Shift + F1`, was
written into the device firmware and read back (`provisioned`), then removed; the
default direct layout was restored. A physical press of the action-bound button
executed the step and appeared as `source: "action_binding"`, `state: "DONE"`,
`detail: "waited 120 ms"`. The host saw no device keys during the first attempts
until the receiver was replugged (keyboard-forward channel recovery). UI
validation used `node --check`, the existing DOM test, and a DOM harness for
action flows — not a physical pointer run.

Still unverified: a full
launch → send keys → toggle microphone chain on hardware; gamepad live
input/learning (no device on the host); live command execution with substituted
arguments; stop-at-first-failure on hardware; sleep/wake; Ulanzi Studio
coexistence; hosted CI with hardware. The full record and manual checklist are in
[hardware action acceptance](docs/testing/hardware-actions-acceptance.md), and
the design is in
[hardware action sequences](docs/design/hardware-actions.md).

## Local speech-to-text (optional native build)

An optional `stt` Cargo feature adds offline speech-to-text with a local
SenseVoice ONNX model. The default preview binary does not include this
dependency; build it explicitly:

```powershell
cargo build --release --features stt
.\target\release\vibdock.exe transcribe --wav sample.wav --json
.\target\release\vibdock.exe dictate --seconds 5 --json
.\target\release\vibdock.exe dictate --seconds 5 --vad --json
.\target\release\vibdock.exe dictate --wav sample.wav --insert
.\target\release\vibdock.exe dictate --hold --trigger "Ctrl+Alt+Space" --insert --target "ChatGPT"
```

`transcribe` handles one WAV. `dictate` is Windows-only and captures from the
default microphone (1–30 seconds), transcribes it, and with `--insert` types the
result into the window that is focused at insertion time using Unicode
`SendInput`. `--vad` trims the captured clip to Silero-detected speech and fails
with a distinct "no speech" error when nothing is detected. `--hold --trigger
<chord>` registers that chord globally and
captures until the key is released; Escape cancels and discards the clip.
`--target <title-substring>` requires `--insert` and refuses to type unless the
foreground window title contains that text (case-insensitive), so the launching
terminal cannot receive the transcript by accident. It never presses Enter,
clicks Send, or submits a form, and it re-checks the foreground window before and
after insertion. A detected focus change is an error; success means input was
sent, not that the target accepted or displayed it. The transcript and audio
stay local.

The model is discovered in this order: `--model-dir`, `VIBDOCK_STT_MODEL_DIR`,
the existing DeepSeek Harness SenseVoice cache (read-only), then
`%LOCALAPPDATA%\VibDock\models\sensevoice`. A missing model fails closed with
the checked paths and the install route; the command never downloads at runtime.
Building with `stt` downloads the pinned sherpa-onnx native archives at build
time, so that build is not offline-friendly. The separate consent-gated model
download lives in [scripts/stt](scripts/stt/README.md).

Still not implemented: configured-device selection, continuous listening, an
interactive insertion target picker, clipboard fallback, and any claim of
verified voice-session or content state. The native build is not part of the
default CI artifact; packaging and model licensing remain open in
[the local speech input design](docs/design/local-speech-input.md). The dated
[local dictation acceptance record](docs/testing/local-dictation-acceptance.md)
lists the live WAV, microphone, hold-trigger and Notepad insertion results.

## Explore the design preview

```powershell
.\vibdock.exe serve
```

Open http://127.0.0.1:43128 to explore the design. The preview uses mock data, never controls audio, and keeps bindings in the browser's own local storage so they survive a reload — they are not written to any backend or device configuration. Close the terminal or press Ctrl+C to stop the preview server. The current `serve` and `listen` commands are separate processes.

## Resident service

`service-start` runs a loopback service on the same address with authenticated
configuration and action endpoints:

```powershell
.\vibdock.exe service-start
.\vibdock.exe service-status
.\vibdock.exe service-stop
```

The service generates a bearer token at startup from the OS entropy source and
records it with the bound port in `%LOCALAPPDATA%\VibDock\service.json`. Reads
and writes under `/api` require that token via `Authorization: Bearer <token>`
or the `X-VibDock-Token` header. Requests are rejected unless the `Host` header
is the bound loopback address and any `Origin` header is that same loopback
origin. `/api/config` validates `device_id` and `hotkey`, then registers the shortcut
before persisting to `config.json`. Missing patch fields retain their previous
values; explicit null clears a field. Hotkeys require a modifier and a supported
key. Character keys use the active Windows keyboard layout; required symbol
modifiers must be included. Registration errors return 409 with the actual
reason and rollback state; disk-write errors return 500. No shell or
arbitrary-command endpoint exists.

Repeated `service-start` invocations reuse the running instance instead of
binding a second listener. The published record is treated only as a candidate:
before it is trusted, the service on that port must answer a fresh 32-byte
challenge at `POST /instance/challenge` with an HMAC-SHA256 proof derived from
the recorded token. Discovery sends only the random nonce, never the bearer
token; an HTTP 200 response alone is not ownership evidence. The complete
challenge has a 700 ms budget and an 8 KiB response limit. Browser-code and
shutdown control requests prove ownership and send their bearer on the same
pinned IPv4 connection, with a two-second total budget across both exchanges
and the same response limit. They never reconnect or fall back to IPv6 after
proof. Only a successful challenge with one valid Content-Length and no
Transfer-Encoding permits one additional HTTP exchange; chunked or ambiguous
framing is always closed after its response.
A stale marker left by a killed service is recovered only after proving no
owner answers; if an unrelated process holds the port, startup fails with an
error and leaves the marker alone. Older preview services without challenge
proofs are not trusted: exit their tray or stop them with their matching older
CLI before replacing the executables. Atomic state writes create exclusive
fresh temporary files before writing; Unix files are created with mode 0600.
Windows state protection relies on the parent directory's inherited ACLs; an
explicit per-user ACL guarantee has not been established.

Request bodies are bounded: an oversized or non-UTF-8 body is answered with
413 rather than read in full. If the OS entropy source fails, the service
refuses to start instead of minting a weaker token.

The desktop launcher requests a one-use browser code through the authenticated
`POST /api/bootstrap/new` endpoint and opens it in a URL fragment. The page must
clear the fragment immediately and redeem it at `POST /api/bootstrap` with
`{"code":"..."}` to receive an in-memory session token. Codes expire after
60 seconds and cannot be reused. Sessions expire after eight hours or a service
restart; they permit configuration and device actions, but not service shutdown,
instance identity, or issuing more codes. Pending codes and sessions are bounded.
The service-mode page consumes the code automatically and clears the fragment
before redemption. Reopen the page from the tray when a code or session expires.

`service-stop` performs a real shutdown: it asks the running service to stop
over the loopback API using the recorded token, and waits for the port to be
released. Only the instance that can prove it holds that token is stopped, so
an unrelated process on the same port is never terminated by name. The service
clears its instance marker as it exits.

## Tray entry (Windows)

Double-click `vibdock-launcher.exe` to start or reuse an authenticated resident
service and open Web Settings directly. Keep both executables in the same directory.
The notification-area icon remains running; there is no Windows resident panel.

- **Settings…**, the primary tray icon click, or another launch opens a fresh one-use handoff to the
  proved service’s actual address. A browser launch failure is visible.
- **Mute microphone / Unmute microphone** targets the exact saved microphone
  identity and reports the verified readback. A missing or unreadable device is
  shown as unavailable; there is no default-device fallback.
- **Pinned software** lists up to eight saved entries and exposes their exact
  playback-session mute/unmute actions. It does not start or stop an
  application’s voice session.
- Closing the browser leaves the tray and service running.
- **Service status** reports service ownership/liveness only. It does not claim an
  audio action, application voice session, or hardware input is working.
- **Exit VibDock** asks the verified service to stop. A shutdown failure leaves the
  controls available for retry. No process-name termination is used.

The existing named mutex keeps one desktop entry. Status is checked off the UI
thread, expires after four seconds, and falls back to unknown/unavailable.
Original monochrome icon variants follow the Windows system tray light/dark
setting; Explorer taskbar recreation re-adds the icon. Original artwork is also
embedded in the executable and settings homepage; it requires no external request.
The separate optional mouse-transparent feedback overlay is unchanged.

Pinned software also appears in a compact logo strip above the primary taskbar's
work area. Badges show playback muted (`×`), active (`▶`), idle (`II`), disconnected
(`–`) or unknown (`?`). Hover text and accessible button names explain the state;
click opens Settings and right-click uses the same exact-session menu. Partial
or expired observations stay unknown. This strip does not report voice activity,
does not modify Explorer, and still requires live pointer/DPI/focus acceptance.

Settings > **应用与麦克风** lists current readable Windows playback sessions. Pin an
observed software entry to keep its location/output selection across restart in
`apps.json`, separately from the global shortcut binding. The settings page and notification
area show its current native resource icon or a generic placeholder. Removal
changes only the saved entry. Stale, partial or unsupported reads stay unknown;
playback activity does not establish microphone use, voice state or agent work.
Windows software-icon menus offer explicit single-session playback mute/unmute,
with identity checks and a verified readback receipt. Mac application presence
remains read-only. See the
[software state, privacy and lifecycle contract](docs/resident-software.md).

Authenticated same-origin `POST /api/apps/bind` accepts exactly
`{"revision":0,"id":"<saved-entry-id>","device_id":"<microphone-endpoint-id>"}`;
use `device_id: null` to clear it. A non-null endpoint must pass a current exact-device
read before the optional `device_id` is persisted. A stale revision or unavailable
endpoint returns 409 and preserves the previous association. This association
does not set an application's operating-system audio route.

On macOS, Settings > **应用集成** instead lists native running applications.
Pinned bundle locations survive restart in a separate `apps-macos.json`. The
floating panel, menu-bar menu and Dock menu show native icons and Running, Not
running or Unknown. Process presence never establishes playback, recording or
agent work. Missing or stale icons use a placeholder. See the
[macOS native contract](docs/macos-desktop.md) and
[desktop acceptance checklist](docs/design/desktop-candidate.md) for validation limits.

Set `VIBDOCK_PORT` to bind a different loopback port when running acceptance
tests, so they do not disturb a service already on the default one.

## Control a microphone on Windows

```powershell
.\vibdock.exe devices
.\vibdock.exe status --device '<full endpoint ID from devices>'
.\vibdock.exe listen --device '<full endpoint ID from devices>'
```

Choose the microphone by its displayed name and full ID. In Ulanzi Studio, map the voice button to **Ctrl+Alt+Shift+F9**. Keep `listen` running; each press toggles the selected microphone and prints its actual mute state. A conflicting hotkey is rejected. Disconnecting the endpoint produces an error instead of silently switching to another microphone.

Use `mute`, `unmute`, or `toggle` in place of `status` for individual commands. Exit codes are zero on success and nonzero on error. Exiting does not restore or change the microphone state.

**Device mute affects every app using that microphone.** It is different from muting one agent's input or output. The agent must use the selected microphone for this control to affect its audio. Hold-to-talk and device-specific button recognition are not implemented.

## Troubleshooting

- **Shortcut is occupied:** choose another combination, or stop the other `listen`/service instance holding it. A failed edit preserves the previous saved binding.
- **Microphone disappeared:** reconnect the configured endpoint and check its identity. VibDock does not silently switch to another microphone.
- **Page is unauthorized after restart:** reopen configuration from the launcher or tray to obtain a fresh one-use browser code. Never share the service token or bootstrap URL.
- **State is unknown:** check that the service is running. The page resumes polling after a timeout; reopening it renews authorization when needed.
- **Another application owns the port:** resolve that conflict or explicitly choose a different `VIBDOCK_PORT`. Startup will not terminate the other application or silently select another port.

## Application playback controls (unreleased Windows source)

These commands are implemented in current source. A dated two-session
exact-session mute/unmute readback check is recorded in the
[application recognition acceptance record](docs/testing/app-recognition-acceptance.md),
and a repeatable opt-in two-application state check is available below;
audible meeting-level isolation remains unverified, and the published v0.1.0
download does not include them.

Application playback sessions are separate from microphone mute: they let one
program's audio be silenced while everything else keeps playing.

```powershell
.\vibdock.exe app-devices
.\vibdock.exe app-list --endpoint '<full endpoint ID from app-devices>'
.\vibdock.exe app-mute --endpoint '<endpoint ID>' --session '<session ID from app-list>'
```

Use `app-status`, `app-unmute`, or `app-toggle` in place of `app-mute`. Each
command prints a readable summary followed by one line of JSON describing the
session that was actually read back, for example:

```json
{"endpoint_id":"{0.0.0.00000000}.{...}","session_instance_id":"{...}","process_id":1234,"display_name":"Music","process_identity":"pid 1234","muted":true,"volume":1.000000,"volume_valid":true,"active":true}
```

Choose a session by its `session_instance_id`, shown as `session:` in the
readable output. Application playback control is deliberately strict:

- Both `--endpoint` and `--session` are required. There is no default endpoint
  and no "first session" fallback, so a mute cannot land on a device or
  application you did not name.
- The session is re-verified immediately before any change. If it has gone away
  or a different process now owns it, the command fails and says so instead of
  controlling a different session.
- Only the mute state is changed, for that one session. The system master mute,
  the volume of other applications, and microphone state are never touched.
- Mute state is never reported from the request: it is read back from Windows
  after the change, and a mismatch is an error.
- `app-toggle` flips the session's state as read immediately before the write,
  not a value observed earlier. A third-party change in between cannot make the
  command write the inverse of what you asked for.
- A volume the platform does not report is emitted as `"volume":null` with
  `"volume_valid":false`, never as `0`, so "silent" and "unknown" stay
  distinguishable.

Applications that split audio across several processes are **not** grouped
automatically. Where the owning process cannot be resolved, it is reported as
`unknown process` and you choose explicitly rather than relying on a guess from
similar names. Only playback (output) sessions are addressed; this is unrelated
to microphone input mute.

On Linux and macOS these commands report that application audio sessions are
unimplemented rather than returning an empty list.

**A readback failure after a write is not the same as no change.** Once the mute
change has been submitted to Windows, a failure to re-read the result is
reported as "submitted but not confirmed" — the change may already be applied.
Query `app-status` before retrying; repeating `app-toggle` could reverse a mute
that did take effect. A failure *before* the write is different: nothing was
changed, and the command says the session is gone instead.

One session whose state cannot be read is skipped during enumeration rather than
failing the whole endpoint, so an unreadable session belonging to another
process cannot make an unrelated session impossible to control. When that
happens `app-list` prints a warning naming how many sessions were skipped, so a
partial list is never presented as a complete one.

Two kinds of skipped sessions are reported separately, because only one is a
problem. Sessions with no session-instance identifier — system sounds, on most
Windows desktops — are **not addressable** and are never selectable; that is
expected and is counted but not treated as a fault. A session that is
addressable but whose state could not be read is a genuine **could not be read**
warning.

### Reproducible two-application isolation check

`scripts/test-playback-isolation.ps1` is an opt-in Windows-only harness that
repeats the two-application check against real playback sessions. It writes two
short distinct WAV tones to a temporary directory, starts two `pwsh` players
using `System.Media.SoundPlayer.PlayLooping()`, requires the single active
render endpoint from `app-devices`, selects exactly the two sessions owned by
the player PIDs, mutes A, and asserts A reads `muted=true` while B remains
`muted=false` and `active=true`. It then unmutes A, verifies both sessions are
still unmuted and active, and always stops the players and deletes the
temporary files in `finally`.

```powershell
cargo build --locked
pwsh -NoProfile -File scripts/test-playback-isolation.ps1
```

The harness never issues a device-level command and never falls back to another
endpoint or session. It is state-level evidence only: no meeting is joined and
audible output is not measured. Because it needs real Windows audio hardware it
is not a hosted-CI step. See [the harness notes](docs/testing/playback-isolation.md).

## Product direction

`vibdock capabilities` reports documentation-derived integration declarations.
Installed-client verification is pending; the report labels those capabilities
as unknown or unverified rather than claiming a working application connection.

- Configure a binding by pressing a hardware button, choosing a target and action, then testing it.
- Keep input mute, application output mute, and platform voice-session controls separate.
- Provide optional non-focus-stealing desktop feedback, with text/icons as well as color. The corner status indicator is implemented on Windows behind an opt-in that defaults to off; the optional breathing border is planned and not implemented.
- Silence an agent's output during a meeting without changing meeting audio. Current Windows source provides playback commands and authenticated exact-session actions from native software-icon menus. A two-session exact-session state check is recorded, while meeting-level isolation and hardware bindings for these playback actions remain open. The opt-in two-application harness checks session state, not audibility.
- Expose explicit capabilities, results, and queryable state to agents through CLI, local API, and later MCP.
- Add adapters for personal-agent platforms and input tools without coupling hardware to any one vendor.

The control API authenticates callers and validates browser origins. The `serve` preview is read-only, loopback-only, and rejects unexpected Host headers. `service-start` exposes authenticated `/api` configuration and action endpoints, including real microphone status, mute, unmute, and toggle through the Windows audio adapter. A sent shortcut must not be reported as a verified application state change.

See [the interaction specification](docs/design/README.md) and [the clickable design source](docs/design/index.html).

## Development

```powershell
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
node scripts/test-binding-ui.cjs
cargo build --locked --release --target-dir target
```

Audio access is confined to the Windows adapter. The command parser and embedded preview run on all supported build platforms.

The opt-in `scripts/test-playback-isolation.ps1` harness is intentionally not a
CI step: it needs a real Windows audio endpoint and two live player processes.
See [the harness notes](docs/testing/playback-isolation.md).

## Releases

Conventional Commits on `main` feed release-please. It opens a version/changelog PR; merging that PR creates a release, and the same workflow builds Windows x64, Linux x64, and macOS ARM64 archives plus SHA256 files. A release tag alone does not establish that all downloads were built successfully; check the workflow and assets.

With the default GitHub token, bot-created release PRs do not automatically trigger PR workflows. Dispatch CI on the release branch and wait for all checks at its current head before merging. A scoped GitHub App token can automate this later. Repository Actions settings must permit workflow-created pull requests.

### Resident launch behavior

On Windows, double-click to start or reuse one resident service and open authenticated Web Settings. Closing the page keeps the tray and service alive; Exit VibDock stops it. Repeated launches reopen Settings through the same tray owner. The CLI, tray entry and authenticated shutdown are implemented in source; a packaged double-click launcher executable is not published yet. The macOS candidate retains its native panel.

WeChat Input and Codex/Mini are the first integration priorities. WorkBuddy and meeting-friendly per-application audio controls follow; Slack and Tencent Meeting scenarios remain integration-specific acceptance work.

The latest product launch design uses a thin Windows launcher/tray executable alongside the console CLI, both sharing one Rust core/service. The `tray` command implements the tray entry and shares the service core with the CLI; current preview downloads still contain only the CLI.

Integration evidence and shortcut ownership are tracked in [voice integration evidence](docs/integrations/voice-evidence.md). Documentation support does not mean an installed client has been controlled successfully.

## License

Licensed under the [MIT License](LICENSE).

