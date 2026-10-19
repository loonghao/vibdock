# Resident software entries and exact Windows playback controls

Settings > Software reads current Windows playback sessions and, independently,
running applications that own a visible top-level window. The two observations
are separate: a window application has no implied audio session, and a playback
session has no implied window. A user can pin up to
eight observed software locations on their observed output endpoint, or remove a
saved entry. Pins and their optional microphone associations survive service
restarts in `apps.json`; the single global microphone/shortcut binding remains
in `config.json`. Settings and the notification area show each saved entry's icon
and observation. The primary VibDock icon and repeated launches open authenticated
Web Settings;
right-click a Windows software icon to choose one explicitly identified playback
session (including its PID), then mute or unmute that session. Offline/stale
entries disable playback controls. Settings/status/exit remain available.

Windows also shows up to eight software logos in a passive strip just above the
primary work area bottom edge. `×`, `▶`, `II`, `–` and `?` describe playback only;
voice activity remains unknown. Native buttons expose the app and state to
accessibility tools and tooltips. Clicking opens Settings; right-clicking retains
the exact software/session menu. An unreadable or too-small work area hides the
strip. The settings list uses the same logo-and-badge vocabulary. This source
change has unit coverage; native pointer, DPI and focus acceptance is pending.

## State and action contract

- A saved location is a correlation hint, not a verified binary identity. PID,
  process creation time and session IDs are never persisted as authority.
- `connected` means at least one current playback session was observed for that
  exact saved location/output. Active/inactive and mute values describe playback,
  not microphone use, voice sessions, agent work, or action success.
- `not_connected` requires a fresh, complete scan of that output with no match.
  Partial scans, missing endpoints, unsupported platforms, stale observations and
  read failures report `unknown`. An incomplete set never claims all muted/idle.
- Windows native software-icon menus offer fixed mute/unmute for an explicit
  current playback session. A multi-session entry requires selecting a session;
  no action targets the first/all sessions, system output, microphone, or another
  process. Launch, terminate, voice control and autostart remain unavailable.
  The Settings software page manages pins and microphone associations; playback
  controls remain in native software-icon menus.
- A saved `device_id` associates a microphone endpoint with a software entry.
  The page offers explicit status/mute/unmute for that exact endpoint, verifies
  the returned device ID and mute state, and disables controls for a missing
  endpoint. Microphone mute affects every application using that device. An
  association does not prove which microphone the application uses, change its
  operating-system input route, or start/end a voice session. Capabilities
  `input_routing`, `start_voice` and `end_voice` remain `not_implemented`.
- Native software icons come only from current observed local images. Offline,
  protected, unsupported or invalid resources use a generic placeholder. The app
  does not read an offline saved path, fetch icon URLs, run an executable, invoke
  shell handlers or load imports/DllMain to extract its icon.

## Application presence and per-application shortcuts

Window observation enumerates visible, non-cloaked, non-tool top-level windows,
resolves each owner's executable through a read-only process handle, and keeps
at most 32 applications. A UWP window owned by `ApplicationFrameHost` is mapped
to the packaged application through its `Windows.UI.Core.CoreWindow` child
process, so the row names the real app rather than the host. Explorer, input
hosts, VibDock itself and other shell-host processes without a distinct hosted
application are skipped because their executable is not the user-facing
identity. A capped scan, an unreadable window owner, or a changed process image
makes the window list partial:
`running`, `not_running` and `unknown` are then reported accordingly, and a
partial list never claims that an application is absent.

A closed set of known integration executables (`wetype*.exe`, `wemeet*.exe`,
`codex.exe`/`chatgpt.exe`, `DeepSeek Harness.exe`, `workbuddy.exe`) is
additionally matched by exact process name so a background input method or tray
agent can be recognised without a window. These rows are marked
`background: true`. That pass is best-effort: an unreadable protected helper is
skipped and does not make the window list partial, and it never establishes
audio, voice, account, or control state. One row per integration is retained
after deduplication against a windowed client.

A recognised executable basename may carry an integration label
(`tencent_meeting`, `wechat_input`, `codex`, `dsh`, `workbuddy`). The label is
recognition metadata only. It does not prove installation, connection, account
state, audio use, or that the client accepts any shortcut. `DeepSeek Harness.exe`
is matched by exact basename (case-insensitive), so unrelated names such as
`deepseek.exe` or `DeepSeek Harness Helper.exe` are not recognised. That
mapping has unit coverage only; no live foreground dispatch against DeepSeek
Harness has been recorded.

`shortcuts.json` (schema 1, revision, at most eight entries) stores one random
id, label, executable location, VibDock trigger and dispatch chord per entry.
`POST /api/shortcuts/add` accepts exactly a revision, a fresh opaque
`application_reference` from the current scan, `trigger`, `dispatch`, an
optional label and an optional `scope` (`foreground` default, or `running` for
a background integration). The service resolves the reference to the observed
executable; the browser never supplies a path. `POST /api/shortcuts/remove`
accepts a revision and saved id. Both require the existing authentication and same-origin
checks. A stale reference, stale revision, duplicate application, duplicate
trigger, or a chord conflict returns 409 and preserves the previous file.
`GET /api/shortcuts` returns labels, trigger/dispatch chords, registration
state and errors, but never an executable path. A saved entry may also name an
optional AU05 `device_button` (`0`-`2`); the service then writes that binding's
trigger into the button's firmware setting and reports the provisioning state in
shortcut and hardware responses. A firmware write failure does not roll back the
saved binding. See [AU05 direct bindings](design/au05-direct-bindings.md).

Registered triggers use the existing modifier-plus-key grammar. A registered
trigger containing `Win`, or consisting of a bare key, is rejected: Windows
reserves `MOD_WIN` for shell bindings, and a bare registered key would swallow
that key everywhere. A dispatch chord may contain `Win` (for example
`Ctrl + Win`); it is injected with `SendInput`. A dispatch chord equal to any
VibDock trigger is rejected: an injected chord is delivered to the same global
hotkey table, so allowing it would make VibDock re-trigger itself. Changing the
microphone trigger to a saved dispatch chord is rejected for the same reason.

The AU05 adapter also has a direct firmware mode stored in `hardware.json`
(schema 1, up to three buttons): a chord or bare key is written into a button
with no host hotkey, no dispatch, and no application-target check. It is how the
first-run defaults are written (`Ctrl + Win` 微信语音, `Enter` 回车, `Escape`
取消, skipping buttons that already have an application binding) and how
`Ctrl + Win` reaches an input method that ignores injected input. A button
carries an application binding, a direct chord, or a hardware action, never more
than one, and a direct chord is never observed in the recent-trigger log. See
[AU05 direct bindings](design/au05-direct-bindings.md).

Hardware action bindings (`actions.json`, schema 1, up to 16) map one AU05
button or HID gamepad button to 1-8 ordered steps: launch an application, send a
chord, toggle an explicit microphone, run a local command from `commands.json`,
or wait. An AU05 action gets a generated trigger, registered and written into
firmware; a gamepad action has no trigger and no firmware write. Steps execute
in order and stop at the first failure, with the result in the recent-trigger
log; there is no action-execute endpoint and no voice start/stop. See
[hardware action sequences](design/hardware-actions.md).

Registration happens before the file is committed, and the previous set is
restored on registration or persistence failure. At startup, a trigger that
cannot be registered is reported per entry and never fires. A fired trigger
reads the foreground window, resolves its executable and compares it with the
saved location; on a mismatch nothing is injected. A `running` scope is an
explicit opt-in for background input methods: the service instead requires the
saved executable to appear in the fresh application scan, then injects the
chord into whatever application is foreground. That is broader by design and is
never inferred automatically. When either check passes, the parsed dispatch
chord is injected with `SendInput` and the receipt is `DISPATCHED`. That is not
a verified application state: no readback establishes that voice started, a
message was sent, or the application acted on the chord. The foreground check
and injection are separate OS calls, so focus can change in between. A second
check before injection narrows the race, and a post-injection check turns a
detected focus change into an error receipt saying the chord may have reached
another application, never a success. Elevated foreground windows may refuse
injected input.

## Explicit action safety and receipts

Opening a menu, polling or preparing an action never changes audio. An explicit
session-menu click captures its displayed random reference, entry and fixed mute
state. The service rechecks that exact endpoint/session-instance/PID/process
creation time/executable observation before preparing and before queuing. It
never recaptures a replacement PID from a saved location. The native helper holds
a read-only process handle and verifies the same process and unique single-owner
non-system playback session before the write and around its readback.

`POST /api/apps/actions/prepare` accepts only `entry_id`, `session_reference` and
`action` (`mute` or `unmute`). Its random one-use capability expires after ten
seconds and belongs to that browser session or native service instance. Execute
accepts only that `operation_id` and `capability`, consumed before dispatch.
Client paths, process IDs, endpoints, toggles and arbitrary actions are rejected.
At most one action is pending; a separate reliable channel preserves its receipt
while observations may be coalesced. Operations are memory-only and bounded to
32 receipts; completed receipts expire after two minutes.

The native client proves each prepare/execute/receipt connection independently,
using the same captured service instance and one privileged exchange per proof.
A lost execute response only triggers receipt polling, never an automatic second
write. A restarted service or missing receipt cannot establish success or prove
that no write occurred. Readbacks are historical action results, not live state.

Receipts distinguish prepared, pending, verified, verified mismatch, rejected
before write, and outcome unconfirmed. Once SetMute was attempted, any failed or
untrusted readback reports uncertainty. Killing a timed-out helper does not undo
a possible audio change. No default device, master-volume or microphone write
exists in this path. Mac application presence has no corresponding action API.

## Data and lifecycle bounds

Authenticated same-origin `GET /api/apps` returns public rows and current opaque
selection IDs. `POST /api/apps/pin` accepts only revision, selection ID and optional
label; remove accepts revision and saved ID. Windows `POST /api/apps/bind` accepts
exactly `{"revision":0,"id":"<saved-entry-id>","device_id":"<microphone-endpoint-id>"}`.
All three fields are required; explicit `device_id: null` clears the association.
A non-empty identifier of at most 512 bytes must pass a current exact-device
status read before commit. Malformed fields return 400, an unknown entry 404,
and a revision conflict or failed device validation 409; failed edits preserve
the previous data. Other platforms return 501 for bind. These routes require
the existing authentication and same-origin checks.

A pin selection is a 256-bit OS-random,
15-second, single-session capability, consumed on success and revalidated against
a scan younger than six seconds. Client paths and arbitrary fields are rejected.
Raw executable, playback endpoint and session identifiers stay server-side.
An associated microphone `device_id` is returned to authenticated settings clients.
Paths in the local file are personal data; do not attach that file to public bug reports.

Schema 1 has a revision and at most eight entries (random ID, label, output ID,
executable location, optional microphone `device_id`). Existing entries with a
missing or null `device_id` remain unbound; no default microphone is substituted.
Atomic writes commit before memory changes. Malformed,
oversized, unsupported or unreadable files remain untouched, report a diagnostic,
and disable edits; they are not silently replaced with empty configuration.

One background owner starts at most one disposable copy of VibDock's own helper.
The same owner serializes scans and explicit action jobs. It scans up to eight outputs and 24 sessions, rechecks session ownership and live
process creation time, bounds ICOs to one 16/24/32px image and 8KiB, and caps output
at 512KiB. A private non-inheritable kill-on-close job is assigned before an stdin
gate permits scanning. Output uses the same exclusively created temporary file
handle, with delete-on-close and a capped post-exit read. Windows files retain the
existing inherited directory ACL policy; this change does not alter permissions.

Action stdin uses a bounded, private nonblocking local pipe, with the existing
process ACL policy unchanged; only the contained child receives its read handle.
Only server-owned exact identity is sent, never a bearer or browser capability.

The owner applies a three-second scan/action deadline and bounded cleanup polling. Failed
or unconfirmed cleanup stops further helpers and reports unavailable. Service exit
has a bounded owner wait; OS termination remains asynchronous, so this is not a
guarantee that every faulty kernel driver completes cleanup within three seconds.
There are no COM or stdout-reader threads inside the resident service. CI checks
ordinary shutdown leaves no owned observer process; driver-hang recovery is not
claimed as live-tested.

## Evidence and remaining platform work

Unit fixtures validate persistence/restart, microphone-association validation,
null clearing and missing-field compatibility, atomic failure, revision conflicts,
selection expiration/replay/PID change, partial/stale states, path redaction and
malformed native resources, action capability/principal/replay binding, stale
references, PID creation changes, queue/owner failure and readback uncertainty.
Fixtures are not real device evidence. A separate hosted Windows test starts the
real contained action helper with an impossible captured process creation time,
verifies a native rejection before any write, and confirms cleanup/cancellation.
This validates private stdin inheritance/gate/EOF and result transport without
muting audio. Hosted Windows
CI compiles the native adapter and exercises the real resident lifecycle; a runner
without playback hardware must show unavailable/unknown and cannot prove live
application playback or real third-party icon behavior.

macOS has a separate native application-presence adapter, not a translation of
Windows playback sessions. It stores schema 1 in `apps-macos.json` with platform,
revision and at most eight entries (random ID, label, canonical bundle location,
optional bundle ID). It never reads, migrates or replaces Windows `apps.json`.
A saved location is only a correlation hint; processes and launch stamps are not
persisted as authority. Current choices are one-use OS-random capabilities bound
to the full observed bundle/executable/PID/launch identity. Client paths are not
accepted. Atomic-write failures, corruption, unsupported schemas and external
file changes preserve the existing file and disable editing with a diagnostic.

The Mac helper caps inspection at 128 applications and publication at 64. It
verifies current-user kernel process creation seconds/microseconds and native
application identity (including AppKit launchDate whenever available),
including apps without a visible Dock icon. Fixed 32px metadata-free PNG icons
are bounded to 16KiB and validated before browser/native rendering. No offline
saved bundle path is read to recover an icon. The native panel expires status and
icons after four seconds, subtracting observation and transport age. Mac API
states are `running`, `not_running` and `unknown`; `actions_enabled` remains false.
A running process is not proof of playback, recording, voice activity or work.

See [macOS native behavior and acceptance](macos-desktop.md) for helper lifecycle,
actual AppKit evidence and remaining physical-desktop/signing limitations. Formal
release readiness still requires representative real Windows playback/control
and software-state acceptance. Candidates are not a full voice-control release.
The dated [application recognition acceptance record](testing/app-recognition-acceptance.md)
covers the window/background detections and per-application dispatch checks made
so far.
