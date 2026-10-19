# Hardware action acceptance

Checked on 2026-10-19 against a physical Ulanzi AU05 Vibe Key (firmware 4.4.0)
on a Windows x64 desktop. This is a dated single-host record, not a platform
guarantee.

The AU05 part exercised the authenticated service's action API, the generated
trigger, the firmware write/readback path, and one physical action-binding press
that executed a `wait` step. No gamepad was detected by Windows on the host
(USB/PnP/XInput enumeration was empty), so the gamepad adapter has compilation
and unit-test evidence only.

No screenshots are part of this record; the observations were API responses,
device readback state, and scripted UI checks.

## Verified on 2026-10-19

1. **AU05 action binding, generated trigger, and firmware write.** With one AU05
   button free, a hardware action containing a `wait` step was added through the
   authenticated `POST /api/actions/add`. The service generated the trigger
   `Ctrl + Alt + Shift + F1`, saved and armed the binding, wrote the trigger into
   the button, and read the firmware slots back as identical
   (`hardware.state = "provisioned"`). The action then appeared in
   `GET /api/actions` with its source, trigger, steps, and armed state.

2. **Removal and default layout restoration.** The action was removed through the
   authenticated API; the binding disappeared and the button slot was cleared.
   The built-in default direct layout was then restored on the three buttons.

3. **Physical action execution.** With the action binding loaded and its
   generated trigger registered, a physical press of the bound AU05 button
   executed the `wait` step and produced a `recent_actions` entry with
   `source: "action_binding"`, `state: "DONE"`, and
   `detail: "waited 120 ms"`. The first physical attempts produced no host key
   events at all, even though the vendor configuration channel stayed
   responsive and the firmware slot read back correctly. Replugging the 2.4G
   receiver restored the keyboard-forward channel and the press then executed
   the action. This matches the known dongle keyboard-forward-channel freeze;
   it is a recovery step, not a VibDock state.

3. **UI contract checks.** `node --check` passed for the page script, the
   existing DOM test passed, and a DOM harness covered the action flows
   (list/editor/source/steps/commands/save/remove/sync rendering). These are
   scripted DOM checks, not a physical pointer or device run.

4. **Recent-trigger contract.** `recent_actions` includes
   `source: "action_binding"` and `source: "gamepad"` events with a `detail`
   field for completed step summaries; a successful run uses `state: "DONE"`
   with that detail, while direct firmware chords still produce no event. The
   physical press above exercised this contract.

## Not verified

- **The launch → send keys → toggle microphone chain on hardware.** Individual
  spawn, injection, and microphone-readback code paths are implemented and unit
  covered, but the combined chain and its stop-at-first-failure behavior were not
  exercised against live applications and audio devices.
- **Gamepad input and learning.** Windows reported no gamepad; the adapter was
  never opened against a live controller and `latest_button` learning was not
  exercised. The UI empty state was checked only with the empty device list.
- **Command execution.** No local command definition was executed with
  substituted parameters; argv substitution, missing/unknown parameter rejection,
  and failure reporting are covered only by unit tests.
- **Wait timing / service-loop blocking.** The blocking effect of `wait` on the
  accept-loop thread was not measured under a real press.
- **Removing an action while the device is absent.** The removal path succeeded
  with the device present; a missing device may leave the firmware slot
  uncleared.
- **Sleep/wake recovery and Ulanzi Studio coexistence.** Not tested for this
  layer.
- **Hosted CI with hardware.** CI has no AU05 and no gamepad; only protocol,
  validation, persistence, and page code paths run there.
- **A physical pointer/DPI run of the action card.** The action UI evidence is
  scripted DOM only, not an OS-level pointer session.

## Manual acceptance checklist

Run this on a Windows 10/11 x64 desktop with a physical AU05 and, if available, a
HID gamepad. Record the AU05 firmware version, Windows build, device names, and
the exact action definitions used. Do not attach `actions.json`,
`commands.json`, or `shortcuts.json` to a public report; they contain local
executable paths and device identifiers.

**AU05 action binding and firmware**

1. Start the launcher, open Settings > 应用与麦克风, and confirm the 硬件动作
   card is visible in service mode.
2. Free one AU05 button: clear its direct chord (清除该按键直发 /
   `hardware-unbind`) and make sure it has no application binding.
3. Create an action with a harmless step, for example
   `launch` a simple local application, or only a `wait` step, and choose the
   freed button. Save.
4. Confirm the response and `GET /api/actions` show a generated
   `Ctrl + Alt + Shift + F1`-style trigger, `armed: true`, and the expected
   steps; confirm the add response reports `hardware.state = "provisioned"`.
5. Run `vibdock.exe hardware-status --json` and confirm the device reports the
   generated trigger slot/readback through `/api/actions/sync` (the hardware
   status object itself does not list action bindings).
6. Press the physical button. Record what each step did and what appears in
   最近触发: expect `source: action_binding`, the action name, generated trigger,
   `device_button`, and a `detail` string. Confirm that a failed step stops the
   later steps and reports the step index/kind.
7. Press the button with the target application in the background and with a
   different foreground window; confirm `send_keys` goes to whatever is
   foreground (no target-app check) and that `launch`/`toggle_microphone` behave
   as configured.
8. Remove the action. Confirm the binding disappears, the trigger is no longer
   registered, and the button slot is cleared; then restore the desired direct or
   application binding.

**Conflicts and validation**

9. Try to add an action on a button that has a direct chord or an application
   binding; confirm `409` and the conflict message.
10. Try to add an application binding or direct chord on a button that already
    has an action; confirm `409`.
11. Use all twelve `Ctrl + Alt + Shift + F1`-`F12` triggers between actions and
    confirm the next AU05 action is refused with `409`.
12. Try an invalid step (unknown type, `wait` over 10000 ms, unknown command id,
    unknown command parameter, required parameter empty, `send_keys` with an
    unparsable chord); confirm `400` and no saved binding.
13. Add a `launch` step whose application is no longer current; confirm the
    server rejects the stale opaque reference rather than accepting a path.

**Commands**

14. Add a harmless local command definition to `commands.json` with a required
    and an optional parameter; refresh the page command list and confirm only
    id/label/parameters are shown, not executable paths or argv.
15. Build a command step with parameter values, press the action, and record
    whether the executable received the expected separate argv values and what
    happened with a missing required value.
16. Point a command at a shell interpreter locally and confirm that this is a
    local-file decision; verify there is no API field that accepts command text.
17. Corrupt `commands.json`, restart the service, and confirm the action list
    fails closed (bytes preserved, edits disabled) when an action references a
    command.

**Gamepad (only if a HID gamepad is available)**

18. Connect the pad; confirm `GET /api/actions/gamepads` lists it with
    `device_key`, vendor/product ids, `button_usages`, and `connected: true`.
19. Open the action editor, choose 游戏手柄按键, press a pad button, confirm
    `latest_button` updates, and click 使用这个按键.
20. Save a gamepad action and press the physical button; record whether the
    steps ran and whether a 最近触发 `source: gamepad` entry appeared.
21. Unplug/reconnect the pad; confirm the saved binding matches again only when
    the same identity returns. For a serial-less pad, record whether a replaced
    unit is treated as the same or a different device.

**Failure and recovery**

22. Unplug the AU05, save an AU05 action, and confirm the binding persists with a
    hardware failure; reconnect, wake the device, sync, and confirm
    `provisioned`.
23. Let the device reach standby and retry the sync; record the timeout message
    and recovery.
24. Start Ulanzi Studio while VibDock owns the device and record interference for
    an action sync.
25. Restart the service; confirm saved actions remain, `armed` reflects the new
    registration, and 最近触发 is empty.

**API auth**

26. Call each `/api/actions` route without the bearer token and confirm `401`;
    with an unapproved `Origin`/`Host`, confirm `403`. Confirm the read-only
    preview hides the action card and exposes no working action mutation.

**What to record**

- Date, Windows build, AU05 firmware, gamepad model/serial if any.
- The exact action definition (source, steps, command id/parameters) and the
  generated trigger.
- `GET /api/actions` and the add/sync `hardware` results with private paths and
  tokens removed.
- The physical action performed and the `recent_actions` entry with `detail`.
- Any mismatch between the brief and the code as a defect, not a new claim.

Until this checklist has been run and recorded here, every item under "Not
verified" stays unverified.

