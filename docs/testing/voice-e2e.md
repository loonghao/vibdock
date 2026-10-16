# Voice and companion-window E2E acceptance

This procedure separates real-service process tests from installed-client voice
and window acceptance. A saved association is not an application input route;
a dispatched shortcut is not a started or ended voice session. This document
is a test plan, not a record that any installed client passed.

## Real-service process E2E

The process harness starts the built Windows
service with isolated state and a private loopback port. It uses real HTTP and
service restarts, not a mocked service. It does not send UI input, change audio
mute, or inspect installed clients. Use Windows and Node.js 18 or later.

```powershell
cargo build --locked --bins
node scripts/test-voice-e2e.cjs target/debug/vibdock.exe
```

Without `--device`, the harness seeds four clearly labelled offline software
and microphone metadata fixtures for WeChat Input, Codex, Dot and Mini. It
checks authentication, bootstrap, restart persistence, association clearing,
entry removal, stale revisions, malformed/missing IDs and disconnected-device
rejection. These fixtures do not establish that the clients exist or are running.
Live microphone association is reported as skipped.

For real microphone validation, first read the full capture endpoint ID:

```powershell
.\target\debug\vibdock.exe devices
```

Then supply that exact ID, preserving its braces and punctuation:

```powershell
node scripts/test-voice-e2e.cjs target/debug/vibdock.exe --device '<exact-capture-endpoint-id>'
```

The optional device run associates each saved fixture through the authenticated
API, requires the selected endpoint's read-only native validation, and checks
persistence after restart. It still does not set any application's input route
or start a voice session. Never substitute another microphone after a failure.

The default evidence directory is `work/voice-e2e/<unique-run-id>/`. Set
`--output '<evidence-directory>'` to choose another directory. Retain `report.json`
and every service generation's stdout/stderr. The harness must omit credentials.
Report the process exit code, each skipped check, and the evidence directory;
do not promote a process pass to installed-client acceptance.

## Installed-client prerequisites and receipts

Use project-owned `dcc-cua` and DCC-MCP `ui-control` for every observation and
input in the scenarios below, including the scratch editor and browser. Before
the first observation or input on each target, report:

```text
provider=dcc-cua runtime=<runtime-version> target=<application> pid=<PID> hwnd=<HWND>
```

Bind the exact installed client and window. Record its version and the relevant
client setting before sending input. If a companion window has a different
HWND or PID, bind and report that identity before observing or controlling it.
When the provider, runtime version, target identity, trigger or required state
readback is unavailable, stop that scenario and mark it blocked. Do not switch
to generic Computer Use or guess an undocumented shortcut/API.

For every scenario, retain a before/after receipt containing the run ID, source
revision, provider/runtime, client version, PID/HWND, selected microphone ID,
exact configured trigger, bounded action, timestamps, observed state and
evidence paths. Omit tokens, bootstrap codes, personal dictation and unrelated
screen content. State whether the result is verified, failed, blocked, skipped
or dispatched without verification. A still image or input acknowledgement
alone cannot certify a dynamic state transition.

## Client scenarios

| Target | Procedure | Required result |
| --- | --- | --- |
| WeChat Input start, dictation and stop | Read the installed client's actual voice trigger and stop control. Focus a new, unsaved scratch editor with no recipient or submission action. Start with that configured trigger, speak an agreed harmless phrase, commit the transcription locally, then use the client's documented or observed stop control. Never send the text. | Read voice-active state after start, the committed phrase in the scratch editor, and inactive/ended state after stop. A microphone icon, injected key or uncommitted candidate alone is insufficient. |
| Codex Voice start and stop | Open a dedicated test chat/task. Read **Settings > Voice > Voice chat hotkey**, then invoke the user's configured trigger. Use the client's actual stop control to end the session. Keep any test work local and avoid a message that sends or publishes content. | Read the actual active voice-session control/state after start and the ended state after stop. Record dictation separately; endpoint mute and transcript presence do not establish a live voice conversation. |
| Dot show, hide and focus | Identify the installed feature the user calls Dot and its actual controls; do not assume it is the same surface as Mini. Exercise its configured show/hide/focus path and reacquire its window identity when necessary. | Read visibility and focused target before and after each action, including return to the originating window. Report only window behavior; Dot appearance proves no voice action. |
| Mini open, close and return | Identify the installed Mini window and its available open/close/return controls. Open it, close or dismiss it using that control, then return to the original chat/task window. | Read the Mini and originating window identities, visibility and focus after each step. Distinguish closing the window from ending voice or quitting the client. |

Do not invent a WeChat shortcut, a universal Codex shortcut, a Dot/Mini API, or
voice-state support. Use only a verified client setting or an observed native
control. If voice is already active, record it and restore that precondition
before testing a start action; a toggle must not be mistaken for an explicit
start or stop.

## Failure and isolation scenarios

- **Microphone unplug:** associate an exact endpoint, read its status, unplug
  it, and retry a read. Require an error/unknown state with the saved identity
  retained and no fallback. Reconnect and explicitly revalidate the same ID.
- **Shortcut conflict:** occupy a test chord with a controlled fixture owner,
  attempt to save it, and require rejection plus the previous saved/live
  binding. Do not take over a client's voice shortcut for device mute.
- **Authorization and service failure:** expire authorization or restart the
  isolated service; require unknown state and disabled mutation controls until
  fresh authorization. Save/readback errors must never become success receipts.
- **Meeting isolation:** use a dedicated meeting/test call and an independent
  playback source. Explicitly select one application's current playback session
  before any authorized playback mute test. Require native readback and
  continuing meeting/other playback. Record microphone mute before/after; it
  must be unchanged by playback controls. Separately document that endpoint
  microphone mute affects every application using that microphone.
- **Cleanup:** discard the scratch editor without sending, remove only test
  entries, and restore explicitly recorded test settings. Do not automatically
  unmute, restore a stale audio state, stop unrelated calls or terminate clients.

Keep service, microphone, application voice, Dot, Mini and meeting-isolation
results separate. A blocked or skipped scenario is unfinished acceptance,
even when compilation, fixtures and process E2E pass.
