# 31 · 简体/繁体切换（`initial_state.script` 生效，OpenCC 数据文件形态）

> 状态：**规划中，待实现**（2026-08-19 立项）。前置阅读：`01-contract.md`、`02-conventions.md`、
> `28-initial-state-settings.md`（已结案，docs/closed）、`17-imedic02-mmap.md`。
> 背景：`initial_state.script`（`ScriptMode::Traditional`）自 28 起仅存默认值、行为后置。
> 本任务书落地「繁体模式 = 简体词库 + 运行时简→繁转换」，数据采用**形态3 = 数据文件跨平台加载**
> （2026-08-19 用户拍板：**不做**编进 DLL、**不做** daemon 持有 + IPC 下发——理由见 §2.2）。

## 1. 目标

- 简体模式（默认）：零变化（行为回归由测试保证）。
- 繁体模式（`initial_state.script = "traditional"`）：候选、预编辑、上屏文本均显示繁体；
  词库/自造词/调权/屏蔽**内部恒为简体**（同全角「录原文不录全角」语义，§4.2）。
- 跨平台：数据为**数据文件**，由各平台前端（Win=iuv-tsf、mac=IMK、Linux=Fcitx）从各自数据目录
  加载，iuv-core/iuv-data 纯 Rust 不依赖平台。

## 2. 形态决策（2026-08-19 用户拍板）

### 2.1 候选方案（曾考虑，均否决）

| 形态 | 说明 | 否决理由 |
|---|---|---|
| 一：每次转换走 daemon IPC | 前端每按键经命名管道请求转换 | 转换在热路径（composition/reading/候选全量每次重算），单次 round-trip ~50-200µs × 每按键多次 = 1ms+；TSF 键处理时间敏感，且 daemon 一死打字即断。**否决** |
| 二：daemon 发布共享段 + 前端本地转换 | 仿用户库 ShmReader 模式 | daemon 本身 Windows-only（非 Win 全 stub），不解决跨平台反而更差；数据是只读静态资源，无写者需求，硬塞 daemon = 双份工作量（shm 段 + 离线降级仍要文件路径兜底）。**否决** |
| 三：数据文件（`iuv.opencc`），与 `iuv.imedic` 同构 | 下载 → 编译成紧凑二进制 → 各平台从数据目录加载 | **采用**。词库已证明此形态跨平台成立（mmap.rs 非 Windows 回退 fs::read），零 IPC、零 daemon 依赖、数据更新=换文件不重编 |

### 2.2 形态3 与词库管线同构

词库现状（`17-imedic02-mmap.md`）：`data/` 拉取源 → `dictc` 编译成 IMEDIC02 平面格式 → 安装/部署
到 `%LOCALAPPDATA%\iuv\iuv.imedic` → 前端 `iuv_data::load`（mmap，非 Win 回退）→ `Engine::new`。
OpenCC 表完全照搬这条链路（§5），保证 macOS/Linux 前端零额外设计。

## 3. 数据源与合规

- 数据源：OpenCC（BYVoid）词典数据，**Apache-2.0**。
  - `STPhrases.txt`（简→繁短语，~8千行）
  - `STCharacters.txt`（简→繁单字，~4千行）
- 获取：`scripts/download-opencc.ps1` 拉取到 `data/opencc/`（gitignore，不入库）。
- 范围：**通用繁体（s2t）**，不做台湾方言词（`网络`→`網絡` 而非 `網路`，2026-08-19 用户拍板）。
- 发布合规：数据不进仓库；发布包含编译产物 `iuv.opencc` 需附 Apache-2.0 NOTICE
  （`02-conventions.md` §6 补条目）。
- 格式：`key\tvalue1 value2 ...`（tab 分隔，空格分隔多个异体字/词；转换取**首值**）。
  - 一简多繁（如 `后`→`后 後`、`发`→`发 髮`）：短语表优先命中（`皇后`→`皇后`），
    单字兜底取首值（`以后` 不在短语表时 `后`→`后`——**已知差距**：无上下文模型，
    取首值可能非语境最优；OpenCC 原生也依赖上下文规则，本方案接受简化）。

## 4. 转换层设计（iuv-core，纯 Rust）

### 4.1 `crates/iuv-core/src/script.rs` 新增

- `ScriptConverter`：由 OpenCC 数据构建的转换器。
  - 数据结构：`phrases: HashMap<String, String>` + `chars: HashMap<char, char>`（或 String）。
  - `from_text(phrases: &str, chars: &str) -> ScriptConverter`：解析两个文本源（兼容 BOM、
    空行/注释行 `#` 跳过、`key\tvalue` 拆解、多值取首值）。
  - `convert(&self, text: &str) -> String`：**正向最长匹配**——逐字符扫描，当前位置先试
    短语表最长键（短语优先），命中则整体替换；否则单字表；两者未命中原样保留（汉字/
    拼音/符号/全角均直通，幂等——已繁体字符不二次转换）。
  - 测试：短语优先、单字兜底、多值取首、非 CJK 直通、已繁体幂等、空串。

- 与全角（`punct.rs`）关系：**并存独立**。`fullwidth_text` 管 ASCII→全角，`ScriptConverter`
  管简→繁，互不重叠。调用顺序：先全角后简繁（或反之均可，字符集不交叠）。

### 4.2 挂点（`session.rs`，与全角同构）

- `to_output(&self, text: String) -> String`（:240）：`fullwidth_text` 之后追加
  `convert_script`（仅当 `config.initial_state.script == Traditional` 且 converter 已装配）。
  - 覆盖 Enter / 无候选空格 / flush / 原文兜底候选提交的拼音原文——但**拼音原文无汉字不转换**
    （`nihao`→`ｎｉｈａｏ` 全角仍生效，简繁对拼音原文无影响）。
- `effect()`（:318）：composition / reading / candidates 文本在输出前套转换——
  **候选窗显示繁体、预编辑显示繁体**。内部 `self.all`/`page_cands` 恒简体（自造词记录
  用简体原文，§2.1 目标）。
- **自造词/调权/屏蔽键不变**：`record_phrase` 走 `c.text`（简体原文，记录前不转换），
  屏蔽用 `word`（简体），与全角「录原文不录全角」同构。
- 转换器装配：`Engine` 新增 `script_converter: Option<Arc<ScriptConverter>>` 字段 +
  `attach_script_converter(Option<Arc<ScriptConverter>>)` 方法；`with_parts` 加参
  （默认 `None`，4 处测试调用点补 `None` 不破坏现有语义——`engine.rs:103`）。
  Session 经 `self.engine` 读取。

### 4.3 失败降级

- 数据文件缺失/损坏 → `attach_script_converter(None)` → 繁体模式下**降级为简体输出**
  （转换层直接跳过，行为同简体模式；不 panic、不影响会话）。记日志（iuv-tsf 侧）。
- 数据文件与词库独立：词库加载失败透明模式不影响简繁表加载（独立装配路径）。

## 5. 数据文件格式与编译（iuv-data）

- 新文件：`data/opencc/*.txt`（下载源）→ 编译为 **`iuv.opencc`**（新二进制格式 `IUVOCC01`）。
- 编译器：`dictc` 扩展（`crates/iuv-data`）或独立小工具——**推荐 dictc 扩展**（同一命令链）。
  格式：段表驱动平面格式，含 phrases/chars 两段 + 偏移索引，与 IMEDIC02 同思路
  （mmap 零加工、段定位）。数据量小（~1万条），运行时解析 HashMap 也可接受——
  **实现时以 dictc 扩展优先**，若数据量验证足够小（<50ms 解析）可简化为直接读文本。
- 安装/部署：install.ps1 / dev-deploy.ps1 增加 `iuv.opencc` 复制到 `%LOCALAPPDATA%\iuv\`
  （与 iuv.imedic 同目录），走 `Replace-InUseFile`（mmap 同款锁处理）。
- `download-opencc.ps1`：仿 `download-dict.ps1` 幂等下载（存在非空跳过）。

## 6. 影响面

| 模块 | 改动 |
|---|---|
| `crates/iuv-core/src/script.rs` | 新增：ScriptConverter + convert（纯 Rust） |
| `crates/iuv-core/src/engine.rs` | `script_converter` 字段 + `attach_script_converter` + `with_parts` 加参（4 测试点补 None） |
| `crates/iuv-core/src/session.rs` | `to_output`/`effect` 挂转换（script 模式判定） |
| `crates/iuv-core/src/config/mod.rs` | `ScriptMode` 注释「仅存默认值」→「生效（s2t 通用繁体）」 |
| `crates/iuv-core/src/lib.rs` | `pub mod script` + 导出 |
| `crates/iuv-data` | dictc 扩展（iuv.opencc 编译）+ 格式定义 |
| `platforms/windows/iuv-tsf/src/` | load_engine 装配 converter（读 `%LOCALAPPDATA%\iuv\iuv.opencc`） |
| `platforms/windows/iuv-daemon/src/settings.rs:302` | 设置页文案「仅记录默认值」→「已生效」 |
| `scripts/download-opencc.ps1` | 新增（幂等拉取） |
| `scripts/install.ps1` / `dev-deploy.ps1` | 增加 iuv.opencc 复制 + 下载/编译链 |
| `docs/plan/02-conventions.md` | §6 补 Apache-2.0 合规条目 |
| `docs/plan/01-contract.md` | §3.3 注释同步；新增 script 模块接口 |

## 7. 测试

- iuv-core：script.rs 单测（§4.1 列表）+ 会话集成（繁体候选/预编辑/上屏、自造词录简体、
  调权/屏蔽键不变、简体回归零变化、数据缺失降级）。
- iuv-data：dictc 编译 iuv.opencc + 加载 roundtrip。
- 引擎：`with_parts` 4 测试点补 None 后全绿。
- 手测（Windows notepad）：繁体模式 `nihao`→候选/上屏「你好」繁体场景（如 `yihou`→以後、
  `nihao`→你好、`shijian`→時間）、候选窗/预编辑繁体、Enter 上屏繁体、自造词后切简体词仍在、
  数据文件删除后降级简体不崩。

## 8. 待确认（实现前问用户）

1. `iuv.opencc` 是否要单独编译格式，还是允许运行时直接解析两个文本文件
   （数据量小，`<50ms` 可接受）？——默认推荐 dictc 扩展（与词库管线一致）。
2. daemon 设置页是否暴露简体/繁体选择（可复用现有 settings 枚举页）？

## 9. 相关文档

- 数据管线：`17-imedic02-mmap.md`（词库形态3 先例）、`20-assembly.md`
- 状态配置：`28-initial-state-settings.md`（docs/closed）、`02-conventions.md`
- 全角挂点先例：`crates/iuv-core/src/punct.rs`、`crates/iuv-core/src/session.rs`
- OpenCC 数据：https://github.com/BYVoid/OpenCC （Apache-2.0，`data/dictionary/STPhrases.txt`、
  `STCharacters.txt`）