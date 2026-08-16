# 00 · 总览：iuv 输入法 MVP（M1）vibecoding 方案书

> 本目录是 M1（最小 MVP）的**可执行**方案书。目标读者：主智能体（组装者）与子智能体（模块实现者）。
> 所有文档中，**`01-contract.md` 是接口的唯一权威来源**；模块任务书与契约冲突时以契约为准。

## 1. M1 目标（验收一句话）

在 Windows 10/11 上注册本输入法后，能在记事本等应用中**用全拼打字**：输入拼音出现预编辑文本与候选窗，
空格/数字选词上屏，支持翻页、退格、Esc、Enter 原文上屏。中英切换走系统机制
（「输入法/非输入法切换」热键，如 Ctrl+Space；Shift 临时英文方案已废弃，见 13 号任务书）。
整句用 unigram Viterbi（只用词库自带 weight），候选排序为**纯静态词频序**。

M1 **不做**（已留槽位，见各任务书"槽位"节）：滞回/学习/钉选、跨平台渲染/托盘/守护进程（M4~M6）、
n-gram 语言模型、双拼/模糊音、设置界面、安装器、x86 架构、逐词确认。
（鼠标点选候选、点击选词、翻页环绕等候选窗交互已在 2026-08-13 补齐，见 `d1dcfb8`/`2cc189b`。）

## 2. 架构与模块

```
┌─────────────────────────── workspace ───────────────────────────┐
│ crates/（跨平台层）                                               │
│   iuv-data   词库编译器(dictc) + 二进制格式 + Dict 查询层   │  叶 crate，无 workspace 内依赖
│   iuv-core   引擎：切分/查词/Viterbi/会话状态机/排序管线    │  依赖 iuv-data
│   iuv-ui     候选窗/菜单绘图：tiny-skia + cosmic-text       │  依赖 iuv-core（UiSnapshot/Theme 消费）
│   iuv-repl   CLI 调试前端（不注册输入法即可测引擎）         │  依赖 iuv-core, iuv-data
│ platforms/（平台层，每平台一套：系统适配 + 门面）                 │
│   windows/iuv-tsf    cdylib：COM/TSF 管线 + 候选窗窗口层   │  依赖 iuv-core, iuv-data, iuv-ui
│   windows/iuv-daemon 守护进程 exe：持有用户库 + 托盘 + 设置页（M6）│  依赖 iuv-data（共享段）, iuv-ui, egui
│   macos/          占位（IMK 适配 + 门面规划，README）            │
│   linux/          占位（Fcitx5/IBus 适配 + 门面规划，README）     │
└──────────────────────────────────────────────────────────────────┘
```

运行时数据流（M1，全在应用进程内）：

```
按键 → TSF(OnTestKeyDown/OnKeyDown) → session_bridge 映射为 iuv_core::Key
      → Session::on_key → Effect ─┬→ composition.rs：更新预编辑文本 / 上屏
                                   └→ ui: CandidateUi.show/update/hide（iuv-ui 渲染，M4 起）
```

## 3. 执行流程（三波）

| 波次 | 执行者 | 内容 | 出口条件 |
|---|---|---|---|
| **W0** | 主智能体 | 按 `01-contract.md` 建 workspace 骨架：全部 Cargo.toml、契约类型/trait、`Dict` 查询层**完整实现**、`ui/mod.rs` **完整实现**，其余 `todo!()` 桩 | `cargo check --workspace` 全绿 |
| **W1** | 5 个子智能体**并行** | A=iuv-data(编译/格式/dictc) B=iuv-core C=iuv-repl D=iuv-tsf 管线 E=iuv-tsf 候选窗 | 各自任务书 DoD 全绿，且只改属主矩阵内的文件 |
| **W2** | 主智能体 | `20-assembly.md`：整体构建 → 词库编译 → repl 冒烟 → 注册 → 记事本手测清单 | M1 验收达成 |

**并行独立性如何保证**：
1. 接口在 W0 冻结（编译通过即契约成立）；
2. B 需要的 `Dict`（含 `from_entries` 测试构造器）由 W0 直接实现完毕，B 不等 A；
3. D/E 共享的 `CandidateUi` trait 与 `UiSnapshot` 由 W0 实现完毕，D、E 文件不相交；
4. 每个 crate 的**文件属主矩阵**见契约 §6，越界改文件 = 返工；
5. 依赖白名单见契约 §2，新增第三方依赖需主智能体批准。

## 4. 已知简化（M1 特意为之，后续里程碑处理）

- 音节切分：`'` 为强制分隔（硬边界）；无撇号时**枚举全部合法切分**（如 `xian` → `[xian]` 与 `[xi,an]`），
  各方案查库合并按权重排序——`xian` 混排单字与"西安"等词，`xi'an` 强制只出词
- 选词即**续接组句**（M1 后期落地）：候选按"砍尾巴逐级前缀"从长到短排列；
  选中间级词 → 悬空入栈（不上屏，预编辑混合显示"已选汉字+尾巴拼音"）尾巴续接，退格取消已选词；
  全部消费才结束会话（完整版 picked/退格回退已实现，逐词光标编辑归 M3）
- 候选窗跨平台自绘（tiny-skia + cosmic-text，M4 起；见 `19-m4-cross-render.md`），
  主题浅色/深色可配；候选窗交互已支持
  鼠标点击选词、悬停高亮、翻页环绕、布局方向配置（2026-08-13，见 `d1dcfb8`/`2cc189b`）
- 引擎进程级单例，词库 IMEDIC02 平面格式 mmap 零加工加载——冷加载 ~70ms、物理内存全系统一份
  （页缓存共享，M1.6 落地，见 `17-imedic02-mmap.md`）；M6 起用户库移守护进程共享（`22-m6-daemon.md`）
- 仅 x64；需管理员权限注册
- 词库（白霜拼音，GPL-3.0）由脚本下载，不进仓库；发布时注意 NOTICE 声明

## 5. 文档索引

| 文件 | 内容 |
|---|---|
| `01-contract.md` | **共享契约**：依赖版本、全部公共 API、行为契约、文件属主矩阵、词典二进制格式 |
| `10-mod-iuv-data.md` | 任务书 A：词库编译器 + 二进制格式 + 下载脚本 |
| `11-mod-iuv-core.md` | 任务书 B：引擎（切分/候选生成/Viterbi/会话/管线桩） |
| `12-mod-iuv-repl.md` | 任务书 C：CLI 调试前端 |
| `13-mod-iuv-tsf-core.md` | 任务书 D：COM/TSF 管线 + 注册 |
| `14-mod-iuv-tsf-candwin.md` | 任务书 E：GDI 候选窗 + 演示程序（M4 起被 `19` 替代，历史保留） |
| `19-m4-cross-render.md` | **M4**：跨平台渲染候选窗（tiny-skia + D2D/DComp 呈现 + 主题） |
| `21-m5-tray-menu.md` | **M5**：托盘图标 + 自绘右键菜单 |
| `22-m6-daemon.md` | **M6**：守护进程（持有用户库 + 设置页 + 托盘接管） |
| `20-assembly.md` | W2 集成组装手册（主智能体用） |
| `30-conventions.md` | 全局约定：代码风格、错误处理、日志、测试纪律 |
