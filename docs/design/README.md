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

## Agent and plugin direction

Adapters declare actions, scope, prerequisites, and state-query capability. CLI, future authenticated loopback HTTP API, and MCP share the same execution contracts. Detection is not connection, connection is not action success. Do not expose arbitrary command execution through a Web binding endpoint.

## Prototype limits

All device/application data is mock data. No audio, global capture, desktop overlay, app detection, plugin loading, or authenticated control API is implemented by this page. The embedded Rust preview serves the same page read-only on Windows, Linux, and macOS; the page probes `/health` to show whether that service is running, and the embedded server's CSP permits same-origin requests for that probe while leaving every other directive locked down.

Bindings are saved to the browser's own `localStorage` under `vibdock.design.bindings.v2`, so they survive a reload or reopen in the same browser. This is still a design artifact, not backend persistence: it is per-browser, is not shared with the resident service, and is not a device configuration. The page states this wherever a save succeeds, and reports an explicit error when the browser refuses to write. The header badge and the banner distinguish three situations: service running in design-preview mode, service running with audio control, and no service reachable.

## Interaction behavior

Setup follows the three guided steps shown above: capture a key, choose a target and action, then test and save. The step indicator marks a step satisfied only after the user actually performs it — choosing an action is not credited merely because the selector shows a default — and flags one step as active at a time.

Switching to a different key while a draft is unsaved asks for confirmation before discarding it, so an entered shortcut is never lost silently. The same draft can also be dropped explicitly with the discard button, which appears only while the editor differs from what is saved.

Because the mock cannot identify a physical device, capture still reports that the source device is unconfirmed rather than inventing one. Saving a shortcut already assigned to another key opens an explicit replace-versus-recapture choice instead of silently overwriting it; replacing removes the older binding. Saving with no shortcut is blocked with an inline field error, and the status line renders failures in the error style rather than the success style.

The scope note under the action selector always states what a control affects. Device input mute warns that every application using that microphone is affected, while application output mute states that meeting and system master output are not.

## Feedback states

The feedback view previews each result state separately: trigger received, action pending, verified success, dispatched without verification, and failure. The status line and toast carry text for every state, so a state is never conveyed by color alone. The breathing border remains opt-in and follows the reduced-motion preference.

## Resident-service lifecycle requirement

The product version must start by double-clicking the executable, launch one local service, and open its configuration page. Closing the browser does not stop the service. A Windows notification-area icon provides Open configuration, status, and Exit. A separate small floating status indicator may be optional; it is not a replacement for a discoverable Exit control.

Repeated launches must discover and reuse the existing service instead of starting a second listener. Hardware and authorized agents connect while that service is alive. Exiting unregisters hotkeys and stops listeners without unexpectedly changing audio mute state. Open configuration must navigate to the actual bound local address, not a guessed port. The current CLI preview does not implement this lifecycle yet.

## Delivery priorities

P0: persist and test bindings; authenticated local configuration and agent actions; resident service lifecycle; Windows hardware capture and device-state readback; WeChat Input shortcut integration; Codex/Mini capability verification and integration.

P1: application output mute with meeting isolation, non-focus-stealing feedback, WorkBuddy integration, extensible adapter contracts and Agent CLI/API/MCP.

P2: Slack/Tencent Meeting scenarios, more hardware/platform adapters, and genuinely supported macOS/Linux audio backends. Cross-platform build success is not platform audio acceptance.

Latest launch decision: use two thin Windows entry executables (a double-click launcher/tray application and a console CLI) backed by the same Rust core and one resident service. This supersedes the earlier single-entry launch proposal; it does not duplicate business logic. The current preview still ships only the CLI executable.
