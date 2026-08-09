# Input 输入法（代号暂定）

Rust + TSF 的 Windows 中文输入法。核心卖点（M2 起）：**滞回稳定排序**——词频接近时候选顺序锁定可盲打，
优势累积到阈值才换位。M1（当前里程碑）：最小可用的全拼输入法。

## 当前状态

- [ ] M1 最小 MVP：全拼打字链路（见 `docs/plan/00-overview.md`）
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
| `crates/ime-tsf` | cdylib：COM/TSF 管线 + GDI 候选窗（Windows） |
| `data/` | 下载的词库（gitignore；白霜拼音 GPL-3.0，不入库） |
| `scripts/` | download-dict / install / uninstall / ime-common（install+uninstall 共享库） |

## 常用命令

```powershell
cargo check --workspace
cargo test --workspace
cargo build -p ime-tsf --release
scripts\download-dict.ps1
scripts\install.ps1        # 安装（管理员，自动弹 UAC）
scripts\uninstall.ps1      # 卸载（管理员，自动弹 UAC）
```

## 硬性约定

- 依赖白名单制（`docs/plan/01-contract.md` §2），新增 crate 需主智能体批准
- 文件属主矩阵（`docs/plan/01-contract.md` §6）：并行开发只改自己属主的文件
- ime-core 保持跨平台纯 Rust；ime-tsf 内绝不 panic 到宿主进程；测试纪律见 `docs/plan/30-conventions.md`
