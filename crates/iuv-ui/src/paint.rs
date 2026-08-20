//! 纯绘制基元（P2.4 自 render.rs 下沉）：阴影/圆角/虚线/路径填充。
//! 候选窗/菜单/工具栏共用；全程 tiny-skia 纯 safe API，分配失败/非法参数
//! 静默降级返回，绝不 panic。

use tiny_skia::{FillRule, Path, PathBuilder, Paint, Pixmap, Rect, Stroke, Transform};

use crate::theme::Theme;

/// 假阴影层数（自内向外逐层膨胀减淡）。
pub(crate) const SHADOW_PASSES: u32 = 4;

/// 阴影整体下移量（模拟顶部光源）。
pub(crate) const SHADOW_OFFSET_Y: f32 = 2.0;

/// 高亮行/悬停圆角半径（@96dpi 基准；render 乘 scale）。
pub(crate) const HL_RADIUS: f32 = 2.0;

/// 阴影：`SHADOW_PASSES` 层膨胀圆角矩形（外缘最淡），整体下移模拟顶部光源。
pub(crate) fn draw_shadow(
    pixmap: &mut Pixmap,
    theme: &Theme,
    scale: f32,
    sx: f32,
    cw: f32,
    ch: f32,
) {
    let shadow_px = theme.shadow_size * scale;
    if !shadow_px.is_finite() || shadow_px <= 0.0 {
        return;
    }
    let off_y = SHADOW_OFFSET_Y * scale;
    for i in 0..SHADOW_PASSES {
        let t_inflate = (i + 1) as f32 / SHADOW_PASSES as f32; // 0.25..1.0 膨胀
        let t_alpha = 1.0 - i as f32 / SHADOW_PASSES as f32; // 1.0..0.25 减淡
        let inflate = shadow_px * t_inflate;
        let alpha = ((theme.shadow[3] as f32) * t_alpha)
            .round()
            .clamp(0.0, 255.0) as u8;
        let radius = theme.corner_radius * scale + inflate;
        let path = rounded_rect_path(
            sx - inflate,
            sx - inflate + off_y,
            cw + inflate * 2.0,
            ch + inflate * 2.0,
            radius,
        );
        if let Some(path) = path {
            fill_path(
                pixmap,
                &path,
                [theme.shadow[0], theme.shadow[1], theme.shadow[2], alpha],
            );
        }
    }
}

/// 填充圆角矩形路径（背景/高亮行/阴影共用）。
pub(crate) fn fill_rounded(
    pixmap: &mut Pixmap,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: f32,
    color: [u8; 4],
) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    if let Some(path) = rounded_rect_path(x, y, w, h, r) {
        fill_path(pixmap, &path, color);
    }
}

/// 虚线圆角矩形框（悬停高亮用）：内缩 1px 防跨行压邻行/文本；
/// dash 规格 [4,3]（4px 线 + 3px 空，物理像素）。
pub(crate) fn stroke_rounded_dashed(
    pixmap: &mut Pixmap,
    x: f32,
    y: f32,
    w: f32,
    h: f32,
    r: f32,
    width: f32,
    color: [u8; 4],
) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    let Some(path) = rounded_rect_path(x + 1.0, y + 1.0, w - 2.0, h - 2.0, r) else {
        return;
    };
    let Some(dash) = tiny_skia::StrokeDash::new(vec![4.0, 3.0], 0.0) else {
        return;
    };
    let mut paint = Paint::default();
    paint.set_color_rgba8(color[0], color[1], color[2], color[3]);
    let stroke = Stroke {
        width,
        dash: Some(dash),
        ..Stroke::default()
    };
    pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
}

/// 路径填充（颜色直写，不叠加 alpha）。
pub(crate) fn fill_path(pixmap: &mut Pixmap, path: &Path, color: [u8; 4]) {
    let mut paint = Paint::default();
    paint.set_color_rgba8(color[0], color[1], color[2], color[3]);
    pixmap.fill_path(path, &paint, FillRule::Winding, Transform::identity(), None);
}

/// 圆角矩形路径：tiny-skia 0.12 无 RoundedRect 图元，四角用三次贝塞尔近似
/// （k = 0.55228475，圆弧标准拟合）。
pub(crate) fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<Path> {
    if !x.is_finite() || !y.is_finite() || !w.is_finite() || !h.is_finite() {
        return None;
    }
    let r = r.max(0.0).min(w / 2.0).min(h / 2.0);
    let mut pb = PathBuilder::new();
    if r <= 0.0 {
        let rect = Rect::from_xywh(x, y, w, h)?;
        pb.push_rect(rect);
        return pb.finish();
    }
    const K: f32 = 0.55228475;
    pb.move_to(x + r, y);
    pb.line_to(x + w - r, y);
    pb.cubic_to(x + w - r * K, y, x + w, y + r * K, x + w, y + r);
    pb.line_to(x + w, y + h - r);
    pb.cubic_to(x + w, y + h - r * K, x + w - r * K, y + h, x + w - r, y + h);
    pb.line_to(x + r, y + h);
    pb.cubic_to(x + r * K, y + h, x, y + h - r * K, x, y + h - r);
    pb.line_to(x, y + r);
    pb.cubic_to(x, y + r * K, x + r * K, y, x + r, y);
    pb.close();
    pb.finish()
}