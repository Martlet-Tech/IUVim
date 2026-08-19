# 28 · 任务书：设置-常用页 = 新 TSF 实例初始状态（initial_state 结构体）

> 状态：**待实现**（2026-08-19 定稿）。前置阅读：`00-overview.md`、`01-contract.md`、`25-settings-tabs.md`、`22-m6-daemon.md`、`30-conventions.md`。
> 背景：用户放弃浮动状态栏（2026-08-19 决策），改为统一「设置→常用」页——该页开关的
> 语义 = **启动一个新 TSF 实例的默认状态**。四个开关（中/英、半角/全角、简/繁、标点）
> 封装为 `initial_state` 结构体，JSON 侧包装在一个父节点下，命名统一为枚举兄弟。

## 1. 目标（验收一句话）

设置页「常用」标签提供四组「新 TSF 实例初始状态」开关（模式/宽度/字形/标点）+
候选数量下拉，存 `config.json` 的 `initial_state` 父节点下；TSF Activate 按
`initial_state.mode` 强制设 OPENCLOSE 初值。默认值 = 主流（中文/半角/简体/中文标点），
与现状零行为变化；半角/全角、简体/繁体**仅存值不生效**（行为后置，与状态栏一起做）。

## 2. 决策记录

- **不做浮动状态栏**（2026-08-19 用户决策）：主流输入法那种可拖动工具条先不做；
  先统一设置-常用页的开关语义 = 新 TSF 实例默认状态。
- **结构体封装**（2026-08-19 用户决策）：四个开关封装为 `InitialState` 结构体，
  JSON 侧包装在 `initial_state` 父节点下；命名统一为 lowercase 枚举
  （`mode`/`width`/`script`/`punct`），杜绝 `default_english`/`full_width` 这类
  平级 bool 的「非兄弟」命名。
- **默认值 = 主流**（用户确认）：中文/半角/简体/中文标点，不改变现有默认行为；
  用户个人习惯（英文标点开启）在设置页自设。
- **daemon 复用 iuv-core 类型**（用户确认）：daemon `Cargo.toml` 增加 `iuv-core`
  依赖，直接使用 `InitialState` 与四个枚举（单一事实源），不做平行类型。
- **中/英默认 = 每次激活强制设默认**（用户确认）：Activate 时把 OPENCLOSE
  compartment 强制设为配置默认（现状「激活即中文」的推广）。中文默认 = 现状完全一致；
  英文默认 = 每个新 TSF 实例从英文起。
- **半角/全角、简体/繁体 = 可选中并存值，不生效**（用户确认）：仅记录默认态，
  全角转换/繁体行为后置（随状态栏或独立里程碑）。
- **旧配置迁移**：顶层 `english_punctuation: bool` 键迁移为 `initial_state.punct`
  枚举（bool→"chinese"/"english"），升级不丢设置。
- `page_size`（候选数量）保持**顶层不动**（非「初始状态」，属页面结构配置；
  设置页下拉 [5,6,7,8,9]，daemon 侧越界钳回 5..=9）。

## 3. 目标 JSON 结构

```jsonc
{
  // 新 TSF 实例初始状态（2026-08-19 起，替换旧顶层 english_punctuation）
  "initial_state": {
    // 模式：chinese = 中文（默认）/ english = 英文
    "mode": "chinese",
    // 宽度：half = 半角（默认）/ full = 全角（仅存值，行为后置）
    "width": "half",
    // 字形：simplified = 简体（默认）/ traditional = 繁体（仅存值，行为后置）
    "script": "simplified",
    // 标点：chinese = 中文标点（默认，全角）/ english = 中文状态使用英文标点
    "punct": "chinese"
  },
  // 每页候选数（默认 5；建议 ≤9 保证数字键可全选当前页）——保持顶层不动
  "page_size": 5
}
```

## 4. 架构

```
iuv-core/src/config/mod.rs
├── enum InitialMode { Chinese, English }        // serde lowercase + Default
├── enum WidthMode { Half, Full }
├── enum ScriptMode { Simplified, Traditional }
├── enum PunctMode { Chinese, English }
├── struct InitialState { mode, width, script, punct }
│   Default = Chinese / Half / Simplified / Chinese（主流默认）
└── Config.english_punctuation: bool ─删除─→ Config.initial_state: InitialState
    from_file：JSON → Value → 迁移 shim（旧顶层 english_punctuation → initial_state.punct）→ from_value

iuv-core/src/lib.rs：pub use config::{InitialState, InitialMode, WidthMode, ScriptMode, PunctMode}

iuv-tsf/src/com/text_service.rs
├── :393 标点判定：engine.config().english_punctuation → initial_state.punct == PunctMode::English
└── Activate（:666-682）：default_open = Config::load().initial_state.mode == InitialMode::Chinese
    强制写 OPENCLOSE compartment + apply_openclose(default_open)

iuv-daemon/Cargo.toml：+ iuv-core = { workspace = true }
iuv-daemon/src/config.rs
├── DaemonConfig.english_punctuation: bool ─删除─→ initial_state: iuv_core::InitialState
├── load_config：读 initial_state 节点；缺失 → 兼容旧顶层 english_punctuation 键
└── save_config：签名重构 5 参数 → save_config(cfg: &DaemonConfig)；
    写 initial_state 节点 + 删除旧 english_punctuation 键

iuv-daemon/src/settings.rs
├── SettingsApp.english_punct: bool ─删除─→ initial: InitialState
└── Tab::Common 重排：
    初始状态
      模式  (•)中文  ( )英文
      [ ] 中文状态使用英文标点        ← checkbox 绑 initial.punct == PunctMode::English
      宽度  (•)半角  ( )全角
      字形  (•)简体  ( )繁体
      small: 字形与全角当前仅记录默认值（功能开发中，预留）
    候选数量  下拉框 [5,6,7,8,9]     ← egui ComboBox，page_size
    apply()：组装 DaemonConfig → save_config + 更新 state.config + bump_config_epoch

scripts/install.ps1 + scripts/dev-deploy.ps1：默认配置模板 english_punctuation → initial_state 节点

docs/plan/25-settings-tabs.md、01-contract.md、AGENTS.md：同步
```

## 5. 任务清单

| # | 任务 | 状态 |
|---|---|---|
| 1 | iuv-core：四枚举 + InitialState + Config 字段替换 + 迁移 shim | ⬜ |
| 2 | iuv-core lib.rs 导出新类型 | ⬜ |
| 3 | iuv-tsf：标点判定 + Activate 默认模式 | ⬜ |
| 4 | iuv-daemon：Cargo.toml 加 iuv-core + config.rs（DaemonConfig/load/save 签名重构/迁移） | ⬜ |
| 5 | iuv-daemon settings.rs：常用页重排 + apply | ⬜ |
| 6 | 脚本模板：install.ps1 / dev-deploy.ps1 | ⬜ |
| 7 | 文档同步：25/01/AGENTS | ⬜ |
| 8 | 测试：iuv-core 默认值/往返/迁移；daemon load/save/迁移/page_size 钳制 | ⬜ |
| 9 | 手测验收 | ⬜ |

## 6. 测试要点

- **iuv-core**：`default_values` 补 `initial_state` 断言；serde 往返；**迁移 shim**：
  老 JSON `{"english_punctuation": true}` → `initial_state.punct == "english"`，
  `false` → `"chinese"`；缺 `initial_state` 节点 → 全默认。
- **iuv-daemon config.rs**：`save_preserves_unknown_fields` 改 `&DaemonConfig` 调用 +
  新字段断言（含 `initial_state` 各枚举）；`load_missing_uses_default` 补默认断言；
  新增旧顶层键迁移测试；新增 `page_size` 越界钳回 5..=9 测试。
- **settings apply**：手测（daemon 运行中改常用页 → 确定 → 新开 app 验证默认中英）。

## 7. DoD

```
cargo check --workspace && cargo test --workspace   # 全绿
cargo build -p iuv-tsf --release && scripts\dev-deploy.ps1
手测：
1. 默认配置 → 新开 notepad 首态中文（回归零）
2. 设「英文」→ 新开 app 首态英文，Ctrl+Space 可切回
3. 设 page_size=9 → 候选窗每页 9 条
4. 半角/全角、简体/繁体 → 设置可存可回显，不影响输入行为
5. 升级兼容：旧 config.json（顶层 english_punctuation）→ 加载后标点设置保留
```

## 8. 已知限制与槽位

- 全角转换行为（`punct::fullwidth` + 直通路径套转换）与繁体实现：**后置**，
  与浮动状态栏一并立项；`width`/`script` 仅存默认值。
- 状态栏如果后续要做：`initial_state` = 初始态，状态栏开关 = 当前态（运行时），
  两者天然分层，互不干扰。
- daemon 新增 iuv-core 依赖：workspace 内首方 crate，无第三方新增。