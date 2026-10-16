# 57 号：光标处状态徽标——server 渲染重构（30 号路线废止重做）

状态：**施工完成，待真机验收**（2026-10-16；cargo test --workspace 全过、fmt/clippy 零告警）
前身：30 号（TSF 客户端自绘徽标）——已随分支退回至 f81e9b0 废止；本任务按
「徽标持有者 = server」重做，对齐 49 号薄客户端架构（TSF 只报信息）与小狼毫
同款交互（光标处独立方框徽标，弹出后自动淡出）。

## 1. 背景与动机

- 用户截图证实小狼毫有「切换瞬间在光标处弹状态徽标（中/英/A）再淡出」交互，
  独立于候选窗面板。
- 30 号把徽标做在 TSF 客户端（statuswin + 弹出判定 + Test/Down 时序特判），
  与「薄客户端」方向相悖；且实测暴露两类问题（首翻被 `mode_initialized`
  误判吞掉、位置用缓存槽落后真实光标 46~78px）。
- 架构判断：徽标的「何时弹、弹什么、弹在哪、何时隐」全部是**策略**，应归
  server（它已持有会话/态/锚点全量信息）；TSF 只该做只有它能做的**感知**
  （量光标、读按键/态变化）并如实上报。

## 2. 职责划分（重构核心）

| 职责 | 30 号（废止） | 57 号（本任务） |
|---|---|---|
| 徽标窗体/渲染/淡出 | TSF statuswin | **server** badge 模块（复用 candwin UI 线程模式） |
| 弹出时机判定 | TSF（apply_openclose/poll_caps 调 show） | **server**（ImeState 变化 diff） |
| 隐藏时机判定 | TSF（每键 Test 先隐 + 0x14 例外特判） | **server**（收到任意 Key/RawKey 即隐） |
| glyph 推导 | TSF | **server**（caps 优先 A，其次 mode） |
| 光标量取 | TSF | TSF（不变——量取原语只在宿主进程） |
| 按键/态上报 | 部分（Pass 键不上报） | TSF 全量上报（RawKey 补 Pass 键） |

## 3. 协议变更（破坏性，PROTO_MIN/MAX 3→4）

1. **`C2S::RawKey { vk: u16, mods: Mods }`**（追加枚举末尾，postcard 序号纪律）：
   到达 TSF 键盘 sink 但未产生 `C2S::Key` 的键（Pass 放行的修饰键/方向键/
   功能键/CapsLock 等）。会话键不重复发。
2. **`ImeState` 补 `caps: bool` 字段**：A 态徽标需要 caps 真值过线；struct
   字段追加同为破坏性变更（postcard 定长序列）。`[u8;4]` 管道编码随之扩为
   `[u8;5]`（仅 ime.rs 内部测试消费，无外部使用方——已核实）。
3. 版本纪律：同 2→3 先例，混合期旧端解不出新格式 → VersionMismatch 拒绝，
   dev-deploy 后 ctfmon 重启即恢复。

## 4. TSF 侧改动（变得更蠢）

### 4.1 RawKey 上报（key_routing.rs `test_key_down`）

```
顺序（wire 全序是正确性来源）：
  action = route_key(vk, false)
  if action == Pass → remote.raw_key(vk, mods)   ← 先发原始键
  poll_caps 最小版 → caps 变化才 sync_state        ← 后发态
```

- **顺序保证**：CapsLock 键 → RawKey 先到（server 隐旧徽标）→ ImeState 后到
  （server 弹新 A 徽标）。30 号的 0x14 特判在 server 决策下**天然成立**，无需
  客户端任何特判。
- RawKey 只在 Test 阶段发（Pass 键不会来 Down——TSF 既有语义）。
- **性能护栏**：`passthrough_apps`/`candidate_owner_apps` 命中进程跳过 RawKey
  （游戏逐键 IPC 不可接受；这些进程本就不渲染 UI）。RawKey 同步等 Ok 应答
  （同 CaretMoved 先例，server 秒回；「每个 C2S 必须有应答」S1 纪律）。

### 4.2 caps 同步（key_routing.rs，30 号 poll_caps 的最小版）

- `TextService` 增加 `caps_cache: Cell<bool>`；`test_key_down` 入口比对
  `capslock_on()`，变化才更新 runtime.caps → `sync_state()`（内部 last_state
  去重兜底）。**无徽标调用**——TSF 不知道徽标存在。

### 4.3 caret 新鲜度（text_service.rs `follow_layout` 扩展）

- 现状：布局跟随只在组词中响应（comp 槽守卫）；commit 后锚点停 组合起点。
- 扩展：comp 槽空时改做**插入点现量**（`query_insertion_caret` 现成）→
  变化才 `sync_caret`。commit 上屏 = 文本插入 = 布局事件 → 锚点自然新鲜化。
- **已知局限**（记录不修）：鼠标点击移动光标不触发布局事件，锚点仍陈旧到
  下一键。候选窗不受影响（组词期锚点本就恒定）；徽标在此窗口内可能偏移，
  下一次布局事件自愈。

### 4.4 不删除任何东西

基线 f81e9b0 上 TSF 无徽标代码（30 号已随退回消失）。本任务 TSF 侧纯增量：
RawKey + caps 同步 + follow_layout 扩展，约 60 行。

## 5. server 侧改动

### 5.1 badge 模块（新建 `platforms/windows/iuv-server/src/badge.rs`）

- 与 candwin 同款骨架：`BadgeHandle`（tx + WakeEvent）+ 每连接一个 UI 线程 +
  ULW 窗口（TOPMOST|TOOLWINDOW|NOACTIVATE|LAYERED|**TRANSPARENT**——纯展示
  整窗穿透，无鼠标交互）。
- 渲染：`iuv_ui::render_badge`（新增，单字形方形小框：正方底 + 细边框 + 居中
  字形，复用 `render_to_surface` 与 Theme，DPI 自算同 candwin）。
- 定位：caret 下沿 + 2px（CARET_GAP 同值）；越界内收 caret 所在显示器工作区。
- 淡出：`SetTimer` 1200ms 兜底自隐（决策层按键隐藏会提前 KillTimer）；
  `WM_TIMER` 在窗口过程内处理。

### 5.2 连接层决策（lib.rs `EngineSession`）

- 新字段：`badge: BadgeHandle`（随 candwin 同主题 spawn）、
  `badge_prev_state: Option<ImeState>`。
- **弹出**（`C2S::ImeState` 臂）：`badge_prev_state` 非 None 且 != 新态
  （首报 = 初始化同步不弹；客户端 last_state 去重保证到达即变化）→ 且
  `self.session.is_none()`（30 号决议承继：打字中弹窗很吵）→ 且
  `self.caret` 有锚点 → `BadgeCmd::Show`。glyph 推导：`caps→A`，
  `mode==English→英`，否则 `中`。
- **隐藏**：`C2S::Key` / `C2S::RawKey` 臂入口（idempotent）；`FocusChanged(false)`、
  `InstanceDeactivated`、`CandwinHide` 同隐。
- 工具栏/语言栏 SetMode 路径：客户端 apply 后 sync_state 回传变化值 → 走
  同一弹出路径，无需特判（echo 同值被 diff 天然滤掉）。

## 6. 已权衡的替代方案（否决理由存档）

- **`BadgePulse { rect, glyph }` 专用消息**：把弹出时机决策留在 TSF（何时
  现量、何时触发），违背「TSF 只报信息」；且位置仍需 TSF 决定。否决。
- **server 纯从缓存锚点弹（零协议改动）**：commit 后锚点滞后 2~3 字宽，
  徽标落点错。否决（caret 新鲜化 + 现有 CaretMoved 已覆盖）。
- **RawKey 全键上报含会话键**：重复流量；会话键已有 Key 消息可隐徽标。否决。

## 7. 风险与边界

- 混合期（新 server + 旧 DLL）：VersionMismatch 拒绝，客户端延迟重连 until
  进程重启——dev-deploy 后 ctfmon 重启立即收敛（既有纪律）。
- RawKey 同步等待：server 忙时 Pass 键延迟上限 = request 300ms 截止（同
  CaretMoved）；游戏进程已跳过。
- 徽标与候选窗同屏：徽标在 caret 下沿、候选窗也在 caret 下沿——重叠时徽标
  先弹（切换发生在打字前/后，候选窗此时不可见）；会话内不弹策略进一步保证
  不同屏。真机复测确认。
- 鼠标点击后徽标位置陈旧：见 §4.3 已知局限。

## 8. 验证清单

- [x] 新进程第一次 Ctrl+Space 弹「英」徽标（退回后重做路径）
- [x] Shift 切换弹中/英，CapsLock 弹 A；连按不残留（16:25:53 A↔英 连按 10 次
  每次先 hide 后 show，wire 全序生效；16:26:04 中↔英 Shift 连切同正确）
- [x] 任意按键（字母/方向键/F 键）徽标立即隐藏（Key/RawKey 两路）
- [x] commit 后立刻切换：徽标落点 = 最新光标（16:26:09 锚点 573→613 前进 40px，
  follow_layout 新鲜化生效）
- [x] 打字中 CapsLock/Shift：会话内不弹
- [x] 失焦/切程序：徽标消失（FocusChanged(false)/InstanceDeactivated 隐藏）
- [ ] 游戏进程（passthrough_apps）：无 RawKey 流量（日志验证）——待游戏实测
- [x] cargo test --workspace 全过；fmt/clippy 零告警
- [x] 热部署真机复测（2026-10-16 16:21 部署，16:25-16:26 双端日志对齐验证）

### 8.1 验收遗留（转入后续提交）

- **工具栏三态**：中英按钮应随 caps 显 A 态（30 号内容随退回丢失；`ImeState.caps`
  已过线，server 工具栏渲染补 caps 分支即可）——2026-10-16 真机验收发现。

## 9. 交付物

- crates/iuv-data/src/ime.rs（caps 字段 + [u8;5] 编码）
- crates/iuv-proto/src/{msg.rs,lib.rs}（RawKey + 版本 4）
- crates/iuv-ui/src/render.rs（render_badge）
- platforms/windows/iuv-server/src/{badge.rs（新）,lib.rs（决策接线）}
- platforms/windows/iuv-tsf/src/com/{key_routing.rs,text_service.rs,remote_host.rs}
- docs/status.md 台账条目
