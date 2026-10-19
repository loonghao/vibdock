# Install and configure VibDock

This guide is for a first-time setup: install, connect the Vibe Key, configure the
buttons, and verify what actually happened. The other path is to ask an agent to
do it through the [VibDock agent skill](../../skills/vibdock/SKILL.md); both paths
write the same local configuration.

> Current status: AU05 firmware chords, application shortcuts, hardware action
> sequences and the recent-trigger log have live device evidence. Single/double/
> long gestures, multi-key combos and the Virtual Pad are still source candidates
> or design previews; this guide marks them clearly.

## 1. Requirements

- Windows 10/11 x64.
- Ulanzi AU05 Vibe Key (3 buttons, knob, microphone) and its 2.4G receiver.
- No Ulanzi Studio required. If Studio is running, quit it first: the vendor HID
  interface is exclusive.
- Source builds need Rust stable. End users do not need Node.js or Python.

## 2. Install

### Option A — release archive

1. Download the Windows archive from
   [Releases](https://github.com/loonghao/vibdock/releases).
2. Extract it to a fixed directory, for example `C:\Program Files\VibDock`.
3. Confirm both `vibdock.exe` and `vibdock-launcher.exe` are present.
4. Run `vibdock-launcher.exe`. It keeps the tray service running and opens
   authenticated local Settings.

Candidate builds are not code-signed and may trigger SmartScreen. Verify the
download source and checksum before running.

### Option B — build from source

```powershell
git clone https://github.com/loonghao/vibdock.git
cd vibdock
cargo build --release
.\target\release\vibdock-launcher.exe
```

CLI-only:

```powershell
.\target\release\vibdock.exe service-start
.\target\release\vibdock.exe hardware-status
```

## 3. First launch

1. Plug in the 2.4G receiver and power the Vibe Key.
2. Press any key to wake the device.
3. Run the launcher. The tray icon appears and Settings opens.
4. Closing the browser does not stop the service. Reopen Settings from the tray,
   or stop it with `vibdock service-stop`.

The first run writes the default layout:

| Button | Default |
| --- | --- |
| 1 | Firmware `Ctrl + Win` (WeChat voice) |
| 2 | Firmware `Enter` |
| 3 | Firmware `Escape` |

Each write is read back; `provisioned` means the slot matched.

## 4. Device and conflict rules

- Only one program can own the vendor HID interface. Do not run Ulanzi Studio and
  VibDock at the same time.
- A button can have exactly one mode: application shortcut, direct firmware
  chord, or hardware action. The service rejects conflicts instead of silently
  overwriting.
- If the page says “device not found”, check the receiver, power, wake the device,
  then refresh or run `vibdock hardware-status --sync`.
- The 2.4G receiver's keyboard-forward channel can freeze while configuration
  still works. Replug the receiver or power-cycle the device, then retest.

## 5. Configure a direct firmware chord

Best for chords VibDock cannot register, such as `Ctrl + Win`, or for single keys.

1. Settings → **Apps & Microphone** → **Vibe Key hardware buttons**.
2. Choose a button under **Direct firmware shortcut**.
3. Enter a chord or single key, for example `Ctrl + Win`, `Enter`, `Escape`.
4. Add an optional label, then click **Write direct chord**.
5. The service reads the firmware slot back; a failure shows the reason.
6. Press the physical button and verify the target application.

CLI:

```powershell
vibdock hardware-bind --button 1 --chord "Ctrl + Win" --label "WeChat voice"
vibdock hardware-unbind --button 1
```

A direct chord is emitted by the device. VibDock does not register or dispatch it,
so it may not appear in the recent-trigger log. A successful write only proves the
firmware slot; it does not prove the target app responded.

## 6. Configure an application shortcut

1. Open the target app so it has a foreground window.
2. Settings → **Apps & Microphone** → **Application shortcuts** → New.
3. Pick the application. Refresh if the list is empty.
4. Set a VibDock trigger, for example `Ctrl + Alt + Shift + F10`.
5. Set the chord sent to the app, for example `Ctrl + Shift + Space`.
6. Optionally bind an AU05 button; the trigger is written and read back.
7. Save, press the button, and check the recent-trigger log.

CLI:

```powershell
vibdock shortcut-apps --json
vibdock shortcut-add --app "DeepSeek Harness" --trigger "Ctrl+Alt+Shift+F11" --dispatch "Ctrl+Shift+Space" --device-button 2
vibdock shortcut-list --json
```

`DISPATCHED` means the chord was sent. It does not mean the app started recording
or ran a command.

## 7. Configure a hardware action sequence

Example: open Codex, wait, toggle the microphone, send a voice shortcut.

1. Settings → **Apps & Microphone** → **Hardware actions** → New.
2. Choose an AU05 button or a gamepad button.
3. Add steps:
   - launch an app (server resolves the path);
   - send a chord;
   - toggle a microphone with an explicit endpoint;
   - run a local command with declared parameters;
   - wait 0–10000 ms.
4. Save. An AU05 action gets a generated trigger written into firmware; gamepad
   actions need no firmware write.
5. Press the physical control. The recent log shows `source: "action_binding"`,
   `state: "DONE"` and the step detail.

`commands.json` example:

```json
{"schema_version":1,"commands":[{
  "id":"build",
  "label":"Build project",
  "executable":"C:\\Tools\\build.exe",
  "args":["--project","{project}"],
  "parameters":[{"name":"project","label":"Project","required":true,"max_length":40}]
}]}
```

Commands use executable + argv, never a shell string.

## 8. Gestures and combos (source/design candidate)

The intended model:

- per key: single / double / long press;
- combos: `1+2`, `1+2+3`, `2+3`, held `1+2`;
- 3 buttons → 9+ logical actions.

AU05 single/double/long is wired: the service writes
`Ctrl+Alt+Shift+F10/F11/F12` into the button slots, polls the sentinel chord and
executes the saved action. The poll does not suppress the sentinel chord;
multi-key combos are still in progress. Wake the device before a firmware write
and trust the readback state. Design references: [gesture workbench](../design/prototype-workbench.html)
and [plugin workbench](../design/plugin-workbench.md).

## 9. First-run guide

[`docs/design/onboarding.html`](../design/onboarding.html) is the guided
first-run design:

1. connect the device;
2. choose a template (voice workflow / coding / WeChat Input / blank);
3. confirm the 9 action slots (single/double/long per key; Profile, manual chord or recorded chord);
4. test and finish.

The production Settings page now serves this wizard in service mode. Saving uses
the authenticated `/api/gestures` API and shows a failed firmware readback instead
of claiming success. The standalone page remains the visual reference and the
source of the **Let an agent configure** prompt. The install and configuration
semantics above remain authoritative.

## 10. Configure with an agent

Read the [agent skill](../../skills/vibdock/SKILL.md) and use the helper at
[`scripts/vibdock-agent.ps1`](../../scripts/vibdock-agent.ps1). A user can say:

> Bind button 2 to open Codex, and button 3 long-press to open DSH. Read the
> current configuration first and do not overwrite existing bindings.

The agent should:

1. read `GET /api/plugins`, `GET /api/actions` and `hardware-status`;
2. resolve application references from `shortcut-apps`, never guess paths;
3. explain the intended write and wait for confirmation;
4. after writing, read back firmware/recent events and report
   `provisioned` / `DONE` / the error;
5. never print the token or expose the loopback service to the LAN.

Helper examples:

```powershell
.\scripts\vibdock-agent.ps1 plugins
.\scripts\vibdock-agent.ps1 actions
.\scripts\vibdock-agent.ps1 action-commands
.\scripts\vibdock-agent.ps1 hardware
.\scripts\vibdock-agent.ps1 pad-execute -Id <saved-action-id>
```

## 11. Verify and troubleshoot

```powershell
vibdock hardware-status --json
vibdock shortcut-list --json
vibdock service-status
```

- **No device:** receiver, power, wake key; quit Studio; refresh or `--sync`.
- **No key events but config is fine:** keyboard-forward channel froze; replug.
- **Conflict:** choose another trigger; the service returns 409.
- **Write failed:** read the reported readback error; do not blind-retry.
- **Service not running:** launcher/`service-start`; closing the browser is not
  stopping the service.
- **Security:** loopback only by default. Do not expose it to a LAN directly;
  remote Pad pairing is not implemented.

## 12. Update and uninstall

- **Update:** stop the service (tray Exit or `vibdock service-stop`), replace the
  executables, start the launcher again. Configuration under
  `%LOCALAPPDATA%\VibDock` is preserved.
- **Uninstall:** stop the service, exit the tray, delete the program directory.
  Delete `%LOCALAPPDATA%\VibDock` only if you also want to remove saved bindings.
- **Reset:** stop, back up, then remove `hardware.json` / `actions.json` /
  `shortcuts.json` and restart.

---

简体中文：[installation.zh-CN.md](installation.zh-CN.md) · Back to [README](../../README.md)



