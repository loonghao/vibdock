# Interaction design · preview 01

## Intent

Help a person holding a microphone bind a physical control without learning CLI syntax. Keep the local Web setup occasional and let hardware plus a small desktop indicator handle daily use.

## Visual direction

A compact dark hardware workbench inspired by Ulanzi Studio's device-and-action layout. Device vocabulary: input, output, button, dial, binding, session. Colors come from charcoal casing, brushed silver, ivory keycaps, amber selection rings, mint status LEDs, and warm red errors. The device outline plus highlighted binding is the signature; no dashboard metric cards, decorative gradients, or ambiguous green success dots.

Use a 14px body, 17px panel headings, 27px page heading, 8px spacing rhythm, restrained borders, and a single amber primary action. The hardware visual has a material treatment; functional controls stay flat. All controls are keyboard accessible; animations respect reduced-motion preferences.

## Views

1. Bindings: device preview on the left, trigger/target/action editor on the right. Quick setup starts bounded key capture.
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

## Agent and plugin direction

Adapters declare actions, scope, prerequisites, and state-query capability. CLI, future authenticated loopback HTTP API, and MCP share the same execution contracts. Detection is not connection, connection is not action success. Do not expose arbitrary command execution through a Web binding endpoint.

## Prototype limits

All device/application data is mock data. Save means page memory only. No audio, global capture, desktop overlay, app detection, or plugin loading is implemented by this page. The embedded Rust preview serves the same page read-only on Windows, Linux, and macOS.

## Service API contract

`service-start` runs the authenticated service. It binds loopback only, generates a bearer token per launch, and records it with the bound port in the service state file.

- Authentication: `Authorization: Bearer <token>`, or `X-VibDock-Token`. Compared in constant time; a missing or wrong token yields 401. The token comes from the OS entropy source, and entropy failure fails closed rather than falling back to a weaker generator.
- Host and origin: the `Host` header must be the bound loopback address and any `Origin` must be the same loopback origin, otherwise 403.
- `GET /api/config` and `POST /api/config` read and patch `device_id` and `hotkey`. Values are plain strings under 512 bytes; a malformed patch yields 400. Writes are persisted atomically and reloaded on the next start, so a restart keeps the saved device and hotkey.
- Request bodies are bounded: an oversized or non-UTF-8 body yields 413 and is never parsed or truncated at an arbitrary byte offset.
- No arbitrary command or shell endpoint is exposed. `/api/actions/echo` exists only to prove the authenticated action path.
- Single instance: a second launch reuses the running service only after the listener on that port answers an authenticated `GET /instance` challenge with the recorded token. TCP reachability alone is not accepted, so an unrelated process that inherits the port is not mistaken for our service. A stale marker is recovered only after proving no owner answers; if an unrelated process holds the port, startup fails loudly and leaves the marker in place.
- `service-status` reports whether a service is listening; `service-stop` reports status and defers process shutdown to the tray Exit item or the service process.
- `VIBDOCK_PORT` overrides the bound loopback port so acceptance runs can use a private port instead of disturbing a service on the default one.

The `serve` preview remains read-only and unauthenticated: it serves the page and reports `design-preview` mode.

## Resident-service lifecycle requirement

The product version must start by double-clicking the executable, launch one local service, and open its configuration page. Closing the browser does not stop the service. A Windows notification-area icon provides Open configuration, status, and Exit. A separate small floating status indicator may be optional; it is not a replacement for a discoverable Exit control.

Repeated launches must discover and reuse the existing service instead of starting a second listener. That reuse check is implemented: the published instance record is only a candidate, and the listener on that port must answer an authenticated `GET /instance` challenge with the recorded token before it is trusted. Hardware and authorized agents connect while that service is alive. Exiting unregisters hotkeys and stops listeners without unexpectedly changing audio mute state. Open configuration must navigate to the actual bound local address, not a guessed port.

Remaining lifecycle work: the double-click launcher/tray executable, the tray Open configuration / status / Exit menu, and the cross-process shutdown signal behind `service-stop`. The CLI entry implements start, status reporting, single-instance reuse, and persisted configuration today. The API exposes only an echo action, so user-selected microphone and shortcut actions are not wired through it yet.

## Delivery priorities

P0: persist and test bindings; authenticated local configuration and agent actions; resident service lifecycle; Windows hardware capture and device-state readback; WeChat Input shortcut integration; Codex/Mini capability verification and integration.

P1: application output mute with meeting isolation, non-focus-stealing feedback, WorkBuddy integration, extensible adapter contracts and Agent CLI/API/MCP.

P2: Slack/Tencent Meeting scenarios, more hardware/platform adapters, and genuinely supported macOS/Linux audio backends. Cross-platform build success is not platform audio acceptance.

Latest launch decision: use two thin Windows entry executables (a double-click launcher/tray application and a console CLI) backed by the same Rust core and one resident service. This supersedes the earlier single-entry launch proposal; it does not duplicate business logic. The current preview still ships only the CLI executable.
