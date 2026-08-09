# 00 · 总览：Input 输入法 MVP（M1）vibecoding 方案书

> 本目录是 M1（最小 MVP）的**可执行**方案书。目标读者：主智能体（组装者）与子智能体（模块实现者）。
> 所有文档中，**`01-contract.md` 是接口的唯一权威来源**；模块任务书与契约冲突时以契约为准。

## 1. M1 目标（验收一句话）

在 Windows 10/11 上注册本输入法后，能在记事本等应用中**用全拼打字**：输入拼音出现预编辑文本与候选窗，
空格/数字选词上屏，支持翻页、退格、Esc、Enter 原文上屏、Shift 临时切英文。
整句用 unigram Viterbi（只用词库自带 weight），候选排序为**纯静态词频序**。

M1 **不做**（已留槽位，见各任务书"槽位"节）：滞回/学习/钉选、Tauri helper 与 WebView 候选窗、IPC、
n-gram 语言模型、双拼/模糊音、设置界面、安装器、x86 架构、逐词确认、鼠标点选候选。

## 2. 架构与模块

```
┌─────────────────────────── workspace ───────────────────────────┐
│ crates/ime-data   词库编译器(dictc) + 二进制格式 + Dict 查询层   │  叶 crate，无 workspace 内依赖
│ crates/ime-core   引擎：切分/查词/Viterbi/会话状态机/排序管线    │  依赖 ime-data
│ crates/ime-repl   CLI 调试前端（不注册输入法即可测引擎）         │  依赖 ime-core, ime-data
│ crates/ime-tsf    cdylib：COM/TSF 管线 + GDI 候选窗             │  依赖 ime-core, ime-data
└──────────────────────────────────────────────────────────────────┘
```

运行时数据流（M1，全在应用进程内）：

```
按键 → TSF(OnTestKeyDown/OnKeyDown) → session_bridge 映射为 ime_core::Key
      → Session::on_key → Effect ─┬→ composition.rs：更新预编辑文本 / 上屏
                                   └→ ui: CandidateUi.show/update/hide（GDI 候选窗）
```

## 3. 执行流程（三波）

| 波次 | 执行者 | 内容 | 出口条件 |
|---|---|---|---|
| **W0** | 主智能体 | 按 `01-contract.md` 建 workspace 骨架：全部 Cargo.toml、契约类型/trait、`Dict` 查询层**完整实现**、`ui/mod.rs` **完整实现**，其余 `todo!()` 桩 | `cargo check --workspace` 全绿 |
| **W1** | 5 个子智能体**并行** | A=ime-data(编译/格式/dictc) B=ime-core C=ime-repl D=ime-tsf 管线 E=ime-tsf 候选窗 | 各自任务书 DoD 全绿，且只改属主矩阵内的文件 |
| **W2** | 主智能体 | `20-assembly.md`：整体构建 → 词库编译 → repl 冒烟 → 注册 → 记事本手测清单 | M1 验收达成 |

**并行独立性如何保证**：
1. 接口在 W0 冻结（编译通过即契约成立）；
2. B 需要的 `Dict`（含 `from_entries` 测试构造器）由 W0 直接实现完毕，B 不等 A；
3. D/E 共享的 `CandidateUi` trait 与 `UiSnapshot` 由 W0 实现完毕，D、E 文件不相交；
4. 每个 crate 的**文件属主矩阵**见契约 §6，越界改文件 = 返工；
5. 依赖白名单见契约 §2，新增第三方依赖需主智能体批准。

## 4. 已知简化（M1 特意为之，后续里程碑处理）

- 音节切分为**单一贪心最长匹配**（不枚举多种切分；`xi'an` 须靠 `'` 手动分隔）
- 选词即整体上屏并结束会话（无"逐词确认后继续组句"）
- 候选窗 GDI 自绘、无鼠标点选、无皮肤
- 引擎每进程一份（60~80MB），M4 迁入 helper 进程共享
- 仅 x64；需管理员权限注册；无中英状态栏图标
- 词库（白霜拼音，GPL-3.0）由脚本下载，不进仓库；发布时注意 NOTICE 声明

## 5. 文档索引

| 文件 | 内容 |
|---|---|
| `01-contract.md` | **共享契约**：依赖版本、全部公共 API、行为契约、文件属主矩阵、词典二进制格式 |
| `10-mod-ime-data.md` | 任务书 A：词库编译器 + 二进制格式 + 下载脚本 |
| `11-mod-ime-core.md` | 任务书 B：引擎（切分/候选生成/Viterbi/会话/管线桩） |
| `12-mod-ime-repl.md` | 任务书 C：CLI 调试前端 |
| `13-mod-ime-tsf-core.md` | 任务书 D：COM/TSF 管线 + 注册 |
| `14-mod-ime-tsf-candwin.md` | 任务书 E：GDI 候选窗 + 演示程序 |
| `20-assembly.md` | W2 集成组装手册（主智能体用） |
| `30-conventions.md` | 全局约定：代码风格、错误处理、日志、测试纪律 |
