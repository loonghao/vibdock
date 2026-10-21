# WeChat Input (微信输入法 / WeType) integration evidence

This is integration evidence and a prerequisite contract, not installed-client acceptance.
No capability here is reported as verified. VibDock is independent of this vendor.

A bounded check on 2026-10-18 found `微信输入法 3.0.0.34` installed under
`C:\Program Files\Tencent\WeType`, with `wetype_server.exe`, `wetype_renderer.exe`,
`wetype_service.exe` and `wetype_update.exe` running. Its **语音输入** settings page
showed `Ctrl Win Shift` for start/stop and `Ctrl Win` for hold-to-talk; sending the
displayed start/stop chord created a `语音输入` window and sending it again removed
it. Microphone capture, audio route, dictation, account state and any queryable
voice state were **not** verified; see
[voice integration evidence](voice-evidence.md#installed-client-inspection-2026-10-18).
The historical 2.1.4.6 snapshot below is retained as a dated sample only.

Current Windows source can recognise a running WeChat Input process by its exact
`wetype*.exe` basename, including a background helper with no window. The user
may save a per-application shortcut with the explicit `running` scope; VibDock
then sends the user's configured chord to the current foreground application
while the WeChat Input process is running. That is presence recognition plus
user-supplied dispatch; it does not establish the trigger, a WeType control
surface, or voice state, and it does not change the `unknown` grades below.

## Official source check: 2026-10-15 retrieval failed

The official site at https://shurufa.weixin.qq.com/ could not be retrieved during the
2026-10-15 check (DNS did not resolve). Official-domain searches did not establish a
current voice shortcut, a default trigger, or an automation API.

This is **missing evidence, not proof of absence**. Under the grading rule in
`src/capabilities.rs`, a probe working from documentation alone must report `unknown`
for anything the documentation does not describe. With no retrievable official source,
every WeChat Input capability is `unknown`.

Nothing is graded `unverified`: that grade asserts documentation describes a capability,
and no such documentation was retrieved.

## Historical single-host snapshot (dated sample, not current-state detection)

> **Scope warning.** Everything below was observed on **one developer host on
> 2026-10-15** and is preserved as a bounded, dated sample. It is **not** a detection
> result and **not** a statement about the current machine, any other machine, or any
> future run. The runtime probe carries none of these facts; see
> `src/integrations/wechat_input.rs`, which reports no installed-client state at all.

Observed on that host on 2026-10-15 with no configuration, audio, keyboard, or process
changes:

- Installed: `微信输入法` 2.1.4.6, publisher 腾讯科技(深圳)有限公司,
  `InstallLocation=C:/Program Files/Tencent/WeType`.
- Running processes `wetype_service`, `wetype_server`, `wetype_update`,
  `wetype_renderer`, all from `C:/Program Files/Tencent/WeType/2.1.4.6/`.
  Command lines were bare exe paths at that moment.
- Windows service `WeType Management Service` (`wetype_service.exe`, auto start).
- TSF/TIP registered: `{86598FB9-66A2-463E-B9C2-AEB906D477AD}=WeType` via
  `C:/WINDOWS/system32/wetype_tip.dll`.
- Voice assets ship with the client: `voice_engine/vad/`, `BuildIn/stt.bin`,
  `BuildIn/tts.bin`, `tensorflowlite_c.dll`, `record_windows_plugin.dll`, and Flutter
  voice/recording icons.
- No listening TCP port and no `wetype|tencent|wxime` named pipe was exposed by any
  WeType process at probe time.

What this does and does not establish:

- The assets prove **bundled voice/recording files**, not a functioning voice feature and
  not a reachable one. File presence is not a capability.
- The snapshot shows **no obvious local control surface** on that host at that time — no
  HTTP listener, no pipe, no CLI arguments. Bare process command lines do **not** prove
  the client has no supported CLI switches; a matched-pipe/TCP snapshot does **not** prove
  there is no API or control surface.
- It does **not** disprove a private, undocumented interface. That is why the probe stops
  at `unknown` and never reports `unsupported`.
- Registry `HKCU/HKLM\SOFTWARE\Tencent\WeType` carried no readable configuration values;
  APPDATA holds only MMKV state and an empty `setup`. No client setting was inspected, so
  nothing here establishes how the voice trigger is configured.

## Still unknown

1. The actual voice trigger. The configured value must be **supplied by the user from
   their own client settings**. **Do not hard-code a shortcut** from a tutorial, from
   another deployment, or from this document. Whether the trigger is user-configurable at
   all is **not established here**: no official documentation was retrieved and no client
   setting was inspected, so the adapter requires a user-supplied trigger without
   asserting configurability as a verified fact.
2. The IME focus / activation rule for the trigger to be honoured.
3. Whether session state is queryable at all.
4. Whether the bundled voice assets are reachable from outside the client, and whether any
   control surface exists. Absence of an obvious one on one host is not evidence of
   absence.

## Prerequisite contract

A WeChat Input voice action may only be enabled when all of the following hold. Until
then the adapter stays declarative and no mutation path is enabled.

- The user supplies the configured trigger from their own client settings.
- An official source or a live inspection establishes that the trigger is honoured under
  the required focus conditions.
- The dispatched action is reported as **dispatch only**, never as voice-active state.

A keyboard event alone produces an unverified dispatch result. It is not evidence that
voice started or stopped.

## Shortcut ownership

An application-owned voice trigger and a VibDock-owned device-mute shortcut have
different owners and effects; assign distinct combinations. Windows can reject a
registration already held by another process, and taking the application's voice trigger
for microphone mute would change the user's intended action.

For direct mapping, configure the hardware utility to emit the application's own
shortcut. That does not require VibDock to register the same chord.

## Optional Vibe Key tap/hold configuration

The existing gesture editor can assign **Single → Toggle microphone** with an
explicit endpoint ID and **Long → Hold shortcut until release** on the same
button. Enter the hold-to-talk chord from your own input-method settings; the
editor leaves it blank. The hold threshold defaults to 550 ms and accepts
150–5000 ms. A double-click slot cannot share this mode: each short release
must run Single immediately, including two rapid taps.

The long slot uses the authenticated gesture API and existing `gestures.json`:

```json
{"revision":1,"name":"Input method hold","button":0,"gesture":"long",
 "steps":[],"hold_keys":"<your configured hold-to-talk chord>","long_press_ms":550}
```

This is an opt-in Windows keyboard dispatch mode. It changes that button's
firmware sentinel to bare F9, F10 or F11 and reserves the corresponding bare
key across keyboards while enabled; it cannot identify which keyboard sent it.
Ordinary gesture buttons keep their modified sentinels and double-click timing.
Conflicting saved bindings must be removed explicitly, and firmware readback or
runtime activation failures are reported separately from successful persistence.
Restarting reloads the saved mode without changing microphone routing.

Long presses never execute Single. A dedicated input thread releases the owned
chord on button release, Escape, configuration change or service exit, even while
another saved action is waiting. Cancellation does not toggle the microphone.
The selected endpoint toggle affects applications sharing that endpoint; it is
separate from input-method dictation and Codex's internal voice controls.

`HELD`/`RELEASED` are keyboard dispatch results, not evidence that WeChat started,
recognized or submitted speech. Native AU05/input-method and real audio acceptance
remain required; configure and test them outside an active voice call.

## Acceptance record required before any verified grade

Retain for the adapter: source URL and check date, client version, installed detection,
configured trigger, focus requirements, supported action, observed result, failure
behavior, and verification method. Mark undocumented or untested facts unknown.

Do not generalize Codex behavior to WeChat Input, a synthetic keyboard event to physical
device identity, or input mute to application playback mute.
