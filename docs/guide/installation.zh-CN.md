# 安装与配置指南

本指南面向第一次使用 VibDock 的用户，说明传统安装、连接 Vibe Key、配置按键和验证结果。另一种方式是让 Agent 通过 [Agent skill](../../skills/vibdock/SKILL.md) 读取本机服务并完成配置；两种方式操作的是同一套配置。

> 当前状态：AU05 固件直发、应用快捷键、硬件动作和最近触发已在真实设备上验证；
> 单击/双击/长按、多键组合和 Virtual Pad 仍是设计/源码候选，尚未全部接入生产页面。
> 本文会明确标注“暂未开放”的部分。

## 1. 系统要求

- Windows 10/11 x64。
- Ulanzi AU05 Vibe Key（3 键 + 旋钮 + 麦克风）及 2.4G 接收器。
- 不需要 Ulanzi Studio；如果 Studio 正在运行，请先退出，避免厂商 HID 接口被占用。
- 从源码构建需要 Rust stable；普通用户使用发布包即可，不需要 Node.js/Python。

## 2. 安装

### 方式 A：发布包

1. 从 [Releases](https://github.com/loonghao/vibdock/releases) 下载 Windows 压缩包。
2. 解压到固定目录，例如 `C:\Program Files\VibDock` 或 `D:\Apps\VibDock`。
3. 确认目录里至少有 `vibdock.exe` 和 `vibdock-launcher.exe`。
4. 运行 `vibdock-launcher.exe`。它会常驻托盘，并打开需要授权的本地 Settings。

发布包可能被 SmartScreen 拦截，因为候选版本尚未做代码签名。请核对下载来源和 SHA-256 后再决定是否运行；不要从第三方站点下载。

### 方式 B：从源码构建

```powershell
git clone https://github.com/loonghao/vibdock.git
cd vibdock
cargo build --release
.\target\release\vibdock-launcher.exe
```

如果只想用 CLI：

```powershell
.\target\release\vibdock.exe service-start
.\target\release\vibdock.exe hardware-status
```

## 3. 第一次启动

1. 插好 2.4G 接收器，给 Vibe Key 通电。
2. 按一下任意按键唤醒设备；状态灯亮起。
3. 运行 launcher。托盘出现 VibDock 图标，浏览器打开本地 Settings。
4. `service-start` 或 launcher 会复用已经运行的服务，不会重复监听端口。
5. 关闭浏览器不会停止后台服务；从托盘菜单重新打开 Settings，或使用 `service-stop` 退出。

首次运行会写入默认布局：

| 按键 | 默认行为 |
| --- | --- |
| 按钮 1 | 固件直发 `Ctrl + Win`（微信语音） |
| 按钮 2 | 固件直发 `Enter`（回车） |
| 按钮 3 | 固件直发 `Escape`（取消） |

写入后会读回校验；只有读回一致才显示 `provisioned`。

## 4. 连接与冲突

- AU05 的厂商 HID 接口同一时间只能被一个程序独占。使用 VibDock 时不要同时运行 Ulanzi Studio。
- 如果页面显示“未检测到设备”：检查接收器、供电、按键唤醒，然后点“刷新”或执行 `vibdock hardware-status --sync`。
- 已知 2.4G 接收器的按键转发通道可能卡死：配置通道仍正常，但主机收不到任何按键。此时重新插拔接收器或给设备断电重启，然后再测试。
- 一个 AU05 按钮只能承载一种模式：应用快捷键、固件直发组合键、硬件动作。冲突由服务端拒绝，不会静默覆盖。

## 5. 配置方式一：固件直发组合键

适合 `Ctrl + Win`、单键、媒体键这类 VibDock 不能注册为全局热键的组合。

1. 打开 Settings → **应用与麦克风** → **Vibe Key 硬件按键**。
2. 在“硬件直发快捷键”选择按键 1/2/3。
3. 输入组合键或单键，例如 `Ctrl + Win`、`Enter`、`Escape`。
4. 填写可选说明，例如“微信语音”。
5. 点“写入直发组合键”。页面会读回固件槽位；失败会显示原因。
6. 按一下物理按键验证目标应用是否响应。

也可以使用 CLI：

```powershell
vibdock hardware-bind --button 1 --chord "Ctrl + Win" --label "微信语音"
vibdock hardware-unbind --button 1
```

注意：固件直发是设备发出的真实按键，VibDock 不注册也不派发，因此最近触发里可能没有对应记录。写入成功只代表固件槽位正确，不代表目标应用一定响应。

## 6. 配置方式二：应用快捷键

适合 Codex、DeepSeek Harness、WorkBuddy、微信输入法等应用。

1. 先打开目标应用，让它拥有一个前台窗口。
2. Settings → **应用与麦克风** → **应用快捷键** → 新建。
3. 选择应用；如果列表为空，先打开应用再刷新。
4. 设置 VibDock 触发键，例如 `Ctrl + Alt + Shift + F10`。
5. 设置发送给应用的快捷键，例如 `Ctrl + Shift + Space`。
6. 可选绑定一个 AU05 按钮；服务会把触发键写入该按钮并读回。
7. 保存后按物理按键；最近触发会记录 `DISPATCHED` 或错误。

CLI：

```powershell
vibdock shortcut-apps --json
vibdock shortcut-add --app "DeepSeek Harness" --trigger "Ctrl+Alt+Shift+F11" --dispatch "Ctrl+Shift+Space" --device-button 2
vibdock shortcut-list --json
```

`DISPATCHED` 只表示组合键已发到前台，不表示应用已经开始录音或执行命令。

## 7. 配置方式三：硬件动作序列

适合“打开 Codex → 等待 → 切换麦克风 → 发送语音快捷键”这样的组合流程。

1. Settings → **应用与麦克风** → **硬件动作** → 新建。
2. 选择来源：AU05 按钮，或游戏手柄按键。
3. 添加步骤：
   - 启动应用：从已识别应用里选，服务端解析路径；
   - 发送快捷键；
   - 切换麦克风（选择明确端点）；
   - 运行命令（从 `commands.json` 选择，填写参数）；
   - 等待 0–10000ms。
4. 保存。AU05 会自动生成一个未占用的触发键并写入固件；手柄不需要固件写入。
5. 按来源按键；最近触发会显示 `source: "action_binding"`、`state: "DONE"` 和步骤 detail。

示例 `commands.json`：

```json
{"schema_version":1,"commands":[{
  "id":"build",
  "label":"构建项目",
  "executable":"C:\\Tools\\build.exe",
  "args":["--project","{project}"],
  "parameters":[{"name":"project","label":"项目","required":true,"max_length":40}]
}]}
```

命令使用 argv 数组，不经过 shell 字符串。不要在网页里填写任意 shell 命令。

## 8. 手势与多键组合（设计/源码候选）

目标模型：

- 每个按键：单击 / 双击 / 长按；
- 多键：`1+2`、`1+2+3`、`2+3` 快按，`1+2` 长按；
- 3 个按键 → 9 个以上动作位。

当前源码有分类器和单元测试，但 Windows 哨兵键监听、持久化和生产 UI 还没有接通。**不要在发布版上承诺这些手势已经可用。** 设计稿见 [手势版工作台](../design/prototype-workbench.html) 和 [plugin workbench 文档](../design/plugin-workbench.md)。

## 9. 首次配置向导（前端引导）

[`docs/design/onboarding.html`](../design/onboarding.html) 是首次配置的交互设计稿：

1. 连接设备；
2. 从模板开始（语音工作流 / 编程助手 / 微信输入法 / 空白）；
3. 确认 9 个动作位（每个按键的单击 / 双击 / 长按，可选 Profile、手动输入或录入组合键）；
4. 测试反馈并完成。

页面右上角还有“让 Agent 配置”，会生成一段可复制的 Agent 指令。当前它是静态原型，生产 Settings 还没有接入这个向导；安装路径和配置语义以本文为准。

## 10. 用 Agent 配置

VibDock 的 Agent skill 在 [`skills/vibdock/SKILL.md`](../../skills/vibdock/SKILL.md)，辅助脚本在 [`scripts/vibdock-agent.ps1`](../../scripts/vibdock-agent.ps1)。用户可以直接说：

> 帮我把按钮 2 配置成打开 Codex，按钮 3 长按打开 DSH；先读当前配置，不要覆盖已有绑定。

Agent 应该：

1. 读取 `GET /api/plugins`、`GET /api/actions` 和 `hardware-status`；
2. 从 `shortcut-apps` 解析应用引用，不猜路径；
3. 在修改前说明将写什么，等待用户确认；
4. 写完后读回固件/最近触发，报告 `provisioned` / `DONE` / 错误；
5. 不打印 token，不暴露 loopback 服务到局域网。

常用 helper：

```powershell
.\scripts\vibdock-agent.ps1 plugins
.\scripts\vibdock-agent.ps1 actions
.\scripts\vibdock-agent.ps1 action-commands
.\scripts\vibdock-agent.ps1 hardware
.\scripts\vibdock-agent.ps1 pad-execute -Id <saved-action-id>
```

## 11. 验证与故障排查

```powershell
vibdock hardware-status --json
vibdock shortcut-list --json
vibdock service-status
```

- **设备未检测到**：接收器、供电、按键唤醒；退出 Studio；点刷新/`--sync`。
- **按键无反应但配置正常**：可能是 2.4G 转发通道卡死，重新插拔接收器。
- **组合键冲突**：换一个触发键；服务端会返回 409。
- **写入失败**：页面显示读回原因；不要反复盲写，先确认设备在线。
- **服务未运行**：launcher/`service-start`；关闭浏览器不等于停止服务。
- **安全**：服务默认只监听 loopback；不要把端口直接暴露到局域网。远程平板配对尚未实现。

## 12. 更新与卸载

- **更新**：停止服务（托盘 Exit 或 `vibdock service-stop`），替换 exe，再启动 launcher。配置保存在 `%LOCALAPPDATA%\VibDock`，不会被覆盖。
- **卸载**：先停止服务并退出托盘；删除程序目录。若要清除配置，再删除 `%LOCALAPPDATA%\VibDock`（会丢失已保存绑定）。
- **重置**：停止服务，备份后删除 `%LOCALAPPDATA%\VibDock\hardware.json` / `actions.json` / `shortcuts.json`，重新启动。

---

English version: [installation.md](installation.md) · 返回 [README](../../README.zh-CN.md)


