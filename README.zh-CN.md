<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/brand/vibdock-symbol-light.svg">
    <img src="assets/brand/vibdock-symbol.svg" alt="VibDock" width="92" height="92">
  </picture>
</p>

<h1 align="center">VibDock</h1>

<p align="center"><b>一个按键，驱动你的整套 AI 工作流。</b><br>
连接实体按键、语音工作流和个人 AI Agent 的本地优先桥梁。</p>

<p align="center">
  <a href="README.md">English</a> ·
  <a href="README.technical.md">技术细节（英文）</a> ·
  <a href="#快速开始">快速开始</a> ·
  <a href="#界面">界面</a> ·
  <a href="#首次配置引导">首次配置</a>
</p>

<p align="center">
  <img alt="Windows" src="https://img.shields.io/badge/Windows-10%20%2F%2011-0078D4?style=flat-square">
  <img alt="Rust" src="https://img.shields.io/badge/Rust-2021-B7410E?style=flat-square">
  <img alt="本地优先" src="https://img.shields.io/badge/%E6%9C%AC%E5%9C%B0%E4%BC%98%E5%85%88-%E4%B8%8D%E4%B8%8A%E4%BA%91-3F7A5F?style=flat-square">
  <img alt="MIT" src="https://img.shields.io/badge/license-MIT-1D1B18?style=flat-square">
</p>

<p align="center">
  <img src="assets/screenshots/workbench.png" alt="VibDock 工作台：点击 Vibe Key 按键、选择手势、执行 Profile" width="920">
</p>

VibDock 把小巧的实体键盘变成 AI 编程助手、语音输入和桌面操作的控制器。
按一下按键就能对 Codex 或 DeepSeek Harness 说话、静音麦克风、启动工作流，
或者执行一串保存好的动作——不需要安装厂商 Studio，也不需要把数据上传到云端。

界面由本机常驻的 Rust 服务提供；设备、全局热键和进程动作都由这个服务执行。
浏览器是配置入口，而不是拥有额外权限的远程控制器。

## 核心能力

- **实体按键，不依赖厂商 Studio。** 首次运行会把 `Ctrl + Win`（微信语音）、
  `Enter`、`Escape` 写入 Ulanzi AU05 的三个按键，并通过读回校验确认写入。
- **一个按键，执行一串动作。** 启动应用、发送组合键、切换麦克风、运行本地
  结构化命令，或者等待；步骤按顺序执行，失败即停。
- **单击 / 双击 / 长按 + 多键组合。** 三个按键可以变成九个以上的动作位，
  支持 `1+2`、`1+2+3`、`2+3` 快按，以及 `1+2` 长按。
- **触屏优先的 Virtual Pad。** 把同一套 Profile 投影成平板或副屏上的大按钮。
  浏览器只请求本机服务执行已保存动作，不直接注入按键。
- **插件化内核。** 设备、输入、动作层已经拆成 Rust crate，配合 manifest 和
  capability 校验；新增键盘或 Agent 集成不需要在 host 里为每个按钮写分支。

## 界面

<table>
<tr>
<td width="50%"><img src="assets/screenshots/workbench.png" alt="设备优先的工作台"><br><sub><b>工作台。</b> 选择按键，查看三种手势和多键组合。</sub></td>
<td width="50%"><img src="assets/screenshots/editor.png" alt="手势动作编辑器"><br><sub><b>编辑器。</b> 抽屉式编辑，保持页面干净：选 Profile、看步骤、调时序。</sub></td>
</tr>
<tr>
<td width="50%"><img src="assets/screenshots/plugins.png" alt="插件面板"><br><sub><b>插件。</b> 设备与应用能力集中管理，安装状态清晰可见。</sub></td>
<td width="50%"><img src="assets/screenshots/pad.png" alt="Virtual Pad"><br><sub><b>Virtual Pad。</b> 为平板或副屏准备的大尺寸触控按键。</sub></td>
</tr>
</table>

> 这些图片是当前的**产品设计预览**。生产 Settings 页面正在迁移到这个工作台；
> 当前源码仍构建旧版 Settings。Pad 已有可用的 `POST /api/pad/execute` 和静态
> 原型，但 `/pad` 路由与配对流程尚未开放。

## 快速开始

**普通用户：** 从 [Releases](https://github.com/loonghao/vibdock/releases) 下载
已发布压缩包，把两个 exe 解压到同一目录，运行 `vibdock-launcher.exe`。启动器会
常驻托盘并打开需要授权的本地 Settings。

**从源码构建（Windows 10/11）：**

```powershell
git clone https://github.com/loonghao/vibdock.git
cd vibdock
cargo build --release
.\target\release\vibdock-launcher.exe
```

不需要 Node.js、Python 或单独启动 Web 服务。已发布的 v0.1.0 早于当前源码；
AU05 / 动作相关能力目前是源码候选版本，不是新的正式发布。

## 首次配置引导

首次配置流程已经接入 Settings（服务模式）：连接设备、选择模板、确认
3×3 手势矩阵、保存。它会通过认证 API 写入真实的 AU05 手势位；固件读回失败会
明确提示，而不是假装成功。

![首次配置引导](assets/screenshots/onboarding.png)

每个按键都提供单击、双击、长按三种手势。点任意格子可以选择 Profile、手动输入
组合键，或直接录入按键；这就是 Workbench 使用的 3×3 手势矩阵。

![手势位编辑器](assets/screenshots/onboarding-editor.png)

- [安装与配置指南](docs/guide/installation.zh-CN.md)：传统安装、固件直发、
  应用快捷键、硬件动作、验证、故障排查和卸载。
- [独立首次配置原型](docs/design/onboarding.html)：视觉参考和 Agent 指令。

## 让 Agent 帮你配置

VibDock 提供 [Agent skill](skills/vibdock/SKILL.md) 和
[辅助脚本](scripts/vibdock-agent.ps1)。你可以直接说：

> 帮我把按钮 2 配置成打开 Codex，按钮 1 放微信语音。先读当前配置，不要覆盖已有绑定。

Agent 会读取 `GET /api/plugins`、`GET /api/actions` 和 `hardware-status`，
从应用引用解析目标而不是猜路径，在写固件前先确认，并在完成后报告读回结果和最近触发。

## 架构

```
vibdock-launcher / vibdock-cli
            │
            ▼
      VibDock host           认证、配置、Profile、动作执行、API
      ├── vibdock-device-au05      AU05 HID + 固件读回
      ├── vibdock-device-gamepad   通用 HID 手柄
      ├── vibdock-actions          动作/Profile 模型与命令
      ├── vibdock-input            热键、手势、组合匹配
      ├── vibdock-au05             AU05 纯协议
      └── vibdock-plugin-api       manifest、capability 与插件 trait
```

设备层和动作层已经不依赖 service 与 web；host 通过 `GET /api/plugins` 暴露
内置插件清单，后续拆分项记录在公开 roadmap 中。

## 当前状态（不夸大）

| 能力 | 状态 |
| --- | --- |
| AU05 固件直发绑定 | 已在真实设备验证：默认布局、读回、直发组合键、物理动作 |
| 硬件动作序列 | `wait` 步骤已实机验证；launch / send-keys / mic / command 链路已实现，完整链路未实机验收 |
| 手势与多键组合 | AU05 的单击/双击/长按动作位已持久化并由服务执行；多键组合与钩子级按键吞掉仍在开发 |
| 游戏手柄 | 适配器已实现；验收环境没有可用手柄 |
| Virtual Pad | 原型 + 已保存动作执行 API；`/pad` 路由和配对未完成 |
| 插件框架 | 设备 / 动作 / 输入 crate 已拆出；内置清单只读；第三方 sidecar 未完成 |

完整验收记录见 [docs/testing](docs/testing)，详细技术说明见
[README.technical.md](README.technical.md)。发送组合键成功或 API 返回成功，
都不代表目标应用已经改变状态。

## 设计与 Agent 文档

- [工作台设计稿（简洁方向）](docs/design/prototype-clean.html)
- [Virtual Pad 设计](docs/design/virtual-pad.md)
- [插件模块化方案](docs/design/plugin-modularization.md)
- [Agent skill](skills/vibdock/SKILL.md)：Agent 的安全 CLI / API 工作流
- [Agent 辅助脚本](scripts/vibdock-agent.ps1)：读取本地 token，但不打印
- [Roadmap issues](https://github.com/loonghao/vibdock/issues/71)

## 开发

```powershell
cargo fmt --all --check
cargo clippy --workspace --locked --all-targets -- -D warnings
cargo test --workspace --locked --all-targets
node scripts/test-binding-ui.cjs
```

完整功能矩阵、API、权限、设备细节和验收清单保留在
**[README.technical.md](README.technical.md)**。

## License

MIT。VibDock 与 Ulanzi、OpenAI、DeepSeek、Tencent、WorkBuddy 均无隶属关系；
产品名称仅用于描述集成目标。

<p align="center"><a href="README.md">Read in English →</a></p>






