# First-run onboarding

Status: clickable prototype, 2026-10-19. Source:
[`onboarding.html`](onboarding.html). The production Settings page does not serve
this wizard yet.

## Goal

Make the first five minutes self-explanatory without turning Settings into a long
document. A new user should connect the hardware, pick a useful layout, verify a
key, and know where to go next. The same flow has a second entry: let an agent do
the configuration from a plain-language prompt.

## Flow

| Step | Content | Exit condition |
| --- | --- | --- |
| 1. Connect | receiver, power/wake, local service, detected firmware | device detected; "next" enabled |
| 2. Template | voice workflow / coding / WeChat Input / blank | a template selected |
| 3. Assign | three single-click rows with profile selects | all rows have a value or "unassigned" |
| 4. Test | physical or virtual test tiles, one mark per key | all three tested, or user skips |

After step 4 the done screen offers **Open Workbench**, **Open Virtual Pad**, or
**Let an agent continue**.

## Template model

| Template | Button 1 | Button 2 | Button 3 |
| --- | --- | --- | --- |
| Voice workflow | WeChat voice | Codex voice | DSH workspace |
| Coding | Codex voice | DSH workspace | microphone mute |
| WeChat Input | WeChat voice | Enter | Escape |
| Blank | unassigned | unassigned | unassigned |

Templates only seed single-click assignments. Double/long press and multi-key
combos are added later in the workbench; the wizard never claims they are active
before the gesture layer exists.

## Agent path

The header and done screen expose **Let an agent configure**. It opens a prompt
that the user can copy:

```text
请用 VibDock 的 agent skill 帮我配置 Vibe Key：
- 按钮 1 单击：微信语音
- 按钮 2 单击：打开 Codex
- 按钮 3 长按：打开 DeepSeek Harness

先读取 GET /api/plugins 和 GET /api/actions，确认当前设备和已保存配置；
不要覆盖已有绑定。每一步修改前先告诉我将写什么，完成后用 hardware-status
或 recent_actions 读回结果。
```

The prompt is deliberately about intent, not raw JSON. The agent resolves the
current state, explains conflicts, asks for confirmation, then uses the same
authenticated API described in [`skills/vibdock/SKILL.md`](../../skills/vibdock/SKILL.md).
The onboarding itself never writes firmware; it is a guide and a handoff.

## Interaction and accessibility

- Keyboard: native buttons/selects, visible focus ring, Escape closes the agent
  modal.
- The stepper is not a progress bar that can be dragged; Back/Next are explicit.
- The device illustration remains a visual; the three mapping rows are the
  authoritative controls.
- Reduced motion disables transitions.
- No authenticated calls are made by the prototype.

## Production integration

When Settings migrates to the clean workbench:

1. Show the wizard only on first run (no saved bindings and no prior completion
   marker).
2. Read the device state from `GET /api/hardware` and the profile catalog from the
   plugin registry.
3. Persist the chosen template only after an authenticated confirmation.
4. A failed write keeps the previous configuration and shows the readback error.
5. Offer "Let an agent configure" with the actual local skill path or a local
   prompt, not a remote marketplace URL.

The install/configuration semantics remain in
[`docs/guide/installation.md`](../guide/installation.md) and
[`docs/guide/installation.zh-CN.md`](../guide/installation.zh-CN.md).
