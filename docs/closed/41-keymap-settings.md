# 41 · 设置界面快捷键：双备选键位 + 游戏式录入 + 全局热键独立层

> 状态：已实现并并入 main（2026-08-28 手测通过；原分支 feat/keymap-settings 已合）。
> 决策记录：本文件同时充当决策台账——管理员拍板的取舍记录在案，实施不得悄悄推翻。

## 1. 背景与动机

设置页「按键」标签目前是灰置占位（M7），键位自定义未开放。现状：

- 会话内快捷键（翻页/候选移动）已可配（`Config.keymap`，4 组 Vec<Key>），但设置 UI 缺失；
- M2 调权（Shift+←/→）与隐藏（Shift+Delete）是 `map_key` 里的**硬编码**，不可配；
- 四态切换（中英/全角/简繁/标点）与打开设置/工具栏显隐**没有快捷键**，只能点工具栏。

## 2. 核心架构：两条完全独立的链路

**关键认识（管理员提示，2026-08-27）**：全局功能与"解析用户码字的按键系统"完全独立。
daemon 本来就是普通 exe——工具栏点击走的就是「daemon 连 focused 实例 ctl 管道发 CtlCmd」，
四态翻转/打开设置/工具栏显隐全都有现成的 daemon 侧实现。全局热键只是把「鼠标点击工具栏」
换成「RegisterHotKey 收到 WM_HOTKEY」，跟 TSF 键 sink 彻底无关，Alt 完全能用。

| 层 | 触发机制 | 功能 | Alt 可用？ |
|---|---|---|---|
| **会话内快捷键** | TSF 键 sink（`ITfKeyEventSink`，走 `route_key`） | 翻上一页/翻下一页/候选左移/候选右移/调权左/调权右/隐藏候选 | ❌ 机制死路（WM_SYSKEYDOWN 不进 sink） |
| **全局热键** | daemon `RegisterHotKey` → 工具栏窗口收 `WM_HOTKEY`（普通软件做法，启动器同款） | 中英切换/全角半角/简体繁体/中文标点/打开设置/工具栏显隐 | ✅ 随便绑 |

- 全局热键触发后**复用工具栏 on_click 的既有分派**：读 `Shared.focused` → `ctl_pipe_name(pid,tid)`
  → `CtlClient` 发 `CtlCmd::SetMode/SetWidth/SetScript/SetPunct`（四态翻转零新增 IPC），
  设置/工具栏走 `PipeClient::Request::OpenSettings/ToggleToolbar`。
- 关键性质：`RegisterHotKey` 命中时系统吞掉该键、不再下发给前台应用/IME → **全局热键优先于
  会话**，无双处理、零性能成本（只在热键按下时唤醒，无逐键钩子，符合仓库"零定时器/事件驱动"纪律）。
- 会话快捷键本来就是读 `engine.config().keymap`（`key_routing.rs:91`），`set_config` 热载已通
  ——键位热载对会话层已天然满足，本次只是把 `map_key` 里硬编码的 Shift+←/→ / Shift+Delete
  改成查表。

## 3. 数据模型（iuv-core，`config/keymap.rs` 重写）

```rust
pub struct Keymap {
    // —— 会话内（TSF 键 sink，仅无修饰/Shift 组合）——
    pub page_prev: TwoSlot,      // 主/备 两槽
    pub page_next: TwoSlot,
    pub candidate_prev: TwoSlot,
    pub candidate_next: TwoSlot,
    pub swap_left: TwoSlot,      // 默认 Shift+←（原硬编码）
    pub swap_right: TwoSlot,     // 默认 Shift+→
    pub hide_candidate: TwoSlot, // 默认 Shift+Delete
    // —— 全局热键（daemon，≥1 修饰键）——
    pub toggle_mode: TwoSlot,
    pub toggle_width: TwoSlot,
    pub toggle_script: TwoSlot,
    pub toggle_punct: TwoSlot,
    pub open_settings: TwoSlot,
    pub toggle_toolbar: TwoSlot,
}
pub struct TwoSlot { pub primary: Option<Combo>, pub secondary: Option<Combo> }
pub struct Combo { pub ctrl: bool, pub alt: bool, pub shift: bool, pub win: bool, pub base: Key }
```

- 序列化 `"Shift+Left"` / `"Ctrl+Alt+I"` / `"PageDown"` / `","`（`Combo` 的 name/parse；
  `Key` 现有 name/from_name 保留作 base）。
- `TwoSlot::map(&Combo) -> Option<Key>` 查表归一化到既有 Key 变体
  （PageUp/PageDown/Left/Right/SwapLeft/SwapRight/HideCandidate）。
- **默认值**（保肌肉记忆）：翻页 主=PageUp 备=`,`；候选移动 主=← 备=空；调权/隐藏 = 原 Shift 组合。
  全局六项默认**空**（决策点 1，见 §8）。
- **迁移 shim**：旧 `"keymap": {"page_prev": ["PageUp",",","Up"],...}`（Vec<Key>）→ 新两槽，
  `io.rs` 仿 initial_state 迁移在 from_file deserialize 前转换，取前两键；避免旧配置整体回退
  默认丢失其他设置。

## 4. 运行时消费

**iuv-tsf**（`session_bridge.rs` / `key_routing.rs`）：
- `route_key` 首部查表：由 (vk,shift,ctrl,alt) 构造 `Combo` → `keymap` 命中会话动作 → 直接
  归一化（翻页/移动/调权/隐藏）；未命中才走现有 `map_key` 字母/数字/标点逻辑。
- **红线不变**：会话动作仍禁 Ctrl（冲突大，放行给应用）、Alt 天然收不到。
- 硬编码的 `VK_LEFT if with_shift => SwapLeft` 等删除，改由默认配置表达。

**iuv-daemon**（新 `hotkey.rs` + `toolbar/window.rs`）：
- `RegisterHotKey(bar_hwnd, id, mods, vk)` 注册全局六功能的主/备槽（id 编号分配），
  `WM_HOTKEY` 进 `bar_wnd_proc` → `on_hotkey(id)` 复用 `on_click` 的 focused→CtlClient 分派。
- 配置变化（settings apply → config_epoch）→ 新 `BarEvent::HotkeysChanged` 入 FIFO →
  工具条线程全量 Unregister+重注册。
- 未注册/注册失败（被占用）→ 记日志 + 设置页保存时提示。

## 5. 设置 UI：游戏式录入（`settings.rs` keymap_tab 重写）

- 两卡片：**「输入会话内」**（7 项）与**「全局快捷键」**（6 项，标注"任意软件生效"）。
  每行：功能名 `[主录入框] [备录入框] [恢复默认]`。
- **录入模式（游戏式）**：点击录入框 → 高亮 + 文案「请按下组合键…（Esc 取消，Backspace 清除）」
  → 主线程装 **`WH_KEYBOARD_LL`**（eframe 主线程泵消息，回调吞键 return 1 防漏进 egui）→
  捕获 (vk + Alt/Ctrl/Shift/Win 键态) → 显示 `"Alt+Shift+1"` 回填槽位 → 卸钩。
- **校验**（保存/回填时）：
  - 会话内：Alt 组合 → 红字警告「Alt 组合不会到达输入法会话，运行时无效」；Ctrl 组合 → 拒绝；
    纯字母无修饰 → 拒绝（会吃掉拼音输入）。
  - 全局：必须 ≥1 修饰键（否则全系统劫持字母/数字）；`Ctrl+Space` 特殊警告
    （系统「输入法/非输入法切换」占用）。
  - **跨功能冲突检测**：新绑定已存在于任一其他槽 → 拒绝并标红指向占用方；
    会话内/全局两表统一查。
- 确定/应用 → `DaemonConfig` 增 `keymap` 字段 → `save_config` 写入（补丁式保留未知字段逻辑不变）
  → bump epoch → 热键重注册。

## 6. 文件改动清单

| 文件 | 改动 |
|---|---|
| `crates/iuv-core/src/config/keymap.rs` | 重写：Combo/TwoSlot/新 Keymap + 查表 + 测试 |
| `crates/iuv-core/src/config/io.rs` | keymap 旧格式迁移 shim + 测试 |
| `crates/iuv-core/src/config/mod.rs` | 字段默认/序列化测试更新 |
| `platforms/windows/iuv-tsf/src/session_bridge.rs` | map_key 去硬编码 Shift 特例，会话动作改查 keymap |
| `platforms/windows/iuv-tsf/src/com/key_routing.rs` | route_key 首部 combo 查表 |
| `platforms/windows/iuv-tsf/src/com/daemon_host.rs` | 热载日志更新（键位热载已通） |
| `platforms/windows/iuv-daemon/src/hotkey.rs` | **新**：RegisterHotKey 管理 + WM_HOTKEY 分派 |
| `platforms/windows/iuv-daemon/src/toolbar/{mod,window}.rs` | HotkeysChanged 事件 + on_hotkey（复用 on_click 逻辑）+ 注册/注销接线 |
| `platforms/windows/iuv-daemon/src/settings.rs` | keymap_tab 重写（录入/校验/冲突） |
| `platforms/windows/iuv-daemon/src/config.rs` | DaemonConfig + keymap 字段读写 + 测试 |
| `platforms/windows/iuv-daemon/src/main.rs` | hotkey 装配接线 |

## 7. 测试

- iuv-core：Combo parse/name 往返、TwoSlot 查表、迁移 shim（旧数组→两槽）、红线校验纯函数。
- iuv-daemon：RegisterHotKey 参数换算（Combo→MOD_*/vk）、冲突检测、keymap 配置往返。
- 手测（dev-deploy）：录入回填、Esc/Backspace、冲突拦截、Alt 全局热键任意软件触发、
  四态翻转生效、会话内翻页/调权/隐藏改绑生效、热载即时。

## 8. 决策记录（管理员拍板，2026-08-27）

| # | 决策 | 结论 |
|---|---|---|
| 1 | 全局六项默认键位 | **全空**（不预占全局键，避免劫持其他软件；用户自己绑） |
| 2 | 会话内 Ctrl 组合 | **维持红线禁止**（放行给应用） |
| 3 | 录入允许 Alt 的警告策略 | **允许 + UI 标警告**（全局 Alt 本来就生效，无警告） |

## 9. 执行授权记录

- 提交策略：每完成一个子阶段且 cargo test 全绿即自动 commit（分支 feat/keymap-settings）。
- 验证边界：只用 cargo test --workspace 验证；dev-deploy 部署后交管理员手测。

## 10. 修复记录（2026-08-28 手测反馈，实测根因）

### 10.1 全局卡片不可见 → keymap_tab 包 ScrollArea

手测反馈「能看到会话内设置、看不到全局部分；Ctrl+- 缩放 UI 后才看到全局部分在下面」。
根因：keymap_tab 13 行内容超出设置窗固定 640×480 可视区，**缺 ScrollArea**（其他 tab 均有），
全局卡片被挤出。修复：keymap_tab 内容包 `ScrollArea::vertical`（max_height = 可用高度 − 12）。

### 10.2 点击录入框后无法录入 → repaint 回调唤醒帧循环

手测反馈「鼠标点击录入框后，没法录入新按键」。根因：钩子回调捕获到按键后只置
`AtomicBool` 标志，**未真正唤醒 eframe 帧循环**——eframe 默认 `ControlFlow::Wait`，
无事件不渲染新帧 → 挂在 `logic()` 上的 `poll_capture` 不被调度 → outcome 永不消费
→ UI 无反应（按键其实已捕获，只是画面不刷新）。
修复：`CaptureState` 增 `repaint: Mutex<Option<Box<dyn Fn()+Send+Sync>>>` 回调槽；
`begin(state, repaint)` 由设置页注入 `egui::Context::request_repaint` 封装；
`hook_proc` 收尾统一走 `finish()`：写 outcome + **调 repaint 回调** + 卸钩。
删除 `request_repaint: AtomicBool`。

### 10.3 纯字母无修饰被静默吞掉 → Rejected 提示

手测乱试时纯字母键无任何反馈（被钩子吞掉继续等）。新增 `CaptureOutcome::Rejected(String)`
——捕获到纯字母无修饰组合时立即结束录入并红字提示「会被拼音输入吞掉」。

### 10.4 关键日志（分析用）

- `[capture] WH_KEYBOARD_LL 钩子已安装/卸载`、`收到 Esc/Backspace`、`捕获组合键：X`、
  `纯字母无修饰被拒`、`vk=0x.. 无基础键映射`
- `[settings] 进入录入模式/录入成功/校验拒绝/录入被拒`、`全局热键首注册/注册：成功 N 失败 M`

### 10.5 WH_KEYBOARD_LL 回调不触发 → 弃用，改 egui 事件流（方案 A，2026-08-28）

手测复测反馈「简繁绑 Ctrl+Shift+F 不好使 / Esc 取消不好使 / Backspace 清除不好使」。
日志实锤：`[capture]` 钩子安装/卸载齐全，但录入期间（两次 17 秒等待）**零条「收到
Esc/Backspace/捕获组合键」**——WH_KEYBOARD_LL 回调**从未被触发**。

根因：低层键盘钩子回调依赖**安装线程的 Win32 消息泵**；daemon 设置窗跑在
eframe/winit 事件循环下（winit 自持消息处理），且录入期间焦点在 `工具条激活`/`失焦`
间频繁切换——钩子在该宿主环境下不可靠。此前 `fd26569` 修的「repaint 唤醒帧循环」
只解决"捕获到后不刷新"，根本问题是捕获根本不发生。

修复（方案 A，管理员拍板）：**完全弃用 WH_KEYBOARD_LL**，改用 egui 自身事件流：
`egui::Event::Key` 提供 `key`/`physical_key`（官方注释明说给 games/input-capture UIs）、
`modifiers`（alt/ctrl/shift）。设置窗有焦点时必然收到（用户录入时焦点必在设置窗），
天然支持 Alt/Ctrl/Shift，绕开消息泵依赖。

- `capture.rs` 重写为纯逻辑：`process_key_event(egui::Key, &Modifiers) -> Option<CaptureOutcome>`
  （Captured/Clear/Cancel/Rejected）；`egui_key_to_base` 映射；删全部钩子机制。
- `settings.rs`：`start_capture` 仅置位；`poll_capture` 遍历 `ctx.input().events` 消费；
  删 capture_state/repaint 接线；on_exit 复位。
- 测试：capture 8 项（Esc/Backspace/Shift 组合/Ctrl+Shift+F/Alt+1/纯字母拒绝/修饰键忽略/标点）。

### 10.6 清除 keymap 键位后仍生效 → map_key 去导航键硬编码（2026-08-28）

手测反馈「清除上翻页的 PageUp 后，PageUp 还能翻页」。config 已正确落盘
（`page_prev.primary = null`），但 TSF 侧有两条绕过 keymap 的硬编码路径：

1. `map_key`（session_bridge.rs）把 `VK_PRIOR → Key::PageUp`、`VK_NEXT → Key::PageDown`、
   方向键 → `Key::Up/Down/Left/Right` **无条件硬编码**；`route_key` 组合键查表 miss
   （PageUp 已清）→ 落到 map_key → 返回 `Key::PageUp` → `Session::on_key` 翻页。
2. `Session::on_key` 对 `Key::PageUp/PageDown` 无条件翻页、方向键无条件移动候选——
   本意是「keymap 命中后归一化再喂」，但 map_key 把物理键直通进了引擎。

修复：`map_key` **不再硬编码导航/翻页键**——这些物理键的会话内语义**完全由 keymap
决定**：route_key 命中 → 归一化（PageUp/Left…）喂 Session；未命中 → map_key 返回
None → Pass 放行给应用。候选移动默认 keymap 补 Up/Down 备槽（保肌肉记忆：
`candidate_prev = Left/Up`、`candidate_next = Right/Down`）。会话外行为不变（本就放行）。

- 改动：session_bridge.rs map_key 删导航键硬编码；iuv-core keymap 默认候选移动
  补 Up/Down 备槽；测试同步（map_key_arrows/paging/shift_arrows/control_keys 改断言
  None、defaults/all_combos 数量更新）。

### 10.7 设置窗打开时热键失效 + 录入态抢键 → 焦点语义 + CaptureMode（2026-08-28）

手测反馈「daemon 设置了快捷键，不会执行动作」。日志实锤两类：

1. **设置窗打开时热键失效**：切到设置窗 → TSF `OnKillThreadFocus` → daemon `FocusLost`
   → `focused = None` → `on_hotkey`「无 focused 实例，忽略」刷屏。但设置窗是 daemon
   自家配置 UI，用户打开设置窗 ≠ 离开 iuv 使用。修复：`FocusLost` 分支加守卫——
   `settings_open` 时保留 focused（不清空），全局热键继续作用于打开设置窗前焦点所在
   的应用（管理员决策：继续作用于原应用）。设置窗关闭后焦点回原应用 → FocusGained
   自然恢复。
2. **录入态抢键**：`RegisterHotKey` 系统级捕获——录入模式按已注册热键时，按键进
   `WM_HOTKEY` 不进 egui 流 → 录不进去。修复：`CaptureMode` 事件——进入录入 →
   工具栏线程 `unregister_all`；退出录入（成功/Esc/Backspace/关窗）→ 按当前配置
   `register_all`。接线：`run_settings(state, toolbar)` + SettingsApp 持 toolbar +
   `set_capture_mode`。




