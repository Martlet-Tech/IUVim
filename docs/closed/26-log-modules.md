# 26 日志分模块使能（设置页开关）

## 背景

一次手测（~200 键）产出 27,945 行 / 2.3MB 的 `iuv-tsf.log`（用户报告近 19 万行）。
根因三叠加：
1. `max_candidates=1024`（iuv-core 默认）→ 候选全量进 TSF UIElement，"wo" 一类输入一次出 100~218+ 条候选（均值 243）；
2. Windows Terminal 每次候选更新把整张候选表逐条 `GetString(i)` 拉一遍（TSF UIElement 标准行为，全量暴露是为游戏 IMM 桥）；
3. `ui_element.rs:300` 每次 GetString 记一行日志 → 每键 200+ 行（本会话 26,727/27,945 行 = GetString，占 95.6%）。

无空闲轮询循环（GetString 数/候选数 ≈ 候选更新次数）。候选量大本身是设计行为（微软对齐全量候选），不动。

## 目标

**日志分模块使能**：config.json 记录禁用模块列表（denylist），TSF 与 daemon 两侧 `log_line` 按消息 `[tag]` 过滤；
设置页「开发者」标签提供每个模块的开关。**默认全开 = 现状零变化**；勾掉某模块即静音。

## 决策（2026-08-18 用户确认）

- 开关放**开发者标签**（dev 构建才有；release 无 UI，沿用 config 默认/已有设置）。
- **denylist 语义**：`disabled_log_modules: Vec<String>`，字段缺省 = 全记录。
- 过滤只按消息前缀 `[tag]` 匹配；无 tag 的日志恒记录（均为低音量事件）。

## 实现

### 1. iuv-core `Config` 加字段（新增配置项唯一入口）

`crates/iuv-core/src/config/mod.rs`：
```rust
/// 禁用日志模块列表（denylist；默认空 = 全记录）。Windows 平台 TSF/daemon 消费：
/// log_line 按消息 `[tag]` 匹配，命中即静音。跨平台字段仅为共享 config.json 语义。
pub disabled_log_modules: Vec<String>,
```
默认 `Vec::new()`。

### 2. 日志过滤（两条 log.rs 同款）

`platforms/windows/iuv-tsf/src/log.rs` 与 `platforms/windows/iuv-daemon/src/log.rs`：

```rust
static DISABLED: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

/// 替换禁用模块集（配置加载/热载调用；清空 = 全记录）。
pub fn set_log_modules_disabled(modules: &[String]) { ... }

fn module_disabled(msg: &str) -> bool {
    // 快路径：禁用集为空 → 全放行（日常零开销）。
    // 提取前缀 [tag]；无 tag 恒放行。
}
```
`log_line` 开头：`if module_disabled(msg) { return; }`。

### 3. tag 补齐（仅热路径，其余不动）

- `com/text_service.rs:380` 按键行 → `[key] 按键：…`
- `session_bridge.rs:145` commit → `[commit] commit：…`（失败行同 tag）

### 4. TSF 装配

`com/text_service.rs`：
- 引擎加载处（~96，`Engine::new(dict, Config::load())`）：把 Config 提出来，同处调 `log::set_log_modules_disabled(&cfg.disabled_log_modules)`。
- `apply_config_hot_reload`（~433，config_epoch 触发）：同处调 `set_log_modules_disabled`。

### 5. daemon 装配

- `config.rs`：`DaemonConfig` 加 `disabled_log_modules: Vec<String>`；`load_config` 解析；`save_config` 补丁写入（保留未知字段）。
- `main.rs`（~59）：启动 `load_config` 后调 `log::set_log_modules_disabled`。
- `settings.rs` apply（~378）：`save_config` 传入新禁用集 + 调 `set_log_modules_disabled`（本进程即时生效）+ bump epoch（TSF 侧热载）。

### 6. 设置页 UI（开发者标签）

`settings.rs` `dev_tab()` 新增「日志模块」区：
- 模块目录（常量列表）：`uielem, key, commit, caret, candwin, menuwin, daemon, main, pipe, settings, state`
  （`immdetect` 已于 2026-08-20 移除——自绘窗抑制改 `candidate_owner_apps` 名单驱动，无矩形探测日志）
- 每个模块一个 `ui.checkbox(启用, 名称 + 说明)`；勾选 = 记录（= 不在禁用集），默认全勾。
- 存 `SettingsApp` 本地 `disabled: Vec<String>`（与目录补集）；apply 时并入保存。
- 附一行说明：改动点「确定/应用」生效，TSF 侧经 config_epoch 热载。

## 改动文件

| 文件 | 改动 |
|---|---|
| `crates/iuv-core/src/config/mod.rs` | `disabled_log_modules` 字段 + 测试 |
| `platforms/windows/iuv-tsf/src/log.rs` | 过滤 + set 函数 |
| `platforms/windows/iuv-tsf/src/com/text_service.rs` | `[key]` tag；两处装配 |
| `platforms/windows/iuv-tsf/src/session_bridge.rs` | `[commit]` tag |
| `platforms/windows/iuv-daemon/src/log.rs` | 过滤 + set 函数 |
| `platforms/windows/iuv-daemon/src/config.rs` | 字段 load/save + 测试 |
| `platforms/windows/iuv-daemon/src/settings.rs` | 开发者页日志模块开关区 |
| `platforms/windows/iuv-daemon/src/main.rs` | 启动应用禁用集 |

## 测试

- `cargo test --workspace` 全绿（新增：core Config 字段解析、daemon config 存取、log 过滤单元测试）。
- 构建：`cargo build -p iuv-daemon --release --features dev`（开发者页含开关区）。
- 手测：设置 → 开发者 → 关 uielem → 确定 → 打字 → iuv-tsf.log 无 `[uielem]` 行、其余照常；开回 → 恢复。