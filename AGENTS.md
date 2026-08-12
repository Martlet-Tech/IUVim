# Input 输入法（代号暂定）

Rust + TSF 的 Windows 中文输入法。核心卖点（M2 起）：**滞回稳定排序**——词频接近时候选顺序锁定可盲打，
优势累积到阈值才换位。M1（当前里程碑）：最小可用的全拼输入法。

## 当前状态

- [x] M1 最小 MVP：全拼打字链路（见 `docs/plan/00-overview.md`）——**已结案**（2026-08-09：手测 1-8 项通过、词库缺失透明模式通过）
  - 已知问题：Alt+Tab 切窗口时未确认的预编辑会残留上屏（TSF 终止 composition 的标准语义，微软拼音同款行为；残留为汉字首选而非拼音原文）——M3+ 或按需处理
  - **已知 bug（2026-08-11，已修）**：续接（选中间级词）后尾巴 commit 失败 `0x8000FFFF (E_UNEXPECTED)`。
    根因：选中间词走「EndComposition 上屏已选词 → 紧接 StartComposition 重建尾巴」，重建的 composition
    被 TSF 在应用（notepad 实测）的下一个 edit session 里终止（日志 `composition 终止通知`），而
    `OnCompositionTerminated` 不清理 → 后续 GetRange/EndComposition 永久失败。
    **修复方案（悬空状态）**：选中间级词不再产生任何 commit 信号——`part_commit` 契约字段删除；
    已选词悬空入栈，预编辑混合显示（`床前ming'yue'guang`），composition 全程单个、只做 set_text 全量更新，
    End→Start 窗口不存在 → bug 不复现；Esc 语义改为有已选词时上屏已选词；`OnCompositionTerminated` 兜底
    清槽+置终止标志，TSF 侧检测后丢弃会话降级重建。改动：session.rs/key.rs（Effect 删 part_commit）、
    session_bridge.rs、composition.rs（sink 共享槽）、text_service.rs（降级）、测试/契约/文档同步。
- 后续：M2 滞回/学习/钉选 · M3 整句增强(LMDG)/简拼/模糊音 · M4 Tauri helper（WebView 候选窗+设置） · M5 安装器/词库导入/x86

## 开发入口

**先读 `docs/plan/01-contract.md`（共享契约，接口唯一权威来源）**，再读对应模块任务书。
执行流程（W0 骨架 → W1 并行实现 → W2 组装）见 `docs/plan/00-overview.md` §3 与 `20-assembly.md`。

## 结构

| 路径 | 说明 |
|---|---|
| `crates/ime-data` | 词库编译器 dictc + 二进制格式 + Dict 查询层 |
| `crates/ime-core` | 引擎：切分/候选生成/unigram Viterbi/会话状态机/排序管线（跨平台纯 Rust） |
| `crates/ime-repl` | CLI 调试前端 |
| `crates/ime-tsf` | cdylib：COM/TSF 管线 + GDI 候选窗 + 语言栏"中/英"切换图标（Windows） |
| `data/` | 下载的词库（gitignore；白霜拼音 GPL-3.0，不入库） |
| `scripts/` | download-dict / install / uninstall / dev-deploy（热部署） / ime-common（共享库：提权/日志/ctfmon/延迟清理/Replace-InUseDll） |

## 常用命令

```powershell
cargo check --workspace
cargo test --workspace
cargo build -p ime-tsf --release
scripts\download-dict.ps1
scripts\install.ps1        # 安装（管理员，自动弹 UAC）
scripts\uninstall.ps1      # 卸载（管理员，自动弹 UAC）
scripts\dev-deploy.ps1     # 热部署：改完代码后免注销生效（默认先构建；-SkipBuild 跳过）
```

## 硬性约定

- 依赖白名单制（`docs/plan/01-contract.md` §2），新增 crate 需主智能体批准
- 文件属主矩阵（`docs/plan/01-contract.md` §6）：并行开发只改自己属主的文件
- ime-core 保持跨平台纯 Rust；ime-tsf 内绝不 panic 到宿主进程；测试纪律见 `docs/plan/30-conventions.md`
