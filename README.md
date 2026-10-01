# iuv 输入法（iuvim，谐音"哎哟喂"）

Rust + TSF 的 Windows 中文输入法。  
**用户掌控排序**——静态词频序默认稳定 找回肌肉记忆打字的感觉  
Shift+←/→ 主动调权（绝对值覆盖，反复调整收敛），支持自造词与隐藏。  
尝试研究游戏兼容问题（按键直通 / 候选自绘抑制，设置页可配）。

> 日常开发导航与里程碑台账见 [AGENTS.md](AGENTS.md) 与 [docs/status.md](docs/status.md)；
> 本文件只保留项目概览。

## 当前状态

- [x] M1 全拼 MVP（预编辑 + 候选窗 + 翻页/鼠标交互）· M1.5 候选策略对齐微软 · M1.6 IMEDIC02 平面词库 + mmap 零加工加载
- [x] M2 用户掌控排序：主动调权 + 用户词库/自造词/隐藏
- [x] M4 跨平台渲染候选窗（tiny-skia + cosmic-text 自绘，iuv-ui）· M5 语言栏右键菜单 · M6 守护进程 + 设置页 · M8 工具栏
- [x] rime 管线（pure-Rust 移植 syllabifier/translator/poet）：候选核心已收敛为 rime 唯一引擎（classic 已删，见 `docs/plan/39-rime-pipeline.md`）
- ◐ M7 安装器/x86（server 登录自启、键位热载已落地）
- ◐ M10 薄客户端重构（49 号，`feat/m10-thin-client`）：全系统一个 `iuv-server.exe` + 薄 TSF 客户端，
  四套旧 IPC 收敛为一条长连接三平面（transport + iuv-proto），本地模式已删、远端唯一形态；
  已并 main（2026-09-30，见 `docs/plan/49-thin-client-arch.md`）
- ⏸ M9 贴图皮肤框架（调研挂起）
- ⬜ M3 整句增强（语言模型）/模糊音 · 符号/emoji 候选 · 学习候选

明确不做：Tauri/WebView（候选窗用 iuv-ui 自绘）、托盘图标（入口 = 语言栏菜单）、钉选交互、Shift+Space 全角热键。

## 架构

```
crates/（跨平台纯 Rust）
  iuv-data   词库编译器 dictc + 二进制格式 + Dict 查询层 + 用户库
  iuv-core   引擎：切分/候选生成/组句/会话状态机/排序管线 + rime 核心
  iuv-ui     候选窗/菜单绘图层：tiny-skia + cosmic-text + Theme
  iuv-repl   CLI 调试前端（不注册输入法即可测引擎）
platforms/
  windows/iuv-tsf     cdylib：COM/TSF 管线 + 候选窗窗口层 + 语言栏"中/英"图标/菜单
  windows/iuv-win     Windows 共享层：ULW 呈现 + 弹窗骨架 + 管道 IPC/共享段 + 共享日志
  windows/iuv-server  引擎服务进程 exe：全系统一份（引擎 + 用户库 + 工具栏/桌宠/设置页/全局热键；M10 ② iuv-daemon 并入）
  macos/, linux/      占位（IMK / Fcitx5·IBus，README）
```

运行时数据流（M10 远端唯一形态）：按键 → TSF 薄客户端 → transport 长连接（iuv-proto 契约）→
iuv-server 引擎 `Session::on_key` → 服务端自绘候选窗；断连自动拉起 + ResumeToken 重绑，
不可达窗口期按键透明放行。用户库由 server 独占持有并发布共享段。

## 快速开始

前置：Rust 1.85+，Windows 10/11 x64。

```powershell
cargo check --workspace       # 检查
cargo test --workspace        # 测试
scripts\build.ps1             # 三车道并行构建（x64/x86 TSF ∥ server）
scripts\install.ps1           # 安装（管理员，自动弹 UAC；词库缺失自动下载编译）
scripts\uninstall.ps1         # 卸载（注销一次收尾）
scripts\dev-deploy.ps1        # 热部署：改完代码免注销生效，新开窗口加载新 DLL
```

注册后需在系统设置中将该输入法设为默认，并在高级键设置把「输入法/非输入法切换」热键设为 Ctrl+Space。
使用中英切换的注意事项见 `docs/plan/00-overview.md`。

## 文档

- `docs/plan/00-overview.md` — 总览与架构
- `docs/plan/01-contract.md` — 共享契约（接口唯一权威来源）
- `docs/plan/02-conventions.md` — 全局约定（测试纪律/日志/代码风格）
- `docs/status.md` — 工作状态台账
- `docs/closed/` — 已结案任务书归档

## 许可

代码 MIT。词库（白霜拼音，GPL-3.0）由脚本下载，不进仓库；发布时需附 NOTICE 声明。
