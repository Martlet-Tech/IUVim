# 54 号 · 工具栏挂载任意桌宠（StrokeIt 式捕捉，点子/未立项）

> 状态：点子备忘——自研桌宠难有竞争力（43 号继续但降级），转向**挂载层**：
> 用 StrokeIt 式 Finder 捕捉机制，把 iuvim 工具栏贴到用户已装的任意桌宠窗口上。
> **进度（2026-10-13）**：第一步「清除自有桌宠 + 工具栏解耦」已落地（见台账）；
> 四宠测试床已就位、真机档案已实测——见 **§7**（含对 §2 的两处修正）。

## 1. 核心思路

桌宠本体千千万，不竞争；竞争"挂件位"。全世界的桌宠都不会显示 IME 状态、
没有工具入口——而四态信号流（Idle/Typing/React/StateFlash）只有输入法有。
让别人的宠物"会打字反应"，这是护城河。

## 2. 机制映射（StrokeIt Add Application 同款）

- **捕捉**：**双路径**（§7-1 实测逼出来的）——
  ① 准星：`SetCapture` + `WindowFromPoint` → `GetClassNameW` / `GetWindowTextW` /
  `QueryFullProcessImageNameW` 存为"目标档案"；
  ② 枚举兜底：`EnumWindows` + 自算 z 序最上层（`WindowFromPoint` 会**跳过
  `WS_EX_TRANSPARENT`**，①抓不到透明覆盖层；②同时是同构多窗口唯一能选的方式）。
- **匹配**：启动时 `EnumWindows` 按档案找回目标（支持通配）
- **挂靠**：绝不用 `SetParent`（layered/topmost/输入路由全是坑）。
  自己的 topmost ULW 窗口 + `SetWinEventHook` 追踪目标位置，锚点贴附布局。
  **事件集须含 `EVENT_OBJECT_SHOW` / `HIDE` / `DESTROY`**，不能只有
  `LOCATIONCHANGE`——宠物可见性是动态的，否则宠物藏起来时工具栏留在原地飘着。

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

## 7. 实测档案与关键认知（2026-10-13）

测试床：`D:\Projects\input-method\pets\`（每宠一目录：`源码` / `安装包` / `安装目标`；
BongoCat、Shimeji、MateEngine、Lively Wallpaper 四个已装可跑）。以下窗口数据均为真机实测。

| 宠物 | 技术栈 | 真身窗口 | exStyle | 可挂性 |
|---|---|---|---|---|
| BongoCat | Tauri / WebView2 | `BongoCatProductOverlayWindow` 350×203 | `TOPMOST\|TOOLWINDOW\|LAYERED\|NOACTIVATE` | 可挂（类名唯一，最易） |
| Shimeji | Java / SWT | `SunAwtWindow` **×8**（4 对同位置、尺寸差 4px） | `TOPMOST\|NOACTIVATE` | 可挂（同构多实例，最难） |
| MateEngine | Unity | `UnityWndClass` 1532×900 ≈ 全屏 | `TOPMOST\|`**`TRANSPARENT`**`\|TOOLWINDOW\|LAYERED` | **不可挂** → 回退 |
| Lively Wallpaper | WinUI3 | 壁纸层，无独立宠物窗口（未实测，预期如此） | — | **不可挂** → 回退 |

**关键认知**（均已回头修正 §2）：

1. **准星路径抓不到透明覆盖层**。`WindowFromPoint` 跳过 `WS_EX_TRANSPARENT`——
   截图工具 PixPin 实测：BongoCat / Shimeji 能选中，MateEngine 选不中。同一条 API
   语义，本方案的准星路径会一模一样地失败。故 §2 捕捉改**双路径**。
2. **"不可挂载"要在建档案时就判定**，别等贴附完才发现锚点是整块屏幕：
   宽高 ≥ 主屏工作区 **且** `LAYERED|TRANSPARENT` → 当场提示"将回退独立工具栏"。
3. **同构多窗口无法靠类名 + 进程识别**。Shimeji 8 个窗口同进程、同类名、同 exStyle、
   **全无标题**；而 AWT 内部窗口（`PopupMessageWindow` / `TrayMessageWindow`）
   **反而有标题**——按标题过滤会恰好全反。档案指纹必须含 `尺寸 + 同构序号`。
4. **一只宠物可能占多个窗口**（Shimeji 成对出现在完全相同的位置）。贴附要同步跟随
   一组 hwnd，不能只认一个。
5. **`WS_EX_TRANSPARENT` 的目标自身点击穿透**——工具栏贴上去后点击会落到桌面，
   §4 第 4 条的"轻贴"模式要一并考虑交互。

**目标档案字段**（据此定）：
`{ 进程路径, 类名, 窗口尺寸, 同构序号, exStyle 掩码, 是否全屏覆盖 }`
