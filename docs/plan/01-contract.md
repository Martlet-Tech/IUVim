# 01 · 共享契约（唯一权威接口源）

> 本文档定义 M1 全部**跨模块**类型、trait、行为契约与文件属主。
> W0 由主智能体按本文档建骨架并冻结；W1 各子智能体只实现、不修改本文档定义的签名。
> 任何签名变更必须回到本文档修改，并由主智能体同步所有受影响模块。

## 1. 工作区结构（W0 产出）

```
D:\Projects\input\
├── Cargo.toml                     # workspace 根（见 §2.1）
├── .gitignore                     # /target, /data
├── AGENTS.md
├── docs\plan\                     # 本方案书
├── data\                          # 下载的词库源文件 + 编译产物（gitignore）
├── scripts\
│   ├── download-dict.ps1          # 【Agent A】下载白霜词库
│   ├── register.ps1               # 【Agent D】注册输入法（管理员）
│   └── unregister.ps1             # 【Agent D】注销
└── crates\
    ├── ime-data\
    │   ├── Cargo.toml             # 【W0】
    │   ├── src\lib.rs             # 【W0】模块声明 + re-export
    │   ├── src\dict.rs            # 【W0】Dict 查询层（完整实现，冻结）
    │   ├── src\format.rs          # 【Agent A】二进制格式读写
    │   ├── src\compile.rs         # 【Agent A】rime yaml → 记录集
    │   ├── src\bin\dictc.rs       # 【Agent A】编译 CLI
    │   └── tests\                 # 【Agent A】
    ├── ime-core\
    │   ├── Cargo.toml             # 【W0】
    │   ├── src\lib.rs             # 【W0】模块声明 + re-export
    │   ├── src\candidate.rs       # 【W0】Candidate 类型（完整，冻结）
    │   ├── src\config.rs          # 【W0】Config（完整，冻结）
    │   ├── src\key.rs             # 【W0】Key / Effect / PageInfo / SessionEnd（完整，冻结）
    │   ├── src\schema.rs          # 【Agent B】InputSchema + Quanpin
    │   ├── src\lm.rs              # 【Agent B】LmProvider + UnigramLm
    │   ├── src\viterbi.rs         # 【Agent B】unigram Viterbi
    │   ├── src\rerank.rs          # 【Agent B】RerankStage + RerankCtx + StaticOrder
    │   ├── src\store.rs           # 【Agent B】UserDataStore + NullStore
    │   ├── src\engine.rs          # 【Agent B】Engine（候选生成）
    │   ├── src\session.rs         # 【Agent B】Session 状态机
    │   └── tests\                 # 【Agent B】
    ├── ime-repl\
    │   ├── Cargo.toml             # 【W0】
    │   └── src\main.rs            # 【Agent C】
    └── ime-tsf\
        ├── Cargo.toml             # 【W0】
        ├── build.rs               # 【Agent D】winres 资源
        ├── src\lib.rs             # 【Agent D】COM 导出（DllGetClassObject 等）
        ├── src\registration.rs    # 【W0 常量 / Agent D 实现】GUID 常量 + 注册逻辑
        ├── src\log.rs             # 【Agent D】文件日志
        ├── src\com\mod.rs         # 【Agent D】
        ├── src\com\class_factory.rs   # 【Agent D】
        ├── src\com\text_service.rs    # 【Agent D】ITfTextInputProcessorEx 等
        ├── src\session_bridge.rs  # 【Agent D】Key 映射 + Effect 应用
        ├── src\composition.rs     # 【Agent D】composition 封装
        ├── src\ui\mod.rs          # 【W0】CandidateUi + UiSnapshot + 映射（完整，冻结）
        ├── src\ui\gdi.rs          # 【Agent E】GdiCandidateWindow
        └── examples\candwin_demo.rs   # 【Agent E】候选窗演示
```

## 2. 依赖（白名单，版本锁定）

### 2.1 根 `Cargo.toml`

```toml
[workspace]
resolver = "2"
members = ["crates/ime-data", "crates/ime-core", "crates/ime-repl", "crates/ime-tsf"]

[workspace.package]
edition = "2021"
rust-version = "1.85"
license = "MIT"

[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
windows = { version = "0.62", features = [
    "Win32_Foundation", "Win32_Graphics", "Win32_Graphics_Gdi",
    "Win32_System_Com", "Win32_System_LibraryLoader", "Win32_System_Ole",
    "Win32_System_SystemServices", "Win32_System_Threading", "Win32_System_Variant",
    "Win32_System_WindowsProgramming", "Win32_UI_Input",
    "Win32_UI_Input_KeyboardAndMouse", "Win32_UI_TextServices",
    "Win32_UI_WindowsAndMessaging",
] }
windows-core = "0.62"
windows-registry = "0.6"
serde_json = "1"
ime-data = { path = "crates/ime-data" }
ime-core = { path = "crates/ime-core" }

[profile.release]
lto = "fat"
codegen-units = 1
```

### 2.2 各 crate 依赖（除此之外禁止新增第三方依赖）

| crate | 依赖 |
|---|---|
| ime-data | `serde`（workspace） |
| ime-core | `serde`（workspace）、`serde_json`（workspace）、`ime-data`（workspace） |
| ime-repl | `ime-core`、`ime-data`（workspace） |
| ime-tsf | `ime-core`、`ime-data`、`windows`、`windows-core`、`windows-registry`（workspace）；build-dep：`winres = "0.1"` |

### 2.3 ime-tsf Cargo.toml 要点

```toml
[lib]
name = "input_ime_tsf"
crate-type = ["cdylib", "rlib"]   # rlib 供 examples/tests 链接
```

## 3. ime-data 公共 API

```rust
// ===== dict.rs（W0 完整实现，冻结）=====
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Entry {
    pub word: String,   // 词条文本，如 "你好"
    pub code: String,   // 音节分隔键（全小写，音节间以 ' 分隔）："ni'hao" / "xi'an"；单字词无分隔如 "xian"——与查询键同形（compile 时拼音空白转 '）
    pub weight: u32,    // 词库静态权重（缺失按 0）
}

#[derive(Clone, Debug, Default)]
pub struct Dict { /* BTreeMap<squashed_code, Vec<Entry>>，每组按 weight 降序 */ }

impl Dict {
    /// 测试/用户词库构造器。items = (squashed_code, word, weight)。
    /// 同码多条按 weight 降序归并；同 (code,word) 去重取最大 weight。
    pub fn from_entries(items: Vec<(String, String, u32)>) -> Dict;

    /// 精确查询：squashed_code 如 "nihao"。返回按 weight 降序切片。
    pub fn exact(&self, squashed_code: &str) -> &[Entry];

    /// 前缀补全：返回 squashed 以 prefix 开头（且不等于 prefix）的词条，
    /// 跨编码按 weight 降序，最多 limit 条。
    pub fn prefix(&self, squashed_prefix: &str, limit: usize) -> Vec<&Entry>;

    /// 全部音节集合（从所有 code 切出），供全拼切分器构造。
    pub fn syllables(&self) -> &BTreeSet<String>;

    pub fn total_weight(&self) -> u64;   // 全部词条 weight 之和（LM 分母）
    pub fn entry_count(&self) -> usize;  // 词条总数
    pub fn max_word_syllables(&self) -> usize; // 最长词的音节数（lattice 宽度上限）
}

// ===== lib.rs 顶部函数（Agent A 实现于 format.rs 后 re-export）=====
/// 加载二进制词典。
pub fn load(path: &std::path::Path) -> std::io::Result<Dict>;

// ===== compile.rs（Agent A）=====
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompileStats { pub files: usize, pub entries: usize, pub codes: usize, pub duplicates: usize }

/// 解析 rime .dict.yaml 文件列表，合并去重，写二进制到 output。
pub fn compile_files(inputs: &[std::path::PathBuf], output: &std::path::Path) -> std::io::Result<CompileStats>;
```

### 3.1 二进制词典格式（`input.imedic`）

```
[0..8]    magic = b"IMEDIC01"
[8..12]   u32 LE  record_count
记录×N:   u8 code_len | code（squashed，全小写 a-z，无空格）
          u16 LE word_utf8_len | word（UTF-8）
          u32 LE weight
记录按 (code 升序, weight 降序) 排列写入；加载时顺序建 BTreeMap。
```

## 4. ime-core 公共 API

```rust
// ===== candidate.rs（W0 完整，冻结）=====
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CandidateKind { Sentence, Word, Char }  // M3+ 可扩：English / Symbol…

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Candidate {
    pub text: String,
    pub kind: CandidateKind,
    pub code: String,   // squashed 编码（学习 key 用）；Sentence 为 seg 拼接
    pub weight: u32,    // 词典 weight；Sentence 恒 0
}

// ===== config.rs（W0 完整，冻结）=====
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Config {
    pub page_size: usize,        // 默认 5
    pub max_candidates: usize,   // 默认 200
    pub max_word_syllables: usize, // lattice 词宽上限，默认 7
}
impl Default for Config { /* page_size:5, max_candidates:200, max_word_syllables:7 */ }

// ===== key.rs（W0 完整，冻结）=====
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key { Char(char), Backspace, Space, Enter, Esc, Digit(u8), PageUp, PageDown, Up, Down }

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PageInfo { pub page: usize, pub page_count: usize, pub page_size: usize, pub total: usize }

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SessionEnd { Commit(String), Cancel }

/// 一次按键后的完整 UI 快照 + 副作用。TSF/REPL 只消费它，不读引擎内部。
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Effect {
    pub composition: String,         // 内嵌预编辑文本：拼音分段（如 "ce'shi"，保留用户按下的强制分隔符 `'`，与 reading 同值）——微软式：拼音留在预编辑，候选窗只放候选；commit 时由 end.text 替换上屏
    pub reading: String,             // 切分显示，如 "ni'hao"（保留用户 `'`，见 schema::display_with_sep）
    pub candidates: Vec<Candidate>,  // 当前页候选（页内索引 0 起）
    pub selected: usize,             // 页内高亮索引
    pub page: PageInfo,
    pub end: Option<SessionEnd>,     // Some → 会话结束（Commit 上屏 / Cancel 取消）
}

// ===== schema.rs（Agent B）=====
pub trait InputSchema: Send + Sync {   // Engine 进程级单例跨线程共享，全部件须 Send+Sync
/// 原始字母串 → 全部可能切分方案（每方案 = 音节序列）。全拼：
/// `'` 为强制分隔（硬边界，空段保留供 display）；各段内部递归枚举全部合法音节切分
/// （有合法音节前缀时不兜底单字母，无则单字母兜底保证永不失败）。
/// 方案按贪心优先排序（方案[0] = 贪心/强制切分，供 viterbi 整句与 display）。
/// 例：`"xian"` → `[[xian], [xi,an]]`；`"xi'an"` → `[[xi,an]]`；`"qaz"` → `[[q,a,z]]`。
fn segment(&self, raw: &str) -> Vec<Vec<String>>;
/// 单个方案 → 显示串：以 ' 连接（空段保留：`["x",""]` → `"x'"`）。
fn display(&self, seg: &[String]) -> String;
}
pub struct Quanpin { /* BTreeSet<String> 合法音节 */ }
impl Quanpin { pub fn new(syllables: std::collections::BTreeSet<String>) -> Self }

// ===== lm.rs（Agent B）=====
pub trait LmProvider: Send + Sync {
    /// prev = 前一个词（整句上下文）。MVP unigram 实现忽略 prev —— 这是 n-gram 槽位，签名不得改。
    fn log_prob(&self, prev: Option<&str>, word: &str, weight: u32) -> f64;
}
pub struct UnigramLm { /* total, entry_count */ }
impl UnigramLm { pub fn new(total_weight: u64, entry_count: usize) -> Self }
// 公式：ln(weight+1) - ln(total_weight)。OOV（词典查不到）由 viterbi 加 OOV_PENALTY = -10.0。

// ===== rerank.rs（Agent B）=====
pub struct RerankCtx<'a> {
    pub raw: &'a str,
    pub seg: &'a [String],
    pub store: &'a dyn UserDataStore,
    pub config: &'a Config,
    pub now: std::time::SystemTime,
}
pub trait RerankStage: Send + Sync {
    fn rerank(&self, ctx: &RerankCtx, cands: &mut Vec<Candidate>);
}
/// 静态序：候选生成顺序即展示顺序（no-op）。M2 的滞回/钉选实现为新增 Stage。
pub struct StaticOrder;
impl RerankStage for StaticOrder { /* 什么都不做 */ }

// ===== store.rs（Agent B）=====
pub trait UserDataStore: Send {
    fn record_selection(&mut self, code: &str, text: &str, now: std::time::SystemTime);
    /// M2 滞回模型用（有效使用强度）；MVP NullStore 恒返回 0.0。
    fn power(&self, code: &str, text: &str, now: std::time::SystemTime) -> f32;
    fn flush(&mut self) {}  // 持久化钩子，MVP 空实现
}
pub struct NullStore;  // 全空实现

// ===== engine.rs（Agent B）=====
pub struct Engine { /* dict, schema, lm, stages, store: Mutex<Box<dyn UserDataStore>>, config */ }
impl Engine {
    /// 默认装配：Quanpin + UnigramLm + [StaticOrder] + NullStore。
    pub fn new(dict: ime_data::Dict, config: Config) -> std::sync::Arc<Engine>;
    /// 全注入构造器（测试与后续里程碑用）。
    pub fn with_parts(
        dict: ime_data::Dict,
        config: Config,
        schema: Box<dyn InputSchema>,
        lm: Box<dyn LmProvider>,
        stages: Vec<Box<dyn RerankStage>>,
        store: Box<dyn UserDataStore>,
    ) -> std::sync::Arc<Engine>;
    pub fn start_session(self: &std::sync::Arc<Self>) -> Session;
    pub fn config(&self) -> &Config;
    /// 调试/REPL 用精确查询。
    pub fn lookup(&self, squashed_code: &str) -> &[ime_data::Entry];
}

// ===== session.rs（Agent B）=====
pub struct Session { /* engine: Arc<Engine>, raw, seg, all: Vec<Candidate>, page, selected */ }
impl Session {
    pub fn on_key(&mut self, key: Key) -> Effect;
    /// 不交按键取当前快照（REPL/测试用）。
    pub fn effect(&self) -> Effect;
    /// 有未提交的原始输入（TSF 据此决定按键是否放行给应用）。
    pub fn is_active(&self) -> bool;
}
```

### 4.1 Session 按键行为契约（Agent B 实现 + 测试；REPL/TSF 依赖此可观察行为）

| 输入 | 行为 |
|---|---|
| `Char('a'..='z' \| '\'')` | 追加 raw → 重切分 → 重新生成候选 → page=0, selected=0。无候选也保持 active |
| `Backspace` | 删 raw 尾字符；删后 raw 为空 → `end = Some(Cancel)`；否则重算候选 |
| `Space` | 有候选 → commit 当前页 selected 项；无候选 → `Commit(raw)`（拼音原文） |
| `Digit(n)` n∈1..=9 | 全表索引 = page×page_size + n−1，存在则 commit 该项；不存在则无操作（仍消费） |
| `Enter` | `Commit(raw)` 原文上屏 |
| `Esc` | `end = Some(Cancel)` |
| `PageUp/PageDown` | page ±1，clamp 到 `[0, page_count−1]`，selected 归 0 |
| `Up/Down` | selected ±1，clamp 到 `[0, 当前页候选数−1]` |
| commit 发生时 | 调 `store.record_selection(code_key, text, now)`：Word/Char 用候选自身 `code`，Sentence 用 `seg.join("'")`；随后 `end = Some(Commit(text))` |

会话结束后该 Session 不再使用（TSF/REPL 丢弃重建）。

### 4.2 候选生成算法契约（`Engine` 内部流程，B 实现；顺序即静态展示序）

设 `seg` = 方案[0]（贪心/强制切分），`n` = seg 段数。

**砍尾巴逐级前缀匹配**：`for k = n, n-1, ..., 1`，对前缀 `seg[0..k]`：

1. `k >= 2` → unigram Viterbi 最优路径（每级 0 或 1 条）→ `Candidate{ kind: Sentence, text: 路径拼接, code: seg.join("'"), weight: 0 }`；
   空段（尾/连续 `'`）过滤后组句。
2. 前缀 `join("'")` 再 `schema.segment` 枚举切分 → 各方案 `join("'")` 键 `dict.exact`：
   词长 ≥2 → `Word`，单字 → `Char`；同 k 内按 weight 降序，去重取前 20。
3. 全部候选按 k **从长到短**排列（长句优先）；同 k 内 Sentence 在前、词按权重。
4. 前缀补全（联想，默认关）：`dict.prefix(seg.join("'"), 20)`（词库键已分隔化）。
5. 按 text 去重（保序，先见先留；跨级同文本只留首个）。
6. 截断到 `config.max_candidates`。
7. 依次过 `stages` 管线（MVP 仅 StaticOrder = no-op）。

例：`chuangqianmingyueguang` → 床前明月光（整句）→ 窗前明月（次长句）→ … → 床前/窗前（词）→ 床/窗/创（单字），翻页总能到达所需层级；
`zheshi` → 这是（整句）→ 这时/这事/…（词）→ 这/是（单字）。

Viterbi 要点：位置 0..=n（音节界）；边 (i,j)（`j−i ≤ max_word_syllables`）= `dict.exact(seg[i..j].join("'"))`
的全部词条；边分 = `lm.log_prob(prev_word, word, weight)`；单音节无词条时给兜底边
（text = 该音节原样，分 = `UnigramLm::log_prob(None, s, 0) + OOV_PENALTY`），保证路径恒存在。

## 5. ime-tsf 内部接缝（W0 完整实现 `ui/mod.rs`，冻结）

```rust
// ===== ui/mod.rs（W0 完整实现）=====
use ime_core::{Effect, PageInfo};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaretRect { pub x: i32, pub y: i32, pub w: i32, pub h: i32 }

#[derive(Clone, Debug, Default, PartialEq)]
pub struct UiSnapshot {
    pub reading: String,          // "ni'hao"
    pub candidates: Vec<String>,  // 页内候选文本
    pub selected: usize,
    pub page: PageInfo,
}

/// Effect → UiSnapshot（W0 实现完整：取 effect.reading / 页内候选 text / selected / page）。
pub fn effect_to_snapshot(e: &Effect) -> UiSnapshot { /* ... */ }

/// 候选窗抽象。MVP 实现 = GdiCandidateWindow（ui/gdi.rs，Agent E）；
/// M4 增加 RemoteCandidateWindow（IPC 转发 Tauri helper），COM 层零改动。
pub trait CandidateUi {
    fn show(&mut self, snap: &UiSnapshot, caret: CaretRect);
    fn update(&mut self, snap: &UiSnapshot);
    fn move_to(&mut self, caret: CaretRect);
    fn hide(&mut self);
    fn is_visible(&self) -> bool;
}

/// 空实现桩（W0 提供）：Agent D 在 Agent E 完成前用它联调管线。
pub struct NullCandidateUi;  // 各方法均为 no-op，is_visible 恒 false
```

### 5.1 注册常量（registration.rs，W0 写入，冻结）

```rust
pub const CLSID_TEXT_SERVICE: &str = "{C69735F1-BAB1-458B-89FC-099ABA877ECB}";
pub const PROFILE_GUID: &str     = "{799E00DD-64C2-4280-AC48-D379A9ABC5BE}";
pub const DISPLAY_ATTR_GUID: &str= "{4953F50B-CD5E-4AAF-BA0D-9F137CC7BC11}"; // M2+ 备用
pub const LANGID_ZH_CN: u16 = 0x0804;
pub const PROFILE_DESCRIPTION: &str = "Input IME";
pub const DICT_FILENAME: &str = "input.imedic"; // 位于 %LOCALAPPDATA%\InputIME\
```

## 6. 文件属主矩阵（W1 并行防冲突）

| 路径 | 属主 | 状态 |
|---|---|---|
| 根 `Cargo.toml` / `.gitignore` / `AGENTS.md` / `docs/**` | 主智能体 | W0 冻结 |
| `ime-data/src/lib.rs`、`dict.rs` | 主智能体 | W0 **完整实现**，冻结 |
| `ime-data/src/format.rs`、`compile.rs`、`bin/dictc.rs`、`tests/**`、`scripts/download-dict.ps1` | **Agent A** | W1 |
| `ime-core/src/{candidate,config,key}.rs`、`lib.rs` | 主智能体 | W0 完整，冻结 |
| `ime-core/src/{schema,lm,viterbi,rerank,store,engine,session}.rs`、`tests/**` | **Agent B** | W1 |
| `crates/ime-repl/**` | **Agent C** | W1 |
| `ime-tsf/src/ui/mod.rs` | 主智能体 | W0 **完整实现**，冻结 |
| `ime-tsf/src/ui/gdi.rs`、`examples/candwin_demo.rs` | **Agent E** | W1 |
| `ime-tsf/src/{lib,registration,log,session_bridge,composition}.rs`、`com/**`、`build.rs`、`scripts/{register,unregister}.ps1`、`Cargo.toml` 内 winres 配置 | **Agent D** | W1 |

规则：只允许改自己属主的文件；发现契约缺陷 → 报告主智能体，禁止私改他人文件。

## 7. 行为集成约定

- TSF 侧每次按键只做三件事（session_bridge）：`vk/char → Key` 映射；`Session::on_key`；应用 Effect
  （更新 composition 文本 → `effect_to_snapshot` → `CandidateUi`；`end` 则上屏/取消并 hide）。
- composition 内嵌文本 = `Effect.composition`（拼音分段）；候选窗只放候选列表（不渲染 reading，微软式，M1 后期修正）。
- `Session::is_active() == false` 时字母键被消费（开启新会话），其余键全部放行。
- 引擎在 DLL 内进程级单例（`OnceLock<Arc<Engine>>`），词典路径 `%LOCALAPPDATA%\InputIME\input.imedic`（用户级数据；DLL 本体在 `%ProgramFiles%\InputIME\`）；
  加载失败：日志记录，所有字母键原样放行（输入法"透明"，绝不卡用户）。
