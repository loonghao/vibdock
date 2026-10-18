# Voice integration evidence

Checked against official sources on 2026-10-15. These are integration requirements and documentation findings, not installed-client acceptance.

## Codex / ChatGPT desktop

[ChatGPT Voice](https://learn.chatgpt.com/docs/features/voice) documents a configurable voice-chat shortcut in Settings > Voice. Availability depends on the client, rollout, account, and workspace. Read the user's configured shortcut rather than assuming a universal key combination. Starting a conversation and dictating prompt text are distinct workflows. The page describes ending a conversation through its UI; it does not establish a third-party API for microphone mute, output mute, or voice-state readback.

[Codex Micro](https://learn.chatgpt.com/docs/features/codex-micro) documents device-specific push-to-talk and voice controls, including microphone toggling and ending a conversation when that mode is available. This establishes a native device integration, not a public control protocol for arbitrary hardware. It does not establish that Ulanzi buttons can invoke those device-specific actions.

The installed client, exact configured shortcut, focus behavior, and returned voice state remain unverified. A keyboard event alone must produce an unverified dispatch result, not a claim that voice started or stopped. Physical microphone mute is separately observable through the Windows audio adapter and affects every app using that endpoint.

## WeChat Input

The official site at https://shurufa.weixin.qq.com/ could not be retrieved during this check, and official-domain searches did not establish a current voice shortcut or automation API. This is missing evidence, not proof that those features are unavailable. Keep the integration pending until the installed application's own settings and an authoritative source are inspected.

Do not hard-code a shortcut from a third-party tutorial or claim voice-state readback from input delivery. Record the installed version, configured action, shortcut, focus requirements, and verification method when acceptance becomes possible.

Full host inspection, source status, and the prerequisite contract are in [WeChat Input integration evidence](wechat-input.md).

## Installed-client inspection (2026-10-18)

Read-only observation on one Windows 11 x64 host. No client settings were
changed, no shortcut was sent, no voice session was started, and no microphone
was recorded.

| Client | Installed identity | Observed surface | Still unknown |
| --- | --- | --- | --- |
| WeChat Input | registry `微信输入法 3.0.0.34`, `C:\Program Files\Tencent\WeType`; running `wetype_server.exe`, `wetype_renderer.exe`, `wetype_service.exe`, `wetype_update.exe` | background helpers and install assets only | configured voice trigger, focus rule, start/dictation/stop result, queryable state |
| Codex/ChatGPT desktop | MSIX `OpenAI.Codex_26.1013.4894.0_x64__2p2nqsd0c76g0`, `...\app\ChatGPT.exe`; multiple running processes | packaged desktop app and a foreground window | configured voice trigger, voice/PTT controls, start/stop result, queryable state |

The WeChat Input settings store is an MMKV binary held by the running client and
was not parsed; the Codex `settings.dat` was not decoded. No documented public
API or state query was found for either client. A configured trigger is therefore
**unknown and must be read from the client by the user**, not guessed from a
tutorial or hard-coded.

This inspection upgrades the installed-version/provenance evidence only. It does
not establish `start_voice`/`end_voice`, dictation, focus requirements, or any
voice-active state, and it does not justify changing a capability from `unknown`
to `unverified` or verified.

## Shortcut ownership

An application-owned voice shortcut and a VibDock-owned device-mute shortcut have different owners and effects. Assign distinct combinations. Windows can reject a registration already held by another process; silently taking the application's voice shortcut for microphone mute would change the user's intended action.

For direct application mapping, configure the hardware utility to emit the application's own shortcut. This does not require VibDock to register that same chord. A future adapter may coordinate actions only through explicit user configuration and must distinguish dispatch from verified state.

## Recognition versus control (Windows source)

Current Windows source recognises running applications that own a visible
top-level window, independently of playback sessions, and labels exact
executable basenames for Tencent Meeting, WeChat Input, Codex/ChatGPT and
WorkBuddy. This is **presence recognition only**. It does not inspect installed
versions, settings, accounts, voice assets, network state, or whether a client
accepts a shortcut.

A user may save one per-application binding with both chords supplied from their
own client configuration. The default scope requires the saved executable to own
the foreground window. An explicit `running` scope exists for background input
methods: it requires the executable to appear in the fresh application scan and
sends the chord to whatever application is foreground. Either way VibDock
records `DISPATCHED`; the result is not voice-active evidence. Background
input-method services are recognised only for the closed `wetype*`, `wemeet*`,
`codex` and `workbuddy` names; other tray-only helpers without a window are not
listed, and a partial window scan reports unknown rather than "not running". Do
not convert this recognition into a claim that Tencent Meeting, WeChat Input, or
Codex is connected, controlled, or verified.

## Acceptance record

For each adapter, retain source URL and check date, client version, installed detection, configured trigger, supported action, observed result, failure behavior, and hardware identity quality. Mark undocumented or untested facts unknown. Do not generalize Codex Micro behavior to Ulanzi, a synthetic keyboard event to physical device identity, or input mute to application playback mute.
