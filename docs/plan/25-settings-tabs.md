# 25 设置页多标签改造（iuv-daemon 设置窗口）

## 目标

设置页从「单屏堆叠 + 保存/关闭」重构为**多标签页 + 确定/取消/应用**的规范对话框形态，
解决当前设置页布局简陋、功能无分类的问题。纯 UI 重构 + 一个开发工具功能（清除日志），
不新增/改动设置项语义。

## 现状

- 位置：`platforms/windows/iuv-daemon/src/settings.rs`（egui/eframe 0.36，主线程 run_settings）
- 窗口 420×560 可缩放；无标签页；底部「保存 / 关闭」
- 一屏堆叠：主题、直通名单、键位自定义（灰置占位）、用户库管理
- 「清除全部」用户库立即落盘（与取消语义冲突）

## 改造方案

### 1. 窗口 / 标题栏（settings.rs `run_settings`）

```rust
.with_inner_size([1024.0, 800.0])
.with_min_inner_size([1024.0, 800.0])
.with_max_inner_size([1024.0, 800.0])   // 锁死 1024×800
.with_resizable(false)                   // 禁最大化/拉伸
.with_maximize_button(false)             // 标题栏只剩 最小化 + 关闭
```

egui 0.36 API 确认：`with_resizable`/`with_maximize_button`/`with_inner_size` 均存在。

### 2. 多标签页

```rust
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab { Common, Keymap, Appearance, Dict, Advanced, Dev }
// title(): 常用 / 按键 / 外观 / 词库 / 高级 / 开发者
// ALL: 全部标签；Dev 仅 dev 构建（见 §4）
```

- 顶部 `egui::Panel::top`（本分支 egui 0.36 的 TopBottomPanel 已更名 Panel）：横向 `selectable_label(选中, RichText::new(title).size(20.0))` 大号字体切换
- 内容区 `CentralPanel` 按 `self.tab` 分派渲染
- 面板 `show` 收 `&mut Ui`（root Ui 传入，非 `&Context`）

### 3. 底部公共按钮（确定 / 取消 / 应用）

`egui::Panel::bottom`，右对齐 `Layout::right_to_left(Align::Center)`：

| 按钮 | 语义 |
|---|---|
| 确定 | apply + 关窗 |
| 取消 | 丢弃未保存改动 + 关窗（`state.config` 不动、不 bump epoch） |
| 应用 | apply + 保持开窗 |

`apply()` = 写 config.json（theme + passthrough）+ bump config_epoch +（若 pending_clear）清空用户库并落盘。

### 4. 标签内容（移动现有项 + 开发者）

| 标签 | 内容 | 来源 |
|---|---|---|
| 常用 | 空占位 | 新（框架预留） |
| 按键 | 键位自定义（灰置 M7 占位） | 现 UI 移入 |
| 外观 | 候选窗主题（浅色/深色单选） | 现 UI 移入 |
| 词库 | 用户库列表 + 清除全部（确认框保留） | 现 UI 移入 |
| 高级 | 按键直通应用名单（多行编辑） | 现 UI 移入 |
| 开发者 | **清除日志按钮**（仅 dev 构建） | 新 |

**dev 构建门控**（2026-08-18 用户决策：feature 门控）：
- `Cargo.toml` 加 `[features] dev = []`
- 代码 `#[cfg(any(debug_assertions, feature = "dev"))]`：debug 构建自动带；release 显式 `--features dev` 带；发布构建（无 feature + `--release`）不带
- 理由：dev-deploy.ps1 走 `--release`，纯 `debug_assertions` 会让已部署 daemon 永远看不到开发者标签

### 5. 保存语义调整（词库清除改「暂挂」）

现「清除全部」立即落盘，与「取消」语义冲突。改为：

1. 词库页点「清除全部」→ 确认框 → 只置 `pending_clear = true`（不触碰 dict、不落盘）
2. 点**确定/应用**才真正执行清空（`UserDict::empty` → `publish()` → `flush_now()`）
3. 点**取消**则什么都没发生

### 6. 开发者 - 清除日志

`log.rs` 新增：

```rust
/// truncate 清空 %TEMP% 下 4 个 iuv 日志；返回 (成功数, 失败数)。
/// 失败多为日志文件被活跃进程占用（TSF/脚本瞬时持有），只计数不报错。
pub fn clear_logs() -> (usize, usize)
```

清除清单（`%TEMP%`）：
- `input-iuv-daemon.log`（守护进程）
- `iuv-tsf.log`（TSF 会话进程）
- `iuv-script.log`（install/dev-deploy 脚本）
- `iuv-cleanup.log`（延迟清理计划任务）

UI：开发者页按钮 + 状态反馈（"已清 N 个，M 个被占用"）。

### 7. dev-deploy.ps1 配合

第 1 步构建追加 daemon：`cargo build -p iuv-daemon --release --features dev`
（保证热部署的 daemon 带开发者标签；发布安装流程不带 feature，标签消失）。

## 改动文件

| 文件 | 改动 |
|---|---|
| `platforms/windows/iuv-daemon/src/settings.rs` | 主体重构（窗口/标签/按钮/移动内容/清除日志 UI/pending_clear） |
| `platforms/windows/iuv-daemon/src/log.rs` | 新增 `clear_logs()` |
| `platforms/windows/iuv-daemon/Cargo.toml` | `[features] dev = []` |
| `scripts/dev-deploy.ps1` | daemon 构建加 `--features dev` + 文案 |

## 测试

- 构建矩阵：
  - `cargo build -p iuv-daemon`（debug）→ 有开发者标签
  - `cargo build -p iuv-daemon --release`（无 feature）→ 无开发者标签
  - `cargo build -p iuv-daemon --release --features dev` → 有开发者标签
- `cargo test --workspace` 全绿（config 测试不受影响；settings 无既有测试）
- 手测：语言栏「中/英」右键菜单 → 设置
  1. 窗口固定 1024×800、标题栏无最大化、可最小化/关闭
  2. 6 标签（dev 构建）/ 5 标签（release）大字号切换、内容分派正确
  3. 主题改深色 → 应用（窗不关，候选窗主题已变）→ 确定关窗；重开已持久化
  4. 直通名单改 → 取消 → 未生效（config.json 不变）
  5. 词库「清除全部」确认 → 确定/应用才清空；取消不清
  6. 开发者 → 清除日志 → 4 个 %TEMP% 日志被清（占用者计失败），状态文本正确
  7. 取消/确定关窗后 daemon 继续常驻（主循环日志正常）
