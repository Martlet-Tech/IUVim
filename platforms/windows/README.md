# platforms/windows — Windows 平台层

## 目录

| 路径 | 说明 | 状态 |
|---|---|---|
| `iuv-tsf/` | TSF 输入法管线（COM/TSF + GDI 候选窗 + 语言栏）+ 注册 | 已实施（M1~M2） |
| `README.md` | 本文档：门面现状 + M4 helper 规划 | — |

## 门面（候选窗）现状

- `iuv-tsf/src/ui/`：`CandidateUi` trait（冻结）+ `GdiCandidateWindow`（GDI 自绘，进程内）
- 能力：竖/横排、页码、悬停高亮、点击选词、翻页环绕、DPI 缩放、工作区内收
- 局限：进程内绘制（每应用一份）、无圆角/透明/皮肤

## M4 helper 规划（候选窗独立进程）

**决策记录（2026-08-14 讨论定稿）**：
- **门面与系统适配层分离**：`iuv-helper` 独立进程（全局单实例）持有渲染后端，TSF DLL 经命名管道发 `UiSnapshot`（文本/高亮/页码/光标坐标）
- **渲染后端能力探测**（不硬编码版本分支）：`D2D1CreateFactory` 成功 → D2D；失败（无 Platform Update/RDP/无 GPU）→ GDI 兜底（现有 1152 行 gdi.rs 平移）
- **跨平台分层不变**：引擎线（iuv-core/iuv-data）纯 Rust 跨平台；Windows 只写适配层 + 门面
- 生命周期：懒启动 + 崩溃自动重启；helper 未就绪期间 DLL 降级（不显示候选窗，打字直通）
- `CandidateUi` trait 不变 → COM 层零改动（契约 §5 已预留 `RemoteCandidateWindow` 槽位）

**明确不做**：Windows XP 兼容（Rust 1.85 最低 Win7；XP 主用 IMM32，TSF 残废版——Weasel 2019 年 2.0 起放弃 XP 同因）。
