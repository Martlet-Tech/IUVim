# iuv 输入法（iuvim，谐音"哎哟喂"）

Rust + TSF 的 Windows 中文输入法。  
**用户掌控排序**——静态词频序默认稳定 找回肌肉记忆打字的感觉  
Shift+←/→ 主动调权（绝对值覆盖，反复调整收敛），支持自造词与隐藏。  
尝试研究游戏兼容问题.

## 当前状态

- [x] M1 最小 MVP：全拼打字链路（预编辑 + GDI 候选窗 + 翻页/鼠标交互），已结案
- [x] M1.5 候选策略对齐微软：单字/全拼/简拼/混拼三路路由
- [x] M1.6 IMEDIC02 平面词库 + mmap 零加工加载（冷加载 ~70ms，125 万条词库实测）
- [x] M2 用户掌控排序：主动调权 + 用户词库/自造词/隐藏，已结案
- 下一步：M3 整句增强（语言模型）/模糊音 · M4 Tauri helper（WebView 候选窗 + 设置） · M5 安装器/词库导入/x86

## 架构

```
crates/（跨平台纯 Rust）
  iuv-data   词库编译器 dictc + 二进制格式 + Dict 查询层
  iuv-core   引擎：切分/候选生成/Viterbi/会话状态机/排序管线
  iuv-repl   CLI 调试前端（不注册输入法即可测引擎）
platforms/
  windows/iuv-tsf   cdylib：COM/TSF 管线 + GDI 候选窗（Windows）
  macos/, linux/    占位（IMK / Fcitx5·IBus，README）
```

运行时数据流：按键 → TSF → session_bridge 映射为 `iuv_core::Key` → `Session::on_key` → 预编辑/上屏 + 候选窗更新。

## 快速开始

前置：Rust 1.85+，Windows 10/11 x64。

```powershell
cargo check --workspace       # 检查
cargo test --workspace        # 测试
cargo build -p iuv-tsf --release
scripts\download-dict.ps1     # 下载词库（白霜拼音，GPL-3.0，不入库）
scripts\install.ps1           # 安装（管理员，自动弹 UAC）
scripts\uninstall.ps1         # 卸载
scripts\dev-deploy.ps1        # 热部署：改完代码免注销生效
```

注册后需在系统设置中将该输入法设为默认，并在高级键设置把「输入法/非输入法切换」热键设为 Ctrl+Space。
使用中英切换的注意事项见 `docs/plan/00-overview.md`。

## 文档

- `docs/plan/00-overview.md` — 总览与架构
- `docs/plan/01-contract.md` — 共享契约（接口唯一权威来源）
- `docs/plan/` 其余文件 — 各模块任务书与里程碑设计

## 许可

代码 MIT。词库（白霜拼音，GPL-3.0）由脚本下载，不进仓库；发布时需附 NOTICE 声明。
