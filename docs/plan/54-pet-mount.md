# 54 号 · 工具栏挂载任意桌宠（StrokeIt 式捕捉，点子/未立项）

> 状态：点子备忘——自研桌宠难有竞争力（43 号继续但降级），转向**挂载层**：
> 用 StrokeIt 式 Finder 捕捉机制，把 iuvim 工具栏贴到用户已装的任意桌宠窗口上。

## 1. 核心思路

桌宠本体千千万，不竞争；竞争"挂件位"。全世界的桌宠都不会显示 IME 状态、
没有工具入口——而四态信号流（Idle/Typing/React/StateFlash）只有输入法有。
让别人的宠物"会打字反应"，这是护城河。

## 2. 机制映射（StrokeIt Add Application 同款）

- **捕捉**：拖准星 → `SetCapture` + `WindowFromPoint` →
  `GetClassNameW` / `GetWindowTextW` / `QueryFullProcessImageNameW` 存为"目标档案"
- **匹配**：启动时 `EnumWindows` 按档案找回目标（支持通配）
- **挂靠**：绝不用 `SetParent`（layered/topmost/输入路由全是坑）。
  自己的 topmost ULW 窗口 + `SetWinEventHook(EVENT_OBJECT_LOCATIONCHANGE)`
  追踪目标位置，锚点贴附布局。

## 3. 现状底子（为什么便宜）

ULW + per-pixel alpha + NOACTIVATE + 像素级点击穿透（`toolbar/mod.rs:429`）、
拖拽/prefs 持久化/全屏避让——直接复用。最贵的那部分已经写好了。

改造大头：**解耦**——宠物区现长在工具栏 HWND 内（栖木式，`iuv-ui/toolbar.rs:208-309`，
PET_OVERHANG=136），需让工具栏成为独立宿主、宠物区降级为可选。

## 4. 风险

1. 不是所有桌宠能挂（透明全屏/游戏内/网页套壳）→ 挂不上自动回退独立工具栏
2. 目标关闭/最小化 → 事件丢失需自动解挂
3. 管理员权限桌宠触发 UIPI；全屏避让（复用 `fullscreen.rs`）
4. 桌宠用户嫌工具栏碍事 → 可做"轻贴"模式（只贴状态灯）

## 5. 落地顺序

1. Finder Tool + 目标档案（小，独立模块）
2. 工具栏解耦 + WinEventHook 贴附（中）
3. 鲁棒性：解挂/重挂/多显示器 DPI/权限
4. 远期：IME 四态反向输出为"桌宠状态 API"

## 6. 待决策

1. 挂靠姿态：全工具栏 / 状态灯轻贴 / 可配置
2. 与 42 号皮肤 mod、43 号全身形象的优先级关系（建议本号先于 43）
