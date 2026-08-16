//! 候选窗/菜单主题：浅色/深色两套内置，颜色对齐原 GDI 常量（gdi.rs 主题段），
//! M4 起 config 可配（`ThemeChoice`），M6 设置页可编辑。

/// 主题色板 + 几何参数。所有颜色为不透明或带 alpha 的 RGBA（`[r, g, b, a]`）。
/// 圆角/阴影按物理像素语义（`scale` 前基准）；`render` 负责乘 DPI scale。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    /// "light" / "dark"
    pub name: &'static str,
    /// 窗口底色 RGBA（圆角外 alpha=0 由 render 处理）
    pub bg: [u8; 4],
    /// 正文
    pub fg: [u8; 4],
    /// 高亮底
    pub hl_bg: [u8; 4],
    /// 高亮字
    pub hl_fg: [u8; 4],
    /// 页码
    pub page_fg: [u8; 4],
    /// 边框
    pub border: [u8; 4],
    /// 阴影色（alpha 为阴影不透明度基准，render 分层递减）
    pub shadow: [u8; 4],
    /// 圆角半径（px，物理像素缩放前，建议 4.0）
    pub corner_radius: f32,
    /// 阴影扩展（px，建议 8.0）
    pub shadow_size: f32,
}

/// 浅色主题：对齐原 gdi.rs 常量（BG 白 / FG 0x1F1F1F / HL 0x0078D7 系）。
pub fn theme_light() -> Theme {
    Theme {
        name: "light",
        bg: [0xFF, 0xFF, 0xFF, 0xFF],      // 背景白
        fg: [0x1F, 0x1F, 0x1F, 0xFF],      // 正文近黑
        hl_bg: [0x00, 0x78, 0xD7, 0xFF],   // 高亮底 #0078D7
        hl_fg: [0xFF, 0xFF, 0xFF, 0xFF],   // 高亮字白
        page_fg: [0x99, 0x99, 0x99, 0xFF], // 页码灰
        border: [0xC0, 0xC0, 0xC0, 0xFF],  // 1px 外框浅灰（白底区分边界）
        shadow: [0x00, 0x00, 0x00, 0x50],  // 淡阴影
        corner_radius: 4.0,
        shadow_size: 8.0,
    }
}

/// 深色主题：bg 0x202020 系、fg 白系、hl 0x0078D7（与浅色一致的高亮语义）、
/// 阴影更浓。
pub fn theme_dark() -> Theme {
    Theme {
        name: "dark",
        bg: [0x20, 0x20, 0x20, 0xFF],
        fg: [0xE6, 0xE6, 0xE6, 0xFF],
        hl_bg: [0x00, 0x78, 0xD7, 0xFF],
        hl_fg: [0xFF, 0xFF, 0xFF, 0xFF],
        page_fg: [0x8A, 0x8A, 0x8A, 0xFF],
        border: [0x3C, 0x3C, 0x3C, 0xFF],
        shadow: [0x00, 0x00, 0x00, 0x8C], // 更浓
        corner_radius: 4.0,
        shadow_size: 8.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn light_and_dark_differ() {
        let light = theme_light();
        let dark = theme_dark();
        assert_eq!(light.name, "light");
        assert_eq!(dark.name, "dark");
        // 视觉关键色必须不同（深色底 + 浅色字）
        assert_ne!(light.bg, dark.bg, "底色不同");
        assert_ne!(light.fg, dark.fg, "正文字色不同");
        assert_ne!(light.page_fg, dark.page_fg, "页码色不同");
        assert_ne!(light.border, dark.border, "边框色不同");
        assert_ne!(light.shadow, dark.shadow, "阴影不同（深色更浓）");
        // 高亮蓝 #0078D7 两套一致（微软系高亮语义统一）
        assert_eq!(light.hl_bg, dark.hl_bg);
        assert_eq!(light.hl_fg, dark.hl_fg);
    }
}
