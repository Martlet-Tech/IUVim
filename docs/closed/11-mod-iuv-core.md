# 11 · 任务书 B：iuv-core（引擎）

> 属主文件：`crates/iuv-core/src/{schema,lm,viterbi,rerank,store,engine,session}.rs`、`crates/iuv-core/tests/**`
> 前置阅读：`00-overview.md`、`01-contract.md`（重点 §4 全部签名 + §4.1 按键行为表 + §4.2 候选生成算法）、`30-conventions.md`
> **禁止**修改 `candidate.rs` / `config.rs` / `key.rs` / `lib.rs`（W0 冻结）。本 crate 可被 repl 与 TSF 同时依赖，**不得引入任何 Windows API**。

## 1. 目标

实现引擎全部逻辑：全拼切分、候选生成（整句 Viterbi + 精确词 + 前缀补全）、排序管线（MVP 静态序）、
会话状态机。全部用 `Dict::from_entries` 构造测试词典完成单测，**不依赖真实词库文件、不依赖 Agent A 的编译器**。

## 2. 交付清单

| 文件 | 内容 |
|---|---|
| `src/schema.rs` | `InputSchema` trait + `Quanpin` 实现 |
| `src/lm.rs` | `LmProvider` trait + `UnigramLm` |
| `src/viterbi.rs` | 最优路径（内部 API，不 expose 到 lib.rs 也可） |
| `src/rerank.rs` | `RerankCtx` / `RerankStage` / `StaticOrder` |
| `src/store.rs` | `UserDataStore` / `NullStore` |
| `src/engine.rs` | `Engine`（两个构造器 + 候选生成 + lookup） |
| `src/session.rs` | `Session` 状态机 |
| `tests/*.rs` | 下文 §4 全部用例 |

## 3. 实现要点

### 3.1 Quanpin 切分（schema.rs）

- 合法音节集来自构造参数（`Dict::syllables()` 的超集/子集都行，引擎建 Engine 时传入）
- 规则：遇 `'` 强制断开；其余从左到右**贪心最长匹配**合法音节；匹配失败则当前字母单独成段（保底，永不 panic）
- `display`：以 `'` join
- 例：`nihao` → `[ni, hao]`；`xi'an` → `[xi, an]`；`xian` → `[xian]`（贪心，不枚举）

### 3.2 UnigramLm / Viterbi

- `log_prob = ln(weight+1) − ln(total_weight)`；`total_weight==0` 时保底返回 `-30.0`
- Viterbi：`dp[0]=0`，转移按契约 §4.2；OOV 边加 `OOV_PENALTY=-10.0`；回溯取词序列拼成 sentence 文本
- 注意 `max_word_syllables` 取自 `config`，与 `dict.max_word_syllables()` 取 min

### 3.3 Engine 候选生成

严格按契约 §4.2 的 6 步。**顺序即静态展示序**（Sentence 在最前）。去重按 text。
`lookup()` 直接代理 `dict.exact()`。
`store` 字段：`Mutex<Box<dyn UserDataStore>>`（`Engine` 进程级单例，TSF 多线程激活时需要 `Send`；
`InputSchema`/`LmProvider`/`RerankStage` 均为无状态只读，无需内部可变）。

### 3.4 Session 状态机

严格按契约 §4.1 表格。要点：
- 内部持有 `all: Vec<Candidate>`（全表），`Effect.candidates` 只装当前页切片
- commit 时先 `record_selection` 再填 `end`；`code` 取值规则见表格末行
- `Digit(n)` 越界 = 消费但无操作（Effect 照常返回当前快照）
- `is_active()` = `!raw.is_empty() && end.is_none()`
- 会话结束后再次被调 `on_key`：返回 `Effect{ end: Some(Cancel) }` 兜底即可，不 panic

### 3.5 槽位（只搭骨架，不写逻辑）

- `RerankStage` 管线在 `engine.rs` 里按 `Vec<Box<dyn RerankStage>>` 顺序调用；`StaticOrder` 是空操作
- `LmProvider.log_prob` 的 `prev` 参数 MVP 忽略（n-gram 槽位）
- `UserDataStore::power` MVP 无人调用，NullStore 返回 0（M2 主动调权已改为 UserDict 覆盖表承载，见 18-m2-user-dict.md）

## 4. 测试（必须全部实现并全绿；用 `Dict::from_entries` 造小词典）

建议词典 fixture：`的(de, 100000)`、`得(de, 300)`、`地(de, 200)`、`你好(ni hao, 8000)`、
`泥(ni, 500)`、`你(ni, 50000)`、`好(hao, 40000)`、`hao` 音节相关、`世界(shi jie, 6000)`、`世(shi, 3000)`、`界(jie, 2500)`。

**schema**：`seg_basic`（nihao→[ni,hao]）、`seg_apostrophe`（xi'an→[xi,an]）、`seg_greedy`（xian→[xian]）、
`seg_invalid_char_fallback`（含非音节前缀不 panic）、`display_joins_with_apostrophe`

**viterbi**：`sentence_prefers_high_freq_path`（构造两条路径，高分词路径胜出）；
`oov_syllable_falls_back`（词典无该音节时 sentence 含原字母）；`single_syllable_no_sentence`（seg.len()<2 不出 Sentence）

**engine 候选生成**：`candidates_sentence_first`（`nihao` 首候选 kind==Sentence 且 text 含"你好"）；
`exact_words_order_by_weight`（`de` → 的/得/地 顺序）；`prefix_completion_recalled`（`nih` 能出 你好）；
`dedup_by_text`、`max_candidates_capped`

**session（对照契约 §4.1 逐行测）**：`type_shows_candidates`、`backspace_to_empty_cancels`、
`space_commits_selected`、`space_without_candidates_commits_raw`、`digit_selects_nth_in_page`、
`digit_out_of_range_noop`、`enter_commits_raw`、`esc_cancels`、`paging_clamps_and_resets_selected`、
`updown_clamps_within_page`、`commit_records_selection`（用 spy store 断言 code/text 入参）

**管线**：`rerank_stage_is_invoked`（spy stage 断言被调用且可重排——这是 M2 槽位的验证）

**回归基准（防跳动预留，M2 充实）**：`static_order_is_deterministic`——同一输入两次候选序列完全一致。

## 5. DoD

```
cargo test -p iuv-core        # 全绿
cargo check -p iuv-core       # 无 warning
```

## 6. 子智能体启动提示词

```
你负责实现 iuv 输入法 MVP 的 iuv-core 模块（引擎：全拼切分/候选生成/unigram Viterbi/会话状态机/排序管线）。
先读 D:\Projects\vaim\docs\plan\00-overview.md、01-contract.md、30-conventions.md，
再读任务书 D:\Projects\vaim\docs\plan\11-mod-iuv-core.md 并严格执行。
接口签名与行为以 01-contract.md §4/§4.1/§4.2 为唯一权威；只能创建/修改属主矩阵中 Agent B 的文件。
candidate.rs/config.rs/key.rs/lib.rs 已冻结禁止修改；Dict 由 iuv-data 提供，测试用 Dict::from_entries 构造，禁止读真实词库文件。
crate 必须保持跨平台纯 Rust（禁止 windows API）。
完成后必须满足 DoD：cargo test -p iuv-core 全绿、cargo check -p iuv-core 无 warning。
最终回复：改动文件清单 + 测试输出摘要 + 任何偏离契约之处（应为无）。
```
