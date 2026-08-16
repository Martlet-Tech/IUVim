//! cosmic-text 0.19 集成：系统字体发现 + 整形/布局/字形光栅。
//!
//! 用法（iuv-tsf 呈现层）：
//! - `TextRenderer::new()` 进程/窗口一次（fontdb 首扫系统字体有一次性开销）；
//! - `measure` 供 layout 纯函数测量（与绘制共用同一 Buffer，文本未变时不重整形）；
//! - `draw` 灰度 AA 字形直接合成到 tiny-skia pixmap（cosmic-text 输出
//!   premultiplied 色块 → 反预乘 → fill_rect）。
//!
//! 家族策略：主家族 = Microsoft YaHei UI（Windows 10+ 中文默认 UI 字体），
//! 回退链经 fontdb generic 家族重映射 + cosmic-text 平台回退（Windows: Segoe UI
//! 系 / macOS: PingFang SC / Linux: fontconfig 解析）。
//! 注：cosmic-text 0.19 的 `Fallback` trait 需 unicode-script 类型（不在依赖
//! 白名单），故链首之外的家族由内置平台回退承接（见 19-m4-cross-render.md §5）。

use cosmic_text::fontdb;
use cosmic_text::{Attrs, Buffer, Color, Family, FontSystem, Metrics, Shaping, SwashCache};
use tiny_skia::{Paint, Pixmap, Rect, Transform};

/// 主字号（14pt @96dpi → px）。pt → px = pt × 96/72。
pub const FONT_PX_96: f32 = 14.0 * 96.0 / 72.0;

/// 行高系数：行高 = 字号 × 本系数（≈ 微软雅黑 1.12em 实际行高 + 一点余量）。
const LINE_HEIGHT_SCALE: f32 = 1.15;

/// 家族回退链（主家族在前）：Windows / macOS / Linux 各有其一命中。
pub const FALLBACK_FAMILIES: [&str; 4] = [
    "Microsoft YaHei UI",
    "Segoe UI",
    "PingFang SC",
    "Noto Sans CJK SC",
];

/// 文本属性：主家族 = 回退链首项；字号/颜色每次绘制单独传参。
const TEXT_ATTRS: Attrs<'static> = Attrs::new().family(Family::Name(FALLBACK_FAMILIES[0]));

/// 文本渲染器：FontSystem（字体库）+ SwashCache（字形光栅缓存）+ 单 Buffer 复用。
///
/// `measure` 与 `draw` 共用同一布局——相同 (text, size_px) 不重复整形，
/// 候选窗每键重绘（≤7 行）开销在 <1ms 量级（无字形缓存需求，swash 自带缓存）。
pub struct TextRenderer {
    font_system: FontSystem,
    swash_cache: SwashCache,
    buffer: Buffer,
    last_text: String,
    last_size: f32,
}

impl Default for TextRenderer {
    fn default() -> Self {
        Self::new()
    }
}

impl TextRenderer {
    /// 创建渲染器：fontdb 扫系统字体 + 主家族重映射 + 空 Buffer（无换行）。
    ///
    /// 首次调用有一次性开销（fontdb 扫 C:\Windows\Fonts 元数据，几十 ms ~ 1s），
    /// 应在候选窗创建时（而非每键）调用。失败不 panic：任何路径返回可用实例，
    /// 缺字体时 measure 返回 (0, 0)、draw 静默不画。
    pub fn new() -> Self {
        let mut db = fontdb::Database::new();
        db.load_system_fonts();
        // generic sans-serif 重映射到主家族（回退链首项）
        db.set_sans_serif_family(FALLBACK_FAMILIES[0]);
        let font_system = FontSystem::new_with_locale_and_db("zh-CN".into(), db);
        let mut buffer =
            Buffer::new_empty(Metrics::new(FONT_PX_96, FONT_PX_96 * LINE_HEIGHT_SCALE));
        // 宽度/高度均不限制：单行布局不换行（measure 取 max line）
        buffer.set_size(None, None);
        TextRenderer {
            font_system,
            swash_cache: SwashCache::new(),
            buffer,
            last_text: String::new(),
            last_size: 0.0,
        }
    }

    /// 测量单行文本 (宽, 高)（物理像素）。空文本/非法字号返回 (0, 0)。
    /// 高度 = 行高（metrics line_height），与 draw 的实际字形框一致。
    pub fn measure(&mut self, text: &str, size_px: f32) -> (i32, i32) {
        if text.is_empty() || !size_px.is_finite() || size_px <= 0.0 {
            return (0, 0);
        }
        self.shape(text, size_px);
        let mut w = 0.0f32;
        let mut h = 0.0f32;
        for run in self.buffer.layout_runs() {
            w = w.max(run.line_w);
            h = h.max(run.line_height);
        }
        let w = if w.is_finite() { w.ceil() as i32 } else { 0 };
        let h = if h.is_finite() { h.ceil() as i32 } else { 0 };
        (w.max(0), h.max(0))
    }

    /// 绘制文本到 pixmap（灰度 AA 字形合成）。`(x, y)` 为文本左上角（物理像素），
    /// 与 `measure` 同字号同文本时与测量矩形精确对齐。
    /// 失败静默：非法参数/空文本/字形缺失直接返回，绝不 panic。
    pub fn draw(
        &mut self,
        pixmap: &mut Pixmap,
        text: &str,
        x: f32,
        y: f32,
        size_px: f32,
        color: [u8; 4],
    ) {
        if !size_px.is_finite() || size_px <= 0.0 || text.is_empty() || color[3] == 0 {
            return;
        }
        if !x.is_finite() || !y.is_finite() || pixmap.width() == 0 || pixmap.height() == 0 {
            return;
        }
        self.shape(text, size_px);
        let text_color = Color::rgba(color[0], color[1], color[2], color[3]);
        let mut paint = Paint::default();
        self.buffer.draw(
            &mut self.font_system,
            &mut self.swash_cache,
            text_color,
            |dx, dy, dw, dh, c| {
                let a = (c.0 >> 24) & 0xFF;
                if a == 0 || dw == 0 || dh == 0 {
                    return;
                }
                // cosmic-text 灰度字形输出 (coverage<<24 | base_rgb)：
                // alpha = 字形覆盖率、RGB = 满强度基色（非预乘）——直接按
                // unpremultiplied 交给 tiny-skia（内部预乘 → 正确的 over 合成）。
                let r = ((c.0 >> 16) & 0xFF) as u8;
                let g = ((c.0 >> 8) & 0xFF) as u8;
                let b = (c.0 & 0xFF) as u8;
                paint.set_color_rgba8(r, g, b, a as u8);
                if let Some(rect) =
                    Rect::from_xywh(x + dx as f32, y + dy as f32, dw as f32, dh as f32)
                {
                    pixmap.fill_rect(rect, &paint, Transform::identity(), None);
                }
            },
        );
    }

    /// 整形 + 布局：文本或字号变化才重做（Buffer 脏标记驱动），否则为廉价 no-op。
    fn shape(&mut self, text: &str, size_px: f32) {
        if self.last_text != text {
            self.buffer
                .set_text(text, &TEXT_ATTRS, Shaping::Advanced, None);
            self.last_text.clear();
            self.last_text.push_str(text);
        }
        if (self.last_size - size_px).abs() > 0.001 {
            self.buffer
                .set_metrics(Metrics::new(size_px, size_px * LINE_HEIGHT_SCALE));
            self.last_size = size_px;
        }
        self.buffer.shape_until_scroll(&mut self.font_system, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn renderer() -> TextRenderer {
        TextRenderer::new()
    }

    #[test]
    fn measure_ascii_and_cjk_nonzero() {
        let mut t = renderer();
        let (w, h) = t.measure("1.你好", FONT_PX_96);
        assert!(w > 20, "数字+两个汉字应有一定宽度，实际 {w}");
        assert!(h > 10, "行高应大于字号一半，实际 {h}");
        let (w0, h0) = t.measure("", FONT_PX_96);
        assert_eq!((w0, h0), (0, 0), "空文本 (0,0)");
    }

    #[test]
    fn measure_scales_with_size() {
        let mut t = renderer();
        let (w1, h1) = t.measure("你好", FONT_PX_96);
        let (w2, h2) = t.measure("你好", FONT_PX_96 * 2.0);
        assert!(w2 > w1, "字号翻倍宽度应增长");
        assert!(h2 > h1, "字号翻倍行高应增长");
    }

    #[test]
    fn measure_invalid_size_returns_zero() {
        let mut t = renderer();
        assert_eq!(t.measure("你好", 0.0), (0, 0));
        assert_eq!(t.measure("你好", f32::NAN), (0, 0));
        assert_eq!(t.measure("你好", -3.0), (0, 0));
    }

    #[test]
    fn draw_produces_ink_pixels() {
        let mut t = renderer();
        let (w, h) = t.measure("你好", FONT_PX_96);
        assert!(w > 0 && h > 0);
        let mut pixmap = Pixmap::new(w as u32 + 4, h as u32 + 4).unwrap();
        t.draw(
            &mut pixmap,
            "你好",
            2.0,
            2.0,
            FONT_PX_96,
            [0x1F, 0x1F, 0x1F, 0xFF],
        );
        // 文本区域应有墨迹（近黑），周围保持透明
        let mut inked = false;
        for yy in 2..(h + 2) {
            for xx in 2..(w + 2) {
                let px = pixmap.pixel(xx as u32, yy as u32).unwrap();
                let c = px.demultiply();
                if c.alpha() > 40 {
                    inked = true;
                    assert!(
                        c.red() < 0x80 && c.green() < 0x80 && c.blue() < 0x80,
                        "近黑文字，实际 {},{},{}",
                        c.red(),
                        c.green(),
                        c.blue()
                    );
                }
            }
        }
        assert!(inked, "文本区域应有非透明像素");
        let corner = pixmap.pixel(0, 0).unwrap();
        assert_eq!(corner.alpha(), 0, "绘制区外保持透明");
    }

    #[test]
    fn draw_invalid_args_silent() {
        let mut t = renderer();
        let mut pixmap = Pixmap::new(64, 64).unwrap();
        // 非法字号/空文本/透明色：静默 no-op（不 panic）
        t.draw(&mut pixmap, "你好", 0.0, 0.0, 0.0, [0, 0, 0, 0xFF]);
        t.draw(&mut pixmap, "", 0.0, 0.0, FONT_PX_96, [0, 0, 0, 0xFF]);
        t.draw(&mut pixmap, "你好", 0.0, 0.0, FONT_PX_96, [0, 0, 0, 0]);
        t.draw(
            &mut pixmap,
            "你好",
            f32::NAN,
            0.0,
            FONT_PX_96,
            [0, 0, 0, 0xFF],
        );
        assert!(
            pixmap.pixels().iter().all(|p| p.alpha() == 0),
            "全部 no-op 后应全透明"
        );
    }
}
