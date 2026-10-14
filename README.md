# VibDock

A local-first bridge between hardware controls, voice workflows, and personal AI agents.

Use a microphone button to talk to an agent, silence an input device, and get back to a meeting without hunting through windows. VibDock is being built for the way people move between coding assistants, personal agents, and collaboration apps.

**Early preview:** Windows microphone commands and authenticated Web configuration are implemented. Service mode reads capture devices, persists one device/shortcut pair, and exposes device actions; design-preview mode uses mock data and browser-local drafts. Global activation of saved shortcuts and native hardware/tray acceptance remain pending. Codex/Dot, WorkBuddy, WeChat Input, Slack, and Tencent Meeting integrations are planned. VibDock is independent of these vendors.

## Download and run

Published v0.1.0 archives contain one Rust executable and this README. Current Windows source builds produce `vibdock.exe` (console CLI) and `vibdock-launcher.exe` (desktop launcher); packaging both is pending. No Node.js, Python, or separate Web server is required.

| Platform | Current capability |
| --- | --- |
| Windows x64 | Microphone enumeration, mute/status commands, global mute hotkey, embedded design preview |
| Linux x64 | Embedded design preview only |
| macOS Apple silicon | Embedded design preview only |

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
origin. `/api/config` accepts `device_id` and `hotkey` as plain strings under
512 bytes and persists them to `config.json`; no shell or arbitrary-command
endpoint exists.

Repeated `service-start` invocations reuse the running instance instead of
binding a second listener. The published record is treated only as a candidate:
before it is trusted, the service on that port must answer an authenticated
`GET /instance` challenge with the recorded token. A stale marker left by a
killed service is recovered only after proving no owner answers; if an
unrelated process holds the port, startup fails with an error and leaves the
marker alone rather than silently falling back to it.

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
service, open its configuration page, and show a notification icon with
**Open configuration**, **Status**, and **Exit**. The launcher and console CLI
share the same Rust core. Keep both executables in the same directory.

```powershell
.\vibdock-launcher.exe
```

The tray owns no business logic: Open launches the service's own bound URL,
Status reports whether a service is answering, and Exit performs the same
authenticated shutdown as `service-stop`. Repeated launches reuse the running
instance rather than starting a second one. The service is a separate process:
closing the browser or terminating the launcher leaves it running. Tray Exit
requests an authenticated shutdown; a shutdown error keeps the tray available.
Native tray interaction still requires desktop acceptance. Service mode provides
authenticated configuration; `serve` keeps the separate read-only design preview.

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

## Product direction

- Configure a binding by pressing a hardware button, choosing a target and action, then testing it.
- Keep input mute, application output mute, and platform voice-session controls separate.
- Provide optional non-focus-stealing desktop feedback, with text/icons as well as color.
- Silence an agent's output during a meeting without changing meeting audio. This requires application-session controls and is not available yet.
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
