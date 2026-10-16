# WeChat Input (微信输入法 / WeType) integration evidence

This is integration evidence and a prerequisite contract, not installed-client acceptance.
No capability here is reported as verified. VibDock is independent of this vendor.

## Official source status: unreachable

The official site at https://shurufa.weixin.qq.com/ could not be retrieved during the
2026-10-15 check (DNS did not resolve). Official-domain searches did not establish a
current voice shortcut, a default trigger, or an automation API.

This is **missing evidence, not proof of absence**. Under the grading rule in
`src/capabilities.rs`, a probe working from documentation alone must report `unknown`
for anything the documentation does not describe. With no retrievable official source,
every WeChat Input capability is `unknown`.

Nothing is graded `unverified`: that grade asserts documentation describes a capability,
and no such documentation was retrieved.

## Local read-only host inspection

Confirmed on 2026-10-15 with no configuration, audio, keyboard, or process changes:

- Installed: `微信输入法` 2.1.4.6, publisher 腾讯科技(深圳)有限公司,
  `InstallLocation=C:/Program Files/Tencent/WeType`.
- Running processes `wetype_service`, `wetype_server`, `wetype_update`,
  `wetype_renderer`, all from `C:/Program Files/Tencent/WeType/2.1.4.6/`.
  Command lines are bare exe paths — **no CLI switches**.
- Windows service `WeType Management Service` (`wetype_service.exe`, auto start).
- TSF/TIP registered: `{86598FB9-66A2-463E-B9C2-AEB906D477AD}=WeType` via
  `C:/WINDOWS/system32/wetype_tip.dll`.
- Voice assets ship with the client: `voice_engine/vad/`, `BuildIn/stt.bin`,
  `BuildIn/tts.bin`, `tensorflowlite_c.dll`, `record_windows_plugin.dll`, and Flutter
  voice/recording icons.
- No listening TCP port and no `wetype|tencent|wxime` named pipe exposed by any WeType
  process.

What this does and does not establish:

- It proves a voice/recording feature **exists in the shipped client**.
- It shows there is **no obvious local control surface** — no HTTP listener, no pipe, no
  CLI switch.
- It does **not** disprove a private, undocumented interface. That is why the probe stops
  at `unknown` and never reports `unsupported`.
- Registry `HKCU/HKLM\SOFTWARE\Tencent\WeType` carried no readable configuration values;
  APPDATA holds only MMKV state and an empty `setup`.

## Still unknown

1. The actual voice trigger. It is user-configurable in the client, so the configured
   value must be read from the user's own settings. **Do not hard-code a shortcut** from a
   tutorial or from another deployment.
2. The IME focus / activation rule for the trigger to be honoured.
3. Whether session state is queryable at all.

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

## Acceptance record required before any verified grade

Retain for the adapter: source URL and check date, client version, installed detection,
configured trigger, focus requirements, supported action, observed result, failure
behavior, and verification method. Mark undocumented or untested facts unknown.

Do not generalize Codex behavior to WeChat Input, a synthetic keyboard event to physical
device identity, or input mute to application playback mute.
