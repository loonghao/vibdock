# Virtual Pad / extension screen

Status: prototype and first execution endpoint, 2026-10-19. The clickable UI is
[`prototype-pad.html`](prototype-pad.html) (try `?page=voice` or `?edit=1`).
It is not yet served by the service, and the pairing flow is not implemented.

## Concept

The physical Vibe Key has three buttons and a knob. A Pad view turns the same
profiles into **large, touch-first virtual keys** on a phone, tablet or second
screen:

- first tile can be a shortcut chord (`微信语音 · Ctrl + Win`);
- second tile can be an application action (`Codex 语音`);
- other tiles can mute a microphone, launch DSH, execute a multi-button combo,
  or run a saved command.

This is not a remote OS keyboard. The Pad page is a browser surface; clicking a
tile asks the resident Rust service to execute a **saved** action/profile. The
service still owns input injection, process launch and microphone control, and
the browser never gets those capabilities directly.

## Layout

The prototype uses a 3 × 2 landscape grid, two rows filling the screen, and a
page switcher (`工作 / 语音 / 媒体`). Each tile has:

- a number (`01` … `06`) and a line icon;
- a large title and a one-line result;
- a kind badge (`快捷键 / 应用 / 组合 / 设备`) and the exact action summary;
- a busy state and a toast after execution.

Portrait falls back to two columns. Targets are at least 140–180 px tall, so the
layout is usable with a thumb or a touchscreen laptop. `编辑` switches to a
configuration sheet; `全屏` uses the browser Fullscreen API. No hover-only
information is required.

The Pad is a **separate route**, not another card in Settings:

```
/            Settings / workbench
/pad         touch-first virtual keypad (future route)
/pad?page=…  page selection
```

## Data model

Pages are projections of the same profiles used by physical buttons:

```json
{
  "pages": [
    {
      "id": "work",
      "name": "工作",
      "tiles": [
        {
          "id": "wechat",
          "label": "微信语音",
          "icon": "mic",
          "kind": "shortcut",
          "detail": "Ctrl + Win",
          "binding_id": "64hex"
        },
        {
          "id": "codex",
          "label": "Codex 语音",
          "icon": "code",
          "kind": "action",
          "detail": "启动 + Ctrl + Shift + Space",
          "binding_id": "64hex"
        }
      ]
    }
  ]
}
```

A tile points at a saved action/profile id. The web page never posts a step list
or an arbitrary chord as a new action.

## Executed API

The first endpoint is implemented:

```
POST /api/pad/execute
Authorization: Bearer <session token>
Origin: same-origin
Content-Type: application/json

{"id":"<saved action id>"}
```

Success:

```json
{"state":"DONE","name":"按钮3测试动作","detail":"waited 120 ms"}
```

Failure keeps the same shape and sets `state: "FAILED"` with `error`; a bad id is
404, a missing id is 400. The action must already exist in `actions.json`; the
endpoint cannot create one. It records a recent event with `source: "pad"` and
the same `detail` semantics as a physical press.

Future:

- `GET /api/pad` returns pages/tiles generated from profiles and installed
  plugins (the prototype currently uses mock data).
- `POST /api/pad/pair` exchanges a short-lived, one-use code shown in Settings
  for a browser session. No token is ever placed in the URL.
- `PATCH /api/pad/layout` saves tile order/pages after an authenticated edit.

## Pairing and network boundary

The service currently binds loopback. A Pad opened on the **same computer**
(second monitor or tablet in USB/desktop mode) can use the normal authenticated
session. A tablet on the LAN would require an explicit opt-in network listener,
pairing and a stricter origin policy; do not expose the current loopback service
directly to the LAN.

Pairing design:

1. Settings shows a QR/short code with a 60-second lifetime.
2. The Pad opens `/pad?pair=<code>`.
3. The browser exchanges the code for a session; the code is consumed once.
4. The page never stores the bearer token in `localStorage`; a same-site,
   HttpOnly session cookie is preferred if the listener is expanded beyond
   loopback.
5. Revoking the pad device is a single action in Settings.

Until that exists, the Pad is a same-machine/second-screen feature.

## Interaction rules

- Tapping a tile executes its saved action once. Long-press is not overloaded
  for edit; editing is an explicit mode.
- A virtual tile representing a combo executes the combo action directly. The
  Pad does not try to infer a physical 1+2/1+2+3 press from two finger taps:
  that would be ambiguous with two independent actions. Multi-touch chord
  capture is out of scope for the first Pad.
- Physical gesture semantics (single/double/long, multi-key combos) stay on the
  device path; the Pad is a projection of the resulting profiles.
- `DONE` means the service executed the saved steps. It does not prove the
  target application changed state or that a voice session started.
- A failed tile returns to idle with a readable error; it does not retry
  silently.
- The Pad respects `prefers-reduced-motion`, safe-area insets and 44 px targets.

## Plugin integration

The Pad is generated by the host, not by each plugin:

- `DevicePlugin` contributes controls and profiles.
- Integration plugins contribute actions/default profiles.
- The host maps saved profiles to Pad tiles and pages.
- A future schema allows a plugin to declare a `pad` contribution with tile
  groups, but the host still owns rendering and execution.

This keeps the Pad a view of the profile engine, not a second hard-coded
configuration surface.

## Implementation status

Done:

- live check on 2026-10-19: a temporary saved action (`wait 80 ms`, gamepad source) was executed through `POST /api/pad/execute`; it returned `{"state":"DONE","detail":"waited 80 ms"}` and produced a `recent_actions` entry with `source: "pad"` and the same detail. The temporary action was then removed;
- touch-first prototype (`prototype-pad.html`), light layout, pages, edit mode,
  fullscreen, busy/toast states;
- `POST /api/pad/execute` with auth/origin checks and saved-action lookup;
- `source: "pad"` recent event and `DONE`/`FAILED` result shape;
- `steps_for_id`/`name_for_id` on the saved action registry, with a unit test.

Not done:

- `/pad` static route and a real Pad page wired to the service;
- `GET /api/pad` projection from profiles;
- pairing/QR and a network listener for a LAN tablet;
- saving Pad page/tile layout;
- live touch/pen acceptance on a real tablet.

## Acceptance

1. The Pad runs on a 1024×768 tablet viewport without horizontal scroll; tiles
   are at least 140 px tall and reachable by touch.
2. A tap on a saved action shows busy → DONE/FAILED and records one recent event.
3. The request cannot create an action, submit a step list, or execute an
   unknown id.
4. The page works with no physical device connected; physical-only tiles show a
   clear unavailable state.
5. Closing the Pad never stops the service or clears physical bindings.
6. The same profile is shown on the physical button and the Pad tile; editing
   one is reflected after refresh.

