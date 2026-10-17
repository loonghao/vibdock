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
2. **Running scope (WeChat Input).** Saved `Ctrl + Alt + F20` with
   `scope=running`, because the IME has no foreground window. With the
   background process present, sending the trigger produced `DISPATCHED`.
3. **Removal.** Both bindings were removed through the authenticated API; the
   registry returned to revision 2 with no entries.
4. **Registration state.** `armed=true` was reported after each save; unit tests
   cover failed registration rollback, stale revisions, duplicate triggers, and
   dispatch chords that would equal a VibDock trigger.

`DISPATCHED` means the parsed chord was injected into the current foreground
application. It does not establish that the target accepted the chord, that a
voice session started, or that any content changed.

## UI evidence

- `node scripts/test-binding-ui.cjs` passes with dependency-free DOM fixtures.
- A real Chrome window titled `VibDock · 本地交互工作台` was snapshot through
  DCC-CUA (`provider=dcc-cua runtime=1.9.4 pid=13768 hwnd=802360966`). The UIA
  tree contained the navigation buttons and the microphone binding editor, and
  the page rendered in service mode after a valid `#bootstrap=` handoff.
- Interactive navigation to the **应用与麦克风** view was not completed through
  DCC-CUA: a one-shot snapshot succeeds, but `start_session`/`get_window_state`
  on a persistent host returned `host_remote_failed`, and a separate `act`
  process rejected the prior element token as `stale_observation`. This is a
  DCC-CUA session/grant limitation on this host, not a page error. Visual
  pointer/DPI/focus acceptance therefore remains open.
- The new section's scripted behavior (background row downgrade, `running`
  request field, disabled/armed state rendering) is covered by the DOM test.

## Remaining limits

- No interactive Settings-page pointer test or DPI/multi-monitor test.
- No meeting audio isolation test and no real voice-session start/stop.
- Tencent Meeting produced a background row at observation time; a visible
  meeting window was not available to exercise foreground scope.
- Elevated foreground windows may refuse injected input; the service reports a
  failure rather than success.
- The foreground check and injection remain separate OS calls.
