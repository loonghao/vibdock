# Application recognition and per-application shortcut acceptance

Checked on 2026-10-17 against branch `feat/app-recognition-shortcuts`
(commit `1d3af0f` plus the follow-up running-scope work) on a Windows 10/11
x64 desktop. This is a dated single-host record, not a platform-wide guarantee.

## What was exercised

- Native window-application observation and background-integration recognition
  through the resident helper.
- Pinning a recognised application and associating a microphone.
- Saving, registering, firing and removing per-application shortcut bindings
  through the authenticated loopback API.
- The Settings page's scripted DOM contracts and a real Chrome window snapshot.

No microphone was muted, no meeting was joined, and no voice session was
started by these checks.

## Detection results

| Application | Observation | Result |
| --- | --- | --- |
| WeChat Input 3.0.0.34 | background `wetype_server.exe` | detected as `wechat_input`, `background=true` |
| Codex desktop | window `ChatGPT.exe` | detected as `codex`, `background=false` |
| Codex CLI helpers | background `codex.exe` | detected as `codex`; deduplicated when the windowed client was present |
| Tencent Meeting 3.46.11.413 | launched `WeMeetApp.exe` | detected as `tencent_meeting`, `background=true`; no visible top-level window was observed during the check |
| Ulanzi Studio | window `UlanziDeck.exe` | detected |
| WorkBuddy | window `WorkBuddy.exe` | detected |
| Chrome, Edge, VS Code, Maya, 3ds Max, Explorer-hosted windows | visible windows | detected |

The full scan returned 22 window applications plus the background WeChat Input
row, with `applications_complete=true`. A later scan returned 23 rows after
Tencent Meeting's background process was recognised. A capped or unreadable
scan reports `applications_complete=false`; pinned presence rows then show
`unknown` rather than `not_running`.

## Shortcut dispatch results

1. **Foreground scope (Codex).** Saved `Ctrl + Alt + F19` as the trigger and
   `Ctrl + Shift + F19` as the dispatch chord. Sending the trigger produced a
   `DISPATCHED` receipt in `last_action`; no voice state was claimed.
2. **Foreground scope (Tencent Meeting).** Launched `WeMeetApp.exe`, detected the
   visible `腾讯会议` window as `tencent_meeting` with `background=false`, saved a
   foreground binding, activated that exact PID/HWND, and sent the trigger. The
   receipt was `DISPATCHED`.
3. **Running scope (WeChat Input).** Saved `Ctrl + Alt + F20` with
   `scope=running`, because the IME has no foreground window. With the
   background process present, sending the trigger produced `DISPATCHED`.
4. **Mismatch refusal.** With a different application foreground, the same
   trigger produced "The bound application is not the foreground window; nothing
   was dispatched" instead of injecting the chord.
5. **Removal.** Both bindings were removed through the authenticated API; the
   registry returned to revision 2 with no entries.
6. **Registration state.** `armed=true` was reported after each save; unit tests
   cover failed registration rollback, stale revisions, duplicate triggers, and
   dispatch chords that would equal a VibDock trigger.

`DISPATCHED` means the parsed chord was injected into the current foreground
application. It does not establish that the target accepted the chord, that a
voice session started, or that any content changed.

## Playback isolation check

Two real PowerShell processes played different looping WAV tones (440 Hz and
660 Hz) so they owned two distinct active playback sessions on the same output
endpoint. Using the exact-session CLI:

- `app-mute` on session A read back `muted=true` and `active=true`;
- session B still read back `muted=false` and `active=true`;
- `app-unmute` on session A read back `muted=false`.

Both sessions remained active and the endpoint master mute was never touched.
This is a real two-application, exact-session state check. It is not a meeting
audio isolation test: no meeting was joined and audibility was not measured.

## UI evidence

- `node scripts/test-binding-ui.cjs` passes with dependency-free DOM fixtures,
  including the background `running` scope, default foreground scope, and the
  post-save remove-button enablement regression.
- `scripts/test-web-ui-windows.cjs` starts a real isolated Windows service,
  seeds a saved shortcut, opens authenticated Settings in real Chrome at
  1280px and 390px, and verifies the apps view, the application-shortcut
  section, saved trigger/dispatch/scope rendering, `dispatch_available=true`,
  path redaction, no horizontal overflow and no JavaScript errors. Screenshots
  and `report.json` are uploaded as `desktop-evidence-windows-latest`.
- A separate local Playwright run with real detected applications found 23
  rows, selected the background 微信输入法 row, confirmed the `running` scope was
  checked and locked, saved the binding (`armed=true`, `scope=running`), read it
  back through the API without a path, and removed it. Its screenshot is
  `work/web-ui-windows/apps-shortcuts.png`.
- An interactive OS-level pointer/DPI run through DCC-CUA was not completed:
  a one-shot snapshot succeeds, but `start_session`/`get_window_state` on a
  persistent host returned `host_remote_failed`, and a separate `act` process
  rejected the prior element token as `stale_observation`. This is a DCC-CUA
  session/grant limitation on this host, not a page error. The real-browser
  Playwright run above covers rendering and responsive layout instead.

## Remaining limits

- No interactive OS-level Settings pointer test or DPI/multi-monitor test; the
  real Chrome/Playwright run covers rendering and responsive layout.
- No actual meeting was joined: Tencent Meeting foreground dispatch was verified,
  but meeting audio isolation and audible output were not.
- No real voice-session start/stop: those capabilities remain
  `not_implemented` and no application API was claimed.
- Elevated foreground windows may refuse injected input; the service reports a
  failure rather than success.
- The foreground check and injection remain separate OS calls.
