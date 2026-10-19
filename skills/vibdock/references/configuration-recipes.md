# Configuration recipes

Detailed recipes for the [VibDock agent skill](../SKILL.md). Read the main skill
for safety and limitations first. The manual setup equivalent is in
[`docs/guide/installation.md`](../../../docs/guide/installation.md).

## Preconditions

```powershell
vibdock service-status
vibdock hardware-status --json
.\scripts\vibdock-agent.ps1 plugins
.\scripts\vibdock-agent.ps1 actions
```

If the service is not running, `vibdock service-start`. Do not read the service
token into user-visible output.

## Recipe: button 1 → WeChat voice (`Ctrl + Win`)

A direct firmware chord is the reliable path for WeChat Input voice.

1. Confirm the user accepts replacing the current behavior of button 1.
2. If button 1 has an application shortcut or hardware action, remove it first
   (the service rejects a mixed mode).
3. Run:

```powershell
vibdock hardware-bind --button 1 --chord "Ctrl + Win" --label "微信语音"
vibdock hardware-status --json
```

4. Report the readback state (`provisioned`) and the exact slot. Do not claim
   WeChat started a voice session; ask the user to press and verify.
5. If the write fails, report the error; do not try a different device or chord
   silently.

## Recipe: button 2 → open Codex

An AU05 action needs the button to be free of a direct chord and an application
shortcut. The default button 2 is `Enter`, so clear it first with confirmation.

1. Resolve the application reference:

```powershell
vibdock shortcut-apps --json
```

Find the row whose `name`/executable identifies Codex and copy its 64-character
`selection_id`. Do not guess a path.

2. Find the current action revision and existing bindings:

```powershell
.\scripts\vibdock-agent.ps1 actions
vibdock hardware-status --json
```

3. If the button is occupied, explain which binding will be replaced and ask.
   For a direct chord:

```powershell
vibdock hardware-unbind --button 2
```

4. Create the saved action:

```json
{
  "revision": 0,
  "name": "打开 Codex",
  "source": { "kind": "au05", "button": 1 },
  "steps": [
    { "type": "launch", "application_reference": "<selection_id>", "args": [] }
  ]
}
```

Send it to `POST /api/actions/add` with the bearer token. Use the revision from
the latest `GET /api/actions`. A 409 means a source/button/trigger conflict; stop
and explain it rather than editing internal JSON.

5. Check the response's `hardware.state` and `GET /api/hardware`; an AU05 action
   gets a generated trigger written into firmware. Report whether provisioning
   succeeded. The action is saved even if firmware provisioning reports a
   failure, so the error must be surfaced.

6. Ask the user to press button 2 and check `recent_actions` for
   `source: "action_binding"`, `state: "DONE"` and the step detail.

## Recipe: button 3 → long press / double press

Not available yet. The gesture classifiers exist in `vibdock-input`, but the
Windows sentinel-key monitor and persisted gesture bindings are not wired. Do not
claim this can be configured; offer one of:

- a single-press action on a free button;
- a direct firmware chord;
- wait for the gesture issues (#63/#64) to land.

## Recipe: app shortcut → AU05 button

Use the application shortcut path when the target accepts a chord and the user
wants a foreground-only binding.

```powershell
vibdock shortcut-apps --json
vibdock shortcut-add --app "DeepSeek Harness" --trigger "Ctrl+Alt+Shift+F11" --dispatch "Ctrl+Shift+Space" --device-button 2
vibdock shortcut-list --json
```

The device button receives the **trigger** chord in firmware. The service only
dispatches the **dispatch** chord when the bound application owns the foreground
window. `DISPATCHED` is not proof the app started a voice session.

## Recipe: run a saved action from the Pad / agent

```powershell
.\scripts\vibdock-agent.ps1 actions
.\scripts\vibdock-agent.ps1 pad-execute -Id <saved-action-id>
```

The endpoint accepts only a saved id. It cannot create an action or submit a step
list. Report the returned `state` and `detail`; if it fails, report the error and
do not retry a destructive action automatically.

## Recipe: inspect plugins and capabilities

```powershell
.\scripts\vibdock-agent.ps1 plugins
```

Use the `controls` list for AU05 button names/rows and the `actions` list for
integration actions. Do not hard-code button labels; read them from the manifest.
Plugin enable/disable and third-party install are not implemented yet.

## Error handling

| Response | Meaning | Agent action |
| --- | --- | --- |
| 401/403 | missing/expired auth or bad origin | report; reopen Settings; do not expose token |
| 400 | invalid request/step/source | report the message; fix only known fields |
| 404 | unknown saved action id/route | re-list ids; do not invent one |
| 409 | button/trigger/binding conflict | ask before removing anything |
| 502 | saved action execution failed | report step error; do not claim success |

## Confirmation template

Before a mutation:

> I will write **button 2 → 打开 Codex** as a hardware action. Button 2 currently
> has `<current binding>`, which will be cleared first. The service will generate
> a trigger and write it into firmware. Shall I continue?

After a mutation:

> Saved action `<id>`. Firmware state: `provisioned` / `failed: <error>`.
> Press button 2 to verify; the recent log should show `action_binding` /
> `DONE`. I have not verified that Codex itself opened.

