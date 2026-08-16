# 19 · 任务书 M4：跨平台渲染候选窗（去 Tauri 决议）

> 状态：规划（2026-08-16）。前置阅读：`00-overview.md`、`01-contract.md`、`14-mod-iuv-tsf-candwin.md`（现状 GDI 实现）、`30-conventions.md`。
> **M4 重定义**（用户决策）：不再做 Tauri helper；候选窗渲染层整体替换为**跨平台纯 Rust 绘图栈**，
> Windows 呈现用 **D2D + DirectComposition**（真透明圆角/阴影）。设置/词库管理 UI 与进程分离
> 归 M6 守护进程（见 `22-m6-daemon.md`）。

## 1. 目标（验收一句话）

候选窗从 GDI 自绘换成 **iuv-ui（tiny-skia + cosmic-text）绘图 + D2D/DComp 呈现**：
真透明圆角矩形 + 阴影、浅色/深色主题（config 可配）、不抢焦点/无闪烁/DPI/工作区内收等
既有行为语义不变。GDI 绘图 API（TextOut/FillRect/CreateFont 等）在 iuv-tsf 中全部移除，
仅窗口管理（HWND/消息循环/鼠标）保留。

## 2. 背景与决策记录

- 原 M4 = Tauri helper（WebView 候选窗 + 设置面板）→ **已废弃**（2026-08-16 用户决策）。
  Tauri/WebView 太重，且候选窗每帧自绘场景用 WebView 是浪费。
- 候选窗是"每像素自绘"场景（无控件、全部自定义绘制），适合**底层绘图库**而非 GUI 框架。
- 真透明圆角需要 per-pixel alpha 合成：GDI blit 做不到（不透明）；D2D `HwndRenderTarget`
  也做不到（画在普通窗口表面，无 DWM alpha 合成）——**必须 D2D 1.1 DeviceContext +
  DirectComposition surface**（微软拼音/新版 Weasel 同款路线），DWM 合成 per-pixel 透明。
- 文本 ClearType → 灰度 AA（cosmic-text 默认）：14pt 下略淡，手测验收可接受。
- 绘图逻辑在 iuv-ui（跨平台一份），macOS/Linux 候选窗将来只写各自窗口层 + 呈现层。

## 3. 架构

```
crates/iuv-ui（新增，跨平台纯 Rust，无 C 依赖）
├── theme.rs   Theme 结构体：bg/fg/hl_bg/hl_fg/page/border/shadow 色 + corner_radius/shadow_size
│               + 布局常量（PAD/ROW_GAP/CAND_GAP 自 gdi.rs 迁入）；light/dark 两套内置
├── layout.rs  layout/hit_test/position 纯函数从 gdi.rs 迁入（测试随迁，零改动断言）
├── text.rs    fontdb 系统字体发现 + cosmic-text 布局/整形/回退；
│               回退链 [Microsoft YaHei UI, Segoe UI, PingFang SC, Noto Sans CJK SC]
└── render.rs  render(snap, theme, scale) -> Surface { w, h, pixels: premultiplied RGBA }
               绘制：圆角矩形底（含 alpha）+ 阴影（模糊）→ 行高亮 → 文本 → 页码小字

platforms/windows/iuv-tsf/src/ui/candwin.rs（gdi.rs 改造/改名）
├── 保留：窗口类注册 / CreateWindowExW(WS_EX_TOPMOST|TOOLWINDOW|NOACTIVATE) / wnd_proc
│         鼠标交互（悬停/点击/命中）/ show/update/move_to/hide/set_suppressed
│         定位纯函数 / DPI（LOGPIXELSY 路径）
├── 删：CreateFontIndirectW/TextOutW/FillRect/FrameRect/GetTextExtentPoint32W/内存 DC 双缓冲
└── 换：D3D11 设备（WARP 软件回退）→ D2D1.1 DeviceContext → IDCompositionSurface
         WM_PAINT：iuv-ui render → surface BeginDraw → CreateBitmap + DrawBitmap(1:1) → EndDraw → Commit
         WM_NCHITTEST：按圆角几何判断，圆角外返回 HTTRANSPARENT（点击穿透到下层）
```

## 4. 任务清单

| # | 任务 | 产出/要点 |
|---|---|---|
| 1 | 建 iuv-ui crate 骨架 | Cargo.toml（tiny-skia 0.12、cosmic-text 0.19，锁版本）、lib.rs 模块声明；白名单/属主矩阵同步（01-contract §2.2/§6） |
| 2 | theme.rs + layout.rs | Theme 结构体 + light/dark；layout/hit_test/position 自 gdi.rs 迁入，测试全量随迁 |
| 3 | text.rs | fontdb `load_system_fonts` + cosmic-text FontSystem/Buffer；measure（供 layout）与 draw 共用同一布局 |
| 4 | render.rs | 圆角矩形 + 阴影 + 行高亮 + 页码 → premultiplied RGBA；像素级单测（背景色/高亮/圆角外 alpha=0） |
| 5 | iuv-tsf candwin.rs 重写 | 窗口层保留；呈现换 D2D/DComp；`CandidateUi` trait 签名零改动；DComp 初始化失败 → 静默降级隐藏（绝不 panic） |
| 6 | config theme 字段 | iuv-core Config + `theme: ThemeChoice`（light/dark，默认 light）+ 测试；text_service 装配时传入候选窗 |
| 7 | candwin_demo 更新 | 走 iuv-ui 渲染路径；演示深色主题切换 |
| 8 | 文档同步 | 00-overview（架构图/已知简化/索引）、01-contract（白名单/属主矩阵/M4 描述）、13/14 任务书槽位、AGENTS.md |
| 9 | 手测回归 | 见 DoD |

## 5. 已知风险与取舍

- ClearType → 灰度 AA：14pt 略淡（已接受，手测验收可读性）
- DComp 依赖 DWM：游戏路径候选窗已被抑制（IMM 检测）不显示，无冲突；
  独占全屏游戏下 DWM 行为不影响主路径
- D3D11 无 GPU 进程：WARP 软件回退；初始化失败静默降级（维持"绝不 panic"哲学）
- 32 位进程（WoW 等）：D2D/DComp 系统组件全架构支持，无额外风险
- DLL 体积 +1~2MB（fat LTO 下可接受）
- fontdb 首扫 C:\Windows\Fonts 元数据：候选窗首显可能有一次几十 ms 延迟，
  手测确认；不可接受则懒加载/缓存
- 阴影 = tiny-skia 软件模糊，每键重绘小窗口（~300×200）性能无虞

## 6. 槽位

- `CandidateUi` trait 不变：M5 托盘菜单（`21-m5-tray-menu.md`）与 M6 守护进程
  （`22-m6-daemon.md`）都只消费渲染栈，不动本接缝
- iuv-ui 的 render 抽象（Surface 缓冲 + Theme）即 M6 守护进程设置页/托盘菜单复用的基础；
  egui/eframe 届时另行引入，与 tiny-skia 栈并存（菜单/候选窗自绘、设置页用控件库）
- macOS/Linux：iuv-ui 直接复用，平台层只写窗口 + 呈现（CALayer/X11）
- 主题槽位兑现：Theme 结构体 + config 切换，M6 设置页可编辑后写 config

## 7. DoD

```
cargo check --workspace            # 无 warning
cargo test --workspace             # 全绿（layout/hit_test/position 随迁 + render 像素级新增）
cargo build -p iuv-tsf --release   # x64；x86 视需要
cargo run -p iuv-tsf --example candwin_demo   # 人眼：真透明圆角/阴影/深色主题/不抢焦点/无闪烁
dev-deploy 后手测：打字/翻页/点击/悬停/深色主题/多显示器 DPI/游戏抑制路径 全回归
```
