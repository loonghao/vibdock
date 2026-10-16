# Resident software entries (read-only status)

Settings > Software reads current Windows playback sessions. A user can pin up to
eight observed software locations on their observed output endpoint, or remove a
saved entry. Pins survive service restarts in a separate `apps.json`; microphone
bindings remain in `config.json`. The resident panel and notification area show
one icon/status row per saved entry. Clicking an entry restores the same panel;
right-click opens the existing Settings/status/hide/exit menu.

## State and action contract

- A saved location is a correlation hint, not a verified binary identity. PID,
  process creation time and session IDs are never persisted as authority.
- `connected` means at least one current playback session was observed for that
  exact saved location/output. Active/inactive and mute values describe playback,
  not microphone use, voice sessions, agent work, or action success.
- `not_connected` requires a fresh, complete scan of that output with no match.
  Partial scans, missing endpoints, unsupported platforms, stale observations and
  read failures report `unknown`. An incomplete set never claims all muted/idle.
- This increment has no playback write, launch, terminate, voice-control or
  autostart operation. All software API rows say `actions_enabled: false`.
- Native software icons come only from current observed local images. Offline,
  protected, unsupported or invalid resources use a generic placeholder. The app
  does not read an offline saved path, fetch icon URLs, run an executable, invoke
  shell handlers or load imports/DllMain to extract its icon.

## Data and lifecycle bounds

Authenticated same-origin `GET /api/apps` returns public rows and current opaque
selection IDs. `POST /api/apps/pin` accepts only revision, selection ID and optional
label; remove accepts revision and saved ID. A selection is a 256-bit OS-random,
15-second, single-session capability, consumed on success and revalidated against
a scan younger than six seconds. Client paths and arbitrary fields are rejected.
Raw executable, endpoint and session identifiers stay server-side. Paths in the
local file are personal data; do not attach that file to public bug reports.

Schema 1 has a revision and at most eight entries (random ID, label, output ID,
executable location). Atomic writes commit before memory changes. Malformed,
oversized, unsupported or unreadable files remain untouched, report a diagnostic,
and disable edits; they are not silently replaced with empty configuration.

One background owner starts at most one disposable copy of VibDock's own helper.
It scans up to eight outputs and 24 sessions, rechecks session ownership and live
process creation time, bounds ICOs to one 16/24/32px image and 8KiB, and caps output
at 512KiB. A private non-inheritable kill-on-close job is assigned before an stdin
gate permits scanning. Output uses the same exclusively created temporary file
handle, with delete-on-close and a capped post-exit read. Windows files retain the
existing inherited directory ACL policy; this change does not alter permissions.

The owner applies a three-second scan deadline and bounded cleanup polling. Failed
or unconfirmed cleanup stops further helpers and reports unavailable. Service exit
has a bounded owner wait; OS termination remains asynchronous, so this is not a
guarantee that every faulty kernel driver completes cleanup within three seconds.
There are no COM or stdout-reader threads inside the resident service. CI checks
ordinary shutdown leaves no owned observer process; driver-hang recovery is not
claimed as live-tested.

## Evidence and remaining platform work

Unit fixtures validate persistence/restart, atomic failure, revision conflicts,
selection expiration/replay/PID change, partial/stale states, path redaction and
malformed native resources. Fixtures are not real device evidence. Hosted Windows
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
verifies current-user kernel process creation data and AppKit launch identity,
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
