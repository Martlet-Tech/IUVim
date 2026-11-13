# 62 号 · 结构化候选空间（权威切分 + 分组配额 + OOV 构造闭环）

> 状态：**已结案**（2026-11-11，分支 `feat/62-candidate-space` 合并回 main；
> 随 v0.2.2 发布）。五层已贯通，真机热部署 + 用户实测 + 散文语料 REPL 回归通过。
> 触发：用户打 `yinguai` 想输入「引怪」（游戏常用词，词库未收录），翻了几十页找不到
> 「引」，只能拆成两次会话分别上屏「引」「怪」。REPL 实测：候选表 1024 条触顶截断，
> 「引」在第 280 位（page_size=5 → 第 56 页）；第 4–275 条全是 `ying` 同音字（含
> 大量 w=0 的扩展区生僻字），而 w=27218 的「因」排在第 276 位。

## 1. 系统性根因

引擎把输入法实现成「对静态词库的检索器 + 一条按分数排序的扁平全表」，于是**词库规模
直接决定用户的交互成本**。四个现象是同一个模型的四个投影：

1. **契约自身矛盾**：15 号「逐级全出、翻页总能到达」、`01-contract.md §4.2`
   「单字全量返回不截断（微软对齐）」、39 号 rime 化「按 `end` 降序全量物化
   （st.cc 对齐）」三代设计叠加，没有统一模型。
2. **切分图泄漏成候选空间**：`syllabifier` 最长优先完整音节匹配会把 `yinguai` 前 4
   字母 `ying` 也当 Normal 边（借走 `guai` 的 g），`yi`、`yin`、`ying` 三条读法的字
   全进同一条表。
3. **权重不是跨桶排序键**：`rime/mod.rs` 以 `end` 降序为桶间唯一键，桶内才按
   `exact → cred → weight`；`kind`（词/字）完全不参与排序。结果 w=0 的生僻字
   排在 w=27218 的「因」前面。改成全局权重也不对——`yi` 组的「以 198619 / 一 99581」
   会霸屏。**任何单一全局排序键都救不了**。
4. **OOV 无一等公民路径**：组句 `max_homophones=1` 只出一条「因怪」，句内改字（M3）
   未做；逐字构造被埋在 1024 条里；自动造词只是逐字选择 commit 时的隐式副作用。

**结论：要换模型，不是换排序键，也不是加过滤。**

## 2. 目标模型

候选空间的大小由交互设计决定（每页 5 × 有限页数），词库规模只影响组内质量。

| 组 | 语义 | 配额 | 折叠 |
|---|---|---|---|
| `Whole` | 覆盖整串的词（exact）+ 句候选 | ~~50~~ → 不限量（63 号） | 否 |
| `Completion` | 尾前缀补全词（`yin'gu` → 因故/银箍） | 10 | 否 |
| `Prefix[k]` | 只消费前 k 段，含首音节单字组 | ~~50/组~~ → 不限量（63 号） | 否 |
| `Alternate` | 非权威切分的其它读法（ying/yi …） | `max_candidates` 兜底 | 是 |
| ~~`Rare`~~ | ~~w=0 / 超配额的扩展区生僻字~~ | **63 号整组删除**：0 权重生僻字按权重留在各原组尾部，翻页可达 | — |

三条硬规则：

- **一个输入只有一个权威解释**：`best_seg` 升为唯一候选生成基线；只有落在切分
  边界 `cum` 上的桶进主空间，其余读法归入折叠组，用户显式切组才展开。
- **组间不竞争名额**：每组独立配额、独立按权重排序（桶内权重序不变），
  `max_candidates` 只作折叠组的全局安全阀。
- **词库是可增长缓存**：整串只有句候选（OOV）时，默认组直达「首字」组，
  用户逐字构造，上屏时写用户词库；下次整串 exact 命中即首选。

## 3. 五层落地（本次）

| 层 | 文件 | 改动 |
|---|---|---|
| 引擎 | `crates/iuv-core/src/candidate_space.rs`（新） | 组结构 / 配额 / 单字裁剪 / `build` 分组 |
| 引擎 | `crates/iuv-core/src/rime/mod.rs` | 删除 `end` 降序扁平流；按权威切分边界 `cum` 分桶；句候选进 Whole 组；前缀联想并入整串组 |
| 引擎 | `crates/iuv-core/src/api.rs` | `Translation` 增 `groups` / `default_group` |
| 会话 | `crates/iuv-core/src/session.rs` | `all_cands/groups/active_group` 状态；组内分页；PageDown/Up 跨组 + `]`/`[` 显式切组；`all_candidates` 恒为当前组 |
| 快照 | `crates/iuv-core/src/key.rs` | `Effect` 增 `groups` / `active_group` |
| 协议 | `crates/iuv-proto/src/msg.rs` / `lib.rs` | `KeyOutcome` / `Effect` 增分组摘要；**PROTO 4→5**（定长序列破坏性变更） |
| TSF | `platforms/windows/iuv-tsf/src/com/dispatch.rs` | 分组摘要合并进 core Effect；`ui_element.rs` 的 IMM 全表自动变为当前组（≤50 条，不再是 1024 条 / 205 页） |
| 服务端 | `platforms/windows/iuv-server/src/lib.rs` | `outcome()` 下发分组摘要 |
| UI | `crates/iuv-ui/src/snapshot.rs` / `layout.rs` / `render.rs` | `UiSnapshot.group_label`；底部小字行显示「首字 2/4 · 1/10」，文本口径 `page_label()` 单点 |

**Compose 构造**：不新建状态机，复用既有 `picked` 栈续接 + `record_phrase`
（`session.rs:292` / `userdict.rs:203`）。默认组直达首字组后，「选引 → 剩 `guai`
续接 → 选怪」在同一会话内完成，满足「picked 全单字 + 整串 ≥2 字」自动写库条件。

## 4. 不做（后续立项）

- N-best 多句候选 + 句内改字（M3）：本次组句仍为单条。
- 组切换键进 `keymap` 可配置：本次用 PageUp/Down 跨组 + `]`/`[` 硬编码。
- 显式「造词记录」设置页、跨会话词频在线学习。
- 39 号语料对拍基线重建（`SEG/CANDS/READING` 快照）：候选行为已变，需新基线。

## 5. 验收

REPL `--batch` 真词库（用户 `config.json`：page_size=5、max_candidates=1024）：

| 输入 | 改前 | 改后（默认组） |
|---|---|---|
| `yinguai` | 1024 条，引 @280（第 56 页） | 50 条常用字，**引 @5（第 1 页）** |
| `yin` | 1024 条触顶 | 50 条常用字，引 @5 |
| `yin'guai` | 1024 条，引 @8 | 50 条常用字，引 @5 |
| `guai` | 340 条 | 6 条（怪 @1，w=0 生僻字折叠） |
| `nihao` | 你好首选 | 你好首选（2 条，整串组） |

- [x] `cargo check --workspace --all-targets` 全绿、`cargo fmt --all -- --check` 0
- [x] 核心集成测试全绿：`engine_session` 106 项 + `engine_switch` 4 项；
      正式 `cargo build -p iuv-tsf --release` 通过
- [x] `scripts/dev-deploy.ps1` 热部署 + 真机（新开窗口）验收
- [x] OOV 构造闭环（真机日志 10:38:25 / 10:38:28）：打 `yinguai` → 按 5 选「引」
      → 空格选「怪」→ 上屏「引怪」；再打 `yinguai` 直接空格 → 上屏「引怪」。
      用户词库 `iuv.user.imedic` 内可见 `yin'guai` / `引怪` 条目
- [x] 散文语料 REPL 回归（`tmp/prose-report.txt`）：22 词 18 首选 / 4 前三 / 0 未出现；
      9 分句整句候选齐备；候选规模 8–64 条，扩展区生僻字 0
- [ ] 完整 `cargo test --workspace` 在智能体会话内被安全中心规则阻止写测试用 `.exe`
      （非代码失败），建议本机终端补跑一次

## 6. 部署注意

PROTO 4→5，server 与 TSF DLL 必须同版本（`dev-deploy` 一次替换）。运行中仍持旧
DLL 的进程需新开窗口/重启应用；脚本会优雅停机重启 `iuv-server` 并重启 ctfmon。
`iuv-tsf/build.rs` 的 winres 依赖 `reg.exe` 查 SDK，沙箱环境类型检查需
`IUV_SKIP_WINRES=1`（该产物**不可装载**，正式构建不能带此变量）。
