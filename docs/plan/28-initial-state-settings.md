# 28 · 任务书：设置-常用页 = 新 TSF 实例初始状态（initial_state 结构体）

> 状态：**待手测**（2026-08-19 落地；全角行为同日补做）。前置阅读：`00-overview.md`、`01-contract.md`、`25-settings-tabs.md`、`22-m6-daemon.md`、`30-conventions.md`。
> 背景：用户放弃浮动状态栏（2026-08-19 决策），改为统一「设置→常用」页——该页开关的
> 语义 = **启动一个新 TSF 实例的默认状态**。四个开关（中/英、半角/全角、简/繁、标点）
> 封装为 `initial_state` 结构体，JSON 侧包装在一个父节点下，命名统一为枚举兄弟。
> 全角行为：**已实现**（2026-08-19）——会话外直通路径套 `fullwidth` 转换（详见 §8），
> 仅「简体/繁体」仍后置（随状态栏或独立里程碑）。

## 1. 目标（验收一句话）

设置页「常用」标签提供四组「新 TSF 实例初始状态」开关（模式/宽度/字形/标点）+
候选数量下拉，存 `config.json` 的 `initial_state` 父节点下；TSF Activate 按
`initial_state.mode` 强制设 OPENCLOSE 初值。默认值 = 主流（中文/半角/简体/中文标点），
与现状零行为变化；**半角/全角已生效**（会话外全角转换），简体/繁体**仅存值不生效**
（行为后置，与状态栏一起做）。

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
- **半角/全角 = 可选中并存值，生效**（2026-08-19 决策反转：全角行为落地，见 §8）；**简体/繁体 =
  可选中并存值，不生效**（用户确认）：仅记录默认态，繁体行为后置（随状态栏或独立里程碑）。
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
    // 宽度：half = 半角（默认）/ full = 全角（会话外全角转换，2026-08-19 生效）
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
iuv-core/src/punct.rs
├── chinese_punct / shifted_punct            // 中文标点（原有）
└── fullwidth(c: char) -> Option<char>       // ASCII → 全角（a-z/Ａ-Ｚ/０-９/0x21..0x7E+0xFEE0/空格→U+3000）
    │
iuv-tsf/src/session_bridge.rs
└── fullwidth_pending(english, width, punct, base, shift, caps) -> Option<String>
    // 纯函数：width==Full 才转；英文模式全转（字母大小写=Shift⊕Caps）、中文模式数字/符号/空格
    // 字母除外；中文标点表内符号归标点开关（punct==Chinese 时不接管）
iuv-tsf/src/com/text_service.rs
├── fullwidth_pending_compute(vk, shift, ctrl, alt, session_active)  // 薄接线：组装入参
├── handle_key_down：白名单 → 英文模式(全角命中则 commit 否则放行) → 中文标点 → 全角
└── test_key_down：同序对称（Test 吃 OnKeyDown 必放，防静默吞键）

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
      small: 字形当前仅记录默认值（功能开发中，预留）
    候选数量  下拉框 [5,6,7,8,9]     ← egui ComboBox，page_size
    apply()：组装 DaemonConfig → save_config + 更新 state.config + bump_config_epoch

scripts/install.ps1 + scripts/dev-deploy.ps1：默认配置模板 english_punctuation → initial_state 节点

docs/plan/25-settings-tabs.md、01-contract.md、AGENTS.md：同步
```

## 5. 任务清单

| # | 任务 | 状态 |
|---|---|---|
| 1 | iuv-core：四枚举 + InitialState + Config 字段替换 + 迁移 shim | ✅ |
| 2 | iuv-core lib.rs 导出新类型 | ✅ |
| 3 | iuv-tsf：标点判定 + Activate 默认模式 | ✅ |
| 4 | iuv-daemon：Cargo.toml 加 iuv-core + config.rs（DaemonConfig/load/save 签名重构/迁移） | ✅ |
| 5 | iuv-daemon settings.rs：常用页重排 + apply | ✅ |
| 6 | 脚本模板：install.ps1 / dev-deploy.ps1 | ✅ |
| 7 | 文档同步：25/01/AGENTS | ✅ |
| 8 | 测试：iuv-core 默认值/往返/迁移；daemon load/save/迁移/page_size 钳制 | ✅ |
| 9 | **全角行为**：punct.rs `fullwidth` + session_bridge `fullwidth_pending` + text_service 接线 + 单测 | ✅ |
| 10 | 手测验收 | ⬜ |

## 6. 测试要点

- **iuv-core**：`default_values` 补 `initial_state` 断言；serde 往返；**迁移 shim**：
  老 JSON `{"english_punctuation": true}` → `initial_state.punct == "english"`，
  `false` → `"chinese"`；缺 `initial_state` 节点 → 全默认。
- **iuv-daemon config.rs**：`save_preserves_unknown_fields` 改 `&DaemonConfig` 调用 +
  新字段断言（含 `initial_state` 各枚举）；`load_missing_uses_default` 补默认断言；
  新增旧顶层键迁移测试；新增 `page_size` 越界钳回 5..=9 测试。
- **全角**：iuv-core `punct::fullwidth` 映射单测（字母/数字/符号/空格/非 ASCII）；iuv-tsf
  `fullwidth_pending` 决策单测（半角放行、中文模式数字/符号/标点归属、英文模式全转含 Shift⊕Caps、
  非 ASCII 放行）。
- **settings apply**：手测（daemon 运行中改常用页 → 确定 → 新开 app 验证默认中英）。

## 7. DoD

```
cargo check --workspace && cargo test --workspace   # 全绿
cargo build -p iuv-tsf --release && scripts\dev-deploy.ps1
手测：
1. 默认配置 → 新开 notepad 首态中文（回归零）
2. 设「英文」→ 新开 app 首态英文，Ctrl+Space 可切回
3. 设 page_size=9 → 候选窗每页 9 条
4. 设「全角」→ 中文模式打 123 → `１２３`、`/` → `／`、`,` → `，`（标点不受宽度影响）、
   拼音会话正常（字母照常组句）；英文模式打 abc → `ａｂｃ`；空格 → 全角空格；设「半角」全回退
5. 简体/繁体 → 设置可存可回显，不影响输入行为
6. 升级兼容：旧 config.json（顶层 english_punctuation）→ 加载后标点设置保留
```

## 8. 全角行为（2026-08-19 落地）与已知限制

**语义**：`initial_state.width == Full` 时，**会话外直通路径**套 `fullwidth` 转换（对齐微软实测：
全半角在英文模式也生效）：
- **中文模式**：数字 `0-9` → `０-９`；中文标点表**未收**的符号（`/` `_` 等）→ 全角形；
  中文标点表内符号归标点开关（`punct==Chinese` 时 `，`→`，`、`[`→`【`，宽度不接管）；空格 → `U+3000`；
  **字母不转**（照常进拼音会话）。
- **英文模式**：字母（大小写 = Shift⊕Caps）/数字/符号/空格全转（`ｍｉｃｒｏｓｏｆｔ１２３`）。
- **拼音会话内不转换**（数字键仍选候选）；直通白名单进程优先于全角（完全透明）；
  Ctrl/Alt 组合一律放行（Ctrl+Space 切换 IME 不受影响）。
- **预编辑原文上屏转全角**（影响点 1，2026-08-19）：Enter/无候选空格/flush（关输入法、Alt+Tab）/
  原文兜底候选提交的拼音原文，全角下输出全角（`nihao`→`ｎｉｈａｏ`、`window`→`ｗｉｎｄｏｗ`）；
  候选提交（汉字）不受影响；自造词记录用原文（不录全角）。实现：session `to_output`（读
  `engine.config().initial_state.width`）套 `punct::fullwidth_text`，`all_text()`/`commit_index`
  一处覆盖，TSF 层零改动。
- 实现：`punct::fullwidth`（ASCII→全角，`0x21..=0x7E` 一律 `+0xFEE0` 无例外）+ `session_bridge::fullwidth_pending`
  （纯函数判定）+ text_service `handle_key_down`/`test_key_down` 对称接线 + session 原文上屏转换。

**已知限制与槽位**：
- **简体/繁体**：`script` 仍仅存默认值，行为后置（随状态栏或独立里程碑）。
- 运行时全半角切换热键（Shift+Space）**不做**（2026-08-19 用户决策）：宽度 = 设置页初始状态，
  热载即改即生效。
- 会话内（拼音 composition 期间）的符号/数字不转换——非直通路径，保持引擎语义。
- 非 US 键盘布局符号经 `shifted_punct` 优雅降级（不误吞，命中即转、未命中原样）。
- 状态栏如果后续要做：`initial_state` = 初始态，状态栏开关 = 当前态（运行时），
  两者天然分层，互不干扰。