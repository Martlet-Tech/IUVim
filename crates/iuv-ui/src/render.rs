//! 渲染：候选窗 / 菜单 / tooltip → `Surface`（premultiplied BGRA 像素缓冲）。
//!
//! 绘制管线（自底向上）：圆角矩形底 → 高亮行底 → 行文本（含编号，原文兜底候选
//! 不编号）→ 页码小字 → 细边框。扁平风格：无阴影，surface 尺寸 = 内容精确尺寸，
//! 内容坐标 = 表面坐标 = 窗口客户区坐标（命中矩形无需任何偏移换算）。
//! 全程 tiny-skia 纯 safe API；分配失败/非法参数静默降级返回空 `Surface`，绝不 panic。
//!
//! `Surface.pixels` 为 **premultiplied BGRA**、u32 对齐行、无 stride 填充——
//! Windows 呈现层（ULW 32bpp DIB / D2D CreateBitmap 等）直供；
//! 其他平台自行转格式。
//!
//! P2.4：纯绘制基元下沉 `paint.rs`，工具栏渲染拆出 `toolbar.rs`。

use tiny_skia::{Paint, Pixmap, Rect, Stroke, Transform};

use crate::layout::{candidate_label, layout, Rect as LayoutRect};
use crate::menu::MenuEntry;
use crate::paint::{fill_path, fill_rounded, stroke_rounded_dashed, rounded_rect_path, HL_RADIUS};
use crate::snapshot::UiSnapshot;
use crate::text::{TextRenderer, FONT_PX_96};
use crate::theme::Theme;

/// 渲染产物：像素缓冲 + 尺寸。pixels = premultiplied BGRA，长度 = w*h*4。
#[derive(Clone, Debug, PartialEq)]
pub struct Surface {
    pub w: u32,
    pub h: u32,
    pub pixels: Vec<u8>,
}

impl Surface {
    /// 空表面（0×0，无像素）。渲染失败/空输入的降级产物。
    pub fn empty() -> Self {
        Surface {
            w: 0,
            h: 0,
            pixels: Vec::new(),
        }
    }
}

/// 渲染候选窗（竖排：每候选一行；横排：单行平铺）。返回 `(Surface, 行矩形列表)`——
/// 行矩形与 `render_candidate` 内部 `layout` 同测量（候选编号/页码/字号同一规则），
/// 直接喂 `hit_test` 做候选行命中（P2.4：消费方不再重复测量，删 `compute_rows`）。
///
/// `hover` 悬停行画虚线框（叠加于选中高亮之上）；`snap.selected` 画高亮底。
/// 原文兜底候选（text == 预编辑原文去 `'`）不编号呈现。
pub fn render_candidate(
    snap: &UiSnapshot,
    theme: &Theme,
    scale: f32,
    text: &mut TextRenderer,
    hover: Option<usize>,
) -> (Surface, Vec<LayoutRect>) {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let size_px = FONT_PX_96 * scale;
    let page_px = size_px / 2.0;
    // 预测量：layout 的测量器须无状态（不捕获渲染器），故先量好再传闭包。
    // 行数 ≤ 页候选 + 页码（≤6），重复扫描开销可忽略。
    let mut sizes: Vec<(String, (i32, i32))> = Vec::with_capacity(snap.candidates.len());
    for (i, cand) in snap.candidates.iter().enumerate() {
        let label = candidate_label(snap, i, cand);
        let size = text.measure(&label, size_px);
        sizes.push((label, size));
    }
    let mut page_size = None;
    if snap.page.page_count > 1 {
        let label = format!("{}/{}", snap.page.page + 1, snap.page.page_count);
        page_size = Some(text.measure(&label, page_px));
    }
    let (cw, ch, rects) = layout(
        snap,
        |s| {
            sizes
                .iter()
                .find(|(t, _)| t == s)
                .map(|(_, sz)| *sz)
                .unwrap_or((0, 0))
        },
        |_s| page_size.unwrap_or((0, 0)),
        snap.orientation,
    );
    let surface = render_to_surface(
        theme,
        scale,
        cw.max(0) as u32,
        ch.max(0) as u32,
        |pixmap| {
            // 候选行：真高亮填充底 + 悬停虚线框（叠加，互不覆盖）+ 文本
            // （原文兜底候选不编号，规则与 layout 一致）
            for (i, cand) in snap.candidates.iter().enumerate() {
                let Some(r) = rects.get(i) else {
                    break; // 防御：布局行数与候选数不一致也不越界
                };
                let sel = i == snap.selected;
                if sel {
                    fill_rounded(
                        pixmap,
                        r.x as f32,
                        r.y as f32,
                        r.w as f32,
                        r.h as f32,
                        (HL_RADIUS * scale).min(r.h as f32 / 2.0),
                        theme.hl_bg,
                    );
                }
                if hover == Some(i) {
                    stroke_rounded_dashed(
                        pixmap,
                        r.x as f32,
                        r.y as f32,
                        r.w as f32,
                        r.h as f32,
                        (HL_RADIUS * scale).min(r.h as f32 / 2.0),
                        1.0_f32.max(scale),
                        theme.hover_border,
                    );
                }
                let label = candidate_label(snap, i, cand);
                let color = if sel { theme.hl_fg } else { theme.fg };
                text.draw(pixmap, &label, r.x as f32, r.y as f32, size_px, color);
            }
            // 页码小字（多页时；行号 = candidates.len()）
            if snap.page.page_count > 1 {
                if let Some(r) = rects.get(snap.candidates.len()) {
                    let label = format!("{}/{}", snap.page.page + 1, snap.page.page_count);
                    text.draw(
                        pixmap,
                        &label,
                        r.x as f32,
                        r.y as f32,
                        page_px,
                        theme.page_fg,
                    );
                }
            }
        },
    );
    (surface, rects)
}

/// 渲染菜单（M5 语言栏右键菜单用）：竖排条目列表，扁平圆角 + 细边框，风格与候选窗一致。
///
/// 返回 `(Surface, 行矩形列表)`——行矩形为 **surface 坐标**（= 内容坐标，无阴影偏移），
/// 窗口客户区 = surface 尺寸时可直接喂 `menu_hit_test`。
/// `id == 0` 条目画分隔线（不参与高亮）；`selected` 行画高亮底 + 高亮字。
pub fn render_menu(
    items: &[MenuEntry],
    selected: Option<usize>,
    theme: &Theme,
    scale: f32,
    text: &mut TextRenderer,
) -> (Surface, Vec<LayoutRect>) {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let size_px = FONT_PX_96 * scale;
    // 测量全部标签（分隔线标签忽略）
    let mut widths = Vec::with_capacity(items.len());
    let mut line_h = 0i32;
    for item in items {
        let (w, h) = if item.is_separator() {
            (0, 0)
        } else {
            text.measure(&item.label, size_px)
        };
        widths.push(w);
        line_h = line_h.max(h);
    }
    if line_h <= 0 {
        line_h = (size_px * 1.15).ceil() as i32;
    }
    let pad_y = (4.0 * scale).ceil() as i32;
    let row_h = line_h + pad_y * 2;
    let max_w = widths.iter().copied().max().unwrap_or(0).max(96);
    let content_w = max_w + crate::layout::PAD_X * 2;
    let content_h = row_h.saturating_mul(items.len() as i32).saturating_add(
        crate::layout::ROW_GAP.saturating_mul(items.len().saturating_sub(1) as i32),
    ) + crate::layout::PAD_Y * 2;
    // 行矩形（surface 坐标，无偏移）
    let mut rows = Vec::with_capacity(items.len());
    let mut y = crate::layout::PAD_Y;
    for _item in items.iter() {
        rows.push(LayoutRect {
            x: crate::layout::PAD_X,
            y,
            w: content_w - crate::layout::PAD_X * 2,
            h: row_h,
        });
        y += row_h + crate::layout::ROW_GAP;
    }
    let surface = render_to_surface(
        theme,
        scale,
        content_w as u32,
        content_h as u32,
        |pixmap| {
            for (i, item) in items.iter().enumerate() {
                let Some(r) = rows.get(i) else {
                    break; // 防御
                };
                let sel = selected == Some(i) && !item.is_separator();
                if sel {
                    fill_rounded(
                        pixmap,
                        r.x as f32,
                        r.y as f32,
                        r.w as f32,
                        r.h as f32,
                        (HL_RADIUS * scale).min(r.h as f32 / 2.0),
                        theme.hl_bg,
                    );
                }
                if item.is_separator() {
                    // 分隔线：水平 1px（行内垂直居中）
                    let cy = r.y as f32 + r.h as f32 / 2.0;
                    if let Some(rect) = Rect::from_xywh(r.x as f32, cy - 0.5, r.w as f32, 1.0)
                    {
                        let mut paint = Paint::default();
                        paint.set_color_rgba8(
                            theme.border[0],
                            theme.border[1],
                            theme.border[2],
                            0x80,
                        );
                        pixmap.fill_rect(rect, &paint, Transform::identity(), None);
                    }
                    continue;
                }
                let color = if sel { theme.hl_fg } else { theme.fg };
                let tx = r.x as f32;
                let ty = r.y as f32 + pad_y as f32;
                text.draw(pixmap, &item.label, tx, ty, size_px, color);
            }
        },
    );
    (surface, rows)
}

// ===== 32-status-toolbar.md §6.6 浮动工具栏 tooltip =====

/// 渲染悬停 tooltip（32-status-toolbar.md §6.6「全半角」「简体/繁体」等）：单行小标签，
/// 风格与候选窗一致（扁平圆角细边框）。返回 Surface（无命中矩形——tooltip 不接收点击）。
pub fn render_tooltip(
    label: &str,
    theme: &Theme,
    scale: f32,
    text: &mut TextRenderer,
) -> Surface {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let size_px = FONT_PX_96 * scale;
    let (w, h) = text.measure(label, size_px);
    let pad_x = (crate::layout::PAD_X as f32 * scale).ceil() as i32;
    let pad_y = (crate::layout::PAD_Y as f32 * scale).ceil() as i32;
    let cw = w + pad_x * 2;
    let ch = h + pad_y * 2;
    render_to_surface(theme, scale, cw.max(1) as u32, ch.max(1) as u32, |pixmap| {
        text.draw(pixmap, label, pad_x as f32, pad_y as f32, size_px, theme.fg);
    })
}

/// 通用外壳：建 Surface 画布（内容精确尺寸）→ 圆角底 + 细边框 → 调用 `draw`。
/// 扁平风格：无阴影；边框宽度 = `(scale).round().max(1)`（100%/125%→1px、150%+→2px），
/// 描边路径内缩 `宽度/2` 使整条边框完整落在位图内（外缘贴齐位图边缘，不被裁半）。
pub(crate) fn render_to_surface(theme: &Theme, scale: f32, cw: u32, ch: u32, draw: impl FnOnce(&mut Pixmap)) -> Surface {
    if cw == 0 || ch == 0 {
        return Surface::empty();
    }
    let Some(mut pixmap) = Pixmap::new(cw, ch) else {
        return Surface::empty(); // 分配失败：静默降级
    };
    // 边框几何：宽 bw 的描边以路径为中心向两侧各延伸 bw/2——路径内缩 bw/2 后
    // 外缘恰达位图边界（完整可见），内缘与底色衔接。
    let bw = scale.round().max(1.0);
    let inset = bw / 2.0;
    // 1) 圆角矩形底 + 2) 细边框（同一路径，圆角同心）
    let radius = (theme.corner_radius * scale - inset).max(0.0);
    let bg_path = rounded_rect_path(inset, inset, cw as f32 - bw, ch as f32 - bw, radius);
    if let Some(path) = &bg_path {
        fill_path(&mut pixmap, path, theme.bg);
        let mut paint = Paint::default();
        paint.set_color_rgba8(
            theme.border[0],
            theme.border[1],
            theme.border[2],
            theme.border[3],
        );
        let stroke = Stroke {
            width: bw,
            ..Stroke::default()
        };
        pixmap.stroke_path(path, &paint, &stroke, Transform::identity(), None);
    }
    // 3) 内容（高亮行/文本/页码）
    draw(&mut pixmap);
    // tiny-skia 0.12 像素缓冲为 premultiplied RGBA（内存序 r,g,b,a）；
    // Surface 契约要求 premultiplied BGRA（Windows ULW DIB / D2D 直供）——交换每像素 R/B。
    let data = pixmap.data();
    let mut pixels = Vec::with_capacity(data.len());
    pixels.extend_from_slice(data);
    for px in pixels.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    Surface { w: cw, h: ch, pixels }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{hit_test, layout};
    use crate::theme::{theme_dark, theme_light};
    use crate::toolbar::{
        render_toolbar, ToolbarIcons, ToolbarSpec, TB_COUNT, TB_GEAR, TB_LOGO, TOOLBAR_GAP,
        TOOLBAR_PAD,
    };
    use iuv_core::{ImeState, InitialMode, Orientation, PageInfo, PunctMode, ScriptMode, WidthMode};

    fn renderer() -> TextRenderer {
        TextRenderer::new()
    }
    fn snap(reading: &str, candidates: &[&str], page: usize, page_count: usize) -> UiSnapshot {
        UiSnapshot {
            reading: reading.to_string(),
            candidates: candidates.iter().map(|s| s.to_string()).collect(),
            all_candidates: candidates.iter().map(|s| s.to_string()).collect(),
            selected: 0,
            page: PageInfo {
                page,
                page_count,
                page_size: 5,
                total: page_count * 5,
            },
            orientation: Orientation::Vertical,
        }
    }

    /// 取像素 (x, y) 的 RGBA（premultiplied → demultiply）。
    fn px(surface: &Surface, x: u32, y: u32) -> (u8, u8, u8, u8) {
        let idx = ((y * surface.w + x) * 4) as usize;
        let (b, g, r, a) = (
            surface.pixels[idx],
            surface.pixels[idx + 1],
            surface.pixels[idx + 2],
            surface.pixels[idx + 3],
        );
        let c = tiny_skia::PremultipliedColorU8::from_rgba(r, g, b, a)
            .unwrap()
            .demultiply();
        (c.red(), c.green(), c.blue(), c.alpha())
    }

    #[test]
    fn render_candidate_corner_transparent() {
        let mut t = renderer();
        let s = snap("ni'hao", &["你好", "泥嚎"], 0, 1);
        let (surf, _rows) = render_candidate(&s, &theme_light(), 1.0, &mut t, None);
        assert!(surf.w > 0 && surf.h > 0);
        // 圆角外四角 alpha = 0（阴影分层同样不触角：角心恒在圆弧外）
        assert_eq!(px(&surf, 0, 0).3, 0, "左上角透明");
        assert_eq!(px(&surf, surf.w - 1, 0).3, 0, "右上角透明");
        assert_eq!(px(&surf, 0, surf.h - 1).3, 0, "左下角透明");
        assert_eq!(px(&surf, surf.w - 1, surf.h - 1).3, 0, "右下角透明");
    }

    #[test]
    fn render_candidate_bg_opaque_at_center() {
        let mut t = renderer();
        let s = snap("ni'hao", &["你好", "泥嚎"], 0, 1);
        let (surf, _rows) = render_candidate(&s, &theme_light(), 1.0, &mut t, None);
        let (r, g, b, a) = px(&surf, surf.w / 2, surf.h / 2);
        assert_eq!(a, 255, "背景中心完全不透明");
        assert_eq!((r, g, b), (0xFF, 0xFF, 0xFF), "浅色主题背景白");
    }

    #[test]
    fn render_candidate_highlight_row_hl_bg() {
        let mut t = renderer();
        // 候选 2 带尾随空格：行矩形尾部无墨迹 → 采样点必为高亮底色
        let s = snap("ni'hao", &["你好", "泥 "], 0, 1);
        let snap_sel = UiSnapshot {
            selected: 1,
            ..s.clone()
        };
        let (surf, _rows) = render_candidate(&snap_sel, &theme_light(), 1.0, &mut t, None);
        // 与 render 相同的测量参数重算布局（相同 renderer 状态 → 矩形完全一致）
        let size_px = FONT_PX_96;
        let mut sizes: Vec<(String, (i32, i32))> = Vec::new();
        for (i, c) in snap_sel.candidates.iter().enumerate() {
            let label = candidate_label(&snap_sel, i, c);
            sizes.push((label.clone(), t.measure(&label, size_px)));
        }
        let (_, _, rects) = layout(
            &snap_sel,
            |s| {
                sizes
                    .iter()
                    .find(|(t, _)| t == s)
                    .map(|(_, sz)| *sz)
                    .unwrap_or((0, 0))
            },
            |_s| (0, 0),
            Orientation::Vertical,
        );
        let row = rects[1];
        // 行矩形尾部 3px 内（尾随空格区）应等于 hl_bg #0078D7
        let (r, g, b, a) = px(
            &surf,
            (row.x + row.w - 3) as u32,
            (row.y + row.h / 2) as u32,
        );
        let [hr, hg, hb, _ha] = theme_light().hl_bg;
        assert!(a > 250, "高亮行完全不透明");
        assert!(
            (r as i16 - hr as i16).abs() <= 2
                && (g as i16 - hg as i16).abs() <= 2
                && (b as i16 - hb as i16).abs() <= 2,
            "高亮行底色 ≈ hl_bg #{hr:02X}{hg:02X}{hb:02X}，实际 {r:02X}{g:02X}{b:02X}"
        );
        // 对照：非高亮行同位置是背景白
        let (r2, g2, b2, _) = px(
            &surf,
            (rects[0].x + rects[0].w - 3) as u32,
            (rects[0].y + rects[0].h / 2) as u32,
        );
        assert_eq!((r2, g2, b2), (0xFF, 0xFF, 0xFF), "非高亮行背景白");
    }

    /// 两个 Surface 的差异像素数（虚线框存在性校验：边框像素改变了表面）。
    fn diff_count(a: &Surface, b: &Surface) -> usize {
        if a.w != b.w || a.h != b.h {
            return usize::MAX;
        }
        a.pixels
            .iter()
            .zip(b.pixels.iter())
            .filter(|(x, y)| x != y)
            .count()
    }

    /// 悬停行行内采样坐标（尾随空格区，远离文字墨迹）。
    fn row_sample(rects: &[LayoutRect], idx: usize) -> (u32, u32) {
        let r = rects[idx];
        ((r.x + r.w - 3) as u32, (r.y + r.h / 2) as u32)
    }

    fn layout_rects(snap: &UiSnapshot, t: &mut TextRenderer) -> Vec<LayoutRect> {
        let size_px = FONT_PX_96;
        let mut sizes: Vec<(String, (i32, i32))> = Vec::new();
        for (i, c) in snap.candidates.iter().enumerate() {
            let label = candidate_label(snap, i, c);
            sizes.push((label.clone(), t.measure(&label, size_px)));
        }
        let (_, _, rects) = layout(
            snap,
            |s| {
                sizes
                    .iter()
                    .find(|(t, _)| t == s)
                    .map(|(_, sz)| *sz)
                    .unwrap_or((0, 0))
            },
            |_s| (0, 0),
            Orientation::Vertical,
        );
        rects
    }

    #[test]
    fn render_candidate_hover_row_shows_dashed_border() {
        let mut t = renderer();
        // 悬停第 1 行（真高亮第 0 行不动）：悬停行无填充底（内部=背景白），
        // 且表面与无悬停渲染不同（虚线框被画上）。
        let s = snap("ni'hao", &["你好", "泥 "], 0, 1);
        let snap_sel = UiSnapshot {
            selected: 0,
            ..s.clone()
        };
        let (base, _) = render_candidate(&snap_sel, &theme_light(), 1.0, &mut t, None);
        let (hovered, _) = render_candidate(&snap_sel, &theme_light(), 1.0, &mut t, Some(1));
        assert!(
            diff_count(&base, &hovered) > 0,
            "悬停渲染必须与无悬停不同（虚线框像素）"
        );
        // 悬停行内部（尾随空格区）= 背景白（无填充底）
        let rects = layout_rects(&snap_sel, &mut t);
        let (x, y) = row_sample(&rects, 1);
        let (r, g, b, a) = px(&hovered, x, y);
        assert!(a > 250, "悬停行内部不透明");
        assert_eq!((r, g, b), (0xFF, 0xFF, 0xFF), "悬停行内部背景白（无填充底）");
        // 对照：真高亮第 0 行内部 = hl_bg
        let (x0, y0) = row_sample(&rects, 0);
        let (r2, g2, b2, _) = px(&hovered, x0, y0);
        let [hlr, hlg, hlb, _] = theme_light().hl_bg;
        assert!(
            (r2 as i16 - hlr as i16).abs() <= 2
                && (g2 as i16 - hlg as i16).abs() <= 2
                && (b2 as i16 - hlb as i16).abs() <= 2,
            "真高亮行底色 ≈ hl_bg"
        );
    }

    #[test]
    fn render_candidate_hover_stacks_on_selection() {
        let mut t = renderer();
        // 悬停与真高亮同位置（第 1 行）：真高亮蓝底**不被覆盖**（内部仍 hl_bg），
        // 虚线框叠加其上（表面与仅真高亮渲染不同）。
        let s = snap("ni'hao", &["你好", "泥 "], 0, 1);
        let snap_sel = UiSnapshot {
            selected: 1,
            ..s.clone()
        };
        let (sel_only, _) = render_candidate(&snap_sel, &theme_light(), 1.0, &mut t, None);
        let (hovered, _) = render_candidate(&snap_sel, &theme_light(), 1.0, &mut t, Some(1));
        assert!(
            diff_count(&sel_only, &hovered) > 0,
            "悬停叠加必须改变表面（虚线框像素）"
        );
        let rects = layout_rects(&snap_sel, &mut t);
        let (x, y) = row_sample(&rects, 1);
        let (r, g, b, _) = px(&hovered, x, y);
        let [hlr, hlg, hlb, _] = theme_light().hl_bg;
        assert!(
            (r as i16 - hlr as i16).abs() <= 2
                && (g as i16 - hlg as i16).abs() <= 2
                && (b as i16 - hlb as i16).abs() <= 2,
            "同位置真高亮蓝底仍在（叠加而非覆盖）"
        );
    }

    #[test]
    fn render_candidate_flat_border_no_shadow() {
        let mut t = renderer();
        let s = snap("ni'hao", &["你好", "泥嚎", "你好吗"], 0, 1);
        let (surf, _rows) = render_candidate(&s, &theme_light(), 1.0, &mut t, None);
        // 无阴影：surface 尺寸 = 内容精确尺寸（无边距），边缘像素不半透明
        // 左缘像素列 0 = 完整覆盖的边框浅灰（scale=1 → 1px 描边）
        let (r, g, b, a) = px(&surf, 0, surf.h / 2);
        let [br, bg_, bb, _] = theme_light().border;
        assert_eq!(a, 255, "左缘为实心边框（无半透明阴影），实际 alpha={a}");
        assert!(
            (r as i16 - br as i16).abs() <= 3
                && (g as i16 - bg_ as i16).abs() <= 3
                && (b as i16 - bb as i16).abs() <= 3,
            "左缘色 ≈ 边框 #{br:02X}{bg_:02X}{bb:02X}，实际 {r:02X}{g:02X}{b:02X}"
        );
        // 内侧一列已是背景白（无边框残留/阴影渐变）
        let (r2, g2, b2, a2) = px(&surf, 1, surf.h / 2);
        assert_eq!((r2, g2, b2, a2), (0xFF, 0xFF, 0xFF, 255), "缘内即背景白");
    }

    #[test]
    fn render_candidate_dark_theme_dark_bg() {
        let mut t = renderer();
        let s = snap("ni'hao", &["你好", "泥嚎"], 0, 1);
        let (surf, _rows) = render_candidate(&s, &theme_dark(), 1.0, &mut t, None);
        let (r, g, b, a) = px(&surf, surf.w / 2, surf.h / 2);
        assert_eq!(a, 255);
        assert!(
            r < 0x40 && g < 0x40 && b < 0x40,
            "深色主题背景 0x202020 系，实际 {r:02X}{g:02X}{b:02X}"
        );
    }

    #[test]
    fn render_candidate_hdpi_doubles_size() {
        let mut t = renderer();
        let s = snap("ni'hao", &["你好", "泥嚎", "你好吗"], 0, 1);
        let (s1, _) = render_candidate(&s, &theme_light(), 1.0, &mut t, None);
        let (s2, _) = render_candidate(&s, &theme_light(), 2.0, &mut t, None);
        // surface 即内容尺寸（无阴影边距）：字号翻倍后 ceil 舍入每行 ≤1px，
        // padding 常量随 scale 缩放，总偏差很小。
        assert!(
            (s2.w as i64 - s1.w as i64 * 2).abs() <= 24,
            "scale=2 宽约 2 倍：{} vs {}",
            s1.w,
            s2.w
        );
        assert!(
            (s2.h as i64 - s1.h as i64 * 2).abs() <= 24,
            "scale=2 高约 2 倍：{} vs {}",
            s1.h,
            s2.h
        );
    }

    #[test]
    fn render_candidate_empty_snapshot_no_panic() {
        let mut t = renderer();
        let (surf, _rows) = render_candidate(&UiSnapshot::default(), &theme_light(), 1.0, &mut t, None);
        // 空快照：极小窗口但恒有像素缓冲（候选窗内容恒非空由引擎保证，这里只验证不 panic）
        assert!(surf.pixels.len() % 4 == 0);
    }

    #[test]
    fn render_menu_rows_and_hit() {
        let mut t = renderer();
        let items = vec![
            MenuEntry::new("中/英文切换", 1),
            MenuEntry::separator(),
            MenuEntry::new("设置", 2),
        ];
        let (surf, rows) = render_menu(&items, Some(0), &theme_light(), 1.0, &mut t);
        assert_eq!(rows.len(), 3, "3 个条目 3 行");
        assert!(rows.iter().all(|r| r.h == rows[0].h), "行高一致");
        // 首行命中 0
        assert_eq!(hit_test(&rows, rows[0].x + 1, rows[0].y + 1), Some(0));
        // 行间 gap 不命中
        assert_eq!(
            hit_test(&rows, rows[0].x + 1, rows[0].y + rows[0].h + 1),
            None
        );
        // 末行命中 2
        assert_eq!(
            hit_test(&rows, rows[2].x + 1, rows[2].y + rows[2].h - 1),
            Some(2)
        );
        // 菜单窗口尺寸 = 行高总和 + gap + padding（无阴影边距）
        assert_eq!(
            surf.w,
            (rows[0].w + crate::layout::PAD_X * 2) as u32
        );
        // 分隔线行：采样点避让中线分隔线（行内靠下）→ 背景底
        let (_, _, _, a) = px(
            &surf,
            (rows[1].x + rows[1].w / 2) as u32,
            (rows[1].y + rows[1].h - 2) as u32,
        );
        assert_eq!(a, 255, "分隔线行是背景底（不透明白）");
    }

    #[test]
    fn menu_hit_test_agrees_with_hit_test() {
        let mut t = renderer();
        let items = vec![MenuEntry::new("a", 1), MenuEntry::new("b", 2)];
        let (_, rows) = render_menu(&items, None, &theme_light(), 1.0, &mut t);
        assert_eq!(
            crate::menu::menu_hit_test(&rows, rows[0].x, rows[0].y),
            Some(0)
        );
        assert_eq!(crate::menu::menu_hit_test(&rows, -5, -5), None);
    }

    #[test]
    fn render_toolbar_geometry_and_hit() {
        let icons = ToolbarIcons::default();
        let spec = ToolbarSpec {
            icons: &icons,
            state: ImeState {
                mode: InitialMode::English,
                width: WidthMode::Half,
                punct: PunctMode::English,
                script: ScriptMode::Simplified,
            },
            hover: Some(TB_GEAR),
            pressed: None,
        };
        let (surf, rects) = render_toolbar(&spec, &theme_light(), 1.0, );
        assert_eq!(rects.len(), TB_COUNT, "6 按钮");
        assert!(rects.iter().all(|r| r.h == rects[0].h), "等高");
        // 按钮横排：x 递增、y 相同
        assert_eq!(rects[0].y, rects[1].y);
        assert!(rects[1].x > rects[0].x);
        assert!(rects[2].x > rects[1].x);
        // 命中测试：首按钮中心命中 0
        assert_eq!(
            hit_test(&rects, rects[0].x + rects[0].w / 2, rects[0].y + rects[0].h / 2),
            Some(TB_LOGO)
        );
        // surface 尺寸 = 内容精确尺寸（扁平无阴影边距）
        assert_eq!(surf.w, (rects[0].w * TB_COUNT as i32 + TOOLBAR_GAP as i32 * (TB_COUNT as i32 - 1) + TOOLBAR_PAD as i32 * 2) as u32);
        assert!(surf.h > 0);
        // 命中区与绘制重合回归锚：矩形即表面坐标（无偏移）——首按钮矩形左上角
        // 必须落在 surface 内且不越出（2026-08-22 曾因阴影 margin 未计入矩形，
        // 命中区相对图标整体左上偏移 8×scale px）。
        let r0 = &rects[TB_LOGO];
        assert!(r0.x >= TOOLBAR_PAD as i32 && r0.y >= TOOLBAR_PAD as i32);
        assert!(
            (r0.x + r0.w) as u32 <= surf.w && (r0.y + r0.h) as u32 <= surf.h,
            "按钮矩形必须完整落在 surface 内"
        );
        // 图标缺失（默认空）：渲染不 panic 且表面有效
        assert!(surf.pixels.len() % 4 == 0);
    }

    #[test]
    fn render_tooltip_smoke() {
        let mut t = renderer();
        let surf = render_tooltip("简体/繁体", &theme_light(), 1.0, &mut t);
        assert!(surf.w > 0 && surf.h > 0);
        assert!(surf.pixels.len() % 4 == 0);
        // 空标签不 panic（极小表面）
        let empty = render_tooltip("", &theme_light(), 1.0, &mut t);
        assert!(empty.pixels.len() % 4 == 0);
    }

    /// 构造合成图标：四角透明 + 中心不透明（模拟真实图标"居中 + 透明边距"）。
    /// from_bbox 漏洞只会采样左上角像素（透明）→ 回归测试能抓住。
    fn center_icon(w: u32, h: u32, inner: u32) -> Pixmap {
        let mut p = Pixmap::new(w, h).unwrap();
        p.fill(tiny_skia::Color::TRANSPARENT);
        let mut paint = Paint::default();
        paint.set_color_rgba8(0x12, 0x34, 0x56, 0xFF);
        let x0 = ((w as i32 - inner as i32) / 2) as f32;
        let y0 = ((h as i32 - inner as i32) / 2) as f32;
        if let Some(rect) = Rect::from_xywh(x0, y0, inner as f32, inner as f32) {
            p.fill_rect(rect, &paint, Transform::identity(), None);
        }
        p
    }

    /// 采样 (x,y) 的 alpha（surface 坐标）。
    fn alpha_at(surface: &Surface, x: u32, y: u32) -> u8 {
        let idx = ((y * surface.w + x) * 4 + 3) as usize;
        surface.pixels[idx]
    }

    #[test]
    fn render_toolbar_icon_visible_not_corner_crop() {
        // 回归：from_bbox 曾导致图标只采样左上角 1 像素（透明 → 整片空白）。
        // from_scale 应把"中心不透明"的图标完整缩小绘制 → 按钮中心采样不透明。
        let mut icons = ToolbarIcons::default();
        icons.logo = Some(center_icon(284, 282, 200));
        let spec = ToolbarSpec {
            icons: &icons,
            state: ImeState::default(),
            hover: None,
            pressed: None,
        };
        let (surf, rects) = render_toolbar(&spec, &theme_light(), 1.0);
        // logo 按钮中心采样（surface 坐标 = 按钮矩形坐标，扁平无偏移）
        let cx = (rects[TB_LOGO].x + rects[TB_LOGO].w / 2) as u32;
        let cy = (rects[TB_LOGO].y + rects[TB_LOGO].h / 2) as u32;
        let a = alpha_at(&surf, cx, cy);
        assert!(a > 0, "logo 按钮中心应绘制出图标（不透明），实际 alpha={a}");
        // 对照：背景（非按钮区，如按钮间隙）应恒透明以外的背景底（不透明）
        let bg_x = (rects[TB_LOGO].x + rects[TB_LOGO].w + 1) as u32;
        let bg_a = alpha_at(&surf, bg_x, cy);
        assert!(bg_a > 0, "按钮间隙应为背景底（不透明）");
    }
}
