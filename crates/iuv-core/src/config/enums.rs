//! 引擎配置枚举（P2.1 从 config/mod.rs 拆出）：主题/布局/四态枚举 + 默认值。

/// 候选窗布局方向。键位语义与布局解耦（由 keymap 配置决定）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum Orientation {
    /// 竖排：候选一列从上到下
    #[default]
    Vertical,
    /// 横排：候选单行从左到右
    Horizontal,
}

/// 候选窗/菜单主题（M4 起，见 `docs/plan/19-m4-cross-render.md`）。
/// 呈现层（iuv-tsf candwin.rs）装配时映射到 iuv-ui 的 `theme_light()`/`theme_dark()`。
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
#[derive(Default)]
pub enum ThemeChoice {
    /// 浅色（默认）：白底近黑字（对齐原 GDI 观感）
    #[default]
    Light,
    /// 深色：0x202020 系底 + 浅色字
    Dark,
}

pub use iuv_data::{ImeMode, ImePunct, ImeScript, ImeWidth};
