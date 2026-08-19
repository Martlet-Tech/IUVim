# 20 · 集成组装手册（主智能体 · W0/W2）

## 1. W0：骨架与契约冻结（派发前完成）

1. 按 `01-contract.md` §1/§2 建 workspace：根 `Cargo.toml`、四个 crate 的 `Cargo.toml`、`.gitignore`（`/target`、`/data`）
2. **完整实现**（冻结件）：
   - `iuv-data/src/dict.rs`（`Entry`/`Dict` 全查询层 + `from_entries`）与 `lib.rs` re-export
   - `iuv-core/src/{candidate,config,key}.rs` 与 `lib.rs`（模块声明 + re-export）
   - `iuv-tsf/src/ui/mod.rs`（`CaretRect`/`UiSnapshot`/`CandidateUi`/`effect_to_snapshot` 完整实现 + `NullCandidateUi` 桩）
   - `iuv-tsf/src/registration.rs` 中写入 §5.1 全部常量
3. 其余文件建空壳：签名照抄契约，函数体 `todo!()` / `Default::default()`，保证 **`cargo check --workspace` 全绿**
4. 检查属主矩阵（契约 §6）无遗漏文件

## 2. W1：并行派发

用 task 工具同时派发 5 个 general 子智能体，提示词 = 各任务书 §末"子智能体启动提示词"原文。
建议顺序同发；A/B/C 互不依赖，D/E 在 iuv-tsf 内文件不相交（D 若先于 E 完成用 `NullCandidateUi` 顶联调）。

**收单检查**（每个 agent 回报后）：
- 只改了属主矩阵内文件（`git status` 若已初始化；否则对照清单）
- 其 DoD 命令本地复跑一遍
- 回报中"偏离契约之处"必须为无；有则回炉或主智能体裁决改契约并同步他方

## 3. W2：集成构建与冒烟

```powershell
# 1. 全量构建
cargo build --workspace --release

# 2. 词库（需先跑过 download-dict.ps1）
scripts\download-dict.ps1
cargo run -p iuv-data --bin dictc -- data\iuv.imedic `
  data\rime-frost\cn_dicts\8105.dict.yaml data\rime-frost\cn_dicts\41448.dict.yaml `
  data\rime-frost\cn_dicts\base.dict.yaml data\rime-frost\cn_dicts\ext.dict.yaml `
  data\rime-frost\cn_dicts\others.dict.yaml
# 期望：entries ≈ 60万级，无解析错误；文件大小 15~40MB

# 3. REPL 冒烟（断言肉眼级）
cargo run -p iuv-repl -- data\iuv.imedic --batch nihao
# 期望：首候选为 Sentence 且含"你好"；exact 词 "你好/泥嚎…" 按 weight 降序
cargo run -p iuv-repl -- data\iuv.imedic --batch de
# 期望：的/得/地 按 weight 降序
```

## 4. W2：注册与真机手测（需管理员 PowerShell）

```powershell
scripts\install.ps1    # 复制 DLL → %ProgramFiles%\iuv、词典 → %LOCALAPPDATA%\iuv，注册，重启 ctfmon
```
然后 `Win+Space` 切到 **iuv 输入法**，打开记事本按清单手测：

| # | 操作 | 期望 |
|---|---|---|
| 1 | 输入 `nihao` | 预编辑显示 `ni'hao`（拼音分段，微软式），候选窗出现且含"你好"（纯候选列表），窗口在光标下方 |
| 2 | 空格 | "你好"上屏，候选窗消失 |
| 3 | 输入 `de` | 候选 的/得/地 按静态词频序 |
| 4 | `.`/`PageDown`（或 `,`/`PageUp`，keymap 可配）翻页后按 `2` | 上屏第 2 页第 2 个候选 |
| 5 | 输入中 Backspace 到空 | 预编辑与候选窗消失，再按 Backspace 删的是正文（按键放行） |
| 6 | 输入 `abc` 按 Enter | 原文 `abc` 上屏 |
| 7 | 输入中 Esc | 预编辑取消，正文无残留 |
| 8 | 按「输入法/非输入法切换」热键（Ctrl+Space，系统机制） | 中英切换，语言栏图标跟随；再按回中文 |
| 9 | 候选窗显示时切换到别的窗口 | 候选窗消失不残留 |
| 10 | 删掉 `%LOCALAPPDATA%\iuv\iuv.imedic` 重试 | 输入法透明（字母直出），日志有记录，**不卡不死** |
| 11 | 输入 `haoshi` 选"好使"按 Shift+← 两次 | 候选立即重排"好使"置顶，空格上屏；重启进程后序仍在（M2 主动调权） |
| 12 | 逐字选词自造 + Shift+Delete | 自造词随查询出现；Shift+Delete 撤销自造/屏蔽基础库词（M2 二期） |

日志：`%TEMP%\input-iuv-tsf.log` 应有 Activate、引擎加载、commit 记录。

手测常见问题排查：
- 语言列表看不到 → 注册脚本是否管理员运行；ctfmon 是否重启；注册表 `HKCR\CLSID\{C69735F1…}` 是否存在
- 能看到但按键无反应 → 日志里引擎加载是否失败；`OnTestKeyDown` 返回是否被正确吃掉
- 候选窗位置离谱 → `GetTextExt` 失败路径（用上次位置兜底）是否生效

## 5. 收尾

- `scripts\unregister.ps1` 验证注销干净
- 更新 `AGENTS.md` 的"当前状态"节：M1 完成度、已知问题清单
- 向用户交付：演示路径（记事本手测）、M3（整句增强/模糊音）准备就绪确认
