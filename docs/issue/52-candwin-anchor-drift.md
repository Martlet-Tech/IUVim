# 52 号 · 候选窗漂移治本：锚点槽跨失焦作废 + 退化矩形拒收 + 服务端跳变检测 + 首拍跳过量取

> 状态：**第二轮修复，待真机验证**（分支 `fix/candwin-anchor-drift`，未提交）。
> 范围：`platforms/windows/iuv-tsf`（`composition.rs` / `com/key_routing.rs` / `com/text_service.rs` /
> `com/dispatch.rs` / `session_bridge.rs` / `build.rs`）+ `platforms/windows/iuv-server`
> （`candwin.rs` / `lib.rs`）。`iuv-core` / `iuv-data` / `iuv-proto` / `iuv-ui` **零改动**。
> 验证：`cargo fmt`/`clippy`/`test` 全绿。
>
> **第一轮修复（治本 A/B/C）部署后被证伪**：2026-10-09 16:10 真机复测瞬移依旧，
> 双端日志对齐后锁定真凶为 **§2.4 的首拍陈旧量取**，追加治本 D。A/B/C 保留
> （A 的失焦清零与 B 的哨兵是正确的卫生措施，C 的跳变检测继续兜底真几何跳变）。

## 1. 现象与用户报告

用户在 WorkBuddy（Electron 宿主）里打字时，候选栏偶发**高速漂移**——从某个旧位置沿斜线或横线
迅速滑到正确位置，速度快到"没法画准"。复现手法（用户原话）：

> 「我只要切换 workbuddy 的窗口最大化与否, 就能大概率复现漂移」

后续补充：X 方向也漂（不只是 Y）：

> 「x方向也漂呀, 因为太快了, 我没法画准, 我刚才打的从而 大概从红框跳到绿框」

## 2. 归因：三个叠加的缺陷

### 2.1 锚点槽跨失焦不清零（~~主因~~ **第一轮误判**，降级为卫生措施）

> **证伪说明（第二轮）**：本节原判"首键拿到的是旧槽残留值"。治本 A/B 部署后瞬移依旧，
> 新埋点证明 selection 现量每次都是当场新量且正确——08:14:57 那个 `x=1013 y=481`
> 其实来自 `set_text` 的**首拍陈旧量取**（§2.4 真凶），与槽残留无关。失焦清零保留，
> 降级为与首键哨兵配套的卫生措施（防"量取失败 + 槽残留"组合透传旧值）。

`TextService::caret` 是一个**跨会话、跨失焦持续存活**的状态槽（`Rc<Cell<CaretRect>>`）。
失焦（`OnKillThreadFocus`）时它**不被清理**，留着离开前的屏幕坐标。

WorkBuddy 失焦/回焦的实证（`%TEMP%\iuv-tsf.log`，2026-10-09）：

```
08:14:47.804  WorkBuddyAI  OnKillThreadFocus（切出 → 失焦上报）   ← 切去 notepad
08:14:55.539  WorkBuddyAI  OnSetThreadFocus（切入）               ← 隔 7.7s 回来
08:14:57.165  [key]   按键：c（远端会话外）                       ← 新会话首键
08:14:57.168  [caret] 锚点：x=1013 y=481 w=0 h=20                ← ★ 首拍陈旧量取（§2.4）
08:14:57.365  [key]   按键：e（远端会话内）
08:14:57.367  [caret] 锚点：x=800  y=681 w=0 h=21                ← ★ 真值，Δ=(-213, +200)
08:14:59.239  [commit] commit：测试
```

~~**判定残留而非现量的依据是 `h` 字段**~~（此推理已被 §2.4 推翻：`h=20` 与 `h=21`
的差别是"宿主布局未刷新 vs 已刷新"，不是"槽残留 vs 现量"——首键那行本来就出自
`set_text` 的现量，只是量到的是缓存旧布局）。

全量统计（WorkBuddy 4966 条 `[caret]`）印证异常 `h` 是系统性现象，而非偶发：

| 键位置 | `h=21`（稳定行盒） | `h=20`（字形盒/残留） | `h=1`（垃圾值） |
|---|---|---|---|
| 首键（会话外） | 977 | **20** | **2** |
| 后续键（会话内） | 3882 | 85 | 0 |

### 2.2 退化矩形未被拒收

`RepositionSession::DoEditSession` / `InsertionCaretSession::DoEditSession` 的可用性判据原先只有
**MSDN 明示的两条**：`clipped` 与 **全零**。任何"有坐标、但形状退化"的返回值都会穿过：

```
2026-10-08 15:25:23.269  [caret] 锚点：x=1227 y=219 w=0 h=1   ← h=1，距真值 (+425,-220)
```

`h=1` 显然不是有效文本框，但旧代码照收，候选窗就画到错误位置。

### 2.3 服务端平滑移动把一个"跳"变成了"漂"

iuv-server 侧的 `ServerCandwin::move_to` 只有一句裸 `SetWindowPos(..., SWP_NOSIZE)`，
**没有跳变检测**。锚点一次性位移 455px 时，窗口就会**平滑地滑过去**——这正是用户看到的"高速漂移"。

455px 的来源明确：`1920 − 1465`，即窗口**最大化前后宽度差**。这解释了为什么"切最大化"能稳定复现。

补充：旧版客户端自绘候选窗**本来有**同款 `JUMP_THRESHOLD = 150.0` 检测（超过阈值则整窗隐藏、
待下一键重现），但随 2026-10-02 品质审查 D3「本地候选窗整链清扫」一并退役；服务端版从零实现时
漏掉了这一层。

### 2.4 首拍陈旧量取（**真凶**，第一轮修复后由双端日志对齐锁定）

第一轮治本 A/B 部署后瞬移依旧。客户端日志（`[caret] 首键插入点（selection 现量）`）
证明 selection 现量**每次都是当场新量且基本正确**——治本 A 归因的"跨失焦缓存"不成立。
真正的机制是**两条量取路径在首键后 20ms 内互相打架**：

一次首键的三次量取（2026-10-09 16:10:33 / 16:10:40，WorkBuddy pid=4808 ↔ iuv-server pid=31660）：

| 时间 | ①selection 现量（首键） | ②set_text 量取（composition 起点） | ③follow 量取（几 ms 后） |
|---|---|---|---|
| 16:10:33.805（测试） | **(660,748)** ✓ | **(417,648)** ✗ Δ(-243,-100) | (660,748) ✓ |
| 16:10:40.013（x…） | (965,265) ✓ | **(660,748)** ✗（**上一个会话的输入点**） | (1069,265) ✓ |

服务端对应收到三次锚点、连跳三次（`[candwin]` 日志）：

```
16:10:40.016  show：锚点 (965,265)                       ← ① 正确
16:10:40.023  锚点远跳：(965,265) → (660,748) 距 571px    ← ② 错误
16:10:40.035  锚点远跳：(660,748) → (1069,265) 距 633px   ← ③ 纠正
```

**用户看到的"瞬移"就是这三帧：对 → 错 → 对，全程 ~20ms。** 全量统计：
`[candwin] 锚点远跳` 109 次，其中大量是 A→B / B→A 的**乒乓对**（间隔 6~8ms）；
`set_text 锚点变化` 的 Δ（-243,-100 / -305,+483）与紧随的 `follow 锚点平移` Δ
严格互为相反数——同一次位移的两端。

**②为什么错**：`set_text` 在 `StartComposition` 后立刻调 `comp.GetRange()` + `GetTextExt`，
Chromium/Electron 宿主此刻尚未为新 composition 刷新布局，返回的是**内部缓存的旧布局**
（旧窗口几何坐标或上一会话输入点；y 差恒定 100 = 最大化/还原的工具栏高度差）。
几毫秒后布局跟随（`follow_layout`）再量就已刷新。

这也修正了 47 号的一个归因：`h=20` vs `h=21` 并非"字形盒 vs 行盒"，
而是"宿主布局未刷新 vs 已刷新"——同一现象的两个观测面。

## 3. 一个"沉默的放大器"：Electron 宿主不支持布局 sink

```
08:14:42.642  [follow] ITfSource QI 失败（来源=OnSetFocus）：
              Error { code: HRESULT(0x80004002), message: "不支持此接口" }（宿主不支持布局 sink，无跟随）
```

WorkBuddy（Chromium/Electron）**不支持 `ITfTextLayoutSink`**，`follow_layout` / `OnLayoutChange`
在该宿主上**永不触发**（对照：notepad 支持，08:14:51 能看到完整的 `[follow] 锚点平移`）。

后果：窗口几何变化（切换最大化）时，**没有任何机制去校正锚点**，只能等下一次按键重新量取。
这本身不是 bug（是宿主能力差异），但它让 2.1/2.2 的错值**没有任何兜底纠正机会**，
一路直达服务端渲染。

## 4. 修复

### 治本 A：锚点槽跨失焦/停用一律作废（客户端）

`OnKillThreadFocus` 与 `deactivate` 均清零 `caret` + `caret_reported`：

```rust
self.caret.set(CaretRect::default());
self.caret_reported.set(CaretRect::default());
```

`caret_reported` 必须一并清——否则"残留旧值 == 已上报值"，差异上报机制会以为无需发送。

### 治本 B：首键只信本次现量，失败即发作废哨兵（客户端）

`key_routing.rs` 的 `StartSession` 分支由 `if let Some` 改为 `match`，两条路径都显式处理：

- `Some(c)`：现量成功 → 上报真值；
- `None`：现量失败 → 清零本地槽 **并且仍然发一次全零 `CaretMoved`**。

第二条是关键——不能靠"本地也置零"来省这次发送。若客户端置零、服务端仍留着别的旧值，
双方会各自以为"没变"而双双跳过，陈旧坐标就此固化（这正是本 bug 的成因之一）。
显式发一个全零哨兵，服务端才知道要弃用缓存。

### 治本 B2：拒收退化矩形（客户端）

新增 `caret_rect_unusable(rc, clipped)` 统一判据，`RepositionSession` 与
`InsertionCaretSession` 共用：

```rust
if clipped || (rc.left == 0 && rc.top == 0 && rc.right == 0 && rc.bottom == 0) {
    return true;                    // MSDN：不可见
}
let w = rc.right - rc.left;
let h = rc.bottom - rc.top;
w < 0 || h <= CARET_MIN_HEIGHT      // CARET_MIN_HEIGHT = 2
```

阈值取 2 而非"正数"：正常行盒高 20~23px，任何 ≤2 都不可能是有效文本框。

### 治本 C：服务端跳变检测（把"漂"改回"弹"）

`ServerCandwin::move_to` 增加与旧客户端版同源的判定：

```rust
const JUMP_THRESHOLD: f64 = 150.0;   // 与退役客户端版同值

let jumped = self.last_caret
    .map(|prev| jump_distance(prev, caret) > JUMP_THRESHOLD)
    .unwrap_or(false);
```

命中即 `hide()` → 回填锚点 → `show_with_current_snapshot()`，用户看到一次瞬间重弹而非滑行。

阈值 150 的安全性：正常打字锚点恒定（Δ=0）；换行/滚动是小步（行高 ~20px）；
实测的两次漂移（455px 横跳、292px 斜跳）都远在阈值之上。

> 第一轮部署后的教训：C 单独存在时，客户端的"错→对"两连上报会被它忠实执行成
> 两次瞬跳（对→错→对三帧），瞬移依旧。**C 是兜底，不是根因修复**——必须配合治本 D。

### 治本 D：会话首拍跳过量取（真凶，第二轮追加）

`Composition` 增加 `first_stroke: Cell<bool>`（每会话新建，随对象销毁自然复位）。
`set_text` 在写会话完成后、量取前检查：首拍直接返回 `Ok(None)` 并记日志——
调用方 `apply_effect` 的 `Ok(None)` 语义本就是"沿用旧锚点"，于是首拍锚点 =
首键 selection 现量（①，已证明当场正确）。第二键起恢复正常量取（日志实证
第二键起量值稳定正确）。

- notepad 等原生宿主零回归：其首键 selection 量取与首拍 composition 量取
  逐字段一致（08:14:51 实测同为 `w=23 h=21`）。
- `commit_punct`（标点直上屏）同样受益：临时 composition 本就不需要锚点，
  首拍跳过还省一次只读 edit session。

## 5. 验证

- `cargo fmt --check -p iuv-tsf -p iuv-server`：通过。
- `cargo clippy -p iuv-tsf -p iuv-server --all-targets`：零告警。
- `cargo test -p iuv-tsf -p iuv-server --lib`：**85 通过**（38 + 47），其中新增 6 条：
  - `composition::tests::caret_rect_accepts_normal_line_box`
  - `composition::tests::caret_rect_rejects_clipped_and_all_zero`
  - `composition::tests::caret_rect_rejects_degenerate_values`
  - `candwin::tests::jump_distance_is_euclidean`
  - `candwin::tests::jump_threshold_separates_typing_from_geometry_change`
  - （原有 `in_rounded_rect_matches_client_version` 保留）

**待真机验证**（第二轮，治本 D 生效性——需重建 + 热部署 + **重启所有宿主进程**）：

1. `scripts\build.ps1` → `scripts\dev-deploy.ps1`；
2. **完全退出并重启 WorkBuddy 与 iuv-server**（第一轮教训：DLL 映射随进程存活，
   只部署不重启 = 新代码不生效；16:10 复测时 pid=4808 已是新 DLL，勿混淆）；
3. 复现手法：反复切 WorkBuddy 最大化 → 打两个字（如 `ce` / `测试`）；
4. 预期日志（客户端 `iuv-tsf.log`）：
   - 首键：`[caret] 首键插入点（selection 现量）` → 紧跟
     `[caret] 首拍跳过量取（宿主布局未刷新…），沿用 selection 现量`；
   - **不再出现**首键后的 `[caret] 锚点变化：…Δ=(±几百,…)`（旧日志里 ② 的错误值）；
   - 第二键起 `[caret] 锚点（composition 起点…）` 恢复且与首键坐标同 y。
5. 预期日志（服务端 `iuv-server.log`）：
   - 打字路径的 `[candwin] 锚点远跳` 应**基本消失**（只保留真窗口几何跳变的偶发）；
   - 不再有 A→B→A 的乒乓对。
6. 目视验收：打首字时候选栏**原地出现**，无瞬移、无乒乓闪烁。

## 6. 附带修正

- **文档漂移**：47 号 §5 承诺的 `[follow] 锚点平移` 日志在代码中并不存在（全仓只在
  `iuv-ui/src/pet.rs` 有同名但无关的注释）。本次在 `follow_layout` 中补上真实日志，
  并注明 Electron 宿主"一条都不出"是预期现象。
- **`build.rs` 逃生舱**：新增 `IUV_SKIP_WINRES` 环境变量（默认不生效）。沙箱/CI 里
  `reg.exe` 被安全策略拦截时 `winres` 查不到 Windows SDK 路径 → build.rs panic →
  整个 `cargo check -p iuv-tsf` 无法做类型检查。置位即跳过资源编译，**产物不可装载**，
  仅供类型检查。
- 移除 4 处临时诊断埋点（`[anchor]`），替换为 3 类常驻日志（`[caret]` 首键现量 /
  `[follow]` 锚点平移 / `[candwin]` show·平移·远跳·hide）。

## 7. 未决 / 后续

- ~~首键锚点为何现量失败~~（第一轮的猜想，**已被日志证伪**）：selection 现量 0 次失败
  （哨兵未触发过），它量的是对的；错的是首拍 composition 量取——由治本 D 规避。
- **首拍 selection 现量偶尔不是"点"而是大矩形**：16:10:40 实测 `w=254 h=21`（像选中了
  一段文本），与最终稳定锚点 x 差 104px，会触发一次 <150px 的平滑平移。影响远小于
  修复前的 263~633px 乒乓，暂不处理；若要进一步，需研究 Chromium 的 selection 语义。
- **`follow_layout` 在 Electron 上部分失效**：`ITfTextLayoutSink` QI 返回 0x80004002，
  但日志显示同进程内部分 context 能挂上（"此前 N 次 QI 失败已清零"）——行为不一致，
  窗口几何变化的主动校正时有时无。跳变检测可兜底大跳，小漂（<150px）仍可能一闪。
- **慢键**：`[perf] on_key 慢键` 频繁 60~190ms，属另一独立议题。
