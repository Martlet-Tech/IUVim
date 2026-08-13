# 17 · 规划：IMEDIC02 平面词库 + mmap 零加工加载（待实施）

> 状态：**规划已定，待实施**（2026-08-13 定稿）。背景见 AGENTS.md「当前状态」已知问题：
> 新开记事本立刻打字前 5-8 个字母直接上屏（引擎冷加载 2.1s 窗口期透明放行）。

## 1. 问题根因

- 引擎是进程级单例（`iuv-tsf/src/com/text_service.rs`），词库每次进程冷加载
- `IMEDIC01` 只存扁平原料，加载 = 读盘 32MB（~20ms）+ **重建查询结构（~2.1s）**：
  125 万条 String 分配 → BTreeMap insert → 组内 weight 排序 → 首字母桶遍历/克隆/排序
- 重建是绝对瓶颈（占 ~98%），且 dictc 编译期已做过全部排序，文件未固化 → 每进程重复劳动
- TSF 架构下 DLL 每进程加载不可避免，但**词库数据可跨进程共享**（文件 mmap 页缓存）

## 2. 目标

- dictc 加工一次：排序、索引、首字母桶、音节表固化进 `IMEDIC02`
- 任何进程加载 = mmap 映射 + 读头，零分配零排序零重建 → **2.1s → ~150ms**
- 物理内存全系统一份（页缓存共享），新开任意软件首键即进拼音

## 3. 格式布局（IMEDIC02，段表驱动）

```
[0..8]   magic = b"IMEDIC02"
[8..12]  u32 段数 N
[12..]   段表：N × { u8 段类型 | u32 偏移 | u32 长度 }
段0 元数据:  u64 total_weight | u32 entry_count | u32 max_word_syllables | 音节表
段1 首字母桶: 26 × { u8 字母 | u32 count | 记录内联 }（单字，weight 降序，≤1000/桶）
段2 记录索引: record_count × u32 记录体偏移（按 code 升序）
段3 记录体:   record_count × { u8 code_len|code | u16 word_len|word | u32 weight }
```

- 段表驱动 → 未来加段（屏蔽段/用户段）= 追加类型，旧加载器忽略未知段，双向兼容
- 记录排序不变量（code asc + weight desc）由 dictc 保证，加载时校验扫描确认（~50-100ms）

## 4. 改动清单

| 文件 | 改动 |
|---|---|
| `iuv-data/src/mmap.rs` **新增** | `MappedFile` RAII：Windows = `CreateFileW`(GENERIC_READ, `FILE_SHARE_READ\|WRITE\|DELETE`) + `CreateFileMappingW` + `MapViewOfFile`；非 Windows = `fs::read` + `Arc<[u8]>`，统一 `&[u8]` 视图 |
| `iuv-data/src/format.rs` | 写 IMEDIC02（段表+桶+索引+记录体）；平面读路径（段定位 O(1) + 校验扫描：边界/code 合法/**code 单调性全查**）；保留 IMEDIC01 旧读路径（magic 分派） |
| `iuv-data/src/dict.rs` | Dict 内部 = MappedFile + 段偏移 + 物化音节表；`exact` 二分索引段→组内物化；`exact_single` 过滤单字；`prefix` 二分范围扫 + 跨组归并（默认关闭低频路径）；`initial_top` 桶段直读；`from_entries` 改走序列化→平面解析统一路径；返回 `&[Entry]`→`Vec<Entry>` |
| `iuv-data/src/compile.rs` | 产出段表/桶/索引（排序逻辑复用） |
| `iuv-data/Cargo.toml` | `windows = { workspace = true }`（仅 Windows target） |
| `iuv-core/src/engine.rs` 等 | 仅类型标注微调（`Vec<&Entry>`→`Vec<Entry>`，消费语法不变） |
| 文档 | `01-contract.md` §3、`10-mod-iuv-data.md`、`AGENTS.md` 同步 |

## 5. 关键风险

- mmap 文件被替换（重编译词库）：`FILE_SHARE_DELETE` 声明，旧映射继续有效
- 截断/坏文件 panic：游标边界检查 + 加载校验扫描，报 `InvalidData`
- 物化 Entry 查询延迟：量级微秒，验收实测

## 6. M2 预留（本轮不实现，格式已支撑）

- 用户词库 `iuv.user.imedic`：同构小文件，查询叠加（`Dict::overlay`），同 (code,word) 取大 weight
- **屏蔽基础词条**：用户库"屏蔽段"存 (code,word)，查询过滤剔除（基础库物理不动）
- 写入：命名 mutex + 写时复制 + `ReplaceFileW`，会话级延迟生效

## 7. 验收

1. `cargo test --workspace` 全绿（新增：02 往返、坏数据/截断、01 兼容、排序不变量）
2. `download-dict.ps1` 重编译 → 日志「引擎加载完成：耗时 <200ms」
3. 手测：新开记事本首键即进拼音；旧 IMEDIC01 仍可加载
