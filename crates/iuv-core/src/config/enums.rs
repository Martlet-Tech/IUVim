//! 引擎配置枚举（P2.1 从 config/mod.rs 拆出）：主题/布局/四态枚举 + 默认值。

/// 候选窗布局方向。键位语义与布局解耦（由 keymap 配置决定）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Orientation {
    /// 竖排：候选一列从上到下
    Vertical,
    /// 横排：候选单行从左到右
    Horizontal,
}

impl Default for Orientation {
    fn default() -> Self {
        Orientation::Vertical
    }
}

/// 候选窗/菜单主题（M4 起，见 `docs/plan/19-m4-cross-render.md`）。
/// 呈现层（iuv-tsf candwin.rs）装配时映射到 iuv-ui 的 `theme_light()`/`theme_dark()`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeChoice {
    /// 浅色（默认）：白底近黑字（对齐原 GDI 观感）
    Light,
    /// 深色：0x202020 系底 + 浅色字
    Dark,
}

impl Default for ThemeChoice {
    fn default() -> Self {
        ThemeChoice::Light
    }
}

/// 新 TSF 实例初始模式（中/英）。`initial_state.mode` 驱动 Activate 时 OPENCLOSE 初值
/// （见 `docs/plan/28-initial-state-settings.md`）。中文默认 = 激活即打开（MS IME 同款语义）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InitialMode {
    /// 中文（默认）：激活后输入法为中文模式
    Chinese,
    /// 英文：每个新 TSF 实例从英文模式起（Ctrl+Space 可切回中文）
    English,
}

impl Default for InitialMode {
    fn default() -> Self {
        InitialMode::Chinese
    }
}

/// 新 TSF 实例初始宽度（半角/全角）。半角默认；全角行为后置（仅存默认值）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WidthMode {
    /// 半角（默认）
    Half,
    /// 全角（2026-08-19 起行为已落地：punct.rs fullwidth + 会话 to_output 钩子）
    Full,
}

impl Default for WidthMode {
    fn default() -> Self {
        WidthMode::Half
    }
}

/// 新 TSF 实例初始字形（简体/繁体）。简体默认；繁体生效 = 简体词库 + 运行时简→繁转换
/// （s2t 通用繁体，见 `docs/plan/31-script-traditional.md`；数据文件 `iuv.opencc` 缺失时降级简体输出）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ScriptMode {
    /// 简体（默认）
    Simplified,
    /// 繁体（简体词库 + 运行时简→繁转换）
    Traditional,
}

impl Default for ScriptMode {
    fn default() -> Self {
        ScriptMode::Simplified
    }
}

/// 中文状态标点风格（中文标点/英文标点）。替代旧顶层 `english_punctuation: bool`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PunctMode {
    /// 中文标点（默认，全角：`，`/`。`；主流输入法默认）
    Chinese,
    /// 英文标点（中文状态按标点键直通英文形：`，`→`,`）
    English,
}

impl Default for PunctMode {
    fn default() -> Self {
        PunctMode::Chinese
    }
}

