# 22 · 任务书 M6：进程分离——守护进程持有用户库 + 设置页

> 状态：**已实现**（2026-08-16 完成；2026-08-17 按用户反馈修正：去托盘、设置窗改主线程、候选窗 BeginDraw 修复）。待手测验收。
> 前置阅读：`00-overview.md`、`01-contract.md`（§3.2 用户库）、
> `18-m2-user-dict.md`（现状多写者）、`19-m4-cross-render.md`、`21-m5-tray-menu.md`。
> 决策记录：用户确认"会话进程只拿词库引用，守护进程持有 + 设置页"（2026-08-16）；
> 入口走语言栏「中/英」按钮右键菜单（2026-08-17，见 `21-m5-tray-menu.md`）。

## 1. 目标（验收一句话）

新增**守护进程**（独立 exe）：唯一持有用户库（IUVUSR02 内存态与写盘）、唯一托管托盘图标
（接管 M5）并提供设置页（egui/eframe）。TSF 会话进程对用户库**只读引用共享内存段**、
**写请求走命名管道 IPC**；守护进程不在线时会话进程**降级**现状（自读文件 + 本地写），
绝不挂键。用户自造词/调权/隐藏在任意应用里生效**即时一致**（不再依赖 mtime 重载）。

## 2. 背景与决策记录

- 现状（18-m2-user-dict.md）：用户库每进程独立内存副本（BTreeMap + 写时复制），各写各的
  文件，靠 mtime 重载**延迟生效**；基本库 mmap 页缓存物理共享，无需守护。
- 用户库写者唯一化：多写者 → 单写者（守护进程），收益 = 即时生效、无写冲突、写盘归零化
  （守护进程聚合写盘，会话进程不再碰用户库文件）。
- **不做 IPC 查询代理**（每键查询走 IPC 在 TSF 实时键路径上不可接受）：会话进程本地查询，
  "引用" = 共享内存只读映射。
- 设置页载体：Tauri 已废（M4 决议）；egui/eframe = 跨平台纯 Rust 控件库（自带按钮/菜单/
  滑块/文本框），守护进程内嵌，与 iuv-ui 自绘栈并存（候选窗/菜单自绘、设置页用控件库）。

## 3. 架构

```
守护进程（platforms/windows/iuv-daemon，独立 exe，纯 Rust）
├── 用户库唯一写者：内存态 UserDict → 写共享段 → 立即写盘（用户库小，替代 2s 聚合）
├── IPC 服务：命名管道（\\.\pipe\iuv-userdict）——会话进程写请求（swap/set/remove/block）
│              + 命令（OpenSettings / Quit，tag 0x06/0x07，不触碰用户库）
├── 入口：语言栏「中/英」按钮右键菜单「设置」（管道 OpenSettings）→ 主线程弹设置页；
│        无托盘图标（2026-08-17 决策）
└── 设置页：egui/eframe 在**主线程**跑（winit 事件循环只能在主线程，独立线程 panic 实测）——
             主题/键位自定义（灰置 M7）/词库管理/直通名单；保存 → 写 config.json + config_epoch 广播

TSF 会话进程（iuv-tsf）
├── 基本库：mmap 只读（现状不变，页缓存共享）
├── 用户库：只读映射共享内存段（布局 + 版本号；版本变化 → 重解析段，替代 mtime 重载）
├── 写请求：命名管道客户端（失败 → 降级本地写路径 + 记日志，绝不 panic）
└── 语言栏菜单：InitMenu 设置/关于；「设置」→ 管道 OpenSettings
```

## 4. 任务清单

| # | 任务 | 状态 |
|---|---|---|
| 1 | iuv-data 共享段扩展 | ✅ iuv-data/src/shm.rs（IUVSHM01 header+版本化+config_epoch，ShmWriter/ShmReader） |
| 2 | iuv-daemon 骨架 | ✅ platforms/windows/iuv-daemon（管道服务/用户库内存态/共享段发布/立即写盘） |
| 3 | 会话进程客户端 | ✅ iuv-tsf/src/daemon_client.rs + iuv-core engine UserMutation/UserRemote + poll + 降级 |
| 4 | 入口（去托盘） | ✅ 语言栏「中/英」按钮右键菜单（InitMenu：设置/关于）；**托盘已删**（21-m5 重定义） |
| 5 | 设置页 | ✅ daemon 主线程 eframe：主题/直通名单/用户库管理；键位自定义灰置（M7）；保存写 config + config_epoch 广播 |
| 6 | 生命周期 | ⏳ 首会话拉起 daemon（CreateProcess）未做——**现状：手动启动或 dev-deploy 拉起**；Quit 管道命令干净退出；崩溃恢复 = 会话降级自读文件；安装器自启归 M7 |
| 7 | 文档同步 | ✅ 本文 + 01-contract/00-overview/AGENTS（见文末变更记录） |

## 5. 已知风险与取舍

- 共享段版本化：布局变更 → 段版本递增，老会话进程读旧版（延迟生效）或强制重连，
  不允许读到半新半旧（版本号与数据区写序：先写数据后 bump 版本）
- IPC 失败语义：写请求丢失 → 会话内降级（调权/隐藏立即失效于本会话但保留内存态）；
  降级优先级高于一切，**绝不挂键/拖慢按键**
- 守护进程崩溃恢复：会话检测管道断开 → 降级自读文件 + 管道重连一次；共享段随最后持有者关闭销毁
- 双进程写 config 竞争：设置页写 config 为唯一写者（会话进程只读 config）
- eframe 0.36 MSRV = 1.95（daemon 单独 `rust-version = "1.95"`；workspace 声明 1.89 是 M4 前下限，不冲突）
- **winit 事件循环只能在主线程**（独立线程 panic，2026-08-17 实测）→ daemon 主线程 =
  命令轮询 + eframe 设置窗；管道/共享段在后台线程
- **首会话自动拉起未实现**（M7）：daemon 需手动启动或 dev-deploy 拉起；未启动时会话走降级路径
- x86：守护进程无需注入宿主，架构独立，无 32 位宿主风险（M7 一并验证）

## 6. 会话进程客户端对接规格（实现参照）

- **读**：`iuv_data::shm::ShmReader::open()` → `version()`/`config_epoch()`/`read() -> Option<UserDict>`；
  会话每键 `poll()` 检测 version 变化 → `Engine::set_user_dict`；config_epoch 变化 → 重载 config.json →
  `Engine::set_config` + `CandwinCandidateWindow::set_theme`
- **写**：`iuv_data::ipc::{PipeClient::connect, Request::Swap/Set/Remove/Block/OpenSettings/Quit, Response}`；
  engine 层抽象 `UserMutation` + `UserRemote` trait（`Engine::set_user_remote`）——写操作构造 mutation，
  远端 `apply` 成功 → 跳过本地写盘、仅内存态更新；失败/离线 → 本地写盘兜底
- **降级**：daemon 离线（ShmReader 打开失败 / 管道连接失败）→ 全部走现状路径（自读文件 + mtime 重载已远端模式关闭），绝不挂键
- **入口**：语言栏「中/英」按钮右键菜单（InitMenu）→ OpenSettings 管道命令 → daemon 主线程弹设置页
- **engine API 变更**：`config()` 由 `&Config` 改 `Config`（克隆快照）；新增 `set_config`/`set_user_dict`/`set_user_remote`

## 7. 槽位

- M7 安装器：守护进程自启注册、卸载清理、词库导入（写库走守护进程 IPC）
- 跨设备同步（18-m2 槽位）：守护进程为天然载体（统一出口）
- macOS/Linux：守护进程概念跨平台（会话/守护分离逻辑在 iuv-data 层），托盘/设置页各平台自写

## 8. DoD（已实现部分）

```
cargo check --workspace / cargo test --workspace     # ✅ 全绿（242 通过）
cargo build -p iuv-daemon --release                  # ✅ 产出 iuv-daemon.exe
cargo build -p iuv-tsf --release                     # ✅ 产出 iuv_tsf.dll（客户端 + 降级）
待手测：语言栏右键「设置」→ daemon 弹设置页（daemon 运行中）；双进程同时输入（词库即时一致）；
守护杀死后打字不挂（降级）；设置页改主题/直通名单即时生效；「关于」对话框
```
