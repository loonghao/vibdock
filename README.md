# VibDock

A local-first bridge between hardware controls, voice workflows, and personal AI agents.

Use a microphone button to talk to an agent, silence an input device, and get back to a meeting without hunting through windows. VibDock is being built for the way people move between coding assistants, personal agents, and collaboration apps.

**Early preview:** Windows microphone controls work today. The Web configuration screen is an interactive design prototype, not a live control panel. Codex/Dot, WorkBuddy, WeChat Input, Slack, and Tencent Meeting integrations are planned, not implemented. VibDock is independent of these vendors.

## Download and run

Release archives contain one Rust executable and this README. No Node.js, Python, or separate Web server is required.

| Platform | Current capability |
| --- | --- |
| Windows x64 | Microphone enumeration, mute/status commands, global mute hotkey, embedded design preview |
| Linux x64 | Embedded design preview only |
| macOS Apple silicon | Embedded design preview only |

```powershell
.\vibdock.exe serve
```

Open http://127.0.0.1:43128 to explore the design. The preview uses mock data, keeps changes only in page memory, and never controls audio. Close the terminal or press Ctrl+C to stop the preview server. The current `serve` and `listen` commands are separate processes.

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

A future control API must authenticate callers and validate browser origins. The current Web server is read-only, loopback-only, rejects unexpected Host headers, and exposes no audio-control endpoints. A sent shortcut must not be reported as a verified application state change.

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

Double-click to start one resident local service and open the Web settings. Close the page to keep hardware/agent connections alive; use the tray menu to reopen settings or exit. Repeated launches reuse the running instance. This lifecycle is planned, not implemented in the current CLI preview.

WeChat Input and Codex/Mini are the first integration priorities. WorkBuddy and meeting-friendly per-application audio controls follow; Slack and Tencent Meeting scenarios remain integration-specific acceptance work.

The latest product launch design uses a thin Windows launcher/tray executable alongside the console CLI, both sharing one Rust core/service. Current preview downloads still contain only the CLI.
