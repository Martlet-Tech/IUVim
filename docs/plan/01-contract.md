# 01 · 共享契约（唯一权威接口源）

> 本文档定义 M1 全部**跨模块**类型、trait、行为契约与文件属主。
> W0 由主智能体按本文档建骨架并冻结；W1 各子智能体只实现、不修改本文档定义的签名。
> 任何签名变更必须回到本文档修改，并由主智能体同步所有受影响模块。

## 1. 工作区结构（W0 产出）

```
D:\Projects\vaim\
├── Cargo.toml                     # workspace 根（见 §2.1）
├── .gitignore                     # /target, /data
├── AGENTS.md
├── docs\plan\                     # 本方案书
├── data\                          # 下载的词库源文件 + 编译产物（gitignore）
├── scripts\
│   ├── download-dict.ps1          # 【Agent A】下载白霜词库
│   ├── register.ps1               # 【Agent D】注册输入法（管理员）
│   └── unregister.ps1             # 【Agent D】注销
└── crates\                        # 跨平台层（引擎线，纯 Rust）
    ├── iuv-data\
    │   ├── Cargo.toml             # 【W0】
    │   ├── src\lib.rs             # 【W0】模块声明 + re-export
    │   ├── src\dict.rs            # 【W0】Dict 查询层（完整实现，冻结）
    │   ├── src\format.rs          # 【Agent A】二进制格式读写
    │   ├── src\compile.rs         # 【Agent A】rime yaml → 记录集
    │   ├── src\bin\dictc.rs       # 【Agent A】编译 CLI
    │   └── tests\                 # 【Agent A】
    ├── iuv-core\
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
    ├── iuv-repl\
    │   ├── Cargo.toml             # 【W0】
    │   └── src\main.rs            # 【Agent C】
    └── platforms\                 # 平台层（每平台一套：系统适配 + 门面）
        └── windows\
            ├── iuv-tsf\           # TSF 管线 + GDI 候选窗（见下）
            │   ├── Cargo.toml             # 【W0】
            │   ├── build.rs               # 【Agent D】winres 资源
            │   ├── src\lib.rs             # 【Agent D】COM 导出（DllGetClassObject 等）
            │   ├── src\registration.rs    # 【W0 常量 / Agent D 实现】GUID 常量 + 注册逻辑
            │   ├── src\log.rs             # 【Agent D】文件日志
            │   ├── src\com\mod.rs         # 【Agent D】
            │   ├── src\com\class_factory.rs   # 【Agent D】
            │   ├── src\com\text_service.rs    # 【Agent D】ITfTextInputProcessorEx 等
            │   ├── src\session_bridge.rs  # 【Agent D】Key 映射 + Effect 应用
            │   ├── src\composition.rs     # 【Agent D】composition 封装
            │   ├── src\ui\mod.rs          # 【W0】CandidateUi + UiSnapshot + 映射（完整，冻结）
            │   ├── src\ui\candwin.rs      # 【Agent E】CandwinCandidateWindow（M4：ULW + iuv-ui 渲染）
            │   └── examples\candwin_demo.rs   # 【Agent E】候选窗演示
            └── README.md                 # 门面现状 + M4~M6 规划（跨平台渲染/托盘/守护进程）
```

> 分层约定：`crates/` = 跨平台（引擎/词库/CLI，纯 Rust）；`platforms/` = 每平台一套
> （系统适配层 + 门面）。macOS（IMK）、Linux（Fcitx5/IBus）为占位目录（README），
> 真做时新建 crate 并加入 workspace members。跨平台分层见 `00-overview.md` §2 与
> `platforms/*/README.md`。

## 2. 依赖（白名单，版本锁定）

### 2.1 根 `Cargo.toml`

```toml
[workspace]
resolver = "2"
members = ["crates/iuv-data", "crates/iuv-core", "crates/iuv-ui", "crates/iuv-repl", "platforms/windows/iuv-tsf", "platforms/windows/iuv-daemon"]

[workspace.package]
edition = "2021"
rust-version = "1.89"
license = "MIT"

[workspace.dependencies]
serde = { version = "1", features = ["derive"] }
windows = { version = "0.62", features = [
    "Win32_Foundation", "Win32_Graphics", "Win32_Graphics_Gdi",
    "Win32_System_Com", "Win32_System_LibraryLoader", "Win32_System_Memory",
    "Win32_System_Ole", "Win32_System_Pipes", "Win32_System_Threading",
    "Win32_System_Variant", "Win32_Storage_FileSystem", "Win32_System_IO",
    "Win32_Security", "Win32_UI_Input_KeyboardAndMouse", "Win32_UI_TextServices",
    "Win32_UI_WindowsAndMessaging",
] }
windows-core = "0.62"
windows-registry = "0.6"
serde_json = "1"
tiny-skia = "0.12"
cosmic-text = "0.19"
iuv-data = { path = "crates/iuv-data" }
iuv-core = { path = "crates/iuv-core" }
iuv-ui = { path = "crates/iuv-ui" }

[profile.release]
lto = "fat"
codegen-units = 1
```

### 2.2 各 crate 依赖（除此之外禁止新增第三方依赖）

| crate | 依赖 |
|---|---|
| iuv-data | `serde`（workspace） |
| iuv-core | `serde`（workspace）、`serde_json`（workspace）、`iuv-data`（workspace） |
| iuv-ui | `tiny-skia`、`cosmic-text`（workspace）；`iuv-core`（workspace，Theme 消费） |
| iuv-repl | `iuv-core`、`iuv-data`（workspace） |
| iuv-tsf | `iuv-core`、`iuv-data`、`iuv-ui`、`windows`、`windows-core`、`windows-registry`（workspace）；build-dep：`winres = "0.1"` |
| iuv-daemon（M6 已实现） | `iuv-data`、`iuv-ui`（workspace）、`windows`、`windows-core`、`serde`、`serde_json`（workspace）、`eframe = "0.36"`（egui 经其重导出；rust-version 单独 1.95） |

### 2.3 iuv-tsf Cargo.toml 要点

```toml
[lib]
name = "iuv_tsf"
crate-type = ["cdylib", "rlib"]   # rlib 供 examples/tests 链接
```

## 3. iuv-data 公共 API

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
pub struct Dict { /* IMEDIC02 mmap 平面视图 + 段偏移（零重建加载）；查询物化 Entry 拷贝。
                      initial_buckets 固化为段2（26 桶，每桶词频降序 top-1000 单字，M1.5） */ }

impl Dict {
    /// 测试/用户词库构造器。items = (squashed_code, word, weight)。
    /// 同码多条按 weight 降序归并；同 (code,word) 去重取最大 weight。
    /// 实现 = 归并 → 序列化 IMEDIC02 → 与文件加载同一条解析路径。
    pub fn from_entries(items: Vec<(String, String, u32)>) -> Dict;

    /// 精确查询：squashed_code 如 "nihao"。返回按 weight 降序的物化词条
    /// （mmap 数据无法零拷贝借用，查询即拷贝；量级微秒，M1.6 实测通过）。
    pub fn exact(&self, squashed_code: &str) -> Vec<Entry>;

    /// 精确查询（单字视图，M1.5 单段档）：返回 code == squashed_code 的单字词条。
    /// 单段档数据契约：只出单字——多字词键（异常数据）在此过滤，引擎侧无需防御。
    pub fn exact_single(&self, squashed_code: &str) -> Vec<Entry>;

    /// 前缀补全：返回 squashed 以 prefix 开头（且不等于 prefix）的词条，
    /// 跨编码按 weight 降序，最多 limit 条。
    pub fn prefix(&self, squashed_prefix: &str, limit: usize) -> Vec<Entry>;

    /// 首字母桶查询（M1.5）：返回 code 以 `initial` 开头的词条，按词频降序，
    /// 最多 limit 条。桶在编译期固化（每字母 top-1000 单字），供单段档（`c`/`sh`/`shi`…）
    /// O(1) 取候选，替代全表前缀扫描排序（'s' 全扫 10 万条不可用于按键热路径）。
    pub fn initial_top(&self, initial: char, limit: usize) -> Vec<Entry>;

    /// 全部音节集合（编译期固化在元数据段，加载物化），供全拼切分器构造。
    pub fn syllables(&self) -> &BTreeSet<String>;

    pub fn total_weight(&self) -> u64;   // 全部词条 weight 之和（LM 分母）
    pub fn entry_count(&self) -> usize;  // 词条总数
    pub fn max_word_syllables(&self) -> usize; // 最长词的音节数（lattice 宽度上限）
}

// ===== 简拼键（M1.5）=====
/// dictc 对 ≥2 音节词额外生成简拼键（每音节首字母串联：`ni'hao`→`nh`、
/// `xi'an`→`xa`、`tian'an'men`→`tam`），与全拼键同表混存，权重复制。
/// 键空间隔离靠查询路由（§4.2）：全拼查询的键要么是完整音节、要么含 `'`，
/// 简拼键不含 `'` 且非完整音节，只在多段简拼输入时被查询，互不命中。
pub const INITIAL_BUCKET_SIZE: usize = 1000; // 每首字母桶上限（可调，实现为准）

// ===== lib.rs 顶部函数（Agent A 实现于 format.rs 后 re-export）=====
/// 加载二进制词典。
pub fn load(path: &std::path::Path) -> std::io::Result<Dict>;

// ===== compile.rs（Agent A）=====
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompileStats { pub files: usize, pub entries: usize, pub codes: usize, pub duplicates: usize }

/// 解析 rime .dict.yaml 文件列表，合并去重，写二进制到 output。
pub fn compile_files(inputs: &[std::path::PathBuf], output: &std::path::Path) -> std::io::Result<CompileStats>;
```

### 3.1 二进制词典格式（`iuv.imedic`，IMEDIC02）

段表驱动平面格式（详情见 `17-imedic02-mmap.md`）。加载 = mmap + 段定位 + 边界校验扫描，
零重建（冷启动 2.1s → ~70ms 实测）。排序不变量（code 升序、组内 weight 降序）由写端保证。

```
[0..8]   magic = b"IMEDIC02"
[8..12]  u32 LE 段数 N
[12..]   段表：N × { u8 段类型 | u32 偏移 | u32 长度 }
段1 元数据:  u64 total_weight | u32 entry_count | u32 max_word_syllables
            | u32 音节数 | 音节 × { u8 len | bytes（UTF-8） }
段2 首字母桶: 26 × { u8 字母 | u32 记录数 | 记录 × N }（单字，weight 降序，≤1000/桶）
段3 记录索引: record_count × u32 记录体段内偏移（按 code 升序）
段4 记录体:   record_count × { u8 code_len | code | u16 word_len | word | u32 weight }
记录: code 为 squashed 键（全小写 a-z，音节间 ' 分隔）；记录按 (code 升序, weight 降序) 排列。
未知段类型 → 加载器忽略（未来屏蔽段/用户段前向兼容）；IMEDIC01 旧格式不再支持读。
```

### 3.2 用户权重覆盖表（`iuv.user.imedic`，IUVUSR02，M2 主动调权 + 自造词/隐藏）

**绝对值覆盖**（无 delta 魔法数字，反复调整收敛）：用户 Shift+←/→ 与相邻候选交换权重
= 双方互写对方**合成权重**（覆盖值优先、否则基本库权重）。**自造词**（逐字选择 commit）
与覆盖统一存段1（来源不区分，权重按场景 0/a/b 判定，见 18-m2-user-dict.md §3.5）。
**隐藏**（Shift+Delete）＝先删用户库条目，否则写屏蔽段（基础库词条隐藏，含 viterbi
整句拦截）。文件为简单线性格式（小文件，不 mmap 零拷贝），**基本库物理不动**
（mmap 只读共享、全系统一份页缓存），查询时叠加（见 §4.2 权重叠加）：

```
[0..8]   magic = b"IUVUSR02"（01 = 旧格式仅覆盖表，读兼容）
[8..12]  u32 覆盖条数
每条:    u8 code_len | code | u16 word_len | word | u32 adjusted
         u32 屏蔽条数
每条:    u8 code_len | code | u16 word_len | word
```

```rust
// ===== userdict.rs（M2 新增）=====
#[derive(Clone, Debug, Default)]
pub struct UserDict;  // 覆盖表 code → [(word, adj)] + 屏蔽表 (code, word) 集合

impl UserDict {
    pub fn empty() -> UserDict;
    pub fn load(path: &std::path::Path) -> std::io::Result<UserDict>; // 缺失/损坏 → Err（调用方降级空库）
    pub fn adjusted(&self, code: &str) -> &[(String, u32)];
    /// 双 code 交换（候选页内相邻词可跨 code：单段档桶候选 sha/shi…同属 `sh`）。
    /// 返回新实例（写时复制，Arc 共享替换）。
    pub fn apply_swap(&self, a_code: &str, a_word: &str, a_adj: u32,
                      b_code: &str, b_word: &str, b_adj: u32) -> UserDict;
    pub fn set_entry(&self, code: &str, word: &str, adj: u32) -> UserDict;   // 自造词/覆盖 upsert
    pub fn remove_entry(&self, code: &str, word: &str) -> UserDict;          // 隐藏自造词 = 删除条目
    pub fn block(&self, code: &str, word: &str) -> UserDict;                 // 屏蔽基础库词条（幂等）
    pub fn is_blocked(&self, code: &str, word: &str) -> bool;
    pub fn save(&self, path: &std::path::Path) -> std::io::Result<()>; // 原子：临时文件 + 先删后 rename，恒写 02
}

// ===== Dict 新增（M2）=====
impl Dict {
    pub fn set_user(&self, user: std::sync::Arc<UserDict>);  // 装配（Arc 写时复制替换）
    pub fn user(&self) -> Option<std::sync::Arc<UserDict>>;
    pub fn effective_weight(&self, code: &str, word: &str) -> Option<u32>; // 覆盖（含自造）优先，否则 base；均无 → None
}
```

## 4. iuv-core 公共 API

```rust
// ===== candidate.rs（W0 完整，冻结；M1 后期契约演进：+seg_len）=====
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CandidateKind { Sentence, Word, Char }  // M3+ 可扩：English / Symbol…

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Candidate {
    pub text: String,
    pub kind: CandidateKind,
    pub code: String,   // squashed 编码（学习 key 用）；Sentence 为 seg 拼接
    pub weight: u32,    // 词典 weight；Sentence 恒 0
    pub seg_len: usize, // 该候选消费的音节段数（所在前缀级 k；续接选词推进用）
}

// ===== config.rs（W0 完整；M1.5/M2 演进：+keymap/+candidate_prefix/+candidate_orientation/+passthrough_apps）=====
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Config {
    pub page_size: usize,        // 默认 5
    pub max_candidates: usize,   // 默认 1024（单字全量可达，微软对齐；可配小值限制）
    pub max_word_syllables: usize, // lattice 词宽上限，默认 7
    pub keymap: Keymap,          // 翻页/候选移动四组语义键（M1.5，8f479f9/d1dcfb8）
    pub candidate_prefix: bool,  // 前缀联想开关，默认 false（候选仅 exact，微软化）
    pub candidate_orientation: Orientation, // 候选窗布局方向，默认 Vertical
    pub passthrough_apps: Vec<String>, // 按键直通白名单（exe 名，大小写不敏感精确匹配，TSF 层消费）
}
impl Default for Config { /* page_size:5, max_candidates:1024, max_word_syllables:7, keymap:默认表, candidate_prefix:false, candidate_orientation:Vertical, passthrough_apps:空 */ }

// ===== key.rs（W0 完整；M1.5/M2 演进：+ShiftChar/+Left/Right/+SwapLeft/SwapRight/+HideCandidate）=====
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),              // 小写字母/撇号/标点
    ShiftChar(char),         // Shift/CapsLock 字母（大写保形进序列，144f75b）
    Backspace, Space, Enter, Esc,
    Digit(u8),
    PageUp, PageDown, Up, Down,
    Left, Right,             // 页内候选移动（M1.5，2cc189b）
    SwapLeft, SwapRight,     // Shift+←/→ 主动调权（M2，2058399）
    HideCandidate,           // Shift+Delete 隐藏（M2 二期，6d29b45）
}

// ===== keymap.rs（M1.5 新增，8f479f9）=====
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Keymap {
    pub page_prev: Vec<Key>,    // 默认 PageUp/,/↑
    pub page_next: Vec<Key>,    // 默认 PageDown/./↓
    pub candidate_prev: Vec<Key>, // 默认 ←
    pub candidate_next: Vec<Key>, // 默认 →
}
/// keymap 命中 → 归一化 PageUp/PageDown/Left/Right；未命中 → None。
pub fn apply_keymap(key: Key, km: &Keymap) -> Key;

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PageInfo { pub page: usize, pub page_count: usize, pub page_size: usize, pub total: usize }

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SessionEnd { Commit(String), Cancel }

/// 一次按键后的完整 UI 快照 + 副作用。TSF/REPL 只消费它，不读引擎内部。
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Effect {
    pub composition: String,         // 内嵌预编辑文本：混合显示——已选词汉字 + 未选部分拼音分段（如选"床前"后 "床前ming'yue'guang"）；候选窗只放候选；commit 时由 end.text 全量替换上屏
    pub reading: String,             // 与 composition 同值（候选窗备用）
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
/// 输入层（session）不允许连续 `'`（已处于分隔尾态时忽略），空段仅来自尾撇号。
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
/// 静态序：候选生成顺序即展示顺序（no-op）。M2 排序决定权交还用户（主动调权/自造词/隐藏，
/// 见 18-m2-user-dict.md；滞回/钉选方案已废弃）。
pub struct StaticOrder;
impl RerankStage for StaticOrder { /* 什么都不做 */ }

// ===== store.rs（Agent B）=====
pub trait UserDataStore: Send {
    fn record_selection(&mut self, code: &str, text: &str, now: std::time::SystemTime);
    /// M2 用户数据 hook；MVP NullStore 恒返回 0.0（主动调权由 UserDict 覆盖表承载，见 §3.2）。
    fn power(&self, code: &str, text: &str, now: std::time::SystemTime) -> f32;
    fn flush(&mut self) {}  // 持久化钩子，MVP 空实现
}
pub struct NullStore;  // 全空实现

// ===== engine.rs（Agent B）=====
pub struct Engine { /* dict, schema, lm, stages, store: Mutex<Box<dyn UserDataStore>>, config */ }
impl Engine {
    /// 默认装配：Quanpin + UnigramLm + [StaticOrder] + NullStore。
    pub fn new(dict: iuv_data::Dict, config: Config) -> std::sync::Arc<Engine>;
    /// 全注入构造器（测试与后续里程碑用）。
    pub fn with_parts(
        dict: iuv_data::Dict,
        config: Config,
        schema: Box<dyn InputSchema>,
        lm: Box<dyn LmProvider>,
        stages: Vec<Box<dyn RerankStage>>,
        store: Box<dyn UserDataStore>,
    ) -> std::sync::Arc<Engine>;
    pub fn start_session(self: &std::sync::Arc<Self>) -> Session;
    pub fn config(&self) -> &Config;
    /// 调试/REPL 用精确查询。
    pub fn lookup(&self, squashed_code: &str) -> &[iuv_data::Entry];
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
| `ShiftChar('A'..='Z')`（Shift/CapsLock 字母） | **大写保形进序列**：`raw` 原样追加大写（不转小写）→ 重切分 → 重新生成候选。匹配只认小写——大写字符不被音节表命中（按不可匹配字符单字母段兜底，`niHAO` → `ni`/`H`/`A`/`O`，候选仍从 `ni` 前缀出），commit 时 `raw` 原样上屏。**也是开会话键**（无会话时 `Hello` 的 `H` 同样进序列）。字母大小写 = Shift 与 CapsLock 的 XOR（恰好一个生效 → 大写；CapsLock+Shift 反转 → 小写）。**CapsLock 生效时 TSF 层例外**：会话外字母放行直通（Caps = 英文模式，不建会话；会话内 Caps 字母照常进序列），见 `session_bridge::caps_passthrough` |
| `Backspace` | 删 raw 尾字符；删后 raw 为空 → `end = Some(Cancel)`；否则重算候选 |
| `Space` | 有候选 → commit 当前页 selected 项；无候选 → `Commit(picked.join + raw)`（全部上屏） |
| `Digit(n)` n∈1..=9 | 全表索引 = page×page_size + n−1，存在则 commit 该项；不存在则无操作（仍消费） |
| `Enter` | `Commit(picked.join + raw)` 原文上屏 |
| `Esc` | **悬空**：有 picked → `end = Some(Commit(picked.join("")))`（已选词上屏，尾巴随之取消）；无 picked → `end = Some(Cancel)` |
| `PageUp/PageDown` | page ±1，clamp 到 `[0, page_count−1]`，selected 归 0 |
| `Up/Down` | selected ±1，clamp 到 `[0, 当前页候选数−1]` |
| `SwapLeft/SwapRight`（Shift+←/→，**M2 主动调权**） | 页内高亮候选与相邻候选**交换权重**（绝对值覆盖，互写对方合成权重）→ 立即重排候选 + **高亮跟随被调词**；会话不结束（松手后可继续导航/上屏）。边界（1 号位左 / 末位右）→ 消费但忽略。写用户库（§3.2）失败不阻断（内存态已生效）。**选 Shift 而非 Alt/Ctrl**：Alt 组合 = `WM_SYSKEYDOWN` 不经过 TSF 键 sink（机制死路）；Ctrl 冲突大（应用词跳转 + 「Ctrl 一律放行」红线）；Shift+方向键无大小写语义（见 18-m2-user-dict.md 附录） |
| `HideCandidate`（Shift+Delete，**M2 隐藏**） | 高亮候选 → 先删用户库条目（自造词/覆盖 = 撤销自造），否则**屏蔽基础库词条**（§3.2 段2）→ 立即重排 + 高亮落在原位置附近。**viterbi 整句同样拦截**（被屏蔽组合不再组出）。裸 Delete 放行给应用 |
| commit 发生时 | 调 `store.record_selection(code_key, text, now)`：Word/Char 用候选自身 `code`，Sentence 用 `seg[..consumed].join("'")`（其覆盖的前缀段） |
| **选词（续接）** | 候选消费 `seg_len` 段：`seg_len >= 当前段数` → `end = Commit(picked.join + 词)` 会话结束；`seg_len < 当前段数` → **悬空**：`picked.push((词, code_key))`、`raw = seg[seg_len..].join("'")` 重算候选（会话继续，**不产生任何 commit 信号**；已选词仅通过 composition 混合显示反馈，见 Effect.composition） |
| **Backspace（有 picked）** | pop picked 栈顶，其 code 拼回 raw 头部（`code + "'" + raw`，raw 空则直接 code），重算候选——取消一次已选，而非删拼音 |
| **Backspace（无 picked）** | 删 raw 尾字符；删后 raw 为空 → `end = Some(Cancel)`；否则重算候选 |

会话结束后该 Session 不再使用（TSF/REPL 丢弃重建）。

### 4.2 候选生成算法契约（`Engine` 内部流程，B 实现；顺序即静态展示序）

设 `seg` = 方案[0]，`n` = seg 段数。**方案[0] 语义（2026-08-14 修正）**：切分器输出
仍按贪心优先（方案[0] = 贪心/强制），但**消费端 `Engine::rank_plans` 按词频重排**
（方案 join 键 exact 词条最大权重降序，稳定保贪心原序）——分节显示/主路径跟随
用户最可能打的词（`keneng` → `ke'neng` 可能；`dier` → `di'er` 第二，而非贪心的
`ken'eng`/`die'r`）。切分函数零改动（全部方案被消费端使用）。

**权重叠加（M2 主动调权）**：所有候选的展示权重 = 基本库 weight 经用户覆盖表
（§3.2，`iuv.user.imedic` 绝对值覆盖）替换后的**合成权重**；合成后稳定排序
（同值保持基本库原序）。合并下沉查询层（`Dict::merged`），引擎算法零改动；
跨进程生效策略：其他进程**新会话创建时** mtime 检测重载（微秒级），本进程内存态即时。

**档位路由（一等概念）**：输入经 `Engine::classify`（唯一判定点）归入
`Route` 枚举（PrefixChars / CompleteChars / AmbiguousSyllable / FullPinyin /
Abbrev / Mixed / Empty），`generate_candidates` 按档位 match 分派；
后续加档（M3 模糊音等）= 加 Route 臂，不在分派函数里叠 if。

**切分规则（M1.5 修正，微软对齐）**：段内无完整音节匹配时按**最长音节前缀**兜底
（`sh` 是 sha/shan/shi… 的前缀 → 整体一段，而非 `s'h` 两段；`zho`/`zhon` 同理）；
无任何音节前缀时才单字母兜底（`qaz`/`v` 等，保证有解）。`nh` 非任何音节前缀 → 仍拆
`n`/`h` 两段（简拼档）。

**路由（M1.5，微软实测对齐，见 docs/research/msime-probe-checklist.txt）**：

| 输入 | 判定 | 候选 |
|---|---|---|
| 整串为音节前缀（`c`/`sh`/`zho`） | `plain`（去 `'`）是某音节真前缀 | **纯单字**：完整音节 → `dict.exact` 全量同音字；严格前缀 → `initial_top(首字母)` 单字桶过滤 `starts_with`。**全量返回不截断**（微软对齐：sh 候选 600+ 全给翻页可达），由全局 `max_candidates` 兜底 |
| 完整单音节无歧义（`shi`/`de`/`ba`） | 单段且无替代切分 | 同上（纯单字，exact 全量） |
| 完整单音节有歧义（`xian`→[xian]+[xi,an]） | 单段且有替代切分 | 全拼 k-loop（替代切分词如"西安"混排，词频序） |
| 单段非前缀（`i`/`u`/`v`） | 非音节、非前缀 | **兜底原文候选**（见下"兜底规则"） |
| 多段全完整（`nihao`/`xi'an`） | 每段为完整音节 | **全拼两通道**（唯一整句 + 砍尾巴 exact，见下） |
| 末音节可补全（`shigechengy`） | 除末段外全为完整音节 + 末段为音节前缀 | **全拼两通道 2b**（末段补全跑 Viterbi 取最高唯一整句 + 按输入砍尾巴） |
| 多段全不完整（`nh`/`nhm`/`nhmsx`） | 每段非完整音节 | **简拼键逐级砍尾巴**：k=n..1 查 `dict.exact(前k段首字母串)`，纯词 |
| 多段混合（`nhao`） | 含完整段 + 不完整段 | 不完整段展开为音节列表（源 `dict.syllables()`），逐级笛卡尔积 `exact(join("'"))`，词频合并；单级组合数 > 2000 该级降级为空 |

**多段判定（2026-08-14 修正，消费端遍历所有方案；2026-08-18 追加尾补全）**：`classify` 不再只看贪心方案[0]——
**存在任一全完整方案 → 全拼档**（`dier` 贪心 `[die,r]` 中 r 是音节前缀（ra/ran/…/ruo），
按段完整性会误判 Mixed 展开出「跌入」，而 `[di,er]` 全完整方案存在却不可达「第二」）；
**否则末段为音节前缀可补全 → 全拼档 2b**（`shigechengy` 末段 `y` → 补 yu/yi/yang/…，避免误判
Mixed 而丢失 2b 整句场景）；其余按段完整性分派（全不完整 → 简拼；混合 → 混拼）。

**兜底规则（"不认识"语义，2026-08-14）**：上述全部路由均无候选且输入非空时，
`generate_candidates` 末尾追加一条**原文候选**（`plain` = 去 `'` 整串，`text`/`code` = plain，
kind 按现有惯例多字符 `Word` / 单字符 `Char`，`seg_len = seg.len()` 全消费），
保证候选窗内容恒非空（`input`/`window`/`i` 等无法命中词库的输入可 1/Space 直接上屏原文）。
候选窗对原文候选**不编号呈现**（text == 预编辑原文去 `'` 即判定，UI 层规则，见
14-mod-iuv-tsf-candwin.md），传达"无匹配"语义。

**全拼两通道（2026-08-18 重写：词库负责"词"、Viterbi 只负责"唯一最佳句子"）**：

1. **整句通道**：对 `seg`（词频最优方案）**至多产出一条** Sentence——
   **2a** 末段为完整音节（`…chengyu`）→ 整串跑一次 unigram Viterbi；
   **2b** 末段为音节前缀（`…chengy` 的 `y`）→ 补全为所有合法音节
   （`y` → yu/yi/yang/ying/…），**每个补齐方案各跑一次** Viterbi，取路径分最高一条。
   2b 句子文本可超出已敲字母（`shigechengy` → 「是一个成语」），预编辑显示仍按输入切分不扩展。
   M2 屏蔽组合拦截（被屏蔽的词条/组合不组句）。
   **不再遍历每级/每切分方案组句**（旧行为：`keneng` 的 `[ken,eng]` 单字组合「啃嗯」等
   临时拼句会按切分方案各出一条 Sentence——已移除，仅保留唯一最佳）。
2. **词条通道（砍尾巴逐级前缀匹配）**：`for k = n, n-1, ..., 1`，对前缀 `seg[0..k]`：
   前缀 `join("'")` 再 `schema.segment` 枚举切分（raw 含撇号仅 prefix 方案）→ 各方案
   `join("'")` 键 `dict.exact`：词长 ≥2 → `Word`，单字 → `Char`；同 k 按 weight 降序，
   去重取前 20；**k=1 追加单字全量**（多段输入翻页可达低频同音字，微软实测 2026-08-14）。
   **两路砍完第一刀后前缀对齐**：末段合法砍完整音节（`…chengyu` 砍 `yu`）、不合法砍
   不完整段（`…chengy` 砍 `y`），不因切分方案产生非词库词。
3. 排序：唯一整句（最前）→ exact 匹配长度从长到短（k 降序）→ 同 k 词按权重。
4. 前缀补全（联想，默认关）：`dict.prefix(seg.join("'"), 20)`（词库键已分隔化）。
5. 按 text 去重（保序，先见先留；跨级同文本只留首个）。
6. 截断到 `config.max_candidates`。
7. 依次过 `stages` 管线（MVP 仅 StaticOrder = no-op）。

**简拼路径**（多段全不完整）：k=n..1 查简拼键（构建期生成，§3），候选 seg_len=k；
选中部分消费 → session 悬空续接把尾巴段重建为组合（与全拼路径选中间级词同一机制）。

**部分消费语义**：候选 seg_len=k < n 时，选中后词上屏（悬空入栈）、
`seg[k..]` 尾巴拼音重建组合继续输入（微软实测"你还没说x"同构：词+尾巴拼音）。

例：`chuangqianmingyueguang` → 床前明月光（唯一整句）→ 窗前明月（词，k4 exact）→ … → 床前/窗前（词）→ 床/窗/创（单字），翻页总能到达所需层级；
`zheshi` → 这是（整句）→ 这时/这事/…（词）→ 这/是（单字）；
`nh` → 你好/泥嚎（简拼词，无单字）；`nhmsx` → 你还没睡醒（k5）→ 你还没说（k4）→ 你还没（k3）→ 你好（k2）；
`nhao` → 你好/您好（混拼词）→ 你/那（单字，词前字后）；`sh` → 是/时/上（纯单字）。

Viterbi 要点：位置 0..=n（音节界）；边 (i,j)（`j−i ≤ max_word_syllables`）= `dict.exact(seg[i..j].join("'"))`
的全部词条；边分 = `lm.log_prob(prev_word, word, weight)`；单音节无词条时给兜底边
（text = 该音节原样，分 = `UnigramLm::log_prob(None, s, 0) + OOV_PENALTY`），保证路径恒存在。

## 5. iuv-tsf 内部接缝（W0 完整实现 `ui/mod.rs`，冻结）

```rust
// ===== ui/mod.rs（W0 完整实现）=====
use iuv_core::{Effect, PageInfo};

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

/// 候选窗抽象。MVP 实现 = GdiCandidateWindow（ui/gdi.rs，Agent E；M4 已下线）；
/// **M4 起**：实现 = CandwinCandidateWindow（ui/candwin.rs）：渲染层 iuv-ui
/// （tiny-skia + cosmic-text）+ ULW 呈现（见 `19-m4-cross-render.md`），
/// trait 签名不变，COM 层零改动。
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
// 语言栏"中/英"切换项：guidItem 必须用系统 GUID_LBI_INPUTMODE（Windows 8+ 只显示该 GUID 的项，
// 自定义 GUID 被静默忽略；MSDN ITfLangBarItemMgr::AddItem）。值 0x2C77A81E-41CC-4178-A3A7-5F8A987568E6。
pub const LANGID_ZH_CN: u16 = 0x0804;
pub const PROFILE_DESCRIPTION: &str = "iuv 输入法";
pub const DICT_FILENAME: &str = "iuv.imedic"; // 位于 %LOCALAPPDATA%\iuv\
```

### 5.2 TSF 类别注册清单（register_with_tsf，8 类别，2026-08-16 对齐 QQ）

| 类别 GUID | windows-rs 常量 | 用途 |
|---|---|---|
| `34745C63-...` | `GUID_TFCAT_TIP_KEYBOARD` | 键盘 TIP（核心类别） |
| `13A016DF-...` | `GUID_TFCAT_TIPCAP_IMMERSIVESUPPORT` | TSF 3.0 客户端（Windows Terminal/UWP）加载前提 |
| `CCF05DD7-...` | `GUID_TFCAT_TIPCAP_INPUTMODECOMPARTMENT` | 老式/IMM 场景兼容（缺此类别时 WoW 1.12 等不激活） |
| `25504FB4-...` | `GUID_TFCAT_TIPCAP_SYSTRAYSUPPORT` | 系统托盘/语言栏兼容 |
| `364215D9-...` | `GUID_TFCAT_TIPCAP_COMLESS` | 老式/IMM 进程激活关键类别（对齐 QQ，8 类别主嫌疑） |
| `49D2F9CF-...` | `GUID_TFCAT_TIPCAP_UIELEMENTENABLED` | TSF 3.0 候选 UI 元素（游戏内候选栏）被系统消费前提 |
| `046B8C80-...` | `GUID_TFCAT_DISPLAYATTRIBUTEPROVIDER` | 显示属性提供者（预编辑属性查询） |
| `534C48C1-...` | `GUID_TFCAT_CATEGORY_OF_TIP` | 输入法类别（语言栏/输入指示器展示为中文输入法） |

注册/注销双路径（`DllRegisterServer`/`DllUnregisterServer`）；32/64 位注册表视图分别由对应位数的 `regsvr32` 写入（部署脚本两个都跑）。原 4 类别（KEYBOARD/IMMERSIVE/INPUTMODECOMPARTMENT/SYSTRAY）为 63c6833 前已有；后 4 类别（COMLESS/UIELEMENTENABLED/DISPLAYATTRIBUTEPROVIDER/CATEGORY_OF_TIP）2026-08-16 落代码（此前为手动注册表，WoW 激活/候选元素依赖）。

## 6. 文件属主矩阵（W1 并行防冲突）

| 路径 | 属主 | 状态 |
|---|---|---|
| 根 `Cargo.toml` / `.gitignore` / `AGENTS.md` / `docs/**` | 主智能体 | W0 冻结 |
| `iuv-data/src/lib.rs`、`dict.rs` | 主智能体 | W0 **完整实现**，冻结 |
| `iuv-data/src/format.rs`、`compile.rs`、`bin/dictc.rs`、`tests/**`、`scripts/download-dict.ps1` | **Agent A** | W1 |
| `iuv-core/src/{candidate,config,key}.rs`、`lib.rs` | 主智能体 | W0 完整，冻结 |
| `iuv-core/src/{schema,lm,viterbi,rerank,store,engine,session}.rs`、`tests/**` | **Agent B** | W1 |
| `crates/iuv-repl/**` | **Agent C** | W1 |
| `iuv-tsf/src/ui/mod.rs` | 主智能体 | W0 **完整实现**，冻结 |
| `iuv-tsf/src/ui/gdi.rs`、`examples/candwin_demo.rs` | **Agent E** | W1（M4 起 gdi.rs → `candwin.rs`，渲染层归 iuv-ui） |
| `crates/iuv-ui/**` | 主智能体 | M4 已实现（2026-08-16） |
| `iuv-tsf/src/ui/candwin.rs` | 主智能体 | M4 已实现（ULW 呈现） |
| `iuv-tsf/src/langbar.rs`（右键菜单部分） | 主智能体 | M5 已实现（语言栏右键菜单，2026-08-17 重定义） |
| `iuv-data/src/{shm,ipc}.rs`、`iuv-daemon/**` | 主智能体 | M6 已实现 |
| `iuv-tsf/src/daemon_client.rs` | 主智能体 | M6 已实现 |
| `iuv-tsf/src/{lib,registration,log,session_bridge,composition,langbar}.rs`、`com/**`、`build.rs`、`scripts/{register,unregister}.ps1`、`Cargo.toml` 内 winres 配置 | **Agent D** | W1 |
| `iuv-tsf/src/ui_element.rs` | 主智能体 | wow-ime（2026-08-16） |

规则：只允许改自己属主的文件；发现契约缺陷 → 报告主智能体，禁止私改他人文件。

## 7. 行为集成约定

- TSF 侧每次按键只做三件事（session_bridge）：`vk/char → Key` 映射；`Session::on_key`；应用 Effect
  （更新 composition 文本 → `effect_to_snapshot` → `CandidateUi`；`end` 则上屏/取消并 hide）。
- composition 内嵌文本 = `Effect.composition`（拼音分段）；候选窗只放候选列表（不渲染 reading，微软式，M1 后期修正）。
- `Session::is_active() == false` 时字母键被消费（开启新会话），其余键全部放行。
- **语言栏"中/英"图标（langbar.rs）**：Activate 时经 `ITfLangBarItemMgr::AddItem` 挂载 `ITfLangBarItemButton`（样式
  `TF_LBI_STYLE_BTN_BUTTON | TF_LBI_STYLE_SHOWNINTRAY`，图标为 DLL 内嵌 .ico 资源 ID 101/102）；Deactivate 时 `RemoveItem`。
  挂载失败仅记日志，不影响输入法主体。
- **中/英切换 = 系统"输入法/非输入法切换"（`GUID_COMPARTMENT_KEYBOARD_OPENCLOSE` compartment，真相源）**：
  系统热键（高级键设置，如 Ctrl+Space）翻转 compartment → TextService 经 `ITfCompartmentEventSink::OnChange`
  统一响应（open=0 → 英文模式，非 0 → 中文模式），更新共享 `Arc<AtomicBool>` 并 `OnUpdate` 刷新图标；
  关闭时清理活动会话。语言栏图标点击归一为写该 compartment（`ITfCompartment::SetValue`，需带本实例
  client id；SetValue 同步重入 OnChange，靠防抖幂等）。不再提供 Shift 切换（2026-08 移除）。
  Activate 时"激活即打开"（初始 VT_EMPTY/关闭 → 写 open=1，保持切入即中文）；compartment 缺失/写失败时本地翻转兜底。
- 引擎在 DLL 内进程级单例（`OnceLock<Arc<Engine>>`），词典路径 `%LOCALAPPDATA%\iuv\iuv.imedic`（用户级数据；DLL 本体在 `%ProgramFiles%\iuv\`）；
  加载失败：日志记录，所有字母键原样放行（输入法"透明"，绝不卡用户）。
- **候选 UI 元素（`ui_element.rs`，wow-ime 2026-08-16）**：候选变化时经 `ITfUIElementMgr`
  （`ITfThreadMgr::QueryInterface` 获取）Begin/Update/End 同步 `ITfCandidateListUIElement`——
  数据语义对齐 CANDIDATELIST：`GetCount`=候选总数（全量）、`GetString(uindex)`=全局索引、
  `GetSelection`=**全局索引**（page×page_size+selected，游戏翻页校验 dwSelection 所在页区间，
  页内索引会导致游戏候选栏翻页关闭）、`GetPageIndex`=每页起始数组。桥把元素数据转给 IMM
  应用（游戏）自绘候选栏；TSF 应用（notepad）pbshow=true 自绘窗照常。
- **自绘窗自动抑制（`ImmDetect`，wow-ime 2026-08-16）**：GetTextExt 退化矩形（w/h≤2）连续 3 次
  = IMM 客户端（游戏自绘候选栏）→ `CandwinCandidateWindow::set_suppressed(true)`（show/update 空操作）；
  任意一次非退化立即恢复。pbshow 不可靠（IMM 应用恒 true——系统认为 TIP 自绘，但桥同时转候选给游戏）。
