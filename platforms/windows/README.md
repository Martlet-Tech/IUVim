# platforms/windows — Windows 平台层

## 目录

| 路径 | 说明 | 状态 |
|---|---|---|
| `iuv-tsf/` | TSF 输入法管线（COM/TSF + 候选窗窗口层 + 语言栏）+ 注册 | 已实施（M1~M2）；M4 起渲染层换 iuv-ui |
| `iuv-daemon/` | 守护进程 exe：唯一持有用户库 + 设置页 | M6（`docs/plan/22-m6-daemon.md`） |
| `README.md` | 本文档：门面现状 + M4~M6 规划 | — |

## 门面（候选窗）现状

- `iuv-tsf/src/ui/`：`CandidateUi` trait（冻结）+ `CandwinCandidateWindow`（ULW 呈现 + iuv-ui 绘图，M4 落地；共享呈现 `ui/ulw.rs`）
- 能力：竖/横排、页码、悬停高亮、点击选词、翻页环绕、DPI 缩放、工作区内收、
  真透明圆角/阴影、浅色/深色主题（config `theme` 字段）

## M4 跨平台渲染规划（`docs/plan/19-m4-cross-render.md`）

**决策记录（2026-08-16 定稿）**：
- **Tauri/WebView 已废**：候选窗每帧自绘场景，改跨平台纯 Rust 绘图栈
- 绘图：`crates/iuv-ui`（tiny-skia 0.12 + cosmic-text 0.19 + fontdb 系统字体），跨平台一份
- 呈现：**UpdateLayeredWindow**（WS_EX_LAYERED + ULW_ALPHA per-pixel 合成，真透明圆角/阴影）——
  D2D/DComp 路线 2026-08-17 实测 E_INVALIDARG 弃用（见 19-m4 §2）；共享呈现模块 `ui/ulw.rs`
- 主题：`Theme` 结构体 + config 浅色/深色；候选窗交互语义（不抢焦点/无闪烁/DPI）不变
- macOS/Linux：复用 iuv-ui，各自写窗口层 + 呈现层（CALayer/X11）

## M5 语言栏右键菜单（`docs/plan/21-m5-tray-menu.md`）

- **2026-08-17 重定义**：不做独立托盘图标——右键语言栏「中/英」按钮弹「设置/关于」
  （TSF `ITfLangBarItemButton::InitMenu` + `ITfMenu` 官方机制，系统绘制）
- 「设置」→ 管道 `OpenSettings` → daemon 弹 egui 设置页；托盘/自绘菜单窗口已删除

## M6 守护进程规划（`docs/plan/22-m6-daemon.md`）

- 独立 exe：唯一持有用户库（共享段只读引用 + 命名管道写请求）、
  egui/eframe 设置页（主线程跑，winit 限制）、无托盘图标（纯后台）
- 会话进程降级路径：守护进程缺失 → 自读文件现状，绝不挂键

**明确不做**：Windows XP 兼容（Rust 1.85 最低 Win7；XP 主用 IMM32，TSF 残废版——Weasel 2019 年 2.0 起放弃 XP 同因）。
