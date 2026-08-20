//! 渲染：候选窗 / 菜单 → `Surface`（premultiplied BGRA 像素缓冲）。
//!
//! 绘制管线（自底向上）：阴影（分层假模糊）→ 圆角矩形底 → 高亮行底 → 行文本
//! （含编号，原文兜底候选不编号）→ 页码小字 → 1px 边框。
//! 全程 tiny-skia 纯 safe API；分配失败/非法参数静默降级返回空 `Surface`，绝不 panic。
//!
//! `Surface.pixels` 为 **premultiplied BGRA**、u32 对齐行、无 stride 填充——
//! Windows 呈现层（ULW 32bpp DIB / D2D CreateBitmap 等）直供；
//! 其他平台自行转格式。

use tiny_skia::{
    FillRule, NonZeroRect, Paint, Path, PathBuilder, Pixmap, PixmapPaint, FilterQuality, Rect, Stroke,
    Transform,
};

use crate::layout::{candidate_label, layout, Rect as LayoutRect};
use crate::menu::MenuEntry;
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

/// 阴影分层数（假模糊：由内向外逐层放大、逐层减淡）。
const SHADOW_PASSES: u32 = 4;
/// 阴影整体下移（顶部光源）。
const SHADOW_OFFSET_Y: f32 = 2.0;

/// 高亮行圆角（px，物理像素缩放前；可简化小圆角）。
const HL_RADIUS: f32 = 2.0;

/// 渲染候选窗：按 `snap` 布局（layout 纯函数 + cosmic-text 测量）→ 绘制 → Surface。
///
/// - `scale`：DPI 缩放（dpi/96）；字号 = `FONT_PX_96 * scale`，页码小字 = 一半；
/// - Surface 尺寸 = 布局宽高 + `2 × shadow_size × scale`（阴影外缘），内容区偏移
///   阴影宽度——与 19-m4-cross-render.md §3 的呈现层接缝一致；
/// - 行高 = cosmic-text 实际 line height（≈ 20px @96dpi 基准，与 GDI 观感一致）；
/// - `hover`：鼠标悬停行（纯视觉，不驱动会话）。悬停行画**虚线框**（`hover_border`），
///   与真高亮**叠加**显示：真高亮蓝底照常，悬停只在其上加框，不覆盖任何东西；
///   悬停行无填充底、文字保持正文色。超出候选数（候选变更后悬停索引未刷新）
///   自然不画任何框。
pub fn render_candidate(
    snap: &UiSnapshot,
    theme: &Theme,
    scale: f32,
    text: &mut TextRenderer,
    hover: Option<usize>,
) -> Surface {
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
    render_to_surface(
        theme,
        scale,
        cw.max(0) as u32,
        ch.max(0) as u32,
        |pixmap, sx| {
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
                        sx + r.x as f32,
                        sx + r.y as f32,
                        r.w as f32,
                        r.h as f32,
                        (HL_RADIUS * scale).min(r.h as f32 / 2.0),
                        theme.hl_bg,
                    );
                }
                if hover == Some(i) {
                    stroke_rounded_dashed(
                        pixmap,
                        sx + r.x as f32,
                        sx + r.y as f32,
                        r.w as f32,
                        r.h as f32,
                        (HL_RADIUS * scale).min(r.h as f32 / 2.0),
                        1.0_f32.max(scale),
                        theme.hover_border,
                    );
                }
                let label = candidate_label(snap, i, cand);
                let color = if sel { theme.hl_fg } else { theme.fg };
                text.draw(
                    pixmap,
                    &label,
                    sx + r.x as f32,
                    sx + r.y as f32,
                    size_px,
                    color,
                );
            }
            // 页码小字（多页时；行号 = candidates.len()）
            if snap.page.page_count > 1 {
                if let Some(r) = rects.get(snap.candidates.len()) {
                    let label = format!("{}/{}", snap.page.page + 1, snap.page.page_count);
                    text.draw(
                        pixmap,
                        &label,
                        sx + r.x as f32,
                        sx + r.y as f32,
                        page_px,
                        theme.page_fg,
                    );
                }
            }
        },
    )
}

/// 渲染菜单（M5 托盘菜单用）：竖排条目列表 + 阴影圆角，风格与候选窗一致。
///
/// 返回 `(Surface, 行矩形列表)`——行矩形为 **surface 坐标**（含阴影偏移），
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
    // 行矩形（surface 坐标，含阴影偏移）
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
        |pixmap, sx| {
            for (i, item) in items.iter().enumerate() {
                let Some(r) = rows.get(i) else {
                    break; // 防御
                };
                let sel = selected == Some(i) && !item.is_separator();
                if sel {
                    fill_rounded(
                        pixmap,
                        sx + r.x as f32,
                        sx + r.y as f32,
                        r.w as f32,
                        r.h as f32,
                        (HL_RADIUS * scale).min(r.h as f32 / 2.0),
                        theme.hl_bg,
                    );
                }
                if item.is_separator() {
                    // 分隔线：水平 1px（行内垂直居中）
                    let cy = sx + r.y as f32 + r.h as f32 / 2.0;
                    if let Some(rect) = Rect::from_xywh(sx + r.x as f32, cy - 0.5, r.w as f32, 1.0)
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
                let tx = sx + r.x as f32;
                let ty = sx + r.y as f32 + pad_y as f32;
                text.draw(pixmap, &item.label, tx, ty, size_px, color);
            }
        },
    );
    (surface, rows)
}

// ===== 32-status-toolbar.md §6.4 浮动工具栏 =====

/// 工具栏按钮几何（物理像素 @96dpi 基准；render 乘 scale）。
pub const TOOLBAR_BTN: f32 = 30.0;
pub const TOOLBAR_GAP: f32 = 4.0;
pub const TOOLBAR_PAD: f32 = 6.0;

/// 工具栏按钮索引（布局顺序：logo | 中英 | 全半角 | 标点 | 简繁 | 齿轮）。
pub const TB_LOGO: usize = 0;
pub const TB_MODE: usize = 1;
pub const TB_WIDTH: usize = 2;
pub const TB_PUNCT: usize = 3;
pub const TB_SCRIPT: usize = 4;
pub const TB_GEAR: usize = 5;
pub const TB_COUNT: usize = 6;

/// 工具栏图标集（daemon 从内嵌 PNG 解码，失败降级 None——按钮留空不 panic，
/// §6.7：源图即最终素材，`Pixmap::decode_png` + `draw_pixmap` 缩放绘制）。
#[derive(Clone, Default)]
pub struct ToolbarIcons {
    /// 输入法 logo（TB_LOGO，拖动把手）。
    pub logo: Option<Pixmap>,
    /// 中英双态（TB_MODE）。
    pub lang_cn: Option<Pixmap>,
    pub lang_en: Option<Pixmap>,
    /// 全半角双态（TB_WIDTH）。
    pub width_half: Option<Pixmap>,
    pub width_full: Option<Pixmap>,
    /// 中英文标点双态（TB_PUNCT）。
    pub punct_cn: Option<Pixmap>,
    pub punct_en: Option<Pixmap>,
    /// 简繁双态（TB_SCRIPT）。
    pub script_simplified: Option<Pixmap>,
    pub script_traditional: Option<Pixmap>,
    /// 齿轮设置（TB_GEAR）。
    pub gear: Option<Pixmap>,
}

/// 工具栏渲染规格：图标集 + 当前四态（u8，与 iuv-data `ToolbarState` 编码一致，
/// 0=中/半/简/中标，1=英/全/繁/英标）+ 交互态。
pub struct ToolbarSpec<'a> {
    pub icons: &'a ToolbarIcons,
    pub mode: u8,
    pub width: u8,
    pub punct: u8,
    pub script: u8,
    /// 悬停按钮（纯视觉，浅底）。
    pub hover: Option<usize>,
    /// 按下按钮（更深底，点击反馈）。
    pub pressed: Option<usize>,
}

/// 渲染浮动工具栏：横排 6 按钮条，风格与候选窗/菜单一致（圆角阴影 + 主题）。
/// 返回 `(Surface, 按钮矩形列表)`——矩形为 surface 坐标（含阴影偏移），
/// 直接喂 `hit_test` 做按钮命中。
pub fn render_toolbar(
    spec: &ToolbarSpec,
    theme: &Theme,
    scale: f32,
) -> (Surface, Vec<LayoutRect>) {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let btn = (TOOLBAR_BTN * scale).ceil();
    let gap = (TOOLBAR_GAP * scale).ceil();
    let pad = (TOOLBAR_PAD * scale).ceil();
    let content_w = (btn * TB_COUNT as f32) + (gap * (TB_COUNT as f32 - 1.0)) + pad * 2.0;
    let content_h = btn + pad * 2.0;
    // 按钮矩形（内容坐标；render_to_surface 回调内叠 sx 阴影偏移）
    let mut rects = Vec::with_capacity(TB_COUNT);
    for i in 0..TB_COUNT {
        rects.push(LayoutRect {
            x: (pad + i as f32 * (btn + gap)).round() as i32,
            y: pad.round() as i32,
            w: btn.round() as i32,
            h: btn.round() as i32,
        });
    }
    let surface = render_to_surface(
        theme,
        scale,
        content_w as u32,
        content_h as u32,
        |pixmap, sx| {
            for (i, r) in rects.iter().enumerate() {
                let hover = spec.hover == Some(i);
                let pressed = spec.pressed == Some(i);
                if hover || pressed {
                    // 悬停浅底 / 按下更深底（用主题正文字色叠加低 alpha，两套主题都成立）
                    let alpha = if pressed { 0x2A } else { 0x14 };
                    fill_rounded(
                        pixmap,
                        sx + r.x as f32,
                        sx + r.y as f32,
                        r.w as f32,
                        r.h as f32,
                        (HL_RADIUS * scale).min(r.h as f32 / 2.0),
                        [theme.fg[0], theme.fg[1], theme.fg[2], alpha],
                    );
                }
                if let Some(icon) = toolbar_icon(spec, i) {
                    // 图标按目标尺寸缩放居中（inset 内边距；源图 ~28-32px 近方形）
                    let inset = (3.0 * scale).ceil();
                    draw_icon_scaled(pixmap, icon, r, inset, sx);
                }
            }
        },
    );
    (surface, rects)
}

/// 按按钮索引 + 当前四态选图标（None = 图标缺失/未知索引）。
fn toolbar_icon<'a>(spec: &'a ToolbarSpec, i: usize) -> Option<&'a Pixmap> {
    match i {
        TB_LOGO => spec.icons.logo.as_ref(),
        TB_MODE => {
            if spec.mode == 0 {
                spec.icons.lang_cn.as_ref()
            } else {
                spec.icons.lang_en.as_ref()
            }
        }
        TB_WIDTH => {
            if spec.width == 0 {
                spec.icons.width_half.as_ref()
            } else {
                spec.icons.width_full.as_ref()
            }
        }
        TB_PUNCT => {
            if spec.punct == 0 {
                spec.icons.punct_cn.as_ref()
            } else {
                spec.icons.punct_en.as_ref()
            }
        }
        TB_SCRIPT => {
            if spec.script == 0 {
                spec.icons.script_simplified.as_ref()
            } else {
                spec.icons.script_traditional.as_ref()
            }
        }
        TB_GEAR => spec.icons.gear.as_ref(),
        _ => None,
    }
}

/// 图标按目标矩形缩放绘制（等比，居中，留 inset 内边距；`sx` 为 surface 阴影偏移）。
/// 缩放 = 预缩放到目标尺寸的临时 Pixmap + identity 绘制（语义直白，避免 transform
/// 叠加歧义）；分配失败静默跳过（按钮留空，不 panic）。
fn draw_icon_scaled(
    canvas: &mut Pixmap,
    icon: &Pixmap,
    r: &LayoutRect,
    inset: f32,
    sx: f32,
) {
    let avail = (r.w as f32 - inset * 2.0).min(r.h as f32 - inset * 2.0);
    if avail <= 0.0 {
        return;
    }
    let iw = icon.width() as f32;
    let ih = icon.height() as f32;
    if iw <= 0.0 || ih <= 0.0 {
        return;
    }
    let scale = (avail / iw).min(avail / ih);
    let dw = (iw * scale).round().max(1.0);
    let dh = (ih * scale).round().max(1.0);
    let Some(mut dst) = Pixmap::new(dw as u32, dh as u32) else {
        return; // 分配失败：静默跳过
    };
    let Some(bbox) = NonZeroRect::from_xywh(0.0, 0.0, dw, dh) else {
        return;
    };
    let paint = PixmapPaint {
        opacity: 1.0,
        quality: FilterQuality::Bilinear,
        ..Default::default()
    };
    // from_bbox：把源图标（iw×ih）仿射映射到目标矩形（0,0,dw,dh）。
    dst.draw_pixmap(0, 0, icon.as_ref(), &paint, Transform::from_bbox(bbox), None);
    let x = (sx + r.x as f32 + (r.w as f32 - dw) / 2.0).round() as i32;
    let y = (sx + r.y as f32 + (r.h as f32 - dh) / 2.0).round() as i32;
    let paint2 = PixmapPaint::default();
    canvas.draw_pixmap(x, y, dst.as_ref(), &paint2, Transform::identity(), None);
}

/// 渲染悬停 tooltip（32-status-toolbar.md §6.6「全半角」「简体/繁体」等）：单行小标签，
/// 风格与候选窗一致（圆角阴影）。返回 Surface（无命中矩形——tooltip 不接收点击）。
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
    let pad_x = (8.0 * scale).ceil() as i32;
    let pad_y = (4.0 * scale).ceil() as i32;
    let cw = w + pad_x * 2;
    let ch = h + pad_y * 2;
    render_to_surface(
        theme,
        scale,
        cw.max(1) as u32,
        ch.max(1) as u32,
        |pixmap, sx| {
            text.draw(
                pixmap,
                label,
                sx + pad_x as f32,
                sx + pad_y as f32,
                size_px,
                theme.fg,
            );
        },
    )
}

/// 通用外壳：建 Surface 画布（内容区 + 阴影外缘）→ 画阴影/圆角底/边框 → 调用 `draw`。
fn render_to_surface(
    theme: &Theme,
    scale: f32,
    cw: u32,
    ch: u32,
    draw: impl FnOnce(&mut Pixmap, f32),
) -> Surface {
    if cw == 0 || ch == 0 {
        return Surface::empty();
    }
    let shadow_px = (theme.shadow_size * scale).round().max(0.0) as u32;
    let margin = shadow_px.saturating_mul(2);
    let (Some(w), Some(h)) = (cw.checked_add(margin), ch.checked_add(margin)) else {
        return Surface::empty();
    };
    let Some(mut pixmap) = Pixmap::new(w, h) else {
        return Surface::empty(); // 分配失败：静默降级
    };
    let sx = shadow_px as f32; // 内容区偏移（阴影内边距）
                               // 1) 阴影（分层假模糊：由内向外膨胀 + 减淡）
    draw_shadow(&mut pixmap, theme, scale, sx, cw as f32, ch as f32);
    // 2) 圆角矩形底
    let radius = theme.corner_radius * scale;
    let bg_path = rounded_rect_path(sx, sx, cw as f32, ch as f32, radius);
    if let Some(path) = &bg_path {
        fill_path(&mut pixmap, path, theme.bg);
    }
    // 3) 内容（高亮行/文本/页码）
    draw(&mut pixmap, sx);
    // 4) 1px 边框（圆角跟随）
    if let Some(path) = &bg_path {
        let mut paint = Paint::default();
        paint.set_color_rgba8(
            theme.border[0],
            theme.border[1],
            theme.border[2],
            theme.border[3],
        );
        let stroke = Stroke {
            width: 1.0_f32.max(scale),
            ..Stroke::default()
        };
        pixmap.stroke_path(path, &paint, &stroke, Transform::identity(), None);
    }
    // tiny-skia 0.12 像素缓冲为 premultiplied RGBA（内存序 r,g,b,a）；
    // Surface 契约要求 premultiplied BGRA（Windows ULW DIB / D2D 直供）——交换每像素 R/B。
    let data = pixmap.data();
    let mut pixels = Vec::with_capacity(data.len());
    pixels.extend_from_slice(data);
    for px in pixels.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    Surface { w, h, pixels }
}

/// 阴影：`SHADOW_PASSES` 层膨胀圆角矩形（外缘最淡），整体下移模拟顶部光源。
fn draw_shadow(pixmap: &mut Pixmap, theme: &Theme, scale: f32, sx: f32, cw: f32, ch: f32) {
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
fn fill_rounded(pixmap: &mut Pixmap, x: f32, y: f32, w: f32, h: f32, r: f32, color: [u8; 4]) {
    if w <= 0.0 || h <= 0.0 {
        return;
    }
    if let Some(path) = rounded_rect_path(x, y, w, h, r) {
        fill_path(pixmap, &path, color);
    }
}

/// 虚线圆角矩形框（悬停高亮用）：内缩 1px 防跨行压邻行/文本；
/// dash 规格 [4,3]（4px 线 + 3px 空，物理像素）。
fn stroke_rounded_dashed(
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

fn fill_path(pixmap: &mut Pixmap, path: &Path, color: [u8; 4]) {
    let mut paint = Paint::default();
    paint.set_color_rgba8(color[0], color[1], color[2], color[3]);
    pixmap.fill_path(path, &paint, FillRule::Winding, Transform::identity(), None);
}

/// 圆角矩形路径：tiny-skia 0.12 无 RoundedRect 图元，四角用三次贝塞尔近似
/// （k = 0.55228475，圆弧标准拟合）。
fn rounded_rect_path(x: f32, y: f32, w: f32, h: f32, r: f32) -> Option<Path> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{hit_test, layout};
    use crate::theme::{theme_dark, theme_light};
    use iuv_core::{Orientation, PageInfo};

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
        let surf = render_candidate(&s, &theme_light(), 1.0, &mut t, None);
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
        let surf = render_candidate(&s, &theme_light(), 1.0, &mut t, None);
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
        let surf = render_candidate(&snap_sel, &theme_light(), 1.0, &mut t, None);
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
        let shadow = theme_light().shadow_size as u32;
        let row = rects[1];
        // 行矩形尾部 3px 内（尾随空格区）应等于 hl_bg #0078D7
        let (r, g, b, a) = px(
            &surf,
            shadow + (row.x + row.w - 3) as u32,
            shadow + (row.y + row.h / 2) as u32,
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
            shadow + (rects[0].x + rects[0].w - 3) as u32,
            shadow + (rects[0].y + rects[0].h / 2) as u32,
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
    fn row_sample(rects: &[LayoutRect], idx: usize, shadow: u32) -> (u32, u32) {
        let r = rects[idx];
        (shadow + (r.x + r.w - 3) as u32, shadow + (r.y + r.h / 2) as u32)
    }

    fn layout_rects(snap: &UiSnapshot, t: &mut TextRenderer) -> (u32, Vec<LayoutRect>) {
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
        (shadow_size_for(snap), rects)
    }

    fn shadow_size_for(_snap: &UiSnapshot) -> u32 {
        theme_light().shadow_size as u32
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
        let base = render_candidate(&snap_sel, &theme_light(), 1.0, &mut t, None);
        let hovered = render_candidate(&snap_sel, &theme_light(), 1.0, &mut t, Some(1));
        assert!(
            diff_count(&base, &hovered) > 0,
            "悬停渲染必须与无悬停不同（虚线框像素）"
        );
        // 悬停行内部（尾随空格区）= 背景白（无填充底）
        let (shadow, rects) = layout_rects(&snap_sel, &mut t);
        let (x, y) = row_sample(&rects, 1, shadow);
        let (r, g, b, a) = px(&hovered, x, y);
        assert!(a > 250, "悬停行内部不透明");
        assert_eq!((r, g, b), (0xFF, 0xFF, 0xFF), "悬停行内部背景白（无填充底）");
        // 对照：真高亮第 0 行内部 = hl_bg
        let (x0, y0) = row_sample(&rects, 0, shadow);
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
        let sel_only = render_candidate(&snap_sel, &theme_light(), 1.0, &mut t, None);
        let hovered = render_candidate(&snap_sel, &theme_light(), 1.0, &mut t, Some(1));
        assert!(
            diff_count(&sel_only, &hovered) > 0,
            "悬停叠加必须改变表面（虚线框像素）"
        );
        let (shadow, rects) = layout_rects(&snap_sel, &mut t);
        let (x, y) = row_sample(&rects, 1, shadow);
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
    fn render_candidate_shadow_soft() {
        let mut t = renderer();
        let s = snap("ni'hao", &["你好", "泥嚎", "你好吗"], 0, 1);
        let surf = render_candidate(&s, &theme_light(), 1.0, &mut t, None);
        let shadow = theme_light().shadow_size as u32;
        assert!(surf.w > shadow * 2 && surf.h > shadow * 2);
        // 阴影区（内容矩形外、surface 边缘内）：alpha 0 < a < 255
        let (_, _, _, a) = px(&surf, 1, surf.h / 2);
        assert!(a > 0 && a < 255, "左缘阴影半透明，实际 alpha={a}");
        let (_, _, _, a2) = px(&surf, surf.w - 2, surf.h / 2);
        assert!(a2 > 0 && a2 < 255, "右缘阴影半透明，实际 alpha={a2}");
    }

    #[test]
    fn render_candidate_dark_theme_dark_bg() {
        let mut t = renderer();
        let s = snap("ni'hao", &["你好", "泥嚎"], 0, 1);
        let surf = render_candidate(&s, &theme_dark(), 1.0, &mut t, None);
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
        let s1 = render_candidate(&s, &theme_light(), 1.0, &mut t, None);
        let s2 = render_candidate(&s, &theme_light(), 2.0, &mut t, None);
        // 内容区（去掉阴影边距）应约 2 倍：padding 为常量不随 scale 缩放，
        // 字号翻倍后 ceil 舍入每行 ≤1px，总偏差很小。
        let m = theme_light().shadow_size as i64;
        let c1w = s1.w as i64 - m * 2;
        let c2w = s2.w as i64 - m * 4;
        assert!(
            (c2w - c1w * 2).abs() <= 24,
            "scale=2 内容宽约 2 倍：{} vs {}",
            c1w,
            c2w
        );
        let c1h = s1.h as i64 - m * 2;
        let c2h = s2.h as i64 - m * 4;
        assert!(
            (c2h - c1h * 2).abs() <= 24,
            "scale=2 内容高约 2 倍：{} vs {}",
            c1h,
            c2h
        );
        // 整体（含阴影边距）比例 ≈ 2
        let rw = s2.w as f64 / s1.w as f64;
        let rh = s2.h as f64 / s1.h as f64;
        assert!(rw > 1.7 && rw < 2.3, "整体宽比例 ≈2：{rw}");
        assert!(rh > 1.7 && rh < 2.3, "整体高比例 ≈2：{rh}");
    }

    #[test]
    fn render_candidate_empty_snapshot_no_panic() {
        let mut t = renderer();
        let surf = render_candidate(&UiSnapshot::default(), &theme_light(), 1.0, &mut t, None);
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
        // 菜单窗口尺寸 = 行高总和 + gap + padding + 阴影外缘
        assert_eq!(
            surf.w,
            (rows[0].w + crate::layout::PAD_X * 2) as u32 + (theme_light().shadow_size as u32) * 2
        );
        // 分隔线行：采样点避让中线分隔线（行内靠下）→ 背景底
        let shadow = theme_light().shadow_size as u32;
        let (_, _, _, a) = px(
            &surf,
            shadow + rows[1].x as u32 + rows[1].w as u32 / 2,
            shadow + rows[1].y as u32 + rows[1].h as u32 - 2,
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
            mode: 1,
            width: 0,
            punct: 1,
            script: 0,
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
        // surface 尺寸 = 内容 + 阴影外缘
        let shadow = theme_light().shadow_size as u32;
        assert_eq!(surf.w, (rects[0].w * TB_COUNT as i32 + TOOLBAR_GAP as i32 * (TB_COUNT as i32 - 1) + TOOLBAR_PAD as i32 * 2) as u32 + shadow * 2);
        assert!(surf.h > 0);
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
}
