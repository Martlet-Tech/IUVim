# 50 号交接：品质检查修复进度（下一个智能体从这里接手）

> 写于 2026-10-02，因会话点数耗尽交接。全景与问题清单见 `50-quality-audit.md`。

## 已完成（已提交 main）

| 提交 | 内容 |
|------|------|
| `389cf6f` | 全仓 fmt 清 88 处 + transport 测试管道名修复 + 50 号检查文档 |
| `1dd937d` | 高危 H1-H5 五连修（真机已回归：引号交替/复位/清库不复活，log 实证） |
| `5c38d68` | transport T1-T5（odd 段/烧号/写保护/clone 分叉/双重 CloseHandle）+ server S1（catch-all Err/panic hook/resume 清扫/清日志目标） |

## 工作区改动（⚠️ 未提交，验证已过，可直接提交）

**内容 = 死代码清扫 D1-D3**（50 号 §3）：
- D1: 删 `iuv-core/src/viterbi.rs` 整模块 + `OOV_PENALTY`（rime poet 取代；LmProvider trait 保留，rime 在用）
- D2: server 死链——`quit_flag`/`close_settings`/`current_version`/`bump_config_epoch`/`ToolbarHost::shutdown` 删除，`handle_request(_req)` → `toggle_visible()`，`show_near` 死参数删除，`empty_sprites` 收私有，设置页「已广播 config_epoch」撒谎文案修正
- D3: **tsf 本地候选窗整链退役**——删 `ui/candwin.rs`（584 行）+ `CandidateUi` trait + `render_locally`/跳变检测/`daemon_poll_tick`/主题收敛 + `examples/candwin_demo.rs`；`apply_effect` 去掉 ui/orientation 参数；净 -1188 行
- 验证状态：fmt 干净、clippy 0 警告、测试 518 过/3 败（3 败 = shm 三测试，本机环境固有，见下）
- **接手第一件事**：`git add -A && git commit`（建议信息：`refactor: 死代码清扫 D1-D3——viterbi 退役 + server daemon 死链 + tsf 本地候选窗整链（50 号 §3）`）

## 接下来做什么（按 50 号 §5 顺序）

1. 提交上述工作区改动
2. **S2 剩余中危**（50 号 §2 剩余项）：toolbar UI 线程 3s 同步 dispatch（对齐 candwin.rs:551 短命线程先例）、tsf 重连重试无差别、降级重连窗 reviving 闸吞 Activate、`Activate` 中途失败无回滚、`deactivate()` 不清服务端会话
3. **S4 文档对账**：`docs/plan/00-overview.md` 与 `docs/knowledge/tsf-interaction.md` 仍描述 daemon 旧架构（误导源之首）、README iuv-win 职责、Cargo.toml 注释里的 iuv-daemon、.gitignore /target-daemon
4. **部署真机回归一次**（这批全提交后）：`powershell -File "D:/Projects/input/iuvim/scripts/dev-deploy.ps1"`（需 UAC，日志 `%TEMP%\iuv-script.log`），确认打字/候选/工具栏全通——D3 删的是不可达路径，预期零行为变化，重点看回归有无意外
5. 全部完成后把 50 号状态行改为「结案」，参照仓库惯例补 status.md 台账正文

## 环境硬知识（省得重新踩坑）

- **本机 Low IL 进程围栏**：镜像在 `D:\Projects\input` 下的进程 = Low 完整性，写树外/内核命名对象全拒，管理员免疫。跑测试必须 `TMP/TEMP` 指树内：`$env:TMP="$PWD\target\tmp"; $env:TEMP=$env:TMP`（bash: `TMP='D:\Projects\input\iuvim\target\tmp' TEMP=同值 cargo test ...`）。shm 三测试（iuv-win lib）需管理员终端单独跑，其余全绿
- **postcard 纪律**：C2S/S2C/ProtoError 新变体必须**追加枚举末尾**（ProtoError::Unsupported 本次就这么加的）
- **TSF 纪律**：COM 回调绝不 panic 到宿主（extern "system" 穿透 = 0xC0000409）；改动注意 Test/Down 双跑对称（route_key 每键跑两次）
- **部署**：dev-deploy.ps1 绝对路径调用；新 DLL 只在新进程生效，旧进程靠 client revive 重连；git bash 下 `scripts\\` 会碎路径，用正斜杠绝对路径
- **git**：main 直接开发（用户惯例：说「提交吧」才提交，说「开新分支」才开分支）；当前目录 `D:\Projects\input` 不是 git 仓库，只有 `iuvim/` 是

## 已知未修（审查发现但明确不动的）

- `RemoteHandle::config_epoch` 客户端侧仅测试用（消费者随 D3 退役，推送泵本身保留——服务端 EngineSession 还依赖 config_epoch）带 `#[cfg_attr(not(test), allow(dead_code))]`
- `iuv-repl` `run_script` 非测试构建死代码（原有注解，属可接受欠账）
- tsf `session_bridge.rs:109` TODO 用户自定义按键映射（38 号任务书真欠账，勿删）
