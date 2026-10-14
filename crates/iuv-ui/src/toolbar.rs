//! 浮动工具栏渲染（32-status-toolbar.md §6.4；P2.4 自 render.rs 拆出）。
//! 横排 7 按钮条（logo | 中英 | 全半角 | 标点 | 简繁 | 瞄准镜 | 齿轮），扁平圆角细边框风格
//! 与候选窗/菜单一致；按钮命中用返回的矩形列表喂 `hit_test`。
//!
//! 2026-10-12（54 号第一步）：原「M1 桌宠骨架 · 复合渲染」（工具栏 + 宠物区合成：
//! `render_composite` / `CompositeSpec` / `PetSpec` / `PET_OVERHANG` 等）随自有桌宠
//! 一并移除——本模块只负责纯工具栏渲染。

use iuv_core::{ImeMode, ImePunct, ImeScript, ImeState, ImeWidth};
use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};

use crate::layout::Rect as LayoutRect;
use crate::paint::{fill_rounded, HL_RADIUS};
use crate::render::{render_to_surface, Surface};
use crate::theme::Theme;

/// 工具栏按钮几何（物理像素 @96dpi 基准；render 乘 scale）。
pub(crate) const TOOLBAR_BTN: f32 = 30.0;
/// 按钮高度比宽度矮（2026-10-07：图标 24px 不动，垂直 inset 3→1、按钮 30→26、
/// pad_y 2——字形到工具栏边缘仅 3px，工具栏整体压扁到 30px 高）。
pub(crate) const TOOLBAR_BTN_H: f32 = 26.0;
pub(crate) const TOOLBAR_GAP: f32 = 4.0;
/// 内边距横竖分开（2026-10-07：竖向压扁，见 TOOLBAR_BTN_H 注释）。
pub(crate) const TOOLBAR_PAD_X: f32 = 6.0;
pub(crate) const TOOLBAR_PAD_Y: f32 = 2.0;

/// 工具栏按钮索引（布局顺序：logo | 中英 | 全半角 | 标点 | 简繁 | 瞄准镜 | 齿轮）。
/// 左 5 个是**双态开关**（有持久状态），右 2 个是**动作类**（点击触发一次）——
/// 瞄准镜（进入捕捉）与齿轮同类，故相邻放右端。
pub const TB_LOGO: usize = 0;
pub const TB_MODE: usize = 1;
pub const TB_WIDTH: usize = 2;
pub const TB_PUNCT: usize = 3;
pub const TB_SCRIPT: usize = 4;
/// 瞄准镜：进入桌宠捕捉态（54-2）。
pub const TB_FINDER: usize = 5;
pub const TB_GEAR: usize = 6;
pub(crate) const TB_COUNT: usize = 7;

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
    /// 瞄准镜（TB_FINDER，54-2：进入桌宠捕捉态）。
    pub finder: Option<Pixmap>,
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

/// 渲染浮动工具栏：横排 6 按钮条，风格与候选窗/菜单一致（扁平圆角细边框）。
/// 返回 `(Surface, 按钮矩形列表)`——矩形为 surface 坐标（= 内容坐标，无边框留白
/// 偏移），与窗口客户区坐标系重合，直接喂 `hit_test` 做按钮命中。
pub fn render_toolbar(spec: &ToolbarSpec, theme: &Theme, scale: f32) -> (Surface, Vec<LayoutRect>) {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let btn = (TOOLBAR_BTN * scale).ceil();
    let btn_h = (TOOLBAR_BTN_H * scale).ceil();
    let gap = (TOOLBAR_GAP * scale).ceil();
    let pad_x = (TOOLBAR_PAD_X * scale).ceil();
    let pad_y = (TOOLBAR_PAD_Y * scale).ceil();
    let content_w = (btn * TB_COUNT as f32) + (gap * (TB_COUNT as f32 - 1.0)) + pad_x * 2.0;
    let content_h = btn_h + pad_y * 2.0;
    // 按钮矩形（内容坐标；无阴影时代内容坐标即表面坐标，命中区与绘制严格重合）
    let mut rects = Vec::with_capacity(TB_COUNT);
    for i in 0..TB_COUNT {
        rects.push(LayoutRect {
            x: (pad_x + i as f32 * (btn + gap)).round() as i32,
            y: pad_y.round() as i32,
            w: btn.round() as i32,
            h: btn_h.round() as i32,
        });
    }
    let surface = render_to_surface(theme, scale, content_w as u32, content_h as u32, |pixmap| {
        draw_toolbar_content(pixmap, spec, theme, &rects, scale);
    });
    (surface, rects)
}

/// 工具栏内容绘制（hover 高亮 + 图标缩放绘制）。
/// 边框 + 圆角底由调用方负责；本函数只画"按钮区"（透明区域上的 hover 底 + 图标）。
pub(crate) fn draw_toolbar_content(
    pixmap: &mut Pixmap,
    spec: &ToolbarSpec,
    theme: &Theme,
    rects: &[LayoutRect],
    scale: f32,
) {
    for (i, r) in rects.iter().enumerate() {
        let hover = spec.hover == Some(i);
        let pressed = spec.pressed == Some(i);
        if hover || pressed {
            // 悬停浅底 / 按下更深底（用主题正文字色叠加低 alpha，两套主题都成立）
            let alpha = if pressed { 0x2A } else { 0x14 };
            fill_rounded(
                pixmap,
                r.x as f32,
                r.y as f32,
                r.w as f32,
                r.h as f32,
                (HL_RADIUS * scale).min(r.h as f32 / 2.0),
                [theme.fg[0], theme.fg[1], theme.fg[2], alpha],
            );
        }
        if let Some(icon) = toolbar_icon(spec, i) {
            // 图标按目标尺寸缩放居中（inset 内边距横竖分开：源图 ~28-32px 近方形，
            // 横 3 竖 1 → 30×26 按钮内可用区 24×24，图标大小与旧版一致）
            let inset_x = (3.0 * scale).ceil();
            let inset_y = (1.0 * scale).ceil();
            draw_icon_scaled(pixmap, icon, r, inset_x, inset_y);
        }
    }
}

/// 按按钮索引 + 当前四态选图标（None = 图标缺失/未知索引）。
fn toolbar_icon<'a>(spec: &'a ToolbarSpec, i: usize) -> Option<&'a Pixmap> {
    match i {
        TB_LOGO => spec.icons.logo.as_ref(),
        TB_MODE => match spec.state.mode {
            ImeMode::Chinese => spec.icons.lang_cn.as_ref(),
            ImeMode::English => spec.icons.lang_en.as_ref(),
        },
        TB_WIDTH => match spec.state.width {
            ImeWidth::Half => spec.icons.width_half.as_ref(),
            ImeWidth::Full => spec.icons.width_full.as_ref(),
        },
        TB_PUNCT => match spec.state.punct {
            ImePunct::Chinese => spec.icons.punct_cn.as_ref(),
            ImePunct::English => spec.icons.punct_en.as_ref(),
        },
        TB_SCRIPT => match spec.state.script {
            ImeScript::Simplified => spec.icons.script_simplified.as_ref(),
            ImeScript::Traditional => spec.icons.script_traditional.as_ref(),
        },
        TB_FINDER => spec.icons.finder.as_ref(),
        TB_GEAR => spec.icons.gear.as_ref(),
        _ => None,
    }
}
/// 缩放 = 预缩放到目标尺寸的临时 Pixmap + identity 绘制（语义直白，避免 transform
/// 叠加歧义）；分配失败静默跳过（按钮留空，不 panic）。
/// 2026-08-21 修：缩放变换用 `from_scale` 而非 `from_bbox`——`from_bbox` 把源坐标
/// (0..iw) 映射到 (0..iw*scale)，目标画布只有 dw 大小 → 只采样源图左上角 ~1 像素
/// （图标居中、四角透明 → 整片空白，实测 32-toolbar 图标全空）。
fn draw_icon_scaled(
    canvas: &mut Pixmap,
    icon: &Pixmap,
    r: &LayoutRect,
    inset_x: f32,
    inset_y: f32,
) {
    let avail = (r.w as f32 - inset_x * 2.0).min(r.h as f32 - inset_y * 2.0);
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
    let x = (r.x as f32 + (r.w as f32 - dw) / 2.0).round() as i32;
    let y = (r.y as f32 + (r.h as f32 - dh) / 2.0).round() as i32;
    let paint2 = PixmapPaint::default();
    canvas.draw_pixmap(x, y, dst.as_ref(), &paint2, Transform::identity(), None);
}
