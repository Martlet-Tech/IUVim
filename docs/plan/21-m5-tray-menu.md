# 21 · 任务书 M5：语言栏右键菜单（2026-08-17 重定义，去托盘）

> 状态：**已实现**（2026-08-17 重定义后完成，待手测验收）。前置阅读：`00-overview.md`、`01-contract.md`、`19-m4-cross-render.md`、`30-conventions.md`。
> **范围修正（2026-08-17 用户实测反馈）**：不做独立托盘图标——右键菜单挂在现有
> 语言栏「中/英」按钮上（TSF 官方 `ITfLangBarItemButton::InitMenu` + `ITfMenu` 机制）。
> 原 M5 方案（Shell_NotifyIcon 托盘 + iuv-ui 自绘菜单窗口）**已废弃**：自绘菜单窗口
> 与托盘代码删除，iuv-ui `render_menu` 保留（候选窗右键菜单 M2 槽位未来可用）。

## 1. 目标（验收一句话）

右键语言栏「中/英」按钮弹出菜单，含「设置」「关于」两项（**无退出**——输入法无退出
语义，daemon 生命周期由卸载脚本管理）。「设置」→ 管道 `OpenSettings` 通知守护进程
弹 egui 设置页；「关于」→ MessageBox 显示版本。

## 2. 背景与决策记录

- 原 M5 = Shell_NotifyIcon 托盘 + 自绘菜单（方案 A 单实例托管）——**2026-08-17 废弃**：
  - 用户实测托盘/自绘菜单均不可见（根因见 19-m4 §5：DCompositionCreateDevice 未关联 D2D，
    BeginDraw E_INVALIDARG），且用户明确「不要独立托盘图标，右键菜单应出现在现有中/英图标上」。
- TSF 官方机制：语言栏按钮右键时，语言栏调用 `ITfLangBarItemButton::InitMenu(ITfMenu)`，
  我们经 `ITfMenu::AddMenuItem` 添加自定义项，`OnMenuSelect(wid)` 分发选择。这是系统绘制
  菜单（放弃自绘样式，换取系统集成与零渲染依赖）。
- 语言栏「中/英」按钮每个激活的应用会话进程都有（TSF 语言栏），任意进程右键均可触发；
  动作（设置）经命名管道发到唯一 daemon，天然单点。
- 兜底：若实测 Win10/11 语言栏不调用 InitMenu（风险标注），退回
  `OnClick(TF_LBI_CLK_RIGHT)` + 系统 `TrackPopupMenu`。

## 3. 架构

```
iuv-tsf/src/langbar.rs（ITfLangBarItemButton）
├── InitMenu(ITfMenu)：AddMenuItem × 2——「设置」wid=1、「关于」wid=2
└── OnMenuSelect(wid)：1 → DaemonClient.send_request(Request::OpenSettings)
                       2 → MessageBoxW 关于
iuv-data/src/ipc.rs：Request 新增 OpenSettings / Quit（编码 tag 0x06/0x07）
iuv-daemon：主线程轮询 open_settings → eframe 设置窗；Quit → 干净退出
```

## 4. 任务清单

| # | 任务 | 状态 |
|---|---|---|
| 1 | langbar InitMenu/OnMenuSelect | ✅ 设置/关于两项；DaemonClient 注入（Activate 时构造顺序调整） |
| 2 | ipc.rs OpenSettings/Quit | ✅ tag 0x06/0x07 + 编解码 |
| 3 | 删除托盘 | ✅ iuv-tsf/src/tray.rs、ui/menu_window.rs、config.tray_icon 全删 |
| 4 | daemon 去托盘 | ✅ daemon tray.rs 删；主线程改命令轮询 + eframe 设置窗（主线程） |
| 5 | 文档同步 | ✅ 本文 + 22/19/01/AGENTS |

## 5. 已知风险与取舍

- **语言栏是否调 InitMenu 待手测**：Win10/11 语言栏按钮右键行为若未走 InitMenu，
  退回 `OnClick(TF_LBI_CLK_RIGHT)` + TrackPopupMenu（系统菜单，同样满足"右键出菜单"）。
- 系统菜单（非自绘）：放弃 iuv-ui 主题化，换取 TSF 官方集成稳定性（用户已认可）。
- 语言栏菜单每个进程一份，但动作收敛到 daemon 单点，无一致性问题。
- 「关于」用 MessageBox（模态简单文本）；后续可换自绘小窗（低优先）。

## 6. 槽位

- 菜单项扩展：词库管理/屏蔽词批量管理（M2 槽位）入口 → 同走 `OpenSettings` 或新增命令。
- 候选窗右键菜单（18-m2 槽位）：iuv-ui `render_menu` 保留待用（系统 ITfMenu 不适用候选窗）。
- M7 安装器：daemon 首会话自启（`Quit` 命令供卸载脚本干净退出）。

## 7. DoD（已实现部分）

```
cargo check --workspace / cargo test --workspace     # ✅ 全绿（242 通过）
cargo build -p iuv-tsf --release                     # ✅
待手测：右键中/英按钮出「设置/关于」菜单；设置 → daemon 弹设置页（daemon 运行中）；
关于 → 对话框；daemon 未运行 → 设置点击静默（记日志）
```