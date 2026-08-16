# 14 · 任务书 E：iuv-tsf 候选窗（GDI 自绘）

> 属主文件：`platforms/windows/iuv-tsf/src/ui/gdi.rs`、`platforms/windows/iuv-tsf/examples/candwin_demo.rs`
> 前置阅读：`00-overview.md`、`01-contract.md`（§5 接缝——`ui/mod.rs` 已冻结并完整实现）、`30-conventions.md`
> **禁止**修改 `ui/mod.rs` 与 iuv-tsf 其他模块。与 Agent D 并行开发，对接面只有 `CandidateUi` trait。

## 1. 目标

实现 `GdiCandidateWindow`：无边框、置顶、**不抢焦点**的候选窗，GDI 双缓冲绘制。
附 `candwin_demo` 示例程序，不注册输入法即可肉眼验收。

## 2. 实现要点（ui/gdi.rs）

```rust
pub struct GdiCandidateWindow { /* HWND, HFONT, 尺寸缓存, 当前 snapshot */ }
impl GdiCandidateWindow { pub fn new() -> Self }
impl super::CandidateUi for GdiCandidateWindow { /* show/update/move_to/hide/is_visible */ }
```

- 窗口类：进程内注册一次（`RegisterClassExW`，类名 `IuvCandidateWindow`）；
  `CreateWindowExW(WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE, WS_POPUP …)`；
  显示用 `ShowWindow(SW_SHOWNA)`，**任何时候不调 SetForegroundWindow/SetFocus**
- 绘制：`WM_PAINT` → 内存 DC 双缓冲 → `BitBlt`。内容：
  - 第 1 行：`reading`（如 `ni'hao`），次要颜色
  - 其后每行：`序号.候选词`（序号 1..=page_size），`selected` 行高亮底色
  - 底部右对齐小字 `page/total_pages`（>1 页时）
- 尺寸：`GetTextExtentPoint32W` 逐行测量取最大宽 + padding；高 = 行高×行数；每次 update 重算并 `SetWindowPos(SWP_NOACTIVATE)`
- 字体：`Microsoft YaHei UI`，按 `GetDpiForWindow`（失败退回 HDC `LOGPIXELSY`）缩放，字号 14pt 等比
- 定位：`show/move_to` 把窗口放在 caret 下方；超出工作区（`SystemParametersInfoW(SPI_GETWORKAREA)`）则右/下边界内收，必要时翻到 caret 上方
- 颜色：硬编码一套浅色主题常量（背景白、文字黑、高亮 #0078D7 系），集中文件顶部——M4 起迁 iuv-ui `Theme`（`19-m4-cross-render.md`）
- 生命周期：`new()` 不建窗；首次 `show` 懒建（窗口必须建在调用线程——TSF 回调线程有消息循环，成立）；
  `Drop` 时 `DestroyWindow`
- 所有 unsafe 块写 `// SAFETY:` 注释；**绝不 panic**（DLL 里 panic 会拖垮宿主进程）：绘制失败静默隐藏

## 3. 演示程序（examples/candwin_demo.rs）

```
cargo run -p iuv-tsf --example candwin_demo
```
- 建窗，循环播放 3~4 页假数据（reading = `ni'hao`，候选 = 你好/泥嚎/…），每 1.5s 切一页并移动位置
- 屏幕显示提示文字"按 Esc 退出"；用消息循环 + 短超时（`MsgWaitForMultipleObjects` 或 `PeekMessage`）实现动画
- 验收人眼要点：窗口不抢焦点（打开记事本保持光标闪烁）、绘制无闪烁、高亮正确、翻页尺寸自适应

## 4. 测试与 DoD

绘制代码不写单测；抽一个纯函数进 ui/gdi.rs 并自测：
`fn layout(snap: &UiSnapshot, measurer: &dyn Fn(&str)->(i32,i32)) -> (i32,i32,Vec<Rect>)`（行矩形计算），
examples/tests 用假 measurer 断言宽高与行数。
DoD：
```
cargo check -p iuv-tsf            # 无 warning（与 D 并行时以 workspace check 为准）
cargo test -p iuv-tsf             # layout 用例绿
cargo run -p iuv-tsf --example candwin_demo   # 人眼验收（W2 主智能体执行）
```

## 5. 槽位

- 主题常量集中 → M4 起迁移 iuv-ui `Theme` 结构体（浅色/深色可配，见 `19-m4-cross-render.md`）
- `CandidateUi` 不变；M4 渲染层替换（iuv-ui + D2D/DComp 呈现）与 `GdiCandidateWindow`
  并存过渡（M4 落地下线 gdi.rs），配置选择
- **输入点远跳跟随已落地**（2026-08-11~13 迭代完成）：初版简单清除未完成输入（cancel）→ 完整版
  仅隐藏候选窗保留 composition（d418e20）→ 判定基准改增量位移（08bd5f8，修正常打字误藏候选窗）。
  现状：远跳时 `ui.hide`（保留 composition 与 Session），下一键 set_text 后自然走 show 分支
  重新定位；点击/换行/拖窗跳变触发，连续打字（每键 ~15px）永不触发。

## 6. 子智能体启动提示词

```
你负责实现 iuv 输入法 MVP 的 GDI 候选窗（iuv-tsf 的 ui/gdi.rs）与演示 example。
先读 D:\Projects\vaim\docs\plan\00-overview.md、01-contract.md、30-conventions.md，
再读任务书 D:\Projects\vaim\docs\plan\14-mod-iuv-tsf-candwin.md 并严格执行。
只能创建/修改 platforms/windows/iuv-tsf/src/ui/gdi.rs 与 platforms/windows/iuv-tsf/examples/candwin_demo.rs；
对接面只有 ui/mod.rs 里已冻结的 CandidateUi trait/UiSnapshot/CaretRect，禁止修改其他文件。
要点：不抢焦点（NOACTIVATE/SW_SHOWNA）、双缓冲无闪烁、DPI 缩放、工作区内收、绝不 panic。
完成后满足 DoD：cargo check -p iuv-tsf 无 warning、cargo test -p iuv-tsf 绿、example 可运行。
最终回复：改动文件清单 + 测试输出摘要 + 已知限制。
```
