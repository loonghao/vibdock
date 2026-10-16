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

Design-preview mode uses mock devices and browser-local drafts. Authenticated service mode can read real Windows capture devices, persist configuration, and register a configurable Windows global shortcut. Physical hardware delivery and platform voice integrations remain unverified.

Bindings are saved to the browser's own `localStorage` under `vibdock.design.bindings.v2`, so they survive a reload or reopen in the same browser. This is a draft, not the authoritative configuration: it is per-browser and is not shared with the resident service.

## Service mode versus design-preview mode

The page runs in one of two modes, reported by `GET /health` and shown in the header badge and banner.

In **design-preview mode** (`audio_control: false`, no token published) the service is read-only. The page keeps its previous behaviour: devices and applications are mock data, actions only preview feedback, and saves go no further than the browser draft. No authorization is offered because there is nothing to authorize against.

In **service mode** (`audio_control: true`) the backend exposes an authenticated API. The page then:

- accepts a one-use bootstrap code when the launcher opens the page with one, exchanges it for a scoped session token, and erases the code from the address bar so it cannot be replayed from a shared link. The session token is held in memory only — never in `localStorage`, never in the URL, never logged — and is sent as `X-Vibdock-Token` on each request. The bootstrap endpoint is service-owned;
- offers manual token paste only as an explicitly labelled advanced troubleshooting route; the normal desktop flow uses one-use browser codes;
- verifies any token against `GET /api/config` before treating the session as authorized, and reports an invalid token as such;
- loads real capture devices from `POST /api/actions/devices` and requires one to be selected before saving;
- writes configuration with `POST /api/config` and updates the page draft **only after the backend confirms the write**, so a failed save never replaces what is actually configured;
- calls `POST /api/actions/{status,mute,unmute,toggle}` for the test action and distinguishes a verified outcome (the service read back a real mute state) from a request that was accepted but not verifiable.

Only the microphone device is offered as a target; Codex, WorkBuddy, and WeChat Input remain labelled unsupported and cannot be selected, so a selection never implies integration support that does not exist. Global hotkey and hardware-origin claims are likewise not made: browser key capture is not global registration, and the page says so.

The backend stores exactly one binding — a device plus a hotkey — and the page represents it that way, in its own section, rather than attributing it to whichever hardware button happens to be selected. Per-key bindings are not a backend capability; the hardware buttons in the device view select which draft is being edited and remain explicitly a draft concept until the service grows per-key configuration.

## Interaction behavior

Setup follows the three guided steps shown above: capture a key, choose a target and action, then test and save. The step indicator marks a step satisfied only after the user actually performs it — choosing an action is not credited merely because the selector shows a default — and flags one step as active at a time.

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
- `service-status` reports whether a service is listening. `service-stop` performs a real authenticated shutdown: it posts to the running service's `/api/shutdown` with the recorded token and waits for the port to be released, so an unrelated process on that port is never stopped by name. `tray` (Windows) runs the resident panel and notification-area icon with Settings, Service status, Hide, and Exit VibDock, sharing the same service core as the CLI.
- `VIBDOCK_PORT` overrides the bound loopback port so acceptance runs can use a private port instead of disturbing a service on the default one.

The `serve` preview remains read-only and unauthenticated: it serves the page and reports `design-preview` mode.


## Resident-service lifecycle requirement

The Windows source now starts by double-clicking the launcher, reuses one local
service, and shows a small interactive floating panel. Settings opens from its
button or right-click menu with a one-use browser code. Closing the panel, pressing
Escape or closing the browser leaves the service running. The taskbar/tray entry
restores the panel; **Exit VibDock** is the discoverable authenticated shutdown.
The independent optional mouse-transparent feedback indicator remains separate.

Repeated launches reuse the existing service and panel. Exiting unregisters
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

The native adapter remains Windows-only. Physical Ulanzi identity, hold-to-talk, desktop overlay, and application-specific voice controls require separate implementation and host acceptance.

## Persisted software observation

The authenticated Software page now supports the read-only Windows resident
software increment: pin/remove current observed playback locations, preserve pins
across restart, and show native resource icons with explicitly partial/unknown
status. It does not activate the planned voice integrations above. See
[resident software contract](../resident-software.md) for API, lifecycle, privacy
and real-device evidence limits.
