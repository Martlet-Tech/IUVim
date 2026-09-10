# 46 号 · 引擎长串性能改造（v2 全模块化：整跨词反查 + 词典游标 + preedit 重划）

> 状态：已定稿（2026-09-10 管理员裁决：定档 v2、接受「整跨词优先」行为变化），待开工。
> 范围：`crates/iuv-data`（dict.rs / format.rs / dictc）+ `crates/iuv-core`（schema.rs / api.rs / rime/{mod,translator}.rs / session.rs）+ `docs/plan/01-contract.md` 契约同步。
> **本任务书不含任何 perf_probe / 日志改造**（管理员指示）。P3 候选惰性物化、提交键 1.83s 冻结另立项。

## 1. 背景与实测证据

**现象**：连打 44+ 字母长拼音串明显卡顿（末段单键 0.6–4.0s），小狼毫同串流畅。

**实测**（2026-09-10 notepad，108 键，onkey 累计 17.77s）：

| 阶段 | 累计 | 占比 | 随长度行为 |
|---|---|---|---|
| `onkey.seg`（切分+重排） | 5.93s | 33.4% | len≤75 时 <25ms；len 78→92 每键近似翻倍：18→…→**1898ms** |
| `onkey.buckets`（桶收集） | 3.88s | 21.8% | 双峰：基线 2–20ms + 约 40% 键尖峰 25–150ms |
| `onkey.assemble` | 0.12s | 0.7% | 涨至 ~2ms 封顶 |
| `onkey.graph` | 0.02s | 0.1% | 可忽略 |
| 未归因余量 | 7.83s | ~44% | 提交键 1.83s（独立问题）+ preedit 二跑 ranked_seg（§2 R1b） |

**librime 参照**：每键同样全量重算，但常数封顶——Dijkstra 切分单顶点单次 trie 走查、`kExpandSearchLimit=512`（syllabifier.cc:209）、`max_homophones=1`（script_translator.h:57）、候选惰性物化。

## 2. 根因定性

- **R1（~60% onkey）切分指数枚举**：`schema.rs:36-88` 全枚举全部切分方案；兜底单字母段（:73-87）在孤点重新打开下游全部分歧 → 叶数指数乘。`MAX_PLANS=128` 截断在枚举完之后。**preedit 二跑**（rime/mod.rs:332，经 session.rs:473 effect()）再翻一倍。
- **R2（21.8%）桶收集单价**：`translator.rs:124-148` 每步拼接 String 键 + `has_code`/`has_prefix` 两次全局二分（125.5 万词条散射 memcmp）+ 带 String 的 HashSet；尾音节未闭合键状态数倍增成尖峰。39 号注释所称「词典游标」实为 String 键串——游标抽象未真正落地。
- **排除**：graph/assemble/候选全量物化（0.1%/0.7%，~3ms/键）。
- **独立问题（不在本任务书）**：空格提交整句冻结 1.83s（commit 路径，嫌疑用户库更新/管道往返）。

## 3. 方案（v2 三归位，零补丁）

### 3.1 切分决策 = 词库整跨词反查（删枚举，替「早停」补丁）

**语义论证**：`rank_plans` 得分 = `exact(join(plan)).first().weight`；得 0 分的方案在稳定排序下按原序保持 → 贪心（方案[0]）胜出；非零分 ⇔ 整串恰为词库某码。故最优切分 = **码去撇号 == raw 归一形的词库码中权重最高者**，无则贪心——一次反查即可，无需枚举。

- **dictc 编译期**：遍历词条，code 按 `'` split，构建 `map<concat(段), (top_weight, code)>`（同 raw 多码取权重最高，平局取码序小者）；固化为排序数组段（格式版本 bump）。
- **运行时**：`Dict::best_code(raw) -> Option<(code, weight)>`，O(log n + 码长)。引擎合成：`best_seg(raw) = best_code ? code.split('\'') : greedy(raw)`。归一口径 = 小写化 + lue→lve / nue→nve（24-ue-input-alias 同一单点）。
- **删除**：`schema.rs` `enumerate_inner`/`backtrack`/`MAX_PLANS`（~60 行）、`api.rs` `rank_plans`（~20 行）。`Quanpin` 只留 O(n·L) 贪心构造（现 backtrack 首条 DFS 路径抽成直接循环，最长优先 + 兜底规则逐字节保留）。
- **行为变化（已裁决接受）**：>128 歧义且整串成词时，由「退化贪心」改为「整跨词优先」。

### 3.2 Dict::Cursor 真正落地（删 String 键串，替「memo/预算」补丁）

词库码按字典序存储 → **同前缀码集恒为连续区间**，游标 = 区间 `{lo, hi}`。

- 接口：`Dict::cursor(prefix)` 根区间；`Cursor::step(seg) -> Option<Cursor>`（区间内 galloping 二分收缩，防等长码簇退化，先例 dict.rs:296-318）；`has_code()`（区间含等长码，零分配）；`expand_search(limit=512)`（尾前缀补全扇出上限——librime ExpandSearchLimit 的契约位，预算属词典接口而非 BFS 队列）；`materialize(limit) -> Vec<Entry>`（此刻才构字符串）。
- `translator.rs`：`Walk { origin, v, cursor, hops, class, cred }`——String 键串仅在桶标记物化时构造一次；visited 去重键改 `(origin, end, lo, hi, completion)`。桶排序/合并逻辑不动。

### 3.3 preedit 职责重划（删二跑，替「复用」补丁）

`recompute` 已存 translate 产出的 seg（session.rs:371-374）；`ImeEngine::preedit` 改为消费 seg 的纯显示调用（`preview_rules` 本为纯函数），签名 `preedit(raw, seg, selected)`；session.effect() 直传 `self.seg`。**01-contract.md** trait 契约与 §4 切分契约同步（segment 契约改述为「贪心切分 + 整跨词反查」）。

### 3.4 Dict::prefix top-k 归并（沿 v1，兑现自留 TODO）

dict.rs:331 自留 TODO：补全物化 64 条改区间内权重 top-k 归并，替换「全范围收集 + 排序 + 截断」。

## 4. 不做什么（负范围）

- perf_probe / 日志模块一概不动。
- P3 候选惰性物化（max_candidates 1024 全量喂 TSF）→ 另立小任务书。
- 提交键 1.83s 冻结 → 独立排查。
- syllabifier.rs / poet.rs / iuv-ui / iuv-tsf / daemon 不动；用户库格式不动。

## 5. 基线与验收

**基线先行**（开工第一步，删除枚举前完成）：REPL/ignored test 跑语料库存 stdout 快照。

- 语料 = 39 号 §15A 十二条 + 4 条长串哨兵：`beiguofengguangqianlibingfengwanlixuepiaowangchang` / `ceshiyixiaxianzaiyongwozhegeshurufadouchunbuganjuezianzaihaoxiang` / `chuangqianmingyueguang` / `nhmsx`。
- 对拍基线 = 改造前现语义输出（B0）；「整跨词优先」行为差异用合成用例专项断言（§6），不混入对拍。

**验收数字**（ignored 基准，`std::time::Instant`，不碰 perf_probe）：

| 指标 | 改造前 | 目标 |
|---|---|---|
| len=92 尾型串单键 translate | 3851ms | **< 10ms** |
| 哨兵②逐键 translate 累计 | ~17.8s | **< 1s** |
| 哨兵③全 translate | 39ms | ≤39ms（不劣化） |
| 尾开放键单键（尖峰） | 25–150ms | < 15ms |
| 对拍 | — | ranked_seg/候选/preedit 输出与 B0 逐字节一致（除专项差异用例） |

## 6. 测试计划

1. **贪心等价**（删除枚举前）：新贪心实现与旧 `enumerate_inner` 首叶并存对拍（现有 10 项 schema 测试 + 随机串 fuzz 一版），绿后删枚举。
2. **反查段**：dictc 构建单测（构建/查询/同 raw 多码平局/归一口径 lue→lve）。
3. **Cursor**：区间收缩性质（随机码序性质测试）+ 等长码簇 galloping 专项。
4. **行为差异专项**：合成「>128 切分方案且整串成词」用例，断言整跨词优先。
5. **translator/preedit**：现有 rime 测试全数保留；`preedit_follows_candidate_rime` 等显示断言对 B0 一致。
6. **基准**：真词库 ignored 计时测试，对照 §5 表填数。

## 7. 风险与回退

- **格式 bump**：旧 `iuv.imedic` 不兼容 → `download-dict.ps1` 重编译（dictc 同仓演进，成本一次）；x86 产物同步。
- **归一口径错位**：反查键必须与运行时归一严格一致（小写 + lue→lve），用测试 2 钉死。
- **Cursor 退化**：等长码簇区间收缩退化 → galloping 二分（先例已有）。
- **贪心等价**：贪心实现与旧首叶不一致会导致全链显示/口径漂移 → 步骤 1 对拍钉死后才准删。
- **回退**：P1（切分反查 + preedit 重划）与 P2（Cursor + prefix 归并）分两个 commit，各附基线 diff，可独立 revert。

## 8. 裁决记录

2026-09-10 管理员拍板：① 定档 **v2 全模块化**（否决 v1 早停/memo/队列预算三补丁与 v1.5 折中）；② **接受** >128 时「退化贪心」→「整跨词优先」行为变化；③ perf_probe 不参与、P3 与提交冻结另立项。原 v1 方案三处补丁定性见会话记录（2026-09-10）。

## 9. 估时与顺序

| 步骤 | 估时 | 说明 |
|---|---|---|
| 基线快照 + 贪心对拍（含步骤 1 双实现并存） | 0.5d | 先行 |
| iuv-data：Cursor + prefix top-k | 1d | |
| iuv-data：反查段（format + dictc） | 0.5–1d | 格式 bump |
| iuv-core：删枚举 + best_seg + translator Cursor 化 + preedit 重划 + contract 同步 | 1d | |
| 重编译词库 + 全量测试 + 验收填数 + status.md 台账 | 1d | |
| **合计** | **3.5–4.5d** | 切分线与游标线可两 commit 串行 |

## 10. 性能档案（2026-09-10 实测留档）

- 环境：notepad，108 键，onkey 总 17.77s（`%TEMP%\iuv-tsf.log` 当日）。
- 分项：seg 5.93s（33.4%）/ buckets 3.88s（21.8%）/ assemble 0.12s / graph 0.02s / 余量 7.83s（含提交键 1.83s）。
- seg 尾部序列（len→ms）：78→18.1、79→59.1、81→101.7、82→150.4、83→327.9、84→210.2、85→357.6、86→477.7、87→445.0、88→738.0、89→930.6、90→775.2、91→1260.2、92→2357.9、93→3851.4。
- buckets 双峰：基线 1.3–19.6ms；尖峰 25–148ms（约四成键）；解开 8/29「len=22 53ms 而 len=24 2.3ms」之谜（尖峰≈尾音节未闭合，与总长无关）。
