# 51 号 · 双字母声母简拼（zh/ch/sh）：音节图补 2 字简拼边

> 状态：**已实施待验收**（2026-10-08 立项；同日按本文件实施，**未提交**，待真机验收后决定）。
> 范围：`iuv-core`（`rime/syllabifier.rs` 一处新增；`translator.rs` / `mod.rs` / config **零改动**）。
> **不需要重编译词库** `data/iuv.imedic`——这是查询期的动态边，不是构建期的预生成键。
> 前置：`6056abe`（候选流排序对齐 librime，废弃 class 分级）已在分支
> `feat/rime-librime-order-align` 落地。

## 1. 背景与现象（repl 真词库复现）

用户用「声母简拼」习惯输入时，**目标词整条候选列表都不可达**（不是排后面，是根本出不来）：

| 输入 | 期望 | 实际（改动前=改动后，均不可达） |
|---|---|---|
| `huochzhan` | 火车站 | 火柴盒 / 火车 / 活成 |
| `tushguan` | 图书馆 | 徒手画 / 菟丝花 / 图书 |
| `zhguo` | 中国 | 在韩国 / 诸侯国 / 总好过 |
| `shdian` | 商店 | 少花点 / 少喝点 / 水和电 |
| `shrf` | 输入法 | 受害人 / 上海人 / 社会人 |
| `tshg` | 图书馆 | 太上皇 / 提升好感度 / 他似乎 |

**对照：把 `ch→c`、`sh→s`、`zh→z`（只取单字母）就全部正常**——证明缺的正是「双字母声母」这一族：

| 输入 | 结果 |
|---|---|
| `huoczhan` | **火车站 #1** ✅ |
| `tusguan` | **图书馆 #1** ✅ |
| `zguo` | **中国 #1** ✅ |
| `tsg` | 提升 / **图书馆 #2** ✅ |
| `cfan` | **吃饭 #1** ✅ |

## 2. 归因：librime 有两条简拼规则，iuv 只实现了第一条

librime 的拼音简拼是**拼写代数**定义的，标准 schema 一次给两条：

```yaml
# ../librime/data/minimal/luna_pinyin.schema.yaml:74-76
speller:
  algebra:
    - erase/^xx$/
    - abbrev/^([a-z]).+$/$1/      # ① 单字母首字母简拼：shu→s、zhong→z、chi→c
    - abbrev/^([zcs]h).+$/$1/     # ② 双字母声母简拼：shu→sh、zhong→zh、chi→ch
```

- 规则② 由 `algo/calculus.cc:213-233 Abbreviation::Apply` 施加，罚分与规则① 同值
  （`kAbbreviationPenalty = log(0.5)`，`calculus.cc:14`）。
- iuv 移植时只落了规则①：`rime/syllabifier.rs::build_graph` 的简拼边**只按 1 个字节**
  （`let initial = &input[s..s + 1]`，见 §4 现状代码），没有 2 字节声母边。
- 后果：输入 `sh` 只能被解释成「s 一个简拼边 + h 一个简拼边」（两个音节），
  而 `图书馆 = tu'shu'guan` 需要「sh 一个边代表 shu」——路径在结构上不存在，
  故整词不可达。**与排序无关，是图连通性问题。**

> 注：`luna_pinyin.schema.yaml:77-86` 的 `derive/.../correction` 系列是**纠错/模糊音**，
> 属 iuv M3 未开工范畴，**不在本任务范围**。

## 3. 方案：音节图补一条 2 字节简拼边

在 `build_graph` 的「单字母简拼边」之后，追加：当 `input[s..s+2]` 为 `zh`/`ch`/`sh`
时，补一条 **跨 2 字节**的 `Abbreviation` 边，携带该声母开头的全部音节
（`sh` → sha/shai/…/shu/…/shuo，共 58 个 zh/ch/sh 音节，`format.rs::SYLLABLES` 已含）。

关键点：**边的 `syllable` 值仍是完整音节（如 `shu`），消耗的输入字节是 2**。
查询侧 `translator.rs` 用 `sp.syllable` 拼键（`'` join），所以
`tu(N) + shu(A 跨"sh") + guan(N)` 得到的键仍是 `tu'shu'guan`，直接命中词库码——**无需改键族、无需改词库**。

`concat` 判定（`translator.rs:149-150`，`Abbreviation && syllable.chars().count()==1`）
不受影响：zh/ch/sh 开头的音节长度均 ≥3，恒走 `'` join 族。

## 4. 改动清单与 DoD

| 文件 | 改动 |
|---|---|
| `crates/iuv-core/src/rime/syllabifier.rs` | `build_graph` 单字母简拼边之后，新增 2 字节声母简拼边（zh/ch/sh）；模块头注释补规则②出处 |
| `crates/iuv-core/src/rime/syllabifier.rs`（测试） | 新增单测：`sh` 声母边命中 `tu'shu'guan`；`zhguo`→中国路径存在 |
| `docs/plan/01-contract.md` | §8.3 候选流/简拼语义补一句「简拼含 1 字首字母 + 2 字声母两族」 |

**现状代码（`build_graph` 循环体内，`syllabifier.rs:112-147`）**——新块插在「族②」之后、
循环体结束之前：

```rust
// 单字母简拼边：两族拼写——①该字母开头的全部音节 …… ②字母串自身 ……
let initial = &input[s..s + 1];
let mut e = s + 1;
while e < n && bytes[e] == b'\'' { e += 1; }
for syl in syllables.iter().filter(|syl| syl.starts_with(initial)) { /* Abbreviation */ }
if initial.as_bytes().first().is_some_and(|b| b.is_ascii_lowercase()) { /* 族② 字母串自身 */ }
```

**拟新增（同位置）**：

```rust
// 双字母声母简拼边（zh/ch/sh）：librime luna_pinyin.schema.yaml:76
// `abbrev/^([zcs]h).+$/$1/`——输入 "sh" 展开为该声母开头的全部音节
// （shu/shi/shang/…），与单字母边同型同罚分（calculus.cc:14 log(0.5)）。
// 无此边时 `huochzhan`/`tushguan`/`shrf` 这类声母简拼整串不可达（51 号根因）。
if s + 2 <= n {
    let two = &input[s..s + 2];
    if matches!(two, "zh" | "ch" | "sh") {
        let mut e2 = s + 2;
        while e2 < n && bytes[e2] == b'\'' { e2 += 1; }
        for syl in syllables.iter().filter(|syl| syl.starts_with(two)) {
            add_spelling(&mut edges, &mut reached, v, e2, syl.clone(),
                         SpellingType::Abbreviation, abbrev_penalty);
        }
    }
}
```

DoD：
1. `cargo test -p iuv-core` 全绿（既有 259 项 + 新增单测）；
2. repl 真词库对拍：§5 六条「期望」全部 #1 命中，且 §5 的「回归不动」清单逐字节不变；
3. clippy 零新增；
4. 真机 `scripts\dev-deploy.ps1` 手测由管理员执行。

## 5. 验证用例（repl 真词库 `data/iuv.imedic`）

**修复目标（改动前不可达 → 改动后应 #1）**

| 输入 | 期望 #1 | 备注 |
|---|---|---|
| `huochzhan` | 火车站 | 中段声母 ch |
| `tushguan` | 图书馆 | 中段声母 sh |
| `zhguo` | 中国 | 段首声母 zh |
| `shdian` | 商店 | 段首声母 sh |
| `shrf` | 输入法 | 段首声母 sh，3 音节 |
| `tshg` | 图书馆 | 全声母简拼 |

**回归不动（必须逐字节不变）**

| 输入 | 现状 |
|---|---|
| `shurfa` | 输入法 #1（6056abe 的成果，勿回退） |
| `shurf` / `shuruf` | 输入法 #1 |
| `shurufa` / `shu` / `shuru` | 输入法 / 书数树 / 输入 |
| `nihao` / `zhongguo` / `woaini` / `xiexie` / `pengyou` | 你好 / 中国 / 我爱你 / 谢谢 / 朋友 |
| `nihma` / `meigxi` / `duosqian` / `zenmban` | 你好吗 / 没关系 / 多少钱 / 怎么办 |
| `srf` / `nhmsx` / `nh` | 杀人犯·输入法 / 你还没 / 你好 |
| `shigechengy` | 是个成员（句候选置顶） |
| `sh` / `zh` | 纯单字（微软对齐前缀档） |
| `xian` / `jian` | 先·线·西安 / 间·见·件 |

## 6. 边界与风险

- **全拼零影响**：顶点有 `Normal` 出边时（如 `shurufa` 的 `shu`），`translator.rs` 的
  `has_normal` 守卫只走 Normal/Completion，2 字节简拼边被跳过 → 全拼路径不变。
- **前缀档零影响**：`sh`/`zh` 单串走 `mod.rs` 的「音节真前缀 → 纯单字」政策，
  在图流之前就返回，不经过新边。
- **显式撇号形态**：只识别字面 `zh`/`ch`/`sh`；`z'h` / `s'h` 这类用户强制分隔形态
  不产 2 字节边（1 字节边照常，行为与今一致）。如需支持另立。
- **性能**：新增的 `syllables.iter().filter(...)` 与既有 1 字节边同型（约 410 音节线性扫），
  且仅在 `input[s..s+2] ∈ {zh,ch,sh}` 的顶点触发，可忽略。若日后要优化，
  可把 `首字母/声母 → Vec<音节>` 预建索引，两条边共用。
- **可能新增的候选**：`shd`/`chx` 这类「声母 + 残段」输入会多出以 `sh*`/`ch*` 开头的
  组合（如 `sha'd…`）。这是规则② 的**正确**语义（librime 同样会出），
  若真机观感异常再单独评估。

## 7. 参考

- `../librime/data/minimal/luna_pinyin.schema.yaml:70-86`（简拼代数两条规则）
- `../librime/src/rime/algo/calculus.cc:14,213-233`（`Abbreviation::Apply` 与罚分）
- `../librime/src/rime/algo/syllabifier.cc:31-258`（音节图构建：`CommonPrefixSearch` 动态派生）
- 本仓：`crates/iuv-core/src/rime/syllabifier.rs`、`translator.rs`、`mod.rs`
- 相邻任务：`48-abbrev-syllable-collision.md`（简拼键与完整音节撞键，构建期过滤）

## 8. 实施记录（2026-10-08，未提交）

**落地改动**（分支 `feat/rime-librime-order-align`，工作区未提交）：

| 文件 | 改动 |
|---|---|
| `crates/iuv-core/src/rime/syllabifier.rs` | ① `build_graph` 单字母简拼边之后新增 2 字节声母边（z/c/s + h 判定先行，切片必落字符边界）；② 模块头注释补两族简拼出处；③ `build_graph` 规则清单重排编号（原「4/5/6」→「5/6/7」，新增为「4」） |
| `crates/iuv-core/src/rime/mod.rs` | 新增 3 条单测：`two_letter_initial_sh_abbrev_hits_word` / `two_letter_initial_zh_abbrev_hits_word` / `two_letter_abbrev_keeps_full_pinyin` |
| `docs/plan/01-contract.md` | §8.3 补「简拼两族」一句 |

**顺带修一处 6056abe 遗留的 clippy deny**：`ABBREVIATION_PENALTY` 字面量 `-0.693_147_180_559_945_3`
被 `clippy::approx_constant`（correctness，deny 级）判为 `-LN_2` 近似值 → 改 `-std::f64::consts::LN_2`（**数值完全相同**，行为零变化）。

**验证结果**：

1. `cargo test -p iuv-core`：**262 项全绿**（原 259 + 新增 3），0 失败。
2. `cargo clippy -p iuv-core`：**零 warning / 零 error**。
3. `cargo check -p iuv-data -p iuv-proto -p iuv-ui -p iuv-repl`：全部通过。
4. **改动前/后逐条对拍**（stash 还原 HEAD 编译对比，29 条输入）：**diff 只覆盖 6 条目标用例**，
   其余 23 条回归项逐字节不变。

**目标用例实际结果**（与 §5「期望 #1」有 3 条出入，但均**从不可达变为可达**）：

| 输入 | 改动前 #1 | 改动后首屏 | 判定 |
|---|---|---|---|
| `huochzhan` | 火柴盒 | **火车站** #1 | ✅ 达 #1 |
| `tushguan` | 徒手画 | **图书馆** #1 | ✅ 达 #1 |
| `zhguo` | 在韩国 | **中国** #1 | ✅ 达 #1 |
| `shdian` | 少花点 | 书店 #1 / **商店** #2 | ⚠️ 可达（#1 是词频更高的 书店） |
| `shrf` | 受害人 | 杀人犯 #1 / **输入法** #2 | ⚠️ 可达（杀人犯 1108 > 输入法 1011） |
| `tshg` | 太上皇 | 提升 #1 / **图书馆** #2 | ⚠️ 可达（提升 54360 ≫ 图书馆 11098） |

> 后 3 条的 #1 差异**不是缺陷**：三者目标词与 #1 词**消费终点相同**（都吃满全串），
> 落在同一桶内按词频降序——这是 6056abe「消费长度优先 + 组内词频降序」的既定语义。
> §5 的「期望 #1」为立项时的乐观估计，实现后按词库权重实际排序即可。

**边界观察**（§6 预判的「新增候选」）：`shd`→上的/说的/时代、`chx`→出现/持续/重新、
`zhd`→真的/中的/知道、`shangd`→上的/上帝/商店/上都，均为规则② 的**正确**语义，
无异常观感。前缀档 `sh`/`zh` 仍为纯单字（是/上/时…、中/这/只…），未受影响。

**未做**：真机 `scripts\dev-deploy.ps1` 手测（由管理员执行）；`docs/status.md` 台账（随提交补）。
