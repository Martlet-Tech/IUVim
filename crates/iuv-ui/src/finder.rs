//! 捕捉态装饰件渲染（54-2）：**取景框 / 目标描边 / 目标提示条**。
//!
//! 三件都是**透明底**覆盖窗——内部必须完全透明，否则会遮住被描边的真实窗口，
//! 故一律走 `render_transparent_surface`（不填底色），提示条除外（它需要底色
//! 保证文字可读，走 `render_to_surface`）。
//!
//! 本模块纯渲染、无 Win32、无状态；窗口管理 / 光标轮询 / 命中检测在
//! `iuv-server/daemon/finder/`，窗口枚举在 `iuv-win/window_probe.rs`。

use tiny_skia::{LineCap, LineJoin, Paint, Path, PathBuilder, Stroke, Transform};

use crate::layout::{PAD_X, PAD_Y};
use crate::paint::{fill_rounded, rounded_rect_path};
use crate::render::{render_to_surface, render_transparent_surface, Surface};
use crate::text::{TextRenderer, FONT_PX_96};
use crate::theme::Theme;

/// 目标可挂性——决定描边与提示条状态点的颜色（54-2 §3.3）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TargetKind {
    /// 可挂载 → 主题强调色（`theme.hl_bg`）
    Mountable,
    /// 不可挂载（全屏覆盖 / 权限不足）→ 警示色（`theme.warn`）
    Unmountable,
}

/// 取景框距显示器边缘内缩（@96dpi；render 乘 scale）。
/// 别贴物理边缘——贴边看着像显示器边框，不像"取景"。
pub const FRAME_INSET: f32 = 16.0;

const FRAME_ARM: f32 = 28.0; // 括号臂长
const FRAME_RADIUS: f32 = 6.0; // 拐角圆角
const FRAME_WIDTH: f32 = 2.0; // 线宽
const OUTLINE_WIDTH: f32 = 2.0; // 目标描边线宽
const DOT_D: f32 = 8.0; // 提示条状态点直径
const DOT_GAP: f32 = 6.0; // 状态点与文本间距

fn norm_scale(scale: f32) -> f32 {
    if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    }
}

/// 描边/状态点颜色：可挂载走强调色，不可挂载走警示色。
fn kind_color(kind: TargetKind, theme: &Theme) -> [u8; 4] {
    match kind {
        TargetKind::Mountable => theme.hl_bg,
        TargetKind::Unmountable => theme.warn,
    }
}

/// 取景框：光标所在**显示器**四角 L 形圆角括号（主题强调色）。
///
/// 只画四角、**不画整圈**——整圈铺满屏幕视觉重量太大，且会与"目标描边"抢语义。
/// 面尺寸 = 显示器 rect；括号内缩 `FRAME_INSET`。面太小画不下时返回全透明面
/// （窗口照常存在，只是无装饰）。
pub fn render_finder_frame(w: u32, h: u32, scale: f32, theme: &Theme) -> Surface {
    let scale = norm_scale(scale);
    let inset = (FRAME_INSET * scale).ceil();
    let arm = (FRAME_ARM * scale).ceil();
    let r = (FRAME_RADIUS * scale).ceil();
    let bw = (FRAME_WIDTH * scale).ceil();
    let color = theme.hl_bg;
    render_transparent_surface(w, h, |pixmap| {
        let (fw, fh) = (w as f32, h as f32);
        // 四角各需 inset + arm + bw，两侧对称 → 小于此则画不下，直接留空
        if fw < (inset + arm + bw) * 2.0 || fh < (inset + arm + bw) * 2.0 {
            return;
        }
        let mut paint = Paint::default();
        paint.set_color_rgba8(color[0], color[1], color[2], color[3]);
        let stroke = Stroke {
            width: bw,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            ..Stroke::default()
        };
        // (角点 x, 角点 y, 水平入内方向, 垂直入内方向)
        let corners = [
            (inset, inset, 1.0, 1.0),             // 左上
            (fw - inset, inset, -1.0, 1.0),       // 右上
            (inset, fh - inset, 1.0, -1.0),       // 左下
            (fw - inset, fh - inset, -1.0, -1.0), // 右下
        ];
        for (cx, cy, sx, sy) in corners {
            if let Some(path) = corner_path(cx, cy, sx, sy, arm, r) {
                pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
            }
        }
    })
}

/// 单角 L 形括号路径：水平臂外端 → 圆角 → 垂直臂外端。
/// `(sx, sy)` 为从角点指向面内的方向（±1）；圆角用三次贝塞尔标准拟合。
fn corner_path(cx: f32, cy: f32, sx: f32, sy: f32, arm: f32, r: f32) -> Option<Path> {
    if !cx.is_finite() || !cy.is_finite() || arm <= 0.0 {
        return None;
    }
    let r = r.max(0.0).min(arm);
    const K: f32 = 0.552_284_8;
    let mut pb = PathBuilder::new();
    pb.move_to(cx + sx * arm, cy);
    pb.line_to(cx + sx * r, cy);
    pb.cubic_to(
        cx + sx * r * (1.0 - K),
        cy,
        cx,
        cy + sy * r * (1.0 - K),
        cx,
        cy + sy * r,
    );
    pb.line_to(cx, cy + sy * arm);
    pb.finish()
}

/// 目标描边：整圈细边框，**内部全透明**（真窗口原样透出，不做任何蒙版）。
/// 颜色由 `kind` 分级——这是"能不能挂"的唯一视觉编码。
pub fn render_target_outline(
    w: u32,
    h: u32,
    scale: f32,
    kind: TargetKind,
    theme: &Theme,
) -> Surface {
    let scale = norm_scale(scale);
    let bw = (OUTLINE_WIDTH * scale).ceil();
    let radius = (theme.corner_radius * scale).max(0.0);
    let color = kind_color(kind, theme);
    render_transparent_surface(w, h, |pixmap| {
        let (fw, fh) = (w as f32, h as f32);
        if fw <= bw || fh <= bw {
            return;
        }
        let inset = bw / 2.0; // 描边以路径为中心：内缩半个线宽，外缘恰贴位图边
        let Some(path) = rounded_rect_path(inset, inset, fw - bw, fh - bw, radius) else {
            return;
        };
        let mut paint = Paint::default();
        paint.set_color_rgba8(color[0], color[1], color[2], color[3]);
        let stroke = Stroke {
            width: bw,
            ..Stroke::default()
        };
        pixmap.stroke_path(&path, &paint, &stroke, Transform::identity(), None);
    })
}

/// 目标提示条：状态色点 + 一行文本，圆角底 + 细边框。
/// 内容由调用方拼（`进程名 · 窗口类 · 尺寸 · 可挂性`）——渲染层不管语义。
pub fn render_target_label(
    label: &str,
    kind: TargetKind,
    theme: &Theme,
    scale: f32,
    text: &mut TextRenderer,
) -> Surface {
    let scale = norm_scale(scale);
    let size_px = FONT_PX_96 * scale;
    let (tw, th) = text.measure(label, size_px);
    let pad_x = (PAD_X as f32 * scale).ceil() as i32;
    let pad_y = (PAD_Y as f32 * scale).ceil() as i32;
    let dot = (DOT_D * scale).ceil() as i32;
    let gap = (DOT_GAP * scale).ceil() as i32;
    let cw = pad_x * 2 + dot + gap + tw;
    let ch = (pad_y * 2 + th.max(dot)).max(1);
    let color = kind_color(kind, theme);
    render_to_surface(theme, scale, cw.max(1) as u32, ch as u32, |pixmap| {
        // 状态点：垂直居中
        let d = dot as f32;
        let cy = ch as f32 / 2.0;
        let cx = pad_x as f32 + d / 2.0;
        fill_rounded(pixmap, cx - d / 2.0, cy - d / 2.0, d, d, d / 2.0, color);
        // 文本：状态点右侧，垂直居中
        let tx = (pad_x + dot + gap) as f32;
        let ty = ((ch - th) / 2).max(0) as f32;
        text.draw(pixmap, label, tx, ty, size_px, theme.fg);
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 取像素（Surface 为 premultiplied BGRA）→ RGBA。
    fn px(s: &Surface, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * s.w + x) * 4) as usize;
        let p = &s.pixels[i..i + 4];
        [p[2], p[1], p[0], p[3]]
    }

    #[test]
    fn frame_draws_four_brackets_not_a_ring() {
        let t = crate::theme::theme_light();
        let s = render_finder_frame(400, 300, 1.0, &t);
        assert_eq!((s.w, s.h), (400, 300), "面尺寸 = 传入尺寸");
        // 左上括号：水平臂（y=inset，x 落在 inset..inset+arm）
        assert!(px(&s, 30, 16)[3] > 0, "左上水平臂应有着色");
        // 左上括号：垂直臂（x=inset）
        assert!(px(&s, 16, 30)[3] > 0, "左上垂直臂应有着色");
        // 右下括号
        assert!(px(&s, 370, 284)[3] > 0, "右下水平臂应有着色");
        assert!(px(&s, 384, 270)[3] > 0, "右下垂直臂应有着色");
        // 四边中点必须透明 —— 证明是四角括号，不是整圈
        assert_eq!(px(&s, 200, 16)[3], 0, "上边中点应透明（不是整圈）");
        assert_eq!(px(&s, 200, 284)[3], 0, "下边中点应透明");
        assert_eq!(px(&s, 16, 150)[3], 0, "左边中点应透明");
        // 中心必须透明 —— 否则会遮住桌面
        assert_eq!(px(&s, 200, 150)[3], 0, "中心必须全透明");
        // 取景框走强调色
        assert_eq!(px(&s, 30, 16), t.hl_bg, "取景框用主题强调色");
    }

    #[test]
    fn frame_tiny_surface_is_blank_not_panic() {
        let t = crate::theme::theme_dark();
        // 面太小画不下括号：返回同尺寸全透明面（窗口仍可创建）
        let s = render_finder_frame(20, 20, 1.0, &t);
        assert_eq!((s.w, s.h), (20, 20));
        assert!(s.pixels.iter().all(|b| *b == 0), "全透明");
        // 0 尺寸 → 空面
        assert_eq!(render_finder_frame(0, 0, 1.0, &t).w, 0);
    }

    #[test]
    fn outline_is_hollow_and_color_codes_mountability() {
        let t = crate::theme::theme_light();
        let ok = render_target_outline(200, 100, 1.0, TargetKind::Mountable, &t);
        let bad = render_target_outline(200, 100, 1.0, TargetKind::Unmountable, &t);
        assert_eq!((ok.w, ok.h), (200, 100));
        // 中心必须透明 —— 真窗口原样透出，不做蒙版
        assert_eq!(px(&ok, 100, 50)[3], 0, "描边内部必须全透明");
        // 上边线心着强调色
        assert_eq!(px(&ok, 100, 1), t.hl_bg, "可挂载 = 强调色");
        // 不可挂载走警示色，且两者必须不同（否则分级无意义）
        assert_eq!(px(&bad, 100, 1), t.warn, "不可挂载 = 警示色");
        assert_ne!(px(&ok, 100, 1), px(&bad, 100, 1), "两种分级颜色必须不同");
    }

    #[test]
    fn outline_degenerate_size_is_blank() {
        let t = crate::theme::theme_light();
        assert_eq!(
            render_target_outline(0, 10, 1.0, TargetKind::Mountable, &t).w,
            0
        );
        // 宽 ≤ 线宽：不画，但面仍在
        let s = render_target_outline(2, 40, 1.0, TargetKind::Mountable, &t);
        assert_eq!((s.w, s.h), (2, 40));
        assert!(s.pixels.iter().all(|b| *b == 0));
    }

    #[test]
    fn label_has_kind_colored_dot_then_text() {
        let t = crate::theme::theme_light();
        let mut tr = TextRenderer::new();
        let s = render_target_label(
            "notepad.exe · Notepad · 800×600",
            TargetKind::Unmountable,
            &t,
            1.0,
            &mut tr,
        );
        assert!(s.w > 0 && s.h > 0, "非空面");
        // 状态点圆心（scale=1：pad_x=8、dot=8 → 圆心 x=12，垂直居中）
        let cy = s.h / 2;
        assert_eq!(px(&s, 12, cy), t.warn, "状态点用分级色");
        // 状态点右侧应有文本着色（白底上的深字，alpha 满）
        let text_zone =
            (14..s.w.saturating_sub(4)).any(|x| px(&s, x, cy)[3] > 0 && px(&s, x, cy) != t.warn);
        assert!(text_zone, "状态点右侧应绘制文本");
    }
}
