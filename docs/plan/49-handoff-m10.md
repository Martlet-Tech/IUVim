# M10 交接文档（49 号薄客户端重构，2026-09-28 快照）

> 读者 = 下一个接手会话/智能体。**细节权威来源仍是 `docs/status.md` 台账 + git log**；
> 本文件只做「一口气读完就能继续干活」的交接。分支 `feat/m10-thin-client`，36+7 commits
> 未并 main，工作树干净。

## 一句话现状

P0-P5 全部落地并真机回归通过；② daemon→server 全量迁移（含 UserMutation 接线、
transport 控制面、candwin 消息泵）**代码完成但未真机回归**；剩 ③ 收口。

## 已完成（全部有台账条目，按阶段）

| 阶段 | 内容 | 真机 |
|---|---|---|
| P0-P3 | RTT 实测 → iuv-proto → transport 长连接 → iuv-server 引擎服务 + TSF 薄客户端（A/B 开关 `use_engine_server` 默认 false） | ✅ |
| P4 首切片 | 配置热载改服务端持有（config_watch mtime 监视 + 请求捎带 Push::ConfigChanged）；远端模式按键路径零轮询 | ✅ |
| P4 候选窗 | 服务端自渲染候选窗（`iuv-server/src/candwin.rs`，每连接 UI 线程 + ULW）；KeyOutcome 候选载荷裁撤（抑制名单 `candidate_owner_apps` 命中的连接才带数据源） | ✅ |
| P5 | 失效语义 C+A：断连 → 客户端拉起 server → ResumeToken 重绑 + full 重同步 | ✅（两轮，258ms 自愈） |
| ②首刀 | daemon 设置页用户库面板远端模式从文件重载（server 是文件真相源） | ✅ |
| ② 全量 | **见下「② 迁移」**，**未真机回归** | ⏳ |

本轮三个真 bug 修复（记录在案，避免重查）：
1. `C2S::ImeState` 服务端不回应答 → 四态同步白等 300ms（「间歇漏键」真凶）；
2. transport 句柄值复用：旧连接 drop 后新连接复用句柄值，旧读线程瓜分新连接字节流
   → 帧错乱（收尾协议重构：句柄由读线程关 + 500ms tick + wait_io 丢数据窗口修复）；
3. daemon 惰性拉起被提权 conhost 感染 → 高完整性管道拒中完整性应用（error 5）。

## ② 迁移（本轮 7 子提交，7f912e2..205ce30）——架构现状

```
iuv-server（唯一服务进程）
  ├─ engine（rime）+ 用户库文件真相源（引擎本地写盘）+ SHM 唯一写者（EngineService.shm）
  ├─ candwin（服务端自绘候选窗，消息泵已补齐——悬停漏斗已根治，待真机确认）
  ├─ config_watch（config.json mtime 监视 → engine.set_config + epoch 自增）
  ├─ daemon/（从 iuv-daemon 整体迁入 ~5400 行：toolbar/prefs/tooltip/fullscreen/
  │   window/settings/state/config/hotkey/capture/pet_assets/toolbar_icons/log）
  └─ transport（ConnSender 控制面：pid/tid → S2C::Ctl → C2S::CtlResult，3s 截止）
iuv-tsf（薄客户端）
  ├─ remote 模式：Key/CaretMoved/FocusChanged/ImeState/TypingActivity/UserMutation/
  │   OpenSettings/ToggleToolbar/ToolbarVisibleQuery 全走 transport；
  │   Ctl 经 ctl.rs 提交钩子（最近激活实例端点 PostMessage 应用）
  └─ local 模式（use_engine_server=false）：全保留，main 行为不变
iuv-daemon：已删除（crate/workspace/脚本/文档同步）
```

## ⏳ 未真机回归清单（管理员，dev-dep + m10-deploy 后）

1. 远端打字 + **工具栏看板**（焦点跟随/四态/桌宠——驱动方 daemon → server，最大变化点）；
2. **工具栏按钮/全局热键四态翻转**（transport Ctl 往返，3s 截止）；
3. **语言栏菜单**：设置页打开/工具栏开关/菜单文案（ToolbarVisibleQuery）；
4. **候选窗悬停**：指针应正常（漏斗根除）、hover 高亮生效（此前 hover/穿透实际全死）；
5. 设置页：用户库面板显示、主题热载（服务端窗 SetTheme + 客户端副本双路）；
6. 混合模式调权 → server SHM 发布。

**部署注意**：m10-deploy 自动停历史 daemon 进程；server 由受限计划任务拉起（中完整性）。

## 已知问题 / 接受的边界

- **混合模式文件覆盖**：本地引擎实例经旧 daemon 管道写用户库会用陈旧内存态覆盖文件
  ——根治 = ③ 删本地模式后自然消失（远端实例 mutation 已全部直达 server）。
- 迁移期 iuv-server --lib 的 7 个失败 = 迁入的 daemon config/state 文件 IO 测试
  （本机 os error 5 存量环境问题，非回归）。
- 服务端候选窗**无鼠标点击选词**（UI 线程触达不了连接线程会话；键盘数字选词可用；
  已有 ConnSender 基建，接通属小改动）。
- server Quit 无优雅停机路径（taskkill；Shutdown push 变体未接线）。

## ③ 收口清单（最后一步，建议顺序）

> **2026-09-30 进度**：1/2/3 已完成（③-1 删 A/B 开关与本地路径、③-2 镜像归一沉底
> iuv-data、过渡代码清理 + 死代码清扫——TSF per-实例 ctl 管道、`ipc::signal`/`ctl`/
> `PipeClient/PipeServer`/Request 数据面 11 变体全删）；4（并 main + 文档终稿）待办。

1. ~~真机回归通过后：删 A/B 开关~~ ✅（③-1，2026-09-30）；
2. ~~镜像归一~~ ✅（③-2，proto↔core 平行类型沉底 iuv-data 唯一定义，转换函数全退役，PROTO 1→2）；
3. ~~远端成为唯一形态后：混合模式边界、settings remote_mode 判定等过渡代码清理~~ ✅（随 ③-1/③-2 + 死代码清扫完成）；
4. 并 main（squash 与否管理员定）；AGENTS.md/README/49 号任务书终稿（本次已同步，余并 main）。

## 常用入口

- 构建/部署：`scripts\m10-build.ps1` → `m10-deploy.ps1`（-SkipBuild）→ `m10-uninstall.ps1`
- 日志：`%TEMP%\iuv-tsf.log` / `iuv-server.log` / `iuv-script.log`（`[backend]`/`[resume]`/`[config]`/`[shm]`/`[perf]` 标签）
- 无头测试：`cargo test --workspace`（存量 os error 5 环境红约 30 处，见台账；
  iuv-server --lib 7 失败 = 迁入的 daemon config/state IO 测试，同源）
- 关键文件：`platforms/windows/iuv-server/src/{lib,candwin,config_watch}.rs`、
  `.../src/daemon/**`、`iuv-win/src/transport/{server,client}.rs`、
  `iuv-tsf/src/com/{remote_host,daemon_host,key_routing,dispatch}.rs`、`iuv-tsf/src/ctl.rs`
