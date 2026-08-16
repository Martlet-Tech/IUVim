//! iuv-ui：跨平台候选窗/菜单绘图层（M4 里程碑核心）。
//!
//! 纯 Rust 光栅栈：tiny-skia 0.12（矢量光栅）+ cosmic-text 0.19（字体发现/整形/布局/字形光栅），
//! 无任何 C 依赖。渲染产出 `Surface`（premultiplied BGRA 像素缓冲，无 stride 填充）——
//! Windows 呈现层（ULW DIB / D2D 等）直接消费像素缓冲；macOS/Linux
//! 平台层自行转格式。
//!
//! 设计约束（见 19-m4-cross-render.md §3 / 30-conventions.md）：
//! - 全部公开函数不 panic：字体缺失返回 0 尺寸、分配失败返回空 Surface，静默降级；
//! - 布局/命中测试/定位为纯函数（自 gdi.rs 迁入，测试断言零改动随迁）；
//! - 主题独立（light/dark 两套内置），调用方（config）决定用哪套。

pub mod layout;
pub mod menu;
pub mod render;
pub mod snapshot;
pub mod text;
pub mod theme;

pub use layout::{
    hit_test, layout, position_for, position_in_area, update_position, Area, Rect, CAND_GAP,
    CARET_GAP, PAD_X, PAD_Y, ROW_GAP,
};
pub use menu::{menu_hit_test, MenuEntry};
pub use render::{render_candidate, render_menu, Surface};
pub use snapshot::{effect_to_snapshot, CaretRect, UiSnapshot};
pub use text::{TextRenderer, FALLBACK_FAMILIES, FONT_PX_96};
pub use theme::{theme_dark, theme_light, Theme};
