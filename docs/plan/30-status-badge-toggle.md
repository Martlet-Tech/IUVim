# 调研：光标处「中/英」切换状态徽标（淡出）

> 状态：**调研完成，暂不实现**（2026-08-19）。目标特性：Ctrl+Space 切换语言时，
> 在当前光标位置下方短暂显示「中/英」状态 logo，持续几百毫秒后淡出消失
> （微软拼音行为）。本文记录机制调研结论与实现预案，落地前先读本文 §4。

## 1. 核心结论

**TSF 没有「声明状态徽标特性 + 把 logo 交给系统」这种接口。** 微软拼音与小狼毫
都是**输入法自己画一个顶层小窗**贴在光标下方，自己控制消失/淡出。TSF 只提供两块
积木：切换事件（compartment）+ 光标位置（edit session 内 GetTextExt）——本工程
两处都已具备。

## 2. 调研依据

### 2.1 为什么不是 TSF 系统接口

TSF 里现存两个「和状态显示沾边」的机制，都不是光标跟随徽标：

- **`ITfLangBarItem`**（语言栏/托盘图标）：`langbar.rs` 已在用（「中/英」按钮）。
  它显示在语言栏/任务栏，**不跟随光标**。
- **`ITfUIElement`**（UI 元素）：`ui_element.rs` 已实现。但 TSF 只定义了候选列表
  元素（`ITfCandidateListUIElement`），**没有「状态徽标元素」**。不能声明一个
  「status element」再喂一张图给系统。

### 2.2 小狼毫证据

- Issue #298 / #520：切换「中/英」时光标处「悬浮提示」是小狼毫**自己画的**
  （黑色 `[中]` / 红色 `[A]` 小框，光标下方闪现约 2 秒）。
- 可配置项 `show_notifications` + `show_notifications_time`（默认 1200ms）：
  `weasel.yaml` 顶层选项——纯前端行为，无系统接口参与。
- 位置跟随光标，另有 `ascii_tip_follow_cursor` 可改为跟随鼠标。
- 微软拼音同机制，额外做了 alpha 淡出动画。

## 3. 已有积木（本工程现状）

| 积木 | 现状 | 位置 |
|---|---|---|
| 切换事件 | `GUID_COMPARTMENT_KEYBOARD_OPENCLOSE` 的 `OnChange` 统一响应（Ctrl+Space / 语言栏点击都归一写该 compartment），`apply_openclose()` 内翻转 `english_mode` | `com/text_service.rs:327`、`:890` |
| 光标位置 | 编辑会话内 `ITfContext::GetSelection` + `ITfContextView::GetTextExt`，存 `Rc<Cell<CaretRect>>` | `composition.rs:170`、`:244` |
| ULW 呈现 | `UlwSurface` 共享模块（DIB + per-pixel alpha 合成），candwin/menu 复用 | `ui/ulw.rs` |
| 自绘栈 | iuv-ui tiny-skia + cosmic-text（`Theme`/`TextRenderer`/`Surface`） | `crates/iuv-ui` |

### 3.1 关键技术点：无 composition 时怎么拿光标

Ctrl+Space 切换时通常**没有活动会话**，`self.caret` 是上次打字的旧值。预案：
向焦点文档请求**只读同步编辑会话**：

```
ITfThreadMgr::GetFocus → ITfDocumentMgr::GetBase → ITfContext
  → RequestEditSession(TF_ES_SYNC | TF_ES_READ)
    → DoEditSession: GetSelection(TF_DEFAULT_SELECTION) → ITfContextView::GetTextExt → CaretRect
```

与 `composition.rs` 现有路径完全同构，只是 `TF_ES_READWRITE` → `TF_ES_READ`
（无 composition 也能请求只读会话；本 TIP 为活动 TIP，同步会话可请求）。
失败兜底：沿用 `self.caret` 旧值（定位逻辑对旧光标无致命影响，仅位置滞后）。

## 4. 实现预案（暂缓，落地时参考）

### 4.1 iuv-ui：`render_status()`

`render.rs` 新增：画一个圆角小徽标（约 32×32 @96dpi），内容「中」/「英」，
复用 `Theme`（bg/fg/corner_radius/border/shadow）+ `TextRenderer`，新增
`alpha: u8` 参数做淡出（重渲时乘 premultiplied alpha）。纯函数，返回 `Surface`。

### 4.2 iuv-tsf：`ui/statuswin.rs` — `StatusWindow`

- 复用 `ulw::UlwSurface` + candwin 同款窗口类（`WS_EX_TOPMOST|TOOLWINDOW|NOACTIVATE|LAYERED`，
  `SW_SHOWNA` 不抢焦点）。
- `show(text, caret)`：`position_in_area` 定位光标下方 → `render_status` → ULW 上屏
  → `SetTimer`（约 700ms，含 ~300ms 淡出；每 tick ~30ms 降 alpha 重渲）。
- `hide()`：KillTimer + 隐藏，幂等。Deactivate / 焦点切换 / 下次按键时清理。
- 徽标不参与命中测试（可全部 `HTTRANSPARENT`，点击穿透）。

### 4.3 TextService 接线

- 新字段 `status: Rc<RefCell<StatusWindow>>`。
- `apply_openclose()` 翻转模式后触发：英文→「英」，中文→「中」；光标取法见 §3.1。
- **Activate 初始同步不弹**（那是进焦点不是用户切换），只在 `OnChange` 用户切换路径弹。

### 4.4 配置（待用户定）

- 候选 A：config.json 加 `show_status_badge: bool`（默认 true）+ `status_badge_duration_ms`，
  随 theme 一起热载（`apply_config_hot_reload`）。
- 候选 B：固定实现，不暴露配置。

## 5. 待定决策（落地前问用户）

1. 徽标样式：微软纯字「中/英」/ 小狼毫方括号 `[中][英]` / 双态同显「中|英」高亮当前。
2. 是否要 config 开关 + 时长。
3. 触发范围：仅 Ctrl+Space，还是语言栏点击也弹。

## 6. 参考

- weasel issue #298（悬浮提示太小）、#520（`show_notifications` 隐藏）、#961
- weasel 源码：`WeaselTSF/WeaselTSF.h`（`_SetCompositionPosition`/`_UpdateCompositionWindow`）、
  `weasel.yaml` 文档（`show_notifications` / `ascii_tip_follow_cursor`）
- MSDN TSF：`ITfContext::RequestEditSession` / `ITfContextView::GetTextExt` /
  `GUID_COMPARTMENT_KEYBOARD_OPENCLOSE`
