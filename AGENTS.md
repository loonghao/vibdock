# Agent instructions

- Keep core command/configuration/action contracts separate from platform-specific audio, input, overlay, and integration adapters. Prefer the smallest complete change; preserve existing error handling and state validation.
- Use English Conventional Commits, repository-local author loonghao <hal.long@outlook.com>, and concise public-safe PR descriptions. Do not expose internal coordination, local paths, or secrets on GitHub.
- Read README.md and docs/design/README.md for verified capabilities versus planned features. Prototype events are not live device evidence. Never claim a sent shortcut proves platform state.
- For agent workflows, read [skills/vibdock/SKILL.md](skills/vibdock/SKILL.md); the helper [scripts/vibdock-agent.ps1](scripts/vibdock-agent.ps1) reads the local token without printing it.
- Required checks: cargo fmt --all --check; cargo clippy --workspace --locked --all-targets -- -D warnings; cargo test --workspace --locked --all-targets. Match hosted CI to the exact PR head before merge.
- When implementing local mutation APIs, require authentication and validate browser origins. Never expose arbitrary shell execution. Preserve device identity and reject silent fallback to another microphone.
- All DCC-MCP application UI tasks must use project-owned dcc-cua and ui-control. Explicit user requests for dcc-cua extend this requirement to other applications and browsers. Report provider, runtime version, PID, and HWND before observing or controlling UI. No generic Computer Use fallback without explicit user authorization.
