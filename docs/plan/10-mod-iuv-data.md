# 10 · 任务书 A：iuv-data（词库编译器 + 二进制格式）

> 属主文件：`crates/iuv-data/src/{format.rs, compile.rs, dict.rs, mmap.rs, bin/dictc.rs, tests/**}`、`scripts/download-dict.ps1`
> 前置阅读：`00-overview.md`、`01-contract.md`（§1 结构、§3 API 与二进制格式、§6 属主）、`30-conventions.md`、`17-imedic02-mmap.md`

## 1. 目标

把白霜拼音 rime 词库（`.dict.yaml`）编译为契约 §3.1 定义的 `iuv.imedic` 二进制（**IMEDIC02**），
使 `iuv_data::load()` 以 mmap 零加工方式读成 `Dict`（冷加载 2.1s → ~70ms）。
产出编译 CLI `dictc` 与词库下载脚本。

## 2. 交付清单

| 文件 | 内容 |
|---|---|
| `src/mmap.rs` | `MappedFile`：Windows = CreateFileW(读共享+延迟删除) + CreateFileMappingW + MapViewOfFile；非 Windows = 整读 Arc 字节；`open`/`from_vec` 统一 `&[u8]` 视图 |
| `src/format.rs` | IMEDIC02 写（段表+元数据+桶+索引+记录体，内部排序保证不变量）与段布局常量 |
| `src/dict.rs` | `Dict` = mmap 视图 + 段偏移；`from_file` 全量边界校验；查询（索引二分物化）；`from_entries` 走序列化→解析统一路径 |
| `src/compile.rs` | `compile_files()` 全量实现（签名见契约 §3；简拼键生成） |
| `src/bin/dictc.rs` | CLI：`dictc <output.imedic> <input1.dict.yaml> [input2...]`；结束打印 `CompileStats` |
| `tests/compile_format.rs` | 集成测试（见 §4） |
| `scripts/download-dict.ps1` | 下载白霜词库 5 个文件到 `data/rime-frost/cn_dicts/`（见 §5） |

## 3. 实现要点

### 3.1 rime .dict.yaml 解析（compile.rs）

文件格式（白霜实测）：
- `#` 开头行 = 注释；yaml 头部到单独一行 `...` 结束；其后为词条
- 词条行：`词<TAB>带空格拼音<TAB>权重`，如 `你好\tni hao\t12345`
- 权重列**可缺省**（按 0 处理）；忽略空行与字段数 <2 的行
- 拼音列转 squashed：空格转 `'`、转小写（`ni hao` → `ni'hao`），squashed 结果作为查询键与 `Entry.code`
- 同 `(squashed_code, word)` 去重取最大 weight，`duplicates` 计数
- M1.5 简拼键：≥2 音节词生成每音节首字母键（权重复制），与全拼键同表混存
- 排序不变量由 `format::write` 内部保证（compile 无需排序）

编码注意：文件为 UTF-8（无 BOM 或有 BOM 都要容忍，读到 BOM 跳过）。`BufRead::lines` 即可。
60 万级词条，避免逐行 `String` 之外的额外分配即可，无需性能玄学。

### 3.2 二进制格式（format.rs）

严格按契约 §3.1（IMEDIC02 段表布局，见 `17-imedic02-mmap.md`）。写用一次性字节组装 + `BufWriter`；
读 = `MappedFile::open` → `Dict::from_file`（mmap + 段定位 + 全量边界校验扫描，不校验排序不变量）。

### 3.3 dictc CLI

```
dictc data\iuv.imedic data\rime-frost\cn_dicts\8105.dict.yaml data\rime-frost\cn_dicts\base.dict.yaml ...
```
参数校验、错误信息带文件名；成功打印 `files=N entries=M codes=K duplicates=D`。

## 4. 测试（`tests/compile_format.rs`，全绿才算完）

fixture 用 `std::env::temp_dir()` 落临时文件，**不写 repo 目录**。用例：

1. `roundtrip_small_dict`：手写 3 词条 yaml（含 1 条缺权重、1 个重复词不同 weight）→ compile → load →
   断言 `exact("ni'hao")` 顺序按 weight 降序、去重生效、缺省 weight=0、简拼键生效
2. `yaml_header_and_comments_skipped`：含 `#` 注释与 `--- ... ...` 头部的文件正确解析
3. `code_keeps_separation`：源文件 `Ni Hao` → `Entry.code == "ni'hao"`，`exact("ni'hao")` 命中
4. `bad_magic_rejected`：篡改首字节 → `load` 返回 `InvalidData`
5. `prefix_query_smoke`：编译产物经 `Dict::prefix("ni", 10)` 能召回 `ni'hao` 词条（验证与查询层协作）
6. `syllables_collected`：`Dict::syllables()` 含 `ni`、`hao`
7. `unknown_segment_ignored`：合法文件尾追加未知段类型 → 加载成功且查询正常（前向兼容）
8. `initial_top_works_from_file`：桶段从文件加载后的查询（词频降序、多字词不入桶）

## 5. 下载脚本（scripts/download-dict.ps1）

源：`https://raw.githubusercontent.com/gaboolic/rime-frost/master/cn_dicts/<file>`，文件清单
（与 WindInput 相同的基础集）：`8105.dict.yaml`、`41448.dict.yaml`、`base.dict.yaml`、`ext.dict.yaml`、`others.dict.yaml`
- 目标目录 `data\rime-frost\cn_dicts\`（不存在则创建）；已存在且大小非零则跳过（幂等）
- `Invoke-WebRequest -UseBasicParsing`，失败即非零退出
- 脚本头部注释写明：白霜拼音 GPL-3.0，数据不入库，仅本地构建用

## 6. DoD（完成定义）

```
cargo test -p iuv-data        # 全绿
cargo check -p iuv-data       # 无 warning
```
并把编译命令用法写入 `20-assembly.md` 引用位置：`dictc data\iuv.imedic data\rime-frost\cn_dicts\*.dict.yaml`

## 7. 槽位（本模块已预留，无需实现）

- 段表驱动：未来加段（屏蔽段/用户段）只追加段类型，旧加载器忽略未知段（`unknown_segment_ignored` 已锁定行为）
- `dictc` 将来加 `--format scel` 等导入器时，只新增解析函数进 `compile.rs`
- M5 用户词库：`Dict::from_entries` 已公开，直接可用（走统一序列化→解析路径）

## 8. 子智能体启动提示词（主智能体派发时原样使用）

```
你负责实现 iuv 输入法 MVP 的 iuv-data 模块（词库编译器+二进制格式）。
先读 D:\Projects\vaim\docs\plan\00-overview.md、01-contract.md、30-conventions.md，
再读本任务书 D:\Projects\vaim\docs\plan\10-mod-iuv-data.md 并严格执行。
接口签名以 01-contract.md §3 为准；只能创建/修改属主矩阵中属于 Agent A 的文件。
Dict 查询层已由主智能体实现（dict.rs），禁止修改。
完成后必须满足任务书 §6 DoD：cargo test -p iuv-data 全绿、cargo check -p iuv-data 无 warning。
这是纯 Rust 逻辑任务，不需要注册输入法或管理员权限。
最终回复：改动文件清单 + 测试输出摘要 + 任何偏离契约之处（应为无）。
```
