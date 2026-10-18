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

### UWP hosted applications

Checked on 2026-10-18 after resolving the `Windows.UI.Core.CoreWindow` child of
`ApplicationFrameHost` windows: launching the built-in Calculator produced a
`计算器` row with `background=false` and a fresh application reference; the row
was absent before this change because the host executable was skipped. The
packaged executable was resolved through the hosted process, not the frame host.
No shortcut binding or dispatch was attempted for the UWP app in this check, so
foreground matching and injected-input acceptance for packaged applications
remain unverified.

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

## CLI projection

Checked on 2026-10-18 with a debug build against a private service state
directory and port, so the user's resident service was untouched:

- `shortcut-list` returned revision 0 and `none` on a fresh store.
- `shortcut-apps` listed the live desktop rows, including
  `ChatGPT  [codex]  window  reference=<64 hex>`.
- `shortcut-add --application <fresh reference> --trigger "Ctrl+Alt+F18"
  --dispatch "Ctrl+Shift+F18" --label "CLI Codex"` saved
  `CLI Codex [foreground] Ctrl + Alt + F18 -> Ctrl + Shift + F18`; the JSON view
  reported `armed=true`, `revision=1`.
- `shortcut-remove --id <saved id>` returned to an empty list at revision 2.
- A second add without `--label` defaulted the label to `ChatGPT`, confirming
  the CLI omits the optional field rather than sending `null`.
- The CLI never dispatches a chord; it only reads the saved view and mutates the
  authenticated registry.

This is a dated single-host record. The command rejects executable paths, uses
only opaque application references from a fresh scan, and requires the verified
service to be running.

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
- The same script launches one real windowed Notepad with a unique document
  title and waits for that row in the detected-application list. When it is
  present, it binds a foreground-scope shortcut, confirms `armed=true` and the
  returned scope through `/api/shortcuts`, confirms the remove button is enabled
  immediately after the save, removes the binding through the confirm dialog,
  and checks the list returns to empty. A host without a visible desktop, an
  existing Notepad, or a conflicting global registration reports an explicit
  `SKIP` with the reason instead of claiming a live save. The live screenshot is
  `work/web-ui-windows/apps-shortcuts-live-1280.png`.
- A separate local Playwright run with real detected applications found 23
  rows, selected the background 微信输入法 row, confirmed the `running` scope was
  checked and locked, saved the binding (`armed=true`, `scope=running`), read it
  back through the API without a path, and removed it. This remains a dated
  single-host record, not automated coverage.
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
