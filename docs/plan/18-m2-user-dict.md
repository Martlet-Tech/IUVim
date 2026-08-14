# 18 · M2 设计：主动调权（Alt+←/→ 权重交换）+ 用户词库叠加

> 状态：**设计定稿，待实现**（2026-08-14）。分支 `feat/m2-user-dict`。
> 背景：M1 里程碑结案后评估滞回稳定排序（原 M2 核心卖点）——滞回只防短期抖动，
> 治不了长期漂移（高频词终将越阈值换位，用户肌肉记忆崩溃）；且自动换位与
> "用户习惯的位置"本质矛盾。**决策：排序决定权交还用户——主动调权取代自动换位。**

## 1. 交互语义

- **Alt+←**：当前页内高亮候选与其**左侧**候选交换权重（词往前提一格）
- **Alt+→**：与**右侧**候选交换（往后降一格）
- 连续按 = 逐步上移/下移，**高亮跟随被调词**（可一路提到 1 号位）
- **立即生效**：交换后当前会话候选窗立即重排（所见即所得）；不关闭 session，
  松手 Alt 后仍可继续 ←/→ 导航与数字/空格上屏
- **边界**：1 号位按 Alt+← / 末位按 Alt+→ → 忽略（消费但无操作）
- 例：`haoshi` 候选「耗时 好事 好使」，← 高亮「好使」→ Alt+← 两次 →「好使 耗时 好事」

### 键位

写死 `Alt+←/→`（与页内导航 ←/→ 紧邻，无需 config 配置；M2 不做键位自定义）。
**选 Alt 而非 Shift**：Shift 已承载字母大小写语义（ShiftChar 保形进序列），未来键位
扩展易与大小写冲突；Alt 是干净的修饰位，且不含大小写歧义。

**注意与现有约定的冲突**：`session_bridge.rs map_key` 顶部约定「Ctrl/Alt 组合键一律
放行给应用」（`if with_ctrl || with_alt { return None }`）。改造点：
- 顶部放行改为仅 `with_ctrl` 直接放行；`with_alt` 仅对 `VK_LEFT/RIGHT` 消费为
  `Key::SwapLeft/SwapRight`，其余 Alt 组合仍放行
- 取舍已知：**会话内** Alt+←/→ 被输入法消费（浏览器后退/前进等应用内组合在打字中
  失效——会话内方向键本就由输入法导航占用，语义一致）；**会话外** Alt+←/→ 仍放行给应用

## 2. 持久化：绝对值覆盖（无魔法数字）

**不存 delta**。用户反复调整几轮后 delta 是"历史运算残留"，不可读不可预测；
绝对值覆盖下用户库每个条目都是「这个字就是这权重」的直接陈述，反复调整 =
覆盖旧值，永远收敛。

- 交换 = 双方互写对方**合成权重**（合成 = 基本库 weight 被用户覆盖后的当前值）
- 用户库存 `(code, word, adjusted_weight)` 三元组
- 词库重编译后覆盖值依然生效（尊重用户意志；条目对应词条不存在则自然成为孤儿，无害忽略）

## 3. 用户库文件（`iuv.user.imedic`）

**与基本库同构**（IMEDIC02 段表格式 + mmap 加载代码，段类型不同），零新格式代码：

```
段0 元数据：版本 | 时间戳 | 各段条数
段1 权重覆盖表：N × { u8 code_len|code | u16 word_len|word | u32 adjusted_weight }
段2 屏蔽表：   M × { u8 code_len|code | u16 word_len|word }   ← 结构预留，交互未决
段3 (预留) 自造词表：同基本库记录体                          ← 未来功能
```

- 文件小（几千~几万条）：mmap + 校验 <1ms
- 缺失/损坏：视为空用户库（记日志），不影响基本库
- 写侧：命名 mutex 串行化 → 构建新文件（写时复制）→ 临时文件 + `ReplaceFileW` 原子替换
  （基本库永不写，页缓存共享不受影响）

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

## 5. 写入链路（Alt+← 按下时）

```
Alt+← 按下
  → Session 定位 all[idx] 与相邻候选（交换对 (code, A, B)）
  → 引擎内存态：A.adj = B.合成权重，B.adj = A.合成权重
  → recompute() 立即重排候选窗（selected 跟随被调词）
  → 立即写盘（写时复制 + mutex + ReplaceFileW，<2ms；Alt+← 低频操作，无需会话级 flush）
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
| `iuv-tsf/src/session_bridge.rs` | `map_key`：顶部「Ctrl/Alt 一律放行」改为仅 Ctrl 放行；`VK_LEFT if with_alt => SwapLeft`、`VK_RIGHT if with_alt => SwapRight`，其余 Alt 组合仍放行（新增单测，含 Alt+其他键放行回归） |
| `docs/plan/01-contract.md` | §3 用户库格式、§4.1 Key 枚举、§4.2 交互同步 |
| `docs/plan/15-input-matching.md` | 若涉及候选行为差异同步 |

## 7. 测试与验收

- iuv-data：用户库读写往返、覆盖 merge（同字覆盖/新字追加/稳定排序）、屏蔽过滤、坏文件→空库、mtime 重载
- iuv-core：SwapLeft/Right 边界（1 号位忽略）、连续上移、selected 跟随、立即重排不关会话、
  交换后上屏正确、与悬空续接/翻页交互无冲突
- iuv-tsf：map_key Alt+方向键单测（含 Alt+其他键仍放行回归）
- 手测：notepad `haoshi` 好使 上移两步 → 立即重排 → 空格上屏；新开进程验证持久化生效；
  其他进程打字验证 mtime 重载生效
- `cargo test --workspace` 全绿 + 热部署（`scripts/dev-deploy.ps1`）

## 8. 未来功能边界（本设计不做，已预留）

- 自动学习换位：**不做**（用户决策）；学习仅可能影响新词/无候选词（列入未来）
- 自造词：段3 结构预留，叠加机制天然支持第三个 merge 源
- 钉选快捷键：被 Alt+←/→ 提至固定位 + 不再调整即为钉选效果；显式"锁死"交互待定
- 屏蔽词交互（候选窗右键/长按数字等）：段2 结构已备，交互另行设计
- 键位自定义、跨设备同步：M4 helper 范畴

## 9. 未决决策（实现时再定）

- [ ] 覆盖表查询用「同构 IMEDIC02 二分」还是「内存 BTreeMap」（文件小，后者构建 ~1ms，更简单）
- [ ] 屏蔽词交互形式（右键菜单 vs 快捷键 vs M4 UI）
- [ ] 用户库写盘失败（文件锁）的重试策略
- [ ] Alt+←/→ 与浏览器后退/前进的冲突取舍已定（会话内消费、会话外放行）——如后续
      用户反馈矛盾再评估（候选窗出现时机 = 会话内，浏览器场景本就无会话）
