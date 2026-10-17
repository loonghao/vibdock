---
name: vibdock-control
description: Inspect and configure a local VibDock service, microphone/software bindings, and bounded shortcut sequences through its authenticated native control CLI.
---

# VibDock control

Use the installed `vibdock` executable containing the `control` commands. Start
with `vibdock control capabilities`; an older executable or unavailable service
is a concrete capability failure, not permission to guess commands.

The native client proves the service on the same IPv4 connection before sending
its credential. Do not read, print, paste or place the service token in a URL.
Use these fixed commands rather than rebuilding authentication in a script:

```text
vibdock control config
vibdock control bindings
vibdock control apps
vibdock control config --json-file config-patch.json
vibdock control bindings --json-file bindings-edit.json
vibdock control cancel
```

Read current configuration and capabilities before an edit. Save a UTF-8 JSON
request file with the user's intended device/application and shortcut; use the
reported current revision for bindings. Preserve other entries. An omitted
legacy field retains its value; explicit null clears it. New bindings live
separately from the original microphone/shortcut pair.

A bindings edit contains `revision` and `bindings`. Each entry contains `id`,
`label`, `enabled`, `trigger`, and `actions`. Supported action objects are:

- `{"type":"microphone","device_id":"exact ID","operation":"mute"}`;
- `{"type":"playback","entry_id":"saved application ID","operation":"mute"}`;
- `{"type":"keys","chord":"Ctrl + L","delay_ms":250}`.

Use only operations reported by the executable. Do not substitute another
microphone or silently choose one of multiple playback sessions. A device mute
affects every application using that endpoint; a saved association is not an
application input route or a privacy permission.

Read the service response and read configuration back after a save. Report
registered, disabled or failed activation separately from persistence. Preserve
draft intent after a revision conflict; refresh and reconcile rather than
overwriting a newer configuration. A key sequence runs from its configured
trigger against the trigger-time foreground target; it is cancellable. The
control CLI does not accept shell execution, arbitrary URLs or arbitrary code.

Treat `dispatched_unverified` as input delivery only. Do not report a voice
session, meeting transition or target-application action without independent
readback. Capability failures and partial/unconfirmed outcomes must remain
visible. Do not retry a possibly completed audio action blindly.

Permission approvals in another agent are not a Skill action. The human must
review and choose allow or deny; do not synthesize approval input, grant the
agent its own permissions, or create an automatic allow-all rule.
