# 10 · 任务书 A：ime-data（词库编译器 + 二进制格式）

> 属主文件：`crates/ime-data/src/{format.rs, compile.rs, bin/dictc.rs, tests/**}`、`scripts/download-dict.ps1`
> 前置阅读：`00-overview.md`、`01-contract.md`（§1 结构、§3 API 与二进制格式、§6 属主）、`30-conventions.md`
> **禁止**修改 `dict.rs` / `lib.rs`（W0 已完整实现并冻结）。

## 1. 目标

把白霜拼音 rime 词库（`.dict.yaml`）编译为契约 §3.1 定义的 `input.imedic` 二进制，
使 `ime_data::load()` 能读成 `Dict`。产出编译 CLI `dictc` 与词库下载脚本。

## 2. 交付清单

| 文件 | 内容 |
|---|---|
| `src/format.rs` | `pub fn write(records, writer)` / `pub fn read(path) -> io::Result<Dict>`；magic 校验、截断/坏数据报错（`io::ErrorKind::InvalidData`） |
| `src/compile.rs` | `compile_files()` 全量实现（签名见契约 §3） |
| `src/bin/dictc.rs` | CLI：`dictc <output.imedic> <input1.dict.yaml> [input2...]`；结束打印 `CompileStats` |
| `tests/compile_format.rs` | 集成测试（见 §4） |
| `scripts/download-dict.ps1` | 下载白霜词库 5 个文件到 `data/rime-frost/cn_dicts/`（见 §5） |
| `crates/ime-data/README` 不需要 | 文档写进代码注释即可 |

## 3. 实现要点

### 3.1 rime .dict.yaml 解析（compile.rs）

文件格式（白霜实测）：
- `#` 开头行 = 注释；yaml 头部到单独一行 `...` 结束；其后为词条
- 词条行：`词<TAB>带空格拼音<TAB>权重`，如 `你好\tni hao\t12345`
- 权重列**可缺省**（按 0 处理）；忽略空行与字段数 <2 的行
- 拼音列转 squashed：去空格、转小写（`ni hao` → `nihao`），squashed 结果同时作为查询键与 `Entry.code`
- 同 `(squashed_code, word)` 去重取最大 weight，`duplicates` 计数
- 全部记录按 `(code 升序, weight 降序)` 排序后交给 `format::write`

编码注意：文件为 UTF-8（无 BOM 或有 BOM 都要容忍，读到 BOM 跳过）。`BufRead::lines` 即可。
60 万级词条，避免逐行 `String` 之外的额外分配即可，无需性能玄学。

### 3.2 二进制格式（format.rs）

严格按契约 §3.1。写用 `BufWriter`，读用流式读取直接构建 `Dict`（复用 `Dict::from_entries`
收集后构造，或按序插入——注意 `from_entries` 已做排序去重，直接复用它最不容易错）。

### 3.3 dictc CLI

```
dictc data\input.imedic data\rime-frost\cn_dicts\8105.dict.yaml data\rime-frost\cn_dicts\base.dict.yaml ...
```
参数校验、错误信息带文件名；成功打印 `files=N entries=M codes=K duplicates=D`。

## 4. 测试（`tests/compile_format.rs`，全绿才算完）

fixture 用 `std::env::temp_dir()` 落临时文件，**不写 repo 目录**。用例：

1. `roundtrip_small_dict`：手写 3 词条 yaml（含 1 条缺权重、1 个重复词不同 weight）→ compile → load →
   断言 `exact("nihao")` 顺序按 weight 降序、去重生效、缺省 weight=0
2. `yaml_header_and_comments_skipped`：含 `#` 注释与 `--- ... ...` 头部的文件正确解析
3. `code_is_squashed`：源文件 `ni hao` → `Entry.code == "nihao"`，`exact("nihao")` 命中
4. `bad_magic_rejected`：篡改首字节 → `load` 返回 `InvalidData`
5. `prefix_query_smoke`：编译产物经 `Dict::prefix("nih", 10)` 能召回 `nihao` 词条（验证与查询层协作）
6. `syllables_collected`：`Dict::syllables()` 含 `ni`、`hao`

## 5. 下载脚本（scripts/download-dict.ps1）

源：`https://raw.githubusercontent.com/gaboolic/rime-frost/master/cn_dicts/<file>`，文件清单
（与 WindInput 相同的基础集）：`8105.dict.yaml`、`41448.dict.yaml`、`base.dict.yaml`、`ext.dict.yaml`、`others.dict.yaml`
- 目标目录 `data\rime-frost\cn_dicts\`（不存在则创建）；已存在且大小非零则跳过（幂等）
- `Invoke-WebRequest -UseBasicParsing`，失败即非零退出
- 脚本头部注释写明：白霜拼音 GPL-3.0，数据不入库，仅本地构建用

## 6. DoD（完成定义）

```
cargo test -p ime-data        # 全绿
cargo check -p ime-data       # 无 warning
```
并把编译命令用法写入 `20-assembly.md` 引用位置：`dictc data\input.imedic data\rime-frost\cn_dicts\*.dict.yaml`

## 7. 槽位（本模块已预留，无需实现）

- 二进制格式 magic 含版本号 `IMEDIC01`，将来加字段升 `02` 并做向后兼容读取
- `dictc` 将来加 `--format scel` 等导入器时，只新增解析函数进 `compile.rs`
- M5 用户词库：`Dict::from_entries` 已公开，直接可用

## 8. 子智能体启动提示词（主智能体派发时原样使用）

```
你负责实现 Input 输入法 MVP 的 ime-data 模块（词库编译器+二进制格式）。
先读 D:\Projects\input\docs\plan\00-overview.md、01-contract.md、30-conventions.md，
再读本任务书 D:\Projects\input\docs\plan\10-mod-ime-data.md 并严格执行。
接口签名以 01-contract.md §3 为准；只能创建/修改属主矩阵中属于 Agent A 的文件。
Dict 查询层已由主智能体实现（dict.rs），禁止修改。
完成后必须满足任务书 §6 DoD：cargo test -p ime-data 全绿、cargo check -p ime-data 无 warning。
这是纯 Rust 逻辑任务，不需要注册输入法或管理员权限。
最终回复：改动文件清单 + 测试输出摘要 + 任何偏离契约之处（应为无）。
```
