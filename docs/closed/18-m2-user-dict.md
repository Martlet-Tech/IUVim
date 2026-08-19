# 18 · M2 设计：主动调权（Shift+←/→ 权重交换）+ 用户词库叠加

> 状态：**已结案**（2026-08-14 手测通过、已并入 main；一期主动调权+用户库、二期自造词+隐藏）。
> 键位演进：初稿 Alt+←/→
> → 实测发现 **Alt 组合是 `WM_SYSKEYDOWN`，不经过 TSF 键 sink（机制死路）**，改回
> Shift+←/→（`WM_KEYDOWN` 必经 TSF；方向键无大小写语义，Shift 的大小写歧义只存在于
> 字母键）——见文末附录「TSF 键 sink 红线」。
> 背景：M1 里程碑结案后评估滞回稳定排序（原 M2 核心卖点）——滞回只防短期抖动，
> 治不了长期漂移（高频词终将越阈值换位，用户肌肉记忆崩溃）；且自动换位与
> "用户习惯的位置"本质矛盾。**决策：排序决定权交还用户——主动调权取代自动换位。**

## 1. 交互语义

- **Shift+←**：当前页内高亮候选与其**左侧**候选交换权重（词往前提一格）
- **Shift+→**：与**右侧**候选交换（往后降一格）
- 连续按 = 逐步上移/下移，**高亮跟随被调词**（可一路提到 1 号位）
- **立即生效**：交换后当前会话候选窗立即重排（所见即所得）；不关闭 session，
  松手 Shift 后仍可继续 ←/→ 导航与数字/空格上屏
- **边界**：1 号位按 Shift+← / 末位按 Shift+→ → 忽略（消费但无操作）
- 例：`haoshi` 候选「耗时 好事 好使」，← 高亮「好使」→ Shift+← 两次 →「好使 耗时 好事」

### 键位

写死 `Shift+←/→`（与页内导航 ←/→ 紧邻，无需 config 配置；M2 不做键位自定义）。
**选 Shift 而非 Alt/Ctrl**：
- Alt 组合 = `WM_SYSKEYDOWN`（系统键），**TSF 收不到**（机制死路，见附录）——实测
  按 Alt 时 notepad 菜单下划线出现、交换不生效，日志无按键
- Ctrl 组合收得到但冲突大：应用通用「词跳转」（Word/浏览器 Ctrl+←/→）+ 违背
  「Ctrl 一律放行」红线
- Shift 组合收得到（与 ShiftChar 同机制）、方向键上无大小写语义、会话内 ←/→
  本就归输入法导航（冲突面最小）
- 注意：Shift+←/→ 在编辑器中是「扩展选中」，但该场景发生在打字会话内，方向键
  已被输入法占用，语义域一致

现状：`session_bridge.rs map_key` 的 `VK_LEFT/RIGHT` 原不检查 with_shift
（Shift+← 当前被当普通 Left）——改造点明确（M2 已实现）。

## 2. 持久化：绝对值覆盖（无魔法数字）

**不存 delta**。用户反复调整几轮后 delta 是"历史运算残留"，不可读不可预测；
绝对值覆盖下用户库每个条目都是「这个字就是这权重」的直接陈述，反复调整 =
覆盖旧值，永远收敛。

- 交换 = 双方互写对方**合成权重**（合成 = 基本库 weight 被用户覆盖后的当前值）
- 用户库存 `(code, word, adjusted_weight)` 三元组
- 词库重编译后覆盖值依然生效（尊重用户意志；条目对应词条不存在则自然成为孤儿，无害忽略）

## 3. 用户库文件（`iuv.user.imedic`）

**简单线性格式 `IUVUSR02`**（实现定稿：小文件不 mmap 零拷贝、覆盖表内存 BTreeMap——
§9 未决决策第 1 条落实；未采用 IMEDIC02 同构段表，那套段表/索引/二分复杂度对
几千条的覆盖表无收益）。**二期升级**（2026-08-14）：新增屏蔽段（Shift+Delete 隐藏
基础库词条），magic 分派兼容读 `IUVUSR01` 旧文件：

```
[0..8]   magic = b"IUVUSR02"（01 = 旧格式仅覆盖表，读兼容）
[8..12]  u32 覆盖条数
每条:    u8 code_len | code | u16 word_len | word | u32 adjusted
         u32 屏蔽条数
每条:    u8 code_len | code | u16 word_len | word（无权重）
```

- 覆盖表内存态：`BTreeMap<code, Vec<(word, adjusted)>>`（不可变共享 + 写时复制替换）；
  屏蔽表 `BTreeSet<(code, word)>`
- 文件小（几千~几万条）：mmap + 校验 <1ms
- 缺失/损坏：视为空用户库（Err 仅记日志），不影响基本库
- 写侧：构建新文件（写时复制）→ 同目录临时文件 + sync → 先删后 rename（原子替换，
  不引入命名 mutex：ReplaceFile 语义保证读侧不见半截文件，并发写 = 后写者赢）
- **基本库永不写**，mmap 页缓存共享不受影响

> 自造词表（段3）等未来扩展：升 `IUVUSR03` 追加段，读侧按 magic 分派。

## 3.5 自造词（逐字选择记录，二期）

**触发**：会话**全消费 commit** 且满足全部条件：
1. **逐字选择**：picked 栈非空且全部单字（含最后 commit 的候选）
2. 整串 ≥2 字（多音节）
3. 场景 0：`exact(full_code)`（叠加视图）含整串 → **跳过**（幂等：重复自造被拦截，权重不漂移）

**权重**（用 `config.page_size`，非 magic）：
- **a**（exact 空）：常量 **8000**
- **b1**（1 ≤ n < page_size）：`cand[n-1].weight − 1`（saturating）→ 词位第 n+1（首页内）
- **b2**（n ≥ page_size）：`avg(cand[ps-2], cand[ps-1])`（u64 防溢出）→ 词位首页最后

`full_code` = picked 各 code_key + 末词 code_key 以 `'` 连接（`xi'an` 类自然拼出）。
写入段1（覆盖表——自造词与覆盖统一为 (code, word, adj)，来源不区分）。

**显示**：`Dict::merged` 追加**用户库独有条目**（词不在基本库组 → 随查询结果显示）；
viterbi 整句路径同样吃到（自造词可被组句，微软同款）。用户嫌权重不够 → Shift+←/→ 手动调。

## 3.6 隐藏（Shift+Delete，二期）

**键位**：`VK_DELETE + Shift` → `Key::HideCandidate`（会话内消费；裸 Delete 放行给应用）。
**语义**（用户决策 3）：先尝试**删除用户库条目**（自造词/覆盖 = 撤销自造），
没有则**屏蔽基础库词条**（段2 屏蔽表）。
**整句拦截**：词条级屏蔽由 `Dict::merged` 过滤；**viterbi 整句同样拦截**（组合被
屏蔽后不再被组出——否则隐藏"手癣"后整句「手癣」仍会出现，隐藏失效）。
**立即生效**：剔除 + 重排 + 高亮落在原位置附近；持久化同覆盖表写盘机制。

## 4. 叠加机制（查询时合并，文件永不合并）

`exact("haoshi")` 流程：

```
基本库 exact(code) ──┐
                     ├→ 逐条应用用户覆盖（同 code+word 命中 → weight 替换为 adjusted）
用户库覆盖表 exact ──┘→ 屏蔽表过滤（命中 (code,word) 剔除）← M2 交互未决，结构已备
                     → 按合成 weight 稳定排序（同值保持基本库原序）
                     → 返回
```

- **引擎侧调用链不变**：merge 下沉 iuv-data（`Dict` 持有可选的 `UserDict` 引用），
  `exact` / `exact_single` / `prefix` / `initial_top` 四个查询方法内部合并
- 每次候选查询多一次用户库二分 + 小归并：微秒级
- 基本库物理不动：mmap 只读共享、全系统一份页缓存

## 5. 写入链路（Shift+← 按下时）

```
Shift+← 按下
  → Session 定位 all[idx] 与相邻候选（交换对 (code, A, B)）
  → 引擎内存态：A.adj = B.合成权重，B.adj = A.合成权重
  → recompute() 立即重排候选窗（selected 跟随被调词）
  → 立即写盘（写时复制 + 先删后 rename 原子替换，<2ms；Shift+← 低频操作，无需会话级 flush）
```

- **跨进程同步**：其他进程**新会话创建时**检查用户库 mtime → 变了重新 mmap（微秒级）
- 本进程无需重载（内存态已最新，文件只是镜像）

## 6. 改动清单（实现时）

| 文件 | 改动 |
|---|---|
| `iuv-data/src/userdict.rs` **新增** | UserDict：mmap 加载（复用 MappedFile + 段定位）、覆盖表查询、屏蔽过滤、写时复制写盘（mutex + ReplaceFileW） |
| `iuv-data/src/dict.rs` | `Dict` 持有 `Option<UserDict>`（或外部组合视图）；四个查询方法内 merge；`with_user()` 装配 |
| `iuv-data/src/lib.rs` | 导出 UserDict、userdict 路径常量 |
| `iuv-core/src/key.rs` | `Key` 新增 `SwapLeft` / `SwapRight`（非 config 可序列化键，同 ShiftChar 先例） |
| `iuv-core/src/session.rs` | `on_key` 新臂：定位交换对 → engine.swap_weights(code, a, b) → recompute（selected 跟随） |
| `iuv-core/src/engine.rs` | `swap_weights`（内存态覆盖更新）、持有 UserDict 的装配、会话创建时 mtime 检查重载 |
| `iuv-tsf/src/session_bridge.rs` | `map_key`：`VK_LEFT if with_shift => SwapLeft`、`VK_RIGHT if with_shift => SwapRight`；Ctrl/Alt 组合保持一律放行（新增单测：Shift+方向键/CapsLock 不影响/组合受控） |
| `docs/plan/01-contract.md` | §3 用户库格式、§4.1 Key 枚举、§4.2 交互同步 |
| `docs/plan/15-input-matching.md` | 若涉及候选行为差异同步 |

## 7. 测试与验收

- iuv-data：用户库读写往返、覆盖 merge（同字覆盖/新字追加/稳定排序）、屏蔽过滤、坏文件→空库、mtime 重载
- iuv-core：SwapLeft/Right 边界（1 号位忽略）、连续上移、selected 跟随、立即重排不关会话、
  交换后上屏正确、与悬空续接/翻页交互无冲突
- iuv-tsf：map_key Shift+方向键单测（含 CapsLock 不干扰、Ctrl/Alt 组合仍放行回归）
- 手测：notepad `haoshi` 好使 上移两步 → 立即重排 → 空格上屏；新开进程验证持久化生效；
  其他进程打字验证 mtime 重载生效
- `cargo test --workspace` 全绿 + 热部署（`scripts/dev-deploy.ps1`）

## 8. 未来功能边界（本设计不做，已预留）

- 自动学习换位：**不做**（用户决策）；学习仅可能影响新词/无候选词（列入未来）
- 自造词表独立段（IUVUSR03）：当前自造词与覆盖统一存段1，未来需区分来源/管理时再升格式
- 钉选快捷键：**不做**（2026-08-14 用户决策）——Shift+←/→ 手动排序 + 自造/隐藏已满足，显式"锁死"交互取消
- 屏蔽词批量管理（M6 设置页）：段2 结构已备，交互另行设计
- 键位自定义、跨设备同步：M6 守护进程设置页范畴（`22-m6-daemon.md`）

## 9. 未决决策（实现时再定）

- [x] 覆盖表查询：**内存 BTreeMap 已定**（文件小，构建 ~1ms；不用同构 IMEDIC02 二分）——2026-08-14 落实，格式同步简化（§3）
- [x] 屏蔽词交互形式（右键菜单 vs 快捷键 vs M6 设置页）——**Shift+Delete 快捷键已定**（2026-08-14 二期落地）
- [x] 用户库写盘失败的重试策略——**已定**：失败静默，内存态生效，下次调整重试（手测通过）
- [x] Shift+←/→ 与编辑器「扩展选中」冲突已定：会话内方向键本就归输入法导航，
      语义域一致，接受（初稿 Alt 方案因 TSF 机制限制废弃，见附录）

## 附录：TSF 键 sink 红线（快捷键设计必读）

Windows 按键消息分两类，**决定输入法能收到哪些组合键**：

| 消息 | 产生条件 | 是否进 TSF `ITfKeyEventSink` |
|---|---|---|
| `WM_KEYDOWN` | 普通键、**Shift/Ctrl 组合** | ✅ 到 `OnKeyDown`（map_key 全部输入都来自这里） |
| `WM_SYSKEYDOWN` | **Alt 组合**（Alt+任何键）、裸 Alt | ❌ 系统级消息，不路由给 TSF |

- **红线：Alt 组合永远进不了输入法**（CJK IME 快捷键惯例用 Ctrl/Shift 正源于此）；
  裸 Alt 会触发应用菜单栏助记符下划线（系统行为，输入法无法干预）
- 输入法可消费的修饰键：Shift（大小写语义只作用于字母键，方向键/功能键无歧义）、
  Ctrl（冲突面大：应用通用编辑快捷键，且「Ctrl 一律放行」是契约红线，需逐案例外化）
- M3+ 设计任何新快捷键（候选翻页/清屏/符号等）时，先过此表选键
