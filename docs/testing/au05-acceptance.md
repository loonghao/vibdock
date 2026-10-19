# AU05 Vibe Key direct-binding acceptance

Checked on 2026-10-19 against a physical Ulanzi AU05 Vibe Key (firmware 4.4.0)
and its 2.4G receiver on a Windows x64 desktop. This is a dated single-host
record, not a platform guarantee.

The record has two parts: a temporary protocol probe that talked to the vendor
HID collection directly, and a later live acceptance of the shipped service,
which seeded the default button layout and observed physical presses in the
recent-trigger log. No screenshots are part of this record; the observations
were report frames, decoded slot and entry bytes, service JSON views, and the
user's confirmation of the host behaviour.

## Protocol probe evidence

These checks used separate probe code, not the shipped Rust adapter.

1. **Vendor collection enumeration.** The AU05 exposed a vendor HID collection
   with VID `0xFFF1`, PID `0x00DD`, interface token `MI_03`, product string
   `AU05`, top-level usage page `0xFFFC`, and 64-byte input and output reports.
   The collection was reachable without Ulanzi Studio running.

2. **Stored button slots read back.** Before any write:
   - index 0 stored a right-side `Ctrl + Alt + Shift + F9` chord;
   - index 1 stored `Enter`;
   - index 2 stored `Esc`.

   These matched the Ulanzi Studio profile for the buttons labelled 导航, 确认,
   and 取消, which shows the probe addressed the same slots the vendor
   application uses.

3. **Frame decoding.** Captured response reports were decrypted, and the probe
   reproduced the same TEA frame encoding as the MIT-licensed
   [OpenVibeKey](https://github.com/palaemonboy/OpenVibeKey) reference
   implementation: report ID `0x55`, 64-byte plaintext with a 4-byte header and
   zero padding, 32-round TEA over little-endian 8-byte blocks, the same fixed
   key, and the first 63 ciphertext bytes on the wire.

4. **Byte-exact write and readback.** Button 0's shortcut was rewritten over HID
   and read back as the identical slot: entry bytes `83 01 83 04 83 02 02 42`
   (left Ctrl, Alt, Shift, F9). This confirmed that the write and readback entry
   layout is the same `[page | sign, value]` form, not merely equivalent after
   canonicalisation.

5. **Battery and heartbeat frames.** Broadcast battery and heartbeat reports
   were decrypted. The shipped feature does not parse or report them; this is
   protocol evidence only.

## Live acceptance through the shipped service

1. **First-run default layout written and read back.** With no
   `hardware.json`, the service seeded the built-in layout and wrote it to the
   physical device. The stored entries read back as:
   - button 1: `83 01 83 08` (`Ctrl + Win`, label 微信语音);
   - button 2: `02 28` (`Enter`, label 回车);
   - button 3: `02 29` (`Escape`, label 取消).

   This exercised the shipped Rust HID adapter's shortcut write and readback
   path for the default entries. The same code path clears the fixed-function
   slot and verifies it before it reports `provisioned`; a write that fails
   either readback is reported as `failed`, not as written.

2. **Host behaviour of all three default buttons.** The user confirmed that,
   with the default layout in place:
   - button 1 invoked WeChat Input's voice action;
   - button 2 sent `Enter`;
   - button 3 sent `Escape`.

   These are firmware-emitted physical key chords. No VibDock trigger, dispatch,
   or application target was involved for these default buttons.

3. **Recent-trigger log for a physical application-bound press.** A physical
   press of a button carrying an application binding appeared in the
   `recent_actions` log returned by `GET /api/hardware` and rendered in
   最近触发. The entry carried the button (`device_button`), bound label, saved
   trigger, dispatched chord, and the `DISPATCHED` receipt. This confirms the
   registered-trigger capture path and the log contract.

4. **Negative injection result for `Ctrl + Win`.** An application binding that
   dispatched `Ctrl + Win` through `SendInput` reported `DISPATCHED`, but WeChat
   Input did not invoke its voice action. The same chord emitted by the device
   firmware did invoke it. `DISPATCHED` therefore means Windows accepted the
   injected input events, not that the target application acted. This result is
   why the direct firmware-chord mode exists for `Ctrl + Win`.

Direct firmware chords do not appear in `recent_actions`: the device emits them
without a host hotkey, so the service never sees the press.

## Not verified

These are not claimed anywhere in the feature documentation:

- **Ulanzi Studio coexistence.** A running Studio holding or communicating on
  the same collection, and interference between the two, was not tested.
- **Device replacement identity.** No serial number is stored or bound, so a
  second AU05 with the same vendor collection is indistinguishable. Swapping
  units was not tested.
- **Persistence across power cycles.** The default entries were read back in the
  same session. A full power cycle, receiver re-pair, or reboot was not tested.
- **Sleep/wake recovery.** The device's standby timeout, the resulting write
  timeout on the shipped path, and recovery by pressing a key and retrying were
  not measured.
- **Clearing a firmware slot on `hardware-unbind`.** Removal of a direct chord
  was not exercised on hardware. The service clears a button's firmware slot
  only for a button it previously wrote in the same service instance; after a
  restart or with the device absent, confirm by pressing the button.
- **A per-button application-binding matrix.** Physical application-bound
  presses were observed in the log, but binding each of the three buttons to an
  application and exercising all three as a matrix was not recorded.
- **Hosted CI with hardware.** CI has no AU05. Only the platform-independent
  protocol module, validation, persistence, and page code paths are covered
  there.
- **Knob, built-in microphone, lighting, standby timers, firmware updates,
  audio routing.** Out of scope and never written by this feature.

## Manual acceptance checklist

Run this on a Windows 10/11 x64 desktop with a physical AU05 and a build from
the current source.

**Preconditions**

- The AU05 is charged, its 2.4G receiver is plugged in, and the device is
  powered on.
- Ulanzi Studio is not running. Record whether it is installed but closed.
- Build the current source (`cargo build --locked`). Keep `vibdock.exe` and
  `vibdock-launcher.exe` in the same directory.
- Record the AU05 firmware version, the Windows build, and the chords used.
  Do not attach `shortcuts.json` or `hardware.json` to a public report;
  `shortcuts.json` contains executable paths.

**First-run defaults**

1. Start `vibdock-launcher.exe`, open Settings, and go to 应用与麦克风. Confirm
   the Vibe Key card reports 已连接.
2. With no prior `hardware.json`, confirm the three buttons show the default
   direct chords: button 1 `Ctrl + Win` (微信语音), button 2 `Enter` (回车),
   button 3 `Escape` (取消), and that each reports read-back success.
3. Run `vibdock.exe hardware-status`. Confirm it lists the direct chords and
   that the three button rows report `provisioned`.
4. Press each default button with a target application focused and confirm the
   expected host effect: WeChat Input voice for button 1, `Enter` for button 2,
   `Escape` for button 3. Confirm these presses do **not** appear in 最近触发
   (direct chords bypass VibDock).

**Direct mode**

5. In 硬件直发快捷键, write a different representable chord (for example
   `Ctrl + Shift + Win`) with a label to one button. Confirm the button reports
   `provisioned` and pressing it emits that chord.
6. Clear that direct chord with 清除该按键直发 / `hardware-unbind --button N`.
   Confirm the configuration shows it unbound. Press the button and record
   whether the old firmware chord is still emitted.
7. Try to write a bare `Win` direct chord and confirm it is rejected. Try an
   unrepresentable chord such as `F13` and confirm `400`.
8. Save a direct chord on a button that already carries an application binding
   and confirm `409`; remove the application binding and retry.

**Application-binding mode**

9. Clear a button's direct chord, then save an application binding that names
   it with a free trigger and the dispatch chord from the target application.
   Confirm the button reports `provisioned`.
10. Press the physical button with the target application in the foreground.
    Confirm 最近触发 shows the newest entry with the button, label, trigger,
    dispatched chord, and `DISPATCHED`; confirm the status line mirrors it.
11. Repeat with a different application in the foreground and confirm the
    foreground-scope refusal; nothing is dispatched.
12. Confirm a registered trigger containing `Win` is rejected with `400`, while
    a dispatch chord containing `Win` is accepted.
13. Remove the binding. Confirm the row disappears and `hardware-status` shows
    the button unbound.

**Failure and recovery**

14. Unplug the receiver and save a direct chord or application button binding.
    Confirm the configuration is saved, the hardware status reports a failure,
    and nothing is rolled back. Reconnect, press any AU05 key, then
    同步到设备 / `--sync`; confirm `provisioned`.
15. Let the device reach its standby timeout, then run a sync. Record whether
    the message asks for a key press; press a key, retry, and confirm recovery.
16. Start Ulanzi Studio without quitting VibDock, then run a sync. Record the
    failure or timeout and whether the two applications interfere. Quit Studio
    and retry.
17. Restart the VibDock service with bindings saved. Confirm defaults are not
    re-seeded, the page shows `pending` (not a stale `provisioned`) until a sync
    rewrites the buttons, and 最近触发 is empty again.
18. Power-cycle the device and receiver, then confirm whether the written
    chords survived and whether a sync is needed.

**CLI and API**

19. Run `vibdock.exe hardware-status`, `--json`, and `--sync`. Confirm the JSON
    contains `schema_version`, `platform`, `available`, `enabled`, `connected`,
    `device`, `buttons` (with `direct_chord`, `direct_label`, `direct_state`,
    `direct_error`), `direct` (`revision`, `entries`, `limit`,
    `storage_writable`, `error`), and `recent_actions`.
20. Run `hardware-bind --button N --chord <chord> [--label <text>]` and
    `hardware-unbind --button N`; confirm the button state is printed and the
    JSON view matches.
21. With the service token, call `GET /api/hardware`, `POST
    /api/hardware/direct` and `POST /api/hardware/direct/remove`. Confirm
    `GET /api/shortcuts` entries include `device_button`, and `GET /api/config`
    includes `last_action` and `recent_actions`. Without the token confirm
    `401`; with an unapproved `Origin`/`Host`, confirm `403`.
22. Confirm the design preview (`vibdock.exe serve`) shows no hardware controls
    and never writes to the device.

**What to record**

- Date, host OS build, AU05 firmware version, receiver state.
- The `hardware-status --json` output with the bearer token and private paths
  removed.
- Per-button readback entry bytes and the observed host behaviour.
- The exact physical action and the `recent_actions` entry it produced.
- Any mismatch between the brief and the code as a defect, not a new claim.

Until this checklist has been run and recorded here, items marked unverified
above stay unverified.
