# Resident software entries and exact Windows playback controls

Settings > Software reads current Windows playback sessions. A user can pin up to
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
