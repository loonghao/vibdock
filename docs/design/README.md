# Interaction design · preview 01

## Intent

Help a person holding a microphone bind a physical control without learning CLI syntax. Keep the local Web setup occasional and let hardware plus a small desktop indicator handle daily use.

## Visual direction

A compact dark hardware workbench inspired by Ulanzi Studio's device-and-action layout. Device vocabulary: input, output, button, dial, binding, session. Colors come from charcoal casing, brushed silver, ivory keycaps, amber selection rings, mint status LEDs, and warm red errors. The device outline plus highlighted binding is the signature; no dashboard metric cards, decorative gradients, or ambiguous green success dots.

Use a 14px body, 17px panel headings, 27px page heading, 8px spacing rhythm, restrained borders, and a single amber primary action. The hardware visual has a material treatment; functional controls stay flat. All controls are keyboard accessible; animations respect reduced-motion preferences.

## Views

1. Bindings: device preview on the left, trigger/target/action editor on the right, with a saved-binding list beside the device. Quick setup starts bounded key capture. Select a key from either the device outline or the list.
2. Audio: distinguish hardware input, hardware output, and application playback sessions. Persist device identity; never silently fall back to another microphone.
3. Integrations: separately show installed detection, connection, available actions, and verified state. Planned integrations remain visibly unavailable.
4. Feedback: preview a corner notification and optional breathing border, including failures.

## Capture contract

The current mock captures keyboard events only while the browser page is focused. It cannot identify the physical device. Real background capture will be performed by the Rust platform adapter, with a bounded capture session, cancellation, timeout, and suspension of matching actions while recording. Raw Input may identify a physical device, but some Ulanzi mappings may arrive as synthetic keyboard events without useful device identity. Report that limitation rather than inventing a device binding.

Direct AU05 Vibe Key provisioning is a separate path from capture. In service mode the Windows adapter writes into one of the device's three managed buttons over the vendor HID collection, clears that button's fixed function first, and reports `provisioned` only after reading both slots back identically. A button can carry one of three things: a saved application binding (its registered trigger is written into firmware; the global registration captures the emitted chord and dispatch keeps the saved `foreground` or `running` scope), a direct firmware chord (no host hotkey and no dispatch; used for chords VibDock cannot register, such as `Ctrl + Win`, and for bare keys such as `Enter` and `Escape`), or a hardware action binding (a generated registered trigger is written into firmware and the press runs an ordered step sequence). Hardware action bindings can also use a HID gamepad button, which needs no firmware write or trigger. On first run with no `hardware.json` the service seeds button 1 = `Ctrl + Win` (微信语音), button 2 = `Enter` (回车), button 3 = `Escape` (取消), skipping buttons that already have an application binding. Live acceptance on 2026-10-19 covered the defaults, all three default buttons' host effects, and an application-bound physical press in the recent-trigger log; the action layer has one AU05 add/write/readback/remove check but no physical execution, and no gamepad was detected on the host. Studio coexistence and other limits remain unverified. Direct firmware chords never appear in the recent log because the device emits them without a host hotkey. The AU05 microphone is a normal Windows capture endpoint, and this path never selects or changes an audio route. See [AU05 direct bindings](au05-direct-bindings.md) and [hardware action sequences](hardware-actions.md).

## Audio and meeting workflows

A global microphone mute affects all applications using that endpoint. Application playback mute targets an actual audio session and must not mute the meeting or system master output. An application's voice-input mute and session start/stop require integration-specific capabilities. Switching audio routes is not assumed to have a universal supported API.

Meeting presets should let a user mute an agent's playback while continuing Slack or Tencent Meeting audio. Apply only explicit selected actions, show partial failures, and never automatically unmute a microphone or restore a stale state when leaving a preset.

## Feedback contract

Distinguish trigger received, action pending, verified success, shortcut dispatched without state verification, and failure. Keep the controlled target visible. A desktop overlay must be non-focus-stealing and mouse-transparent; the current prototype simulates it inside the browser only.

Status: the corner status indicator is implemented on Windows behind an explicit opt-in that defaults to off. Enable it with `VIBDOCK_OVERLAY=1` (`on`, `true`, `yes` accepted); `VIBDOCK_OVERLAY_CORNER` chooses `bottom-right` (default), `bottom-left`, `top-right`, or `top-left`. It is a separate non-activating, mouse-transparent window showing the last completed action (`LIVE`, `MUTED`, a real failure, or unknown) together with the target captured when that action completed. It renders state the service already recorded and never performs or invents an action; a status reports success only from a verified platform readback. The optional breathing border remains **planned** and is not implemented.

It is placed in the taskbar-free work area of the primary monitor, so a left, top, or bottom taskbar is excluded. That work area is a preference, not a guarantee: if `SPI_GETWORKAREA` cannot be read the window falls back to the full monitor rectangle, which spans any taskbar band, on the grounds that a misplaced indicator is still better than one that never appears. Multi-monitor placement is not handled — only the primary monitor is considered.

The indicator has not been accepted against live Windows: no run has observed it on a real desktop, so typing/focus, multi-monitor behaviour, and failed-action feedback are unverified in practice. Unit tests cover the status contracts without creating a window.

## Agent and plugin direction

Adapters declare actions, scope, prerequisites, and state-query capability. CLI and authenticated loopback HTTP share execution contracts; MCP remains planned. Detection is not connection, connection is not action success. Do not expose arbitrary command execution through a Web binding endpoint.

## Prototype limits

Design-preview mode uses mock devices and browser-local drafts. Authenticated service mode can read real Windows capture devices, persist configuration, and register a configurable Windows global shortcut. Platform voice integrations remain unverified. Direct AU05 provisioning has live device readback evidence for the first-run default layout and an application-bound physical press captured in the recent-trigger log. Hardware action bindings have live evidence only for one AU05 add → firmware write/readback → remove flow; physical execution, live command execution, and any gamepad input are unverified (no gamepad was detected on the acceptance host). Studio coexistence, device replacement, power-cycle persistence, and sleep/wake recovery remain unverified.

Design-preview bindings are saved to the browser's own `localStorage` under `vibdock.design.bindings.v2`. They are per-browser drafts and are not shared with the resident service. Service mode uses the authoritative service configuration and never writes these drafts.

## Service mode versus design-preview mode

The page runs in one of two modes, reported by `GET /health` and shown in the header badge and banner.

In **design-preview mode** (`audio_control: false`, no token published) the service is read-only. The page keeps its previous behaviour: devices and applications are mock data, actions only preview feedback, and saves go no further than the browser draft. No authorization is offered because there is nothing to authorize against. This mode does not seed or write the AU05 default layout.

In **service mode** (`audio_control: true`) the backend exposes an authenticated API. The page then:

- accepts a one-use bootstrap code when the launcher opens the page with one, exchanges it for a scoped session token, and erases the code from the address bar so it cannot be replayed from a shared link. The session token is held in memory only — never in `localStorage`, never in the URL, never logged — and is sent as `X-Vibdock-Token` on each request. The bootstrap endpoint is service-owned;
- offers manual token paste only as an explicitly labelled advanced troubleshooting route; the normal desktop flow uses one-use browser codes;
- verifies any token against `GET /api/config` before treating the session as authorized, and reports an invalid token as such;
- loads real capture devices from `POST /api/actions/devices` and requires one to be selected before saving;
- writes the single microphone/shortcut pair with `POST /api/config` and renders the backend-confirmed configuration; a failed save preserves the previous binding and no browser-local draft is written;
- calls `POST /api/actions/{status,mute,unmute,toggle}` for the test action and distinguishes a verified outcome (the service read back a real mute state) from a request that was accepted but not verifiable.

The global shortcut targets one selected microphone. The service reports whether Windows registered that shortcut; browser capture alone is not registration or physical-device evidence. Pinned Windows software entries can also retain an exact microphone association and expose explicit status/mute/unmute controls for that endpoint. The association is metadata, not an application input-route change. Application `start_voice`, `end_voice` and `input_routing` remain `not_implemented` and cannot be invoked.

The backend stores one global microphone binding — a device plus a hotkey — and
up to eight per-application shortcut bindings. Service mode presents one
microphone editor with real microphone choices, plus an application-shortcut
section fed by the current window-application scan. Per-key hardware drafts
belong to design-preview mode. Software microphone associations are separate
from the global shortcut; saving one does not register another shortcut or
change the existing binding.

## Interaction behavior

In service mode, choose a microphone, capture a shortcut, test its endpoint and save the single binding. Read saved configuration and registration status back from the service. Software association forms save against the current revision; a conflict or unavailable microphone leaves the prior association intact. A disappeared saved microphone stays unavailable and never selects another endpoint silently.

The same page shows a Vibe Key card in 应用与麦克风 when the service is authorized. It lists the three managed buttons with their application binding or direct chord and provisioning state, a 最近触发 list of completed captured triggers, and a 硬件直发快捷键 form. On first run the service seeds and writes the default layout; 同步到设备 re-detects the device and rewrites every saved button. Saving a binding or a direct chord writes it and reads the slots back; a device failure keeps the saved configuration and is reported separately. A button cannot hold an application binding, a direct chord, and a hardware action at once, so the later save is refused until the other is cleared. A registered trigger cannot contain `Win`; a direct chord may, and bare keys are accepted only by direct mode. Direct chords bypass the host and never appear in 最近触发. Quit Ulanzi Studio while VibDock owns the device; a sleeping vendor channel times out and asks for an AU05 key press before retrying.

The 硬件动作（按键 → 动作序列） card binds one AU05 button or HID gamepad button to 1-8 ordered steps (launch, send keys, toggle microphone, run a local command, wait). An AU05 action gets a generated trigger, registered and written into the button with readback; a gamepad action is learned by pressing a pad button (`latest_button`) and needs no trigger or firmware write. The editor reorders/deletes steps and cannot create command definitions; the command selector only lists sanitized entries from local `commands.json` and fills their declared parameters. Execution stops at the first failing step; results appear in 最近触发, while a firmware write failure after save is shown as a warning with a sync retry. The card is hidden in the read-only preview; gamepad device polling happens only while learning.

The following draft interactions apply to design-preview mode. Setup follows the three guided steps: capture a key, choose a target and action, then test and save. The step indicator marks a step satisfied only after the user actually performs it and flags one step as active at a time.

Switching to a different key while a draft is unsaved asks for confirmation before discarding it, so an entered shortcut is never lost silently. The same draft can also be dropped explicitly with the discard button, which appears only while the editor differs from what is saved.

Because the mock cannot identify a physical device, capture still reports that the source device is unconfirmed rather than inventing one. Saving a shortcut already assigned to another key opens an explicit replace-versus-recapture choice instead of silently overwriting it; replacing removes the older binding. Saving with no shortcut is blocked with an inline field error, and the status line renders failures in the error style rather than the success style.

The scope note under the action selector always states what a control affects. Device input mute warns that every application using that microphone is affected, while application output mute states that meeting and system master output are not.

## Feedback states

The feedback view previews each result state separately: trigger received, action pending, verified success, dispatched without verification, and failure. The status line and toast carry text for every state, so a state is never conveyed by color alone. The breathing border remains opt-in and follows the reduced-motion preference.

## Service API contract

`service-start` runs the authenticated service. It binds loopback only, generates a bearer token per launch, and records it with the bound port in the service state file.

- Authentication: `Authorization: Bearer <token>`, or `X-VibDock-Token`. Compared in constant time; a missing or wrong token yields 401. The token comes from the OS entropy source, and entropy failure fails closed rather than falling back to a weaker generator.
- Host and origin: the `Host` header must be the bound loopback address and any `Origin` must be the same loopback origin, otherwise 403.
- `GET /api/config` and `POST /api/config` read and patch `device_id` and `hotkey`. Values are plain strings under 512 bytes; a malformed patch yields 400. Writes are persisted atomically and reloaded on the next start, so a restart keeps the saved device and hotkey.
- Request bodies are bounded: an oversized or non-UTF-8 body yields 413 and is never parsed or truncated at an arbitrary byte offset.
- No arbitrary command or shell endpoint is exposed. `/api/actions/{devices,status,mute,unmute,toggle}` run through the same closed allowlist and the shared Windows adapter; an unrecognised verb is a 404 that never reaches the adapter. Actions always name their endpoint, taken from the request body or the user's saved device, never a platform default, so an action cannot silently apply to a microphone the caller did not choose. Results report the mute state read back from the device, and a failed action is an error rather than an assumed success.
- Single instance: a second launch reuses the running service only after the listener on that port answers a fresh nonce at `POST /instance/challenge` with an HMAC-SHA256 proof derived from the recorded token. The discovery request does not transmit the bearer. Browser handoff and shutdown prove ownership again on the same pinned IPv4 connection used for the privileged request, sharing one two-second deadline; they never reconnect or fall back to IPv6 after proof. A successful challenge permits at most one additional exchange. Atomic state writes use exclusive fresh temporary files, created with mode 0600 on Unix; Windows protection still relies on inherited directory ACLs. TCP reachability or HTTP 200 alone is not accepted, so an unrelated process that inherits the port is not mistaken for our service. A stale marker is recovered only after proving no owner answers; if an unrelated process holds the port, startup fails loudly and leaves the marker in place.
- `service-status` reports whether a service is listening. `service-stop` performs a real authenticated shutdown: it posts to the running service's `/api/shutdown` with the recorded token and waits for the port to be released, so an unrelated process on that port is never stopped by name. `tray` (Windows) runs a hidden owner and notification-area icons with Settings, Service status and Exit VibDock, sharing the same service core as the CLI.
- Windows `POST /api/apps/bind` accepts only `revision`, saved-entry `id` and `device_id` (string or explicit null). A current exact-device read validates a non-null microphone before its optional ID is atomically persisted in `apps.json`. Old entries without this field are unbound. The association and microphone endpoint controls do not set application input routes or start/end voice sessions. See the [software contract](../resident-software.md).
- `GET /api/shortcuts` lists saved per-application bindings and their live registration state. `POST /api/shortcuts/add` accepts only `revision`, a fresh opaque `application_reference`, `trigger`, `dispatch`, an optional `label`, an optional `scope` (`foreground` default, or `running` for a background integration), and an optional `device_button` (`0`, `1`, `2`, or explicit null for no button; anything else is `400`); `POST /api/shortcuts/remove` accepts a revision and id. Paths are resolved server-side. A saved trigger is registered globally; its dispatch chord is injected only when the saved executable owns the foreground window, or, for an explicit `running` scope, when the executable is running and the chord is sent to the current foreground application. A registered trigger must not contain `Win` or be a bare key (`400` otherwise); a dispatch chord may contain `Win`. A binding that names a device button must carry an AU05-expressible trigger and the button must not already hold a direct chord or a hardware action (`400`/`409` otherwise). Shortcut responses may include a top-level `hardware` status. The result is reported as dispatched, never as a verified application state. See the [software contract](../resident-software.md).
- `GET /api/hardware` reports AU05 detection, the three managed buttons (application status plus `direct_chord`, `direct_label`, `direct_state`, `direct_error`), the `direct` object (`revision`, `entries`, `limit`, `storage_writable`, `error`), and `recent_actions`; `POST /api/hardware/sync` re-detects the device and rewrites every saved application binding and direct chord. `POST /api/hardware/direct` accepts only `revision`, `button` (`0`-`2`), `chord`, and an optional `label`; `POST /api/hardware/direct/remove` accepts `revision` and `button`. Direct chords are firmware-emitted with no host hotkey and no dispatch, accept a chord or a bare key (except bare `Win`), and are stored in `hardware.json`; a button with an application binding or a hardware action returns `409`; a direct chord equal to a registered application or microphone trigger is rejected with `409`, and a new application trigger equal to an existing direct chord is likewise rejected. All routes use the same bearer, `Host` and `Origin` checks as the other `/api` routes. A button's combined `state` is `provisioned` (written and read back identical), `failed`, `pending`, or `unbound`; `direct_state` uses the same labels except `unbound` and is `null` when no direct chord exists. A platform or mode that cannot control hardware reports `available: false`. The adapter writes only the trigger/chord and clears the fixed function for buttons 0-2; application mode never passes it the executable path or dispatch chord, and it does not change audio routes. `provisioned` is a firmware readback, never an application or voice state. The hardware object does not list hardware action bindings. See [AU05 direct bindings](au05-direct-bindings.md).
- `GET /api/actions` returns saved hardware actions (`revision`, `bindings` with id/name/source/trigger/steps/armed/error, `limit` 16, `storage_writable`, `error`); `GET /api/actions/commands` returns sanitized command definitions; `GET /api/actions/gamepads` returns HID gamepads with `device_key`, vendor/product ids, `button_usages`, `connected`, and the learning-only `latest_button`. `POST /api/actions/add` accepts only `revision`, `name`, `source` (AU05 button `0`-`2` or gamepad `device_key` + `usage` + `label`), and 1-8 `steps` (`launch`, `send_keys`, `toggle_microphone`, `command`, `wait`); `POST /api/actions/remove` accepts `revision` and id; `POST /api/actions/sync` rewrites every AU05 action trigger. Paths and command argv stay server-side: a launch step uses an opaque application reference resolved by the service, and a command step names a local definition plus string parameters, never a shell string. An AU05 action gets a generated `Ctrl + Alt + Shift + F1`-`F12` trigger that is registered and written to firmware; a gamepad action has no trigger or firmware write. AU05 add responses may carry `hardware:{button,state,error}` and sync returns a `hardware` array; a firmware failure keeps the saved action. Steps execute in order and stop at the first failure, with results reported in `recent_actions`; there is no action-execute endpoint. A button can carry an application shortcut, a direct chord, or an action, not more than one. See [hardware actions](hardware-actions.md).
- `GET /api/config` also returns `last_action` and `recent_actions`. The recent-trigger log is in memory only, newest last and bounded to 20 entries, and each entry has `sequence`, `at` (milliseconds since the Unix epoch), `source` (`vibe_key`, `application_shortcut`, `action_binding`, `gamepad`, or `microphone`), `label`, `trigger`, `dispatch`, `device_button`, `state`, `error`, and `detail`. `label` is the binding or action name (the endpoint id for `microphone`), and `detail` carries the completed action-step summary for action sources; a successful action run uses `state: "DONE"`. It records that a registered trigger was received and what was sent; `DISPATCHED` does not mean the application acted, and direct firmware chords never appear in it.
- `VIBDOCK_PORT` overrides the bound loopback port so acceptance runs can use a private port instead of disturbing a service on the default one.

The `serve` preview remains read-only and unauthenticated: it serves the page and reports `design-preview` mode.


## Resident-service lifecycle requirement

The Windows source now starts by double-clicking the launcher, reuses one local
service, and opens authenticated Web Settings. Clicking the primary tray icon,
its Settings menu, or launching again requests a fresh one-use browser code.
Closing the browser leaves the tray and service running; **Exit VibDock** performs
authenticated shutdown. The old Windows resident panel is removed.
The independent optional mouse-transparent feedback indicator remains separate.

Repeated launches reuse the existing service and tray owner. Exiting unregisters
hotkeys and stops listeners without changing microphone mute state. Settings
navigates to the proved service's actual bound address, never a guessed port.
See [candidate evidence and platform limits](desktop-candidate.md).

## Delivery priorities

P0: persist and test bindings; authenticated local configuration and agent actions; resident service lifecycle; Windows hardware capture and device-state readback; WeChat Input shortcut integration; Codex/Mini capability verification and integration.

P1: application output mute with meeting isolation, non-focus-stealing feedback, WorkBuddy integration, extensible adapter contracts and Agent CLI/API/MCP.

P2: Slack/Tencent Meeting scenarios, more hardware/platform adapters, and genuinely supported macOS/Linux audio backends. Cross-platform build success is not platform audio acceptance.

Latest launch decision: use two thin Windows entry executables (a double-click launcher/tray application and a console CLI) backed by the same Rust core and one resident service. This supersedes the earlier single-entry launch proposal; it does not duplicate business logic. The published v0.1.0 preview still ships only the CLI executable; current Windows source builds and packages both entries.

### Resident hotkey action feedback (Windows source)

Authenticated `GET /api/config` includes `hotkey_live` (the service currently holds a Windows registration), `activation_error`, and `last_action`:

```json
{"sequence":0,"state":null,"error":null}
```

The sequence starts at zero for each service instance and advances after each handled hotkey action. A successful audio readback sets `state` to `LIVE` or `MUTED`; failure clears `state` and sets `error`. Registration and saving alone do not create a successful action. Each queued press is handled separately; Windows `MOD_NOREPEAT` suppresses held-key repeats. This status is a completed action result, not a continuous meter of changes made by other applications. Clients must reset their event cursor on service restart.

The audio and global-hotkey adapters remain Windows-only. Physical Ulanzi identity, hold-to-talk and application-specific voice controls require separate implementation and host acceptance. The implemented optional Windows overlay still requires host acceptance.

## Persisted software observation

The authenticated Software page supports pin/remove for observed Windows playback
locations, persistent per-entry microphone associations, and native icons with
explicit partial/unknown status. Microphone controls target that exact saved
endpoint; native software-icon menus control one explicitly selected playback
session. Neither path implements application input routing or voice start/stop. See
[resident software contract](../resident-software.md) for API, lifecycle, privacy
and real-device evidence limits.

## Plugin workbench direction (proposal)

The current Settings page is feature-oriented; the next direction is device-first and plugin-oriented. The architecture proposal is [plugin-workbench.md](plugin-workbench.md). The recommended visual target is [prototype-clean.html](prototype-clean.html); the earlier three-direction explorations are in [prototype-workbench.html](prototype-workbench.html) (?variant=A|B|C). These are explorations, not production pages: they do not persist, do not call the service, and do not change any acceptance claim. The clean light layout is the visual target; A/B/C remain references for the information architecture.

The gesture direction adds three slots per physical control (单击 / 双击 / 长按), so three AU05 buttons become nine actions. The timing classifier is unit-tested in src/gesture.rs and the multi-button matcher in src/combo.rs (sequences, holds, longer-sequence precedence); the Windows sentinel-key hook and profile wiring are not implemented yet. The prototype's 单击 / 双击 / 长按 tabs show the intended UI.
