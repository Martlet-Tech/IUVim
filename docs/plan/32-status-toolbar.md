# 32 · 浮动工具栏（全局唯一看板，daemon 持有）

> 状态：**规格定稿**（2026-08-20，§7 决策点已全部拍板，待实现）。前置阅读：`01-contract.md`、
> `02-conventions.md`、`28-initial-state-settings.md`（docs/closed）、`31-script-traditional.md`（已结案）、
> `22-m6-daemon.md`（docs/closed）。
> 背景：28 曾决策「不做浮动状态栏，统一设置-常用页 = 新实例初始态」；2026-08-20 用户决策**反转**：
> 做浮动工具栏，且架构 = **daemon 持有全局唯一工具栏（看板）**，TSF 各实例经 IPC 上报/接收四态。

## 1. 目标（验收一句话）

主流输入法同款浮动工具栏：`logo | 中英 | 全半角 | 中英文标点 | 简繁 | 齿轮设置`。
daemon 持有**全局唯一**工具栏窗口 + 全局显示/隐藏偏好 + 全局位置记忆；每个 TSF 实例
（= 一个窗口/线程的 TextService）持有自己的运行时四态（`mode/width/script/punct`），
启动时 = 设置页 `initial_state` 默认值（#5），工具栏只渲染**当前焦点实例**的四态（#4）。

## 2. 行为规格（用户原话整理 + 确认语义）

### 2.1 结构

```
输入法logo | 中英 | 全半角 | 中英文标点 | 简繁 | 齿轮设置logo
```

### 2.2 显示/隐藏（全局偏好）

- 入口：现有语言栏「中/英」按钮右键菜单**新增「显示/隐藏工具栏」**（与现有 设置/关于 并列）。
- 语义：iuv 被选中时始终保持「显示或隐藏」按偏好。偏好 = **全局持久**（非 per 窗口）。
- 切换至别的输入法（Win+Space / Ctrl+Space 切 IME）→ 隐藏；切回 iuv → 按偏好重新显示
  （偏好=显示则恢复）。
- **区分「被选中」与「中文模式」**：Ctrl+Space 把 iuv 切成英文（OPENCLOSE=0）时 iuv 仍是
  **被选中**输入法，工具栏保持显示（中英钮变「英」）；只有切到别的输入法才隐藏。

### 2.3 位置（全局记忆）

- 全局唯一位置；用户拖拽后记忆，隐藏再显示还原上次位置；切换不同 app 位置不变
  （跨进程共享 = daemon 唯一写者持久化，见 §6.3）。
- 首次无记忆位置 → **主屏右下角**（2026-08-20 用户拍板）。

### 2.4 四态 per-实例（运行时值，非 config）

- 中英/全半角/标点/简繁四态，**每个 TSF 实例持有自己的值**（#4）。
  例：开两个记事本窗口，窗口 A 点过简繁→繁体、窗口 B 未点→简体；两窗口间切换时，
  工具栏简繁按钮随焦点窗口的设置而变化。同理适用于不同 app 窗口 = **每 TSF 会话实例**。
- 同一实例 Alt+Tab 往返后运行时值**保留**（不重置为 config 默认）。
- 中英 = 现有 OPENCLOSE compartment（per-thread，天然 per 实例）真相源；
  全半角/标点/简繁 = 新增 per-实例 `RuntimeState`（§5.1）。

### 2.5 设置页语义（默认值）

- 设置里 中英/全半角/中英文标点/简繁 = **每当新建一个 TSF 会话（新开窗口/线程）时的默认值**。
- 现有实例不随设置改动（config 热载不改动已运行实例的运行时值）。

## 3. 架构

```
┌───────────────────────────── daemon（唯一持有） ─────────────────────────────┐
│  工具栏窗口（全局唯一，看板） 实例表 {pid:tid → {state, active}}  focused      │
│  显示/隐藏偏好 + 位置（持久化） 前台看板判定（GetForegroundWindow 轮询）       │
└──────┬───────────────────────────────────────────────┬──────────────────────┘
       │ TSF→daemon：Register/StateSync/Active/Unregister/ToggleToolbar（单向管道）
       │ daemon→TSF：Cmd::SetState + StateResult（反向通道，见 §4.2）
       ▼
┌───────────────────────────── TSF 每实例（per 窗口/线程） ─────────────────────┐
│  TextService.runtime: Arc<RuntimeState{mode,width,script,punct}>             │
│  （启动 = config.initial_state；运行时操作才改）                                │
│  控制管道 accept 线程 + 隐藏消息窗（跨线程分发到 TSF 线程，STA）                │
└──────────────────────────────────────────────────────────────────────────────┘
```

## 4. IPC 协议（iuv-data ipc.rs 扩展）

### 4.1 TSF→daemon（走现有单向命名管道 `\\.\pipe\iuv-userdict`，新增 Request 变体）

| 变体 | 载荷 | 触发 |
|---|---|---|
| `Register` | instance_id(pid:tid), state | Activate 时注册 + 上报初始四态 |
| `StateSync` | instance_id, state | 状态变化上报（OPENCLOSE OnChange / SetState 成功后） |
| `Active` | instance_id, active | Activate/Deactivate 通知（daemon 判「iuv 被选中」） |
| `ToggleToolbar` | — | 语言栏右键菜单「显示/隐藏工具栏」 |
| `Unregister` | instance_id | 实例 Drop 注销 |

### 4.2 daemon→TSF（新增反向通道，定稿 = 按需连接）

TSF 每实例一个 accept 线程 + 服务管道 `\\.\pipe\iuv-ctl-<pid>-<tid>`。daemon 点按钮时
对 focused 实例连入 → 发一帧 `Cmd::SetState{field,value}` → TSF 应用 → 回
`StateResult{ok,new_state}` → 断开。贴合现有「一请求一连接」风格（2026-08-20 用户拍板）。

**选型分析（为何不用持久双向管道）**：本通道只在**用户点击工具栏按钮**这种低频人工动作时
使用，两端空闲时都阻塞等待、零 CPU；按需连接单次往返 ~1-3ms（连接+写读+断开，仅点击时），
持久 ~0.1-0.5ms——差距远低于人类感知，且**不触碰打字热路径**（状态上报仍走 §4.1 单向管道）。
真正决定感知延迟的是「控制线程→TSF 线程」跨线程分发（§4.3），两案相同。按需连接每次都是
新连接，天然自愈（daemon 重启/实例死亡 → 连接失败干净退出），无需持久方案的断线检测 +
重连 + 心跳 + 并发协调复杂度。

### 4.3 跨线程分发（技术核心）

控制管道线程收到 Cmd → 必须转到 TSF 线程（STA）应用（OPENCLOSE 写 / 候选窗 / 会话刷新）：
隐藏消息窗（`HWND_MESSAGE`）+ `PostMessage` → 该线程 wndproc（随应用消息泵执行）应用 →
信号控制线程 → 控制线程写响应帧。

### 4.4 daemon 重启恢复

TSF 复用 `DaemonClient.poll` 的在线翻转检测（离线→在线）→ **重新 Register**，
避免 daemon 重启后实例表清空、看板失联。

## 5. iuv-core / iuv-tsf 现状改动（关键约束）

### 5.1 四态读点从 config 改 per-实例 RuntimeState

现状读点（都读 `engine.config().initial_state`，进程级全局 → 两个记事本共享，违反 #4）：

- `crates/iuv-core/src/session.rs:242` `to_output` → `fullwidth_text(text, width)`
- `crates/iuv-core/src/session.rs:249` `convert_script` 判定 `script`
- `crates/iuv-core/src/session.rs:260,376` `effect` 显示边界简繁转换
- `platforms/windows/iuv-tsf/src/com/text_service.rs:428` `chinese_punct_pending` 读 `punct`
- `platforms/windows/iuv-tsf/src/com/text_service.rs:460-461` `fullwidth_pending_compute` 读 `width/punct`

改动：

- iuv-core 新增 `RuntimeState { mode, width, script, punct }`；`Session` 构造接收
  `Arc<RuntimeState>`（**live 读**，非快照——点简繁当前候选/预编辑立即重渲 = 用户已确认）。
- `with_parts` 测试调用点（`engine.rs:103` 等）补默认值。
- TSF TextService 新增 `runtime: Arc<RuntimeState>`，初始化自 `config.initial_state`。

### 5.2 Activate 不再每次强制写 config 默认

`text_service.rs:746` 当前 Activate 时若 OPENCLOSE ≠ 默认即改写。若 Activate 在
「切走输入法再切回」时重触发，会把用户在该窗口改过的中英重置回默认——违反 #4。
改为：**仅 compartment 未设置（VT_EMPTY，全新线程）时写默认**；运行时值随实例存活。
默认值语义（#5）= 实例创建时（`TextService::new` / 首次 Activate）初始化一次。

### 5.3 其他 TSF 侧接线

- 隐藏消息窗 + 控制管道 accept 线程（懒建，Activate 起，Deactivate 停）。
- `OnChange`（OPENCLOSE）→ `StateSync`（上报 mode）。
- passthrough_apps 命中进程**不注册**（iuv 完全透明 → 无工具栏）。

## 6. daemon 侧

### 6.1 新增状态（DaemonState）

- 实例表 `{pid:tid → {state, active}}`、`focused`、`toolbar_visible(pref)`、`toolbar_pos`。

### 6.2 前台看板判定

daemon 定时（~250ms）`GetForegroundWindow` + `GetWindowThreadProcessId`：前台 pid:tid
命中「active 实例」→ focused = 该实例、渲染其四态；否则隐藏。轮询兜底天然覆盖：
切 app、切输入法、实例死亡、失焦时序竞态。TSF `Active` 通知用于即时性，轮询保证正确性。
**设置窗聚焦时工具栏隐藏**（前台 = daemon 自身，非实例）——可接受，spec 注明。

### 6.3 持久化

- 显示/隐藏偏好 + 位置 = 全局，daemon 唯一写者。
- 定稿：独立 **`toolbar.json`**（`%LOCALAPPDATA%\iuv\toolbar.json`，2026-08-20 用户拍板）。
  独立文件不触发 config_epoch 热载噪声（config.json 每次写都会 bump epoch 广播给会话进程）。
  写盘复用现有「临时文件 + rename 原子替换」模式，失败不阻断（内存态已生效）。

### 6.4 渲染技术（定稿 = B：iuv-ui + ULW 自绘）

daemon 新增 `iuv-ui` 依赖（非新 crate，白名单无碍）；**ULW 窗口代码从 iuv-tsf 抽到共享
Windows crate**（新 crate，按契约 §2 白名单需批准）供 daemon/TSF 复用；工具栏由 daemon
一条**独立 win32 消息泵线程**承载（ULW 可在任意线程建窗，不碰 winit 主线程约束，设置页
egui 与主线程轮询循环不动）。

**选型依据（以 33-skin.md 为锚，2026-08-20 用户拍板）**：
1. 皮肤框架（IUVSKIN01）渲染层落在 iuv-ui，工具栏同栈则皮肤能力**免费继承**（同一
   `Theme` + NinePatch 通道覆盖背景/悬停/按下态），无需在 egui 另写一套皮肤渲染；
2. 视觉统一：候选窗/菜单/工具栏同为 iuv-ui 浮层，主流输入法皮肤恰好覆盖状态条+候选窗；
3. 架构线清晰：egui = 设置页（配置窗口，主流不给设置窗换肤），iuv-ui = 所有运行时 IME
   界面；33 挂起条件正是「工具栏效果差需先改进」，工具栏放 iuv-ui 与候选窗同栈受益；
4. 实现成本可控：工具栏本质是横排 `MenuWindow`（iuv-ui 已有菜单渲染 + 命中/悬停先例），
   新增 `render_toolbar` + 交互为增量。

代价（已知悉）：ULW 抽取新 crate（白名单批准）；daemon 需消息泵线程 + 按钮命中/悬停/
拖拽/tooltip 自绘。

### 6.5 点击协议（严格请求/响应）

工具栏按钮点击 → 对 focused 实例发 `Cmd::SetState` → TSF 应用 → 回 `StateResult` →
daemon 按结果更新实例表 + 按钮图标（成功/失败分别呈现）。中英钮与语言栏/Ctrl+Space
共用 OPENCLOSE 真相源，三入口最终 OnChange → StateSync 双向一致。

### 6.6 工具栏交互

- 不抢焦点（NOACTIVATE / 点击穿透空白区之外）；点击不打断活动 composition。
- 拖拽：任意非按钮空白区拖动 → 更新位置 + 持久化。
- 悬停 tooltip（「全半角」「简体/繁体」等）。
- **无自身右键菜单**（2026-08-20 用户拍板）：显隐唯一入口 = 语言栏「中/英」右键菜单。

### 6.7 工具栏图标（2026-08-20 决策：不建工具，内嵌 + 运行时处理）

- 源图已入库：`assets/main.png`（输入法主 logo，202×198，2026-08-21 补）+ `assets/toolbar-icons/*.png`
  （gear + 4 组双态 = 9 张，~28-32px 近方形）。
- **tiny-skia 自带 PNG 解码（`Pixmap::decode_png`）+ `draw_pixmap` 缩放绘制** → 无需独立
  缩放/转换工具（曾考虑的 build 工具方案否决，2026-08-20；源图即最终素材，改图即生效）。
- 编译期：daemon `include_bytes!("../../assets/...")` 内嵌进 exe，零外部文件依赖。
- 运行时：`toolbar_icons.rs` 解码成 `Pixmap`（一次，失败降级 None 不 panic），
  `render_toolbar` 按目标尺寸/DPI 用 `draw_pixmap` 缩放绘制——归一化到按钮规格在渲染层
  完成（源图已近 32px，高 DPI 自动放大）。

## 7. 决策点（已全部拍板，2026-08-20）

1. **渲染技术** = **B：iuv-ui + ULW 自绘**（依据 33-skin.md，皮肤框架免费继承，见 §6.4）。
2. **反向通道形态** = **按需连接** per-instance 管道（选型分析见 §4.2）。
3. **持久化位置** = **独立 `toolbar.json`**（见 §6.3）。
4. **游戏类进程（candidate_owner_apps 如 WoW）内是否显示工具栏** = **暂不处理**，后期实测
   游戏再修（spec 不设开关、不判断）。
5. **工具栏自身右键菜单** = **不加**（显隐唯一入口 = 语言栏「中/英」右键菜单）。
6. **首次默认位置** = **主屏右下角**。

## 8. 影响面

| 模块 | 改动 |
|---|---|
| `crates/iuv-data/src/ipc.rs` | 新增 Request 变体（Register/StateSync/Active/Unregister/ToggleToolbar）+ 反向控制通道（Cmd::SetState/StateResult）+ 帧编解码测试 |
| `crates/iuv-data/src/lib.rs` | 导出新类型 |
| `crates/iuv-core/src/session.rs` | `to_output`/`convert_script`/`effect` 改读 `Arc<RuntimeState>`（live） |
| `crates/iuv-core/src/engine.rs` | `with_parts` 加参（测试调用点补默认）；`RuntimeState` 导出 |
| `platforms/windows/iuv-tsf/src/com/text_service.rs` | `runtime` 字段 + Activate 默认值只写一次 + 控制管道/消息窗接线 + OnChange→StateSync + passthrough 不注册 |
| `platforms/windows/iuv-tsf/src/session_bridge.rs` | `fullwidth_pending_compute`/`chinese_punct_pending` 收 runtime 参数（纯函数） |
| `platforms/windows/iuv-tsf/src/langbar.rs` | 右键菜单新增「显示/隐藏工具栏」 |
| `platforms/windows/iuv-tsf/src/daemon_client.rs` | Register/StateSync/Active 上报 + poll 在线翻转重注册 |
| `platforms/windows/iuv-daemon/src/state.rs` | 实例表/focused/pref/pos + 看板判定 + 点击协议 + toolbar.json 持久化 |
| `platforms/windows/iuv-daemon/src/main.rs` | 工具栏宿主：独立 win32 消息泵线程（ULW 窗口 + 前台看板轮询定时器） |
| `platforms/windows/iuv-daemon/Cargo.toml` | `iuv-ui` 依赖已存在（2026-08-21 核对），无需新增 |
| `platforms/windows/iuv-daemon/src/toolbar_icons.rs` | 图标内嵌 `include_bytes!`（`assets/main.png` + `assets/toolbar-icons/*.png`）+ 运行时解码成 `Pixmap`（失败降级 None，见 §6.7） |
| **新共享 Windows crate**（抽自 iuv-tsf） | `ulw.rs` ULW 窗口代码抽取（daemon/TSF 复用；契约 §2 白名单需批准） |
| `platforms/windows/iuv-daemon/src/settings.rs` | 无工具栏相关改造（设置页不动） |
| `crates/iuv-ui/src/render.rs` | 新增 `render_toolbar`（横排按钮条，`draw_pixmap` 缩放绘制图标，复用 Theme/MenuWindow 模式） |
| `scripts/`、`docs/plan/01-contract.md`、`AGENTS.md` | 同步 |

## 9. 测试要点

- iuv-core：Session 运行时态（默认、切换后 commit 全角/繁体、点简繁即时重渲、config 热载不影响实例）。
- iuv-data ipc：新变体编解码 roundtrip、控制管道 Cmd 往返、非法载荷拒绝。
- daemon：实例表增删、focused 判定纯函数（注入前台 pid:tid）、偏好/位置持久化 roundtrip、
  点击协议成功/失败分支、daemon 重启后 TSF 重注册。
- TSF：控制管道 accept→分发→响应、runtime 各读点切换、Activate 默认只写一次、
  passthrough 进程不注册。
- 手测（验收 #2~#5）：两个记事本窗口 per-window 四态独立；Alt+Tab 往返运行时值保留；
  切输入法隐藏/切回恢复；位置跨 app/跨进程记忆；新窗口用设置默认值；
  语言栏菜单显隐；passthrough 进程无工具栏。