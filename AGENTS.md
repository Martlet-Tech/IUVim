# iuv 输入法（代号 iuvim，谐音"哎哟喂"）

Rust + TSF 的 Windows 中文输入法。核心卖点（M2 起）：**用户掌控排序**——静态词频序默认稳定
（肌肉记忆安全）+ Shift+←/→ 主动调权（绝对值覆盖，反复调整收敛，见 `docs/plan/18-m2-user-dict.md`）。

## 仓库管理员想对读到这里的智能体说一句话:

**现在的代码≠世界的真相**  
这个仓库的代码,管理员用好多个智能体轮流编程和改bug,时刻要记得:  

当管理员陪着你改bug,改了好几轮也不成功时候, 应该思考,包括但不限于:
- 是不是还有不存在于当前项目内, 但事实存在着的api没有利用起来?   
- 有没有在用的库/api的官方文档可以看看? (以便发现问题1)
- 是不是知识的缺失导致方向/情况的认识不足? (搜索->支撑起问题2)
- 有没有同类开源代码可以参考? 

## 当前状态

> 完整工作台账（每项落地的根因/方案/改动/测试记录）见 **[docs/status.md](docs/status.md)**。
> 细节权威来源：git log > `docs/plan/` 任务书 > 台账。本文件只留导航，不堆流水账。
> 提交约定：**允许提交 = 已测试通过**，台账不设「待手测」状态。

### 里程碑

- ✅ **M1** 全拼 MVP · **M1.5** 候选策略对齐微软 · **M1.6** IMEDIC02 词库 mmap 零加工加载
- ✅ **M2** 用户掌控排序（调权 + 用户词库/自造词/隐藏）——当前核心卖点
- ✅ **M4** 跨平台渲染候选窗 · **M5** 语言栏右键菜单 · **M6** 守护进程 + 设置页
- ◐ **M7** 安装器/x86（daemon 首会话自启 ✅；键位热载 ✅ 2026-08-28——快捷键双槽可配 + 全局热键 + 设置页游戏式录入，`41-keymap-settings.md`）
- ⏸ **M9** 贴图皮肤框架（调研定稿挂起；前置 M8 工具栏已多轮打磨，可重新评估）
- ⬜ **M3** 整句增强(LMDG)/模糊音 · 符号/emoji 候选 · 学习候选

### 活跃事项

- **未开工**：M3 整句增强(LMDG)/模糊音 · 符号/emoji 候选 · 学习候选
- **点子库**：Tab 键用途（`docs/plan/29-tab-ideas.md`，语义分配未定）

### 关键设计决策（防反复横杠）

- **焦点切换永不打断会话**：Esc/Enter/空格上屏或 Ctrl+Space 关闭前不断开（同小狼毫；
  Alt+Tab 期间预编辑保留返回继续；`OnSetFocus` 只隐藏候选窗不 flush）
- **中英切换走系统机制**：OPENCLOSE compartment 真相源，激活初值 = config `initial_state.mode`
- **明确不做**：钉选交互（M2 调权已覆盖）、Tauri/WebView（候选窗用 iuv-ui 自绘）、托盘图标
  （入口 = 语言栏「中/英」右键菜单）、Shift+Space 全角热键、Alt 组合快捷键（WM_SYSKEYDOWN
  不进 TSF 键 sink，机制死路）

## 开发入口

**先读 `docs/plan/01-contract.md`（共享契约，接口唯一权威来源）**，再读对应模块任务书。
执行流程（W0 骨架 → W1 并行实现 → W2 组装）见 `docs/plan/00-overview.md` §3 与 `20-assembly.md`。

## 结构

| 路径 | 说明 |
|---|---|
| `crates/iuv-data` | 词库编译器 dictc + 二进制格式 + Dict 查询层 + 用户库（跨平台） |
| `crates/iuv-core` | 引擎：切分/候选生成/unigram Viterbi/会话状态机/排序管线（跨平台纯 Rust） |
| `crates/iuv-ui` | 候选窗/菜单绘图层：tiny-skia + cosmic-text + Theme（跨平台纯 Rust，M4 已实现） |
| `crates/iuv-repl` | CLI 调试前端（跨平台） |
| `platforms/windows/iuv-tsf` | cdylib：COM/TSF 管线 + 候选窗窗口层（ULW 呈现）+ 语言栏"中/英"切换图标/右键菜单（Windows） |
| `platforms/windows/iuv-win` | Windows 共享层：ULW 呈现（`ulw.rs`）+ 自绘弹窗骨架（`popup.rs` LayeredWindow）+ M6 管道 IPC/共享段（`ipc/`+`shm.rs`，2026-08-21 自 iuv-data 移入） |
| `platforms/windows/iuv-daemon` | 守护进程 exe：唯一持有用户库（共享段+管道 IPC）+ egui 设置页（M6 已实现，纯后台无图标） |
| `platforms/{macos,linux}/` | 占位：IMK / Fcitx5·IBus 适配层 + 门面规划（README，见各目录） |
| `data/` | 下载的词库（gitignore；白霜拼音 GPL-3.0，不入库） |
| `docs/status.md` | 工作状态台账：每项落地的根因/方案/改动/测试记录（AGENTS.md 指向此处） |
| `scripts/` | download-dict / install / uninstall / dev-deploy（热部署） / iuv-common（共享库：提权/日志/ctfmon/延迟清理/Replace-InUseDll） |

## 常用命令

```powershell
cargo check --workspace
cargo test --workspace
cargo build -p iuv-tsf --release
scripts\download-dict.ps1
scripts\install.ps1        # 安装（管理员，自动弹 UAC）
scripts\uninstall.ps1      # 卸载（管理员，自动弹 UAC）
scripts\dev-deploy.ps1     # 热部署：改完代码后免注销生效（默认三路并行构建；-SkipBuild 跳过）
```

## 硬性约定

- 依赖白名单制（`docs/plan/01-contract.md` §2），新增 crate 需主智能体批准
- 文件属主矩阵（`docs/plan/01-contract.md` §6）：并行开发只改自己属主的文件
- iuv-core 保持跨平台纯 Rust；iuv-tsf 内绝不 panic 到宿主进程；测试纪律见 `docs/plan/30-conventions.md`
