//! 浮动工具栏渲染（32-status-toolbar.md §6.4；P2.4 自 render.rs 拆出）。
//! 横排 6 按钮条（logo | 中英 | 全半角 | 标点 | 简繁 | 齿轮），圆角阴影风格
//! 与候选窗/菜单一致；按钮命中用返回的矩形列表喂 `hit_test`。

use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};

use iuv_core::{ImeState, InitialMode, PunctMode, ScriptMode, WidthMode};

use crate::layout::Rect as LayoutRect;
use crate::paint::{fill_rounded, HL_RADIUS};
use crate::render::{render_to_surface, Surface};
use crate::theme::Theme;

/// 工具栏按钮几何（物理像素 @96dpi 基准；render 乘 scale）。
pub(crate) const TOOLBAR_BTN: f32 = 30.0;
pub(crate) const TOOLBAR_GAP: f32 = 4.0;
pub(crate) const TOOLBAR_PAD: f32 = 6.0;

/// 工具栏按钮索引（布局顺序：logo | 中英 | 全半角 | 标点 | 简繁 | 齿轮）。
pub const TB_LOGO: usize = 0;
pub const TB_MODE: usize = 1;
pub const TB_WIDTH: usize = 2;
pub const TB_PUNCT: usize = 3;
pub const TB_SCRIPT: usize = 4;
pub const TB_GEAR: usize = 5;
pub(crate) const TB_COUNT: usize = 6;

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

/// 工具栏渲染规格：图标集 + 当前四态（iuv-core `ImeState`，全仓唯一四态表示）+ 交互态。
pub struct ToolbarSpec<'a> {
    pub icons: &'a ToolbarIcons,
    /// 当前实例运行时四态。
    pub state: ImeState,
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
        TB_MODE => match spec.state.mode {
            InitialMode::Chinese => spec.icons.lang_cn.as_ref(),
            InitialMode::English => spec.icons.lang_en.as_ref(),
        },
        TB_WIDTH => match spec.state.width {
            WidthMode::Half => spec.icons.width_half.as_ref(),
            WidthMode::Full => spec.icons.width_full.as_ref(),
        },
        TB_PUNCT => match spec.state.punct {
            PunctMode::Chinese => spec.icons.punct_cn.as_ref(),
            PunctMode::English => spec.icons.punct_en.as_ref(),
        },
        TB_SCRIPT => match spec.state.script {
            ScriptMode::Simplified => spec.icons.script_simplified.as_ref(),
            ScriptMode::Traditional => spec.icons.script_traditional.as_ref(),
        },
        TB_GEAR => spec.icons.gear.as_ref(),
        _ => None,
    }
}

/// 图标按目标矩形缩放绘制（等比，居中，留 inset 内边距；`sx` 为 surface 阴影偏移）。
/// 缩放 = 预缩放到目标尺寸的临时 Pixmap + identity 绘制（语义直白，避免 transform
/// 叠加歧义）；分配失败静默跳过（按钮留空，不 panic）。
/// 2026-08-21 修：缩放变换用 `from_scale` 而非 `from_bbox`——`from_bbox` 把源坐标
/// (0..iw) 映射到 (0..iw*scale)，目标画布只有 dw 大小 → 只采样源图左上角 ~1 像素
/// （图标居中、四角透明 → 整片空白，实测 32-toolbar 图标全空）。
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
    let paint = PixmapPaint {
        opacity: 1.0,
        quality: FilterQuality::Bilinear,
        ..Default::default()
    };
    // from_scale(dw/iw, dh/ih)：源图标 (iw×ih) 等比缩放到目标尺寸 (dw×dh) 的左上角原点。
    dst.draw_pixmap(
        0,
        0,
        icon.as_ref(),
        &paint,
        Transform::from_scale(dw / iw, dh / ih),
        None,
    );
    let x = (sx + r.x as f32 + (r.w as f32 - dw) / 2.0).round() as i32;
    let y = (sx + r.y as f32 + (r.h as f32 - dh) / 2.0).round() as i32;
    let paint2 = PixmapPaint::default();
    canvas.draw_pixmap(x, y, dst.as_ref(), &paint2, Transform::identity(), None);
}