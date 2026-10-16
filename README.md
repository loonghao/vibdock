<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/brand/vibdock-symbol-light.svg">
  <img src="assets/brand/vibdock-symbol.svg" alt="VibDock logo: diamond with a negative-space voice channel" width="88" height="88">
</picture>

# VibDock

A local-first bridge between hardware controls, voice workflows, and personal AI agents.

Use a microphone button to talk to an agent, silence an input device, and get back to a meeting without hunting through windows. VibDock is being built for the way people move between coding assistants, personal agents, and collaboration apps.

**Early preview:** Published v0.1.0 provides Windows microphone commands and a design preview. Current source adds authenticated Web configuration, a resident shortcut binding, a branded Windows floating panel/tray, two executable entry points, and dual-entry packaging. These source features are not a new published release. Physical Ulanzi buttons, real default-browser launch, tray pointer interaction and DPI/focus behavior still require desktop acceptance. Codex/Dot, WorkBuddy, WeChat Input, Slack, and Tencent Meeting voice controls remain planned; VibDock is independent of these vendors.

**Service acceptance:** Integrated baseline `3b44b770` passes [hosted Windows, Linux and macOS CI](https://github.com/loonghao/vibdock/actions/runs/38841945041), including the local service identity and authorization regressions. That baseline does not validate the later desktop panel. New desktop candidates must pass their exact-head CI and the native/browser evidence checks below. Cross-platform build success is not platform audio or physical-device acceptance.

**Desktop status feedback:** The corner status indicator is implemented in source as an opt-in Windows overlay, **off by default**. Enable it by setting `VIBDOCK_OVERLAY=1` (`on`, `true`, and `yes` also work) before starting the service; `VIBDOCK_OVERLAY_CORNER` selects a corner (`bottom-right` default, or `bottom-left`, `top-right`, `top-left`). The window is placed in the taskbar-free work area of the primary monitor; if that work area cannot be read it falls back to the full monitor rectangle, which is **not** taskbar-aware and may let the indicator overlap a taskbar band. It has unit tests for its status contracts and placement but has **not** been accepted against live Windows: no run has verified typing/focus behaviour, multi-monitor placement, or failed-action feedback on a real desktop. The optional breathing border is planned and not implemented.

## Download and run

Published v0.1.0 archives contain one Rust executable and this README. Current Windows source builds produce `vibdock.exe` (console CLI) and `vibdock-launcher.exe` (desktop launcher); the pending release pipeline packages both and checks their executable subsystems. No Node.js, Python, or separate Web server is required.

| Platform | Published v0.1.0 | Pending source |
| --- | --- | --- |
| Windows x64 | Microphone commands, fixed global mute shortcut, design preview | Authenticated configuration, resident configurable shortcut, launcher/tray, dual-entry archive |
| Linux x64 | Design preview | Local configuration service; audio and global shortcuts unsupported |
| macOS Apple silicon | Design preview | Local configuration service; audio and global shortcuts unsupported |

## Configure a hardware shortcut (pending Windows source)

This workflow requires a build containing both `vibdock.exe` and `vibdock-launcher.exe`; the published v0.1.0 download does not contain the launcher.

1. Extract both executables into the same directory. Start `vibdock-launcher.exe` to show the small resident control panel. Right-click the panel or tray and choose **Settings…**, or use the panel’s **Settings…** button, to open the local configuration page.
2. Select the intended microphone from the service's device list. In the binding editor, record a combination such as **Ctrl+Alt+Shift+F9** and save it.
3. Check the separate **background binding** row. “Registered” means Windows accepted the shortcut; it does not mean an audio action already ran. A conflict leaves the previous configuration and registration in place.
4. In Ulanzi Studio, map the desired button to that exact keyboard combination using its shortcut action. Physical-device identification is not inferred from a mapped keyboard event.
5. Press the hardware button and verify the microphone's actual mute state. The page reports the last completed action or its error. Physical Ulanzi verification remains pending; a saved mapping alone is not proof of input delivery.
6. Close the browser to leave the service running. Reopen **Settings…** from the panel or tray; use **Exit VibDock** or `vibdock.exe service-stop` to stop the service.

Only one microphone/shortcut pair is currently active. Do not run legacy `listen` with the same chord: Windows allows only one registration. Input-device mute affects every application using that microphone. Per-agent input, per-application playback mute, and voice-session start/stop are separate planned controls.

The status page uses bounded, serial polling. When the service cannot answer, it shows unknown rather than preserving a successful state. “Last action” is a readback after a shortcut action, not continuous tracking of changes made by other applications. A service restart clears action history and requires a fresh browser session.

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
service and show a small movable control panel with a taskbar entry and notification
icon. Keep both executables in the same directory. Windows chooses the initial
panel position; no fixed corner is assumed.

- **Settings…** (button or right-click menu) opens a fresh one-use handoff to the
  proved service’s actual address. A browser launch failure is visible.
- Close, **Hide panel**, or Escape hides the panel without stopping the service.
  Click the tray icon or double-click the launcher again to restore the same panel.
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

The current app controls are explicitly unavailable without verified adapters.
Per-software icons, persistent software selections and working-state controls are
not yet implemented. macOS still packages only the CLI, with no menu-bar/Dock entry.
See [desktop acceptance and platform plan](docs/design/desktop-candidate.md).

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

## Application playback controls (pending Windows source)

These commands are an unreleased implementation under review. Live mutation
acceptance is on hold; the existing preview download does not include them.

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

## Product direction

`vibdock capabilities` reports documentation-derived integration declarations.
Installed-client verification is pending; the report labels those capabilities
as unknown or unverified rather than claiming a working application connection.

- Configure a binding by pressing a hardware button, choosing a target and action, then testing it.
- Keep input mute, application output mute, and platform voice-session controls separate.
- Provide optional non-focus-stealing desktop feedback, with text/icons as well as color. The corner status indicator is implemented on Windows behind an opt-in that defaults to off; the optional breathing border is planned and not implemented.
- Silence an agent's output during a meeting without changing meeting audio. Application playback commands are implemented in pending Windows source. Hardware bindings, service API access, and accepted two-application isolation tests remain open.
- Expose explicit capabilities, results, and queryable state to agents through CLI, local API, and later MCP.
- Add adapters for personal-agent platforms and input tools without coupling hardware to any one vendor.

The control API authenticates callers and validates browser origins. The `serve` preview is read-only, loopback-only, and rejects unexpected Host headers. `service-start` exposes authenticated `/api` configuration and action endpoints, including real microphone status, mute, unmute, and toggle through the Windows audio adapter. A sent shortcut must not be reported as a verified application state change.

See [the interaction specification](docs/design/README.md) and [the clickable design source](docs/design/index.html).

## Development

```powershell
cargo fmt --all --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo build --locked --release --target-dir target
```

Audio access is confined to the Windows adapter. The command parser and embedded preview run on all supported build platforms.

## Releases

Conventional Commits on `main` feed release-please. It opens a version/changelog PR; merging that PR creates a release, and the same workflow builds Windows x64, Linux x64, and macOS ARM64 archives plus SHA256 files. A release tag alone does not establish that all downloads were built successfully; check the workflow and assets.

With the default GitHub token, bot-created release PRs do not automatically trigger PR workflows. Dispatch CI on the release branch and wait for all checks at its current head before merging. A scoped GitHub App token can automate this later. Repository Actions settings must permit workflow-created pull requests.

### Launch behavior planned for the product

Double-click to start one resident local service and open the Web settings. Close the page to keep hardware/agent connections alive; use the tray menu to reopen settings or exit. Repeated launches reuse the running instance. The CLI, the tray entry, and the authenticated shutdown that backs the tray Exit item are implemented; a packaged double-click launcher executable is not published yet.

WeChat Input and Codex/Mini are the first integration priorities. WorkBuddy and meeting-friendly per-application audio controls follow; Slack and Tencent Meeting scenarios remain integration-specific acceptance work.

The latest product launch design uses a thin Windows launcher/tray executable alongside the console CLI, both sharing one Rust core/service. The `tray` command implements the tray entry and shares the service core with the CLI; current preview downloads still contain only the CLI.

Integration evidence and shortcut ownership are tracked in [voice integration evidence](docs/integrations/voice-evidence.md). Documentation support does not mean an installed client has been controlled successfully.

## License

Licensed under the [MIT License](LICENSE).
