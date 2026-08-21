//! 候选窗/菜单主题：浅色/深色两套内置，颜色对齐原 GDI 常量（gdi.rs 主题段），
//! M4 起 config 可配（`ThemeChoice`），M6 设置页可编辑。

/// 主题色板 + 几何参数。所有颜色为不透明或带 alpha 的 RGBA（`[r, g, b, a]`）。
/// 圆角按物理像素语义（`scale` 前基准）；`render` 负责乘 DPI scale。
/// 扁平风格：无阴影，边界由 `border` 细边框承担（宽度 render 按 DPI 取 1/2px）。
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
    /// 悬停高亮框（纯视觉，不驱动会话——鼠标悬停候选时的虚线框；与真高亮**叠加**显示）
    pub hover_border: [u8; 4],
    /// 页码
    pub page_fg: [u8; 4],
    /// 边框
    pub border: [u8; 4],
    /// 圆角半径（px，物理像素缩放前，建议 4.0）
    pub corner_radius: f32,
}

/// 浅色主题：对齐原 gdi.rs 常量（BG 白 / FG 0x1F1F1F / HL 0x0078D7 系）。
pub fn theme_light() -> Theme {
    Theme {
        name: "light",
        bg: [0xFF, 0xFF, 0xFF, 0xFF],      // 背景白
        fg: [0x1F, 0x1F, 0x1F, 0xFF],      // 正文近黑
        hl_bg: [0x00, 0x78, 0xD7, 0xFF],   // 高亮底 #0078D7
        hl_fg: [0xFF, 0xFF, 0xFF, 0xFF],   // 高亮字白
        hover_border: [0x40, 0x40, 0x40, 0xFF], // 悬停虚线框：深灰——白底与高亮蓝底上都可见
        page_fg: [0x99, 0x99, 0x99, 0xFF], // 页码灰
        border: [0xC0, 0xC0, 0xC0, 0xFF],  // 细边框浅灰（白底区分边界）
        corner_radius: 4.0,
    }
}

/// 深色主题：bg 0x202020 系、fg 白系、hl 0x0078D7（与浅色一致的高亮语义）。
pub fn theme_dark() -> Theme {
    Theme {
        name: "dark",
        bg: [0x20, 0x20, 0x20, 0xFF],
        fg: [0xE6, 0xE6, 0xE6, 0xFF],
        hl_bg: [0x00, 0x78, 0xD7, 0xFF],
        hl_fg: [0xFF, 0xFF, 0xFF, 0xFF],
        hover_border: [0xC8, 0xC8, 0xC8, 0xFF], // 悬停虚线框：亮灰——深底与高亮蓝底上都可见
        page_fg: [0x8A, 0x8A, 0x8A, 0xFF],
        border: [0x3C, 0x3C, 0x3C, 0xFF],
        corner_radius: 4.0,
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
        // 高亮蓝 #0078D7 两套一致（微软系高亮语义统一）
        assert_eq!(light.hl_bg, dark.hl_bg);
        assert_eq!(light.hl_fg, dark.hl_fg);
        // 悬停虚线框 ≠ 窗口边框（可辨识）
        assert_ne!(light.hover_border, light.border, "悬停框色与窗口边框不同");
        assert_ne!(light.hover_border, light.bg, "悬停框色与窗口底不同");
        assert_ne!(dark.hover_border, dark.border, "悬停框色与窗口边框不同");
        assert_ne!(dark.hover_border, dark.bg, "悬停框色与窗口底不同");
        // 浅深两套悬停框色不同（深色需更亮）
        assert_ne!(light.hover_border, dark.hover_border);
    }
}
