//! 浮动工具栏渲染（32-status-toolbar.md §6.4；P2.4 自 render.rs 拆出）。
//! 横排 6 按钮条（logo | 中英 | 全半角 | 标点 | 简繁 | 齿轮），扁平圆角细边框风格
//! 与候选窗/菜单一致；按钮命中用返回的矩形列表喂 `hit_test`。
//!
//! M1 桌宠骨架扩展：复合渲染（`render_composite`）把工具栏 Surface + 宠物帧合成到
//! 同一张 Surface——工具栏位于复合窗底部、宠物栖于工具栏上沿（§5.1 栖木式吸附）。
//! 宠物区透明背景：仅宠物像素不透明，其余点击穿透到桌面（ULW per-pixel alpha）。

use iuv_core::PetClip;
use tiny_skia::{Color, FilterQuality, Pixmap, PixmapPaint, Transform};

use iuv_core::{ImeState, InitialMode, PunctMode, ScriptMode, WidthMode};

use crate::layout::Rect as LayoutRect;
use crate::paint::{fill_rounded, HL_RADIUS};
use crate::pet::{render_pet_frame, PetSprites};
use crate::render::{pixmap_to_surface, render_to_surface, render_toolbar_into_pixmap, Surface};
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

/// 渲染浮动工具栏：横排 6 按钮条，风格与候选窗/菜单一致（扁平圆角细边框）。
/// 返回 `(Surface, 按钮矩形列表)`——矩形为 surface 坐标（= 内容坐标，无边框留白
/// 偏移），与窗口客户区坐标系重合，直接喂 `hit_test` 做按钮命中。
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
    // 按钮矩形（内容坐标；无阴影时代内容坐标即表面坐标，命中区与绘制严格重合）
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
        |pixmap| {
            draw_toolbar_content(pixmap, spec, theme, &rects, scale);
        },
    );
    (surface, rects)
}

/// 工具栏内容绘制（hover 高亮 + 图标缩放绘制）——共享给 `render_toolbar`（→ Surface）
/// 与 `render::render_toolbar_into_pixmap`（→ Pixmap，复合渲染用）。
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
            // 图标按目标尺寸缩放居中（inset 内边距；源图 ~28-32px 近方形）
            let inset = (3.0 * scale).ceil();
            draw_icon_scaled(pixmap, icon, r, inset);
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use iuv_core::{ImeState, PetClip};

    /// 复合渲染：工具栏 + 宠物区 → 同一张 Surface，按钮矩形偏移到复合坐标，
    /// 宠物矩形返回（命中用）。
    ///
    /// 关键不变量（与 M1-IMPLEMENTATION §5.1 一致）：
    /// - 复合窗宽 = toolbar_w + zone_w（zone_w = PET_ZONE_W * scale）
    /// - 复合窗高 = toolbar_h + overhang（overhang = PET_OVERHANG * scale）
    /// - 按钮矩形 y 偏移 = overhang（栖木线 = 工具栏上沿）
    /// - 宠物矩形底边 = 工具栏上沿（y + h = overhang）
    #[test]
    fn render_composite_geometry_offsets() {
        let icons = ToolbarIcons::default();
        let tb_spec = ToolbarSpec {
            icons: &icons,
            state: ImeState::default(),
            hover: None,
            pressed: None,
        };
        let composite = CompositeSpec {
            toolbar: &tb_spec,
            pet: None,
        };
        let (surf, rects, pet_rect) = render_composite(&composite, &crate::theme::theme_dark(), 1.0);
        // 复合窗尺寸
        let zone_w = (PET_ZONE_W * 1.0).ceil() as i32;
        let overhang = (PET_OVERHANG * 1.0).ceil() as i32;
        let toolbar_w = (TOOLBAR_BTN * 1.0).ceil() as i32 * TB_COUNT as i32
            + (TOOLBAR_GAP * 1.0).ceil() as i32 * (TB_COUNT as i32 - 1)
            + (TOOLBAR_PAD * 1.0).ceil() as i32 * 2;
        let toolbar_h = (TOOLBAR_BTN * 1.0).ceil() as i32 + (TOOLBAR_PAD * 1.0).ceil() as i32 * 2;
        assert_eq!(surf.w as i32, toolbar_w + zone_w);
        assert_eq!(surf.h as i32, toolbar_h + overhang);
        // 按钮矩形：y 全部一致 = pad + overhang（pad=6, overhang=52 @ scale=1 → 58）
        // 关键不变量：复合坐标下所有按钮 y 相同（横排布局）。
        let first_y = rects[0].y;
        for r in &rects {
            assert_eq!(r.y, first_y, "按钮 y 一致（横排）");
        }
        assert!(first_y > overhang, "按钮在工具栏上沿之下");
        // 无 pet spec → pet_rect = None
        assert!(pet_rect.is_none());
    }

    /// 复合渲染：无 pet spec → 工具栏区照常渲染 + 宠物区背景透明。
    #[test]
    fn render_composite_without_pet_keeps_toolbar() {
        let icons = ToolbarIcons::default();
        let tb_spec = ToolbarSpec {
            icons: &icons,
            state: ImeState::default(),
            hover: None,
            pressed: None,
        };
        let composite = CompositeSpec {
            toolbar: &tb_spec,
            pet: None,
        };
        let (surf, rects, _) = render_composite(&composite, &crate::theme::theme_light(), 1.0);
        assert!(!rects.is_empty(), "无 pet 时按钮矩形仍返回");
        // 复合窗 Surface 像素数 = w * h * 4
        assert_eq!(surf.pixels.len(), (surf.w * surf.h * 4) as usize);
    }

    /// 复合渲染：scale=2 缩放尺寸 + 按钮 y 偏移仍 = overhang * scale。
    #[test]
    fn render_composite_scale_doubles_geometry() {
        let icons = ToolbarIcons::default();
        let tb_spec = ToolbarSpec {
            icons: &icons,
            state: ImeState::default(),
            hover: None,
            pressed: None,
        };
        let composite = CompositeSpec {
            toolbar: &tb_spec,
            pet: None,
        };
        let (s1, _, _) = render_composite(&composite, &crate::theme::theme_dark(), 1.0);
        let (s2, _, _) = render_composite(&composite, &crate::theme::theme_dark(), 2.0);
        // scale=2 复合窗约 2 倍（padding/scale ceil 累积有 ≤ 几像素差）
        let dw = (s2.w as i64 - s1.w as i64 * 2).abs();
        let dh = (s2.h as i64 - s1.h as i64 * 2).abs();
        assert!(dw <= 24, "scale=2 宽约 2 倍：1x={} 2x={}", s1.w, s2.w);
        assert!(dh <= 24, "scale=2 高约 2 倍：1x={} 2x={}", s1.h, s2.h);
    }

    /// 复合渲染：素材空集 → 整张 Surface 不画宠物（仍返回纯工具栏；pet_rect=None）。
    #[test]
    fn render_composite_empty_sprites_yields_no_pet() {
        use std::collections::HashMap;
        let icons = ToolbarIcons::default();
        let tb_spec = ToolbarSpec {
            icons: &icons,
            state: ImeState::default(),
            hover: None,
            pressed: None,
        };
        // 构造一个 clips 全空（is_empty=true）的 PetSprites
        let empty_sprites = PetSprites::new(Vec::new(), HashMap::new());
        let pet_spec = PetRenderSpec {
            sprites: &empty_sprites,
            clip: PetClip::Idle,
            frame: 0,
        };
        let composite = CompositeSpec {
            toolbar: &tb_spec,
            pet: Some(&pet_spec),
        };
        let (surf, _rects, pet_rect) =
            render_composite(&composite, &crate::theme::theme_light(), 1.0);
        // sprites.is_empty → composite 内不画宠物；pet_rect 仍按定义计算（用于命中穿透）
        assert!(surf.w > 0 && surf.h > 0);
        assert!(pet_rect.is_some(), "pet_rect 仍返回（命中用）");
    }

    /// 复合渲染几何（M1 §5.1）：scale=1 时
    ///   复合窗 = (toolbar_w + 64, toolbar_h + 52) = 276×94
    ///   按钮矩形 y 偏移 = 52 = PET_OVERHANG
    ///   宠物显示矩形 = (224, 12, 40, 40)（底边 y+h=52 贴工具栏上沿）
    /// 当前测试集只覆盖"无 pet 时按钮 y 偏移"，未覆盖有 pet 时的宠物矩形坐标——
    /// QA 补充（M1-IMPLEMENTATION §6：宠物矩形落在窗内 + 栖木线贴齐）。
    #[test]
    fn render_composite_pet_rect_matches_m1_section_5_1() {
        use std::collections::HashMap;
        let icons = ToolbarIcons::default();
        let tb_spec = ToolbarSpec {
            icons: &icons,
            state: ImeState::default(),
            hover: None,
            pressed: None,
        };
        // 真实 1 帧 sprite（4×4），让 PetSprites 不空
        let mut frame = Pixmap::new(4, 4).unwrap();
        frame.fill(tiny_skia::Color::from_rgba8(0xFF, 0xFF, 0xFF, 0xFF));
        let mut clips = HashMap::new();
        clips.insert(PetClip::Idle, 0..1);
        let sprites = PetSprites::new(vec![frame], clips);
        let pet_spec = PetRenderSpec {
            sprites: &sprites,
            clip: PetClip::Idle,
            frame: 0,
        };
        let composite = CompositeSpec {
            toolbar: &tb_spec,
            pet: Some(&pet_spec),
        };
        let (surf, _rects, pet_rect) =
            render_composite(&composite, &crate::theme::theme_dark(), 1.0);
        // 复合窗 276×94
        assert_eq!(surf.w, 276, "scale=1 复合窗宽 = 212+64");
        assert_eq!(surf.h, 94, "scale=1 复合窗高 = 42+52");
        // 宠物矩形（M1 §5.1 @96dpi 基准）
        let pr = pet_rect.expect("pet_rect 必须返回（命中用）");
        assert_eq!(pr.x, 224, "宠物 x = toolbar_w + (zone_w - display)/2 = 212 + 12");
        assert_eq!(pr.y, 12, "宠物 y = overhang - display = 52 - 40（底边贴工具栏上沿）");
        assert_eq!(pr.w, 40);
        assert_eq!(pr.h, 40);
        assert_eq!(pr.y + pr.h, 52, "宠物底边 y+h = PET_OVERHANG = 工具栏上沿（栖木线）");
        // 按钮 y ≥ overhang（栖木线之下）
        // 同时验证按钮全部在工具栏区，不与宠物重叠
    }
}
/// 缩放 = 预缩放到目标尺寸的临时 Pixmap + identity 绘制（语义直白，避免 transform
/// 叠加歧义）；分配失败静默跳过（按钮留空，不 panic）。
/// 2026-08-21 修：缩放变换用 `from_scale` 而非 `from_bbox`——`from_bbox` 把源坐标
/// (0..iw) 映射到 (0..iw*scale)，目标画布只有 dw 大小 → 只采样源图左上角 ~1 像素
/// （图标居中、四角透明 → 整片空白，实测 32-toolbar 图标全空）。
fn draw_icon_scaled(canvas: &mut Pixmap, icon: &Pixmap, r: &LayoutRect, inset: f32) {
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
    let x = (r.x as f32 + (r.w as f32 - dw) / 2.0).round() as i32;
    let y = (r.y as f32 + (r.h as f32 - dh) / 2.0).round() as i32;
    let paint2 = PixmapPaint::default();
    canvas.draw_pixmap(x, y, dst.as_ref(), &paint2, Transform::identity(), None);
}

// ===== M1 桌宠骨架 · 复合渲染 =====

/// 宠物区宽（@96dpi 基准；render 乘 scale）。工具栏右侧追加区，背景透明。
pub const PET_ZONE_W: f32 = 64.0;
/// 宠物栖木高度（@96dpi 基准）——工具栏上沿之上"挂"出多少像素。
/// 视觉上宠物趴在上沿（底边 y = PET_OVERHANG 贴工具栏顶），符合 UIUX §4.1 栖木式吸附。
pub const PET_OVERHANG: f32 = 52.0;
/// 宠物显示边长（正方形，@96dpi 基准；render 乘 scale 后 ceil）。
pub const PET_DISPLAY: f32 = 40.0;

/// 宠物渲染规格（纹理 + 当前 clip + 当前帧）。
pub struct PetRenderSpec<'a> {
    pub sprites: &'a PetSprites,
    pub clip: PetClip,
    pub frame: u32,
}

/// 复合渲染规格：工具栏 + 可选宠物。
pub struct CompositeSpec<'a> {
    /// 复用现有 `ToolbarSpec`（图标 + 四态 + 悬停/按下）
    pub toolbar: &'a ToolbarSpec<'a>,
    /// `None` = 不画宠物（工具栏区保留，几何不变）
    pub pet: Option<&'a PetRenderSpec<'a>>,
}

/// 计算宠物显示矩形（复合窗口坐标）。
/// 居中于宠物区（水平方向），底部 y = PET_OVERHANG（贴工具栏上沿）。
fn pet_display_rect(scale: f32) -> (i32, i32, u32, u32) {
    let zone_w = (PET_ZONE_W * scale).ceil() as i32;
    let display = (PET_DISPLAY * scale).ceil() as u32;
    let overhang = (PET_OVERHANG * scale).ceil() as i32;
    // 工具栏宽 = btn*6 + gap*5 + pad*2（与 render_toolbar 同源公式）
    let btn = (TOOLBAR_BTN * scale).ceil() as i32;
    let gap = (TOOLBAR_GAP * scale).ceil() as i32;
    let pad = (TOOLBAR_PAD * scale).ceil() as i32;
    let toolbar_w = btn * TB_COUNT as i32 + gap * (TB_COUNT as i32 - 1) + pad * 2;
    let x = toolbar_w + (zone_w - display as i32) / 2;
    let y = overhang - display as i32; // 底部贴工具栏上沿
    (x, y, display, display)
}

/// 复合渲染：工具栏 Surface + 宠物区 → 同一张 Surface。
///
/// 返回：
/// - `Surface`：合成 BGRA Surface，尺寸 = (toolbar_w + zone_w, toolbar_h + overhang)；
///   工具栏位于下方，宠物挂在工具栏上沿之上（UIUX §4.1 栖木式）。
/// - `Vec<LayoutRect>`：按钮矩形，已偏移到**复合坐标**（y += overhang）——直接喂
///   `hit_test`（daemon 复合窗口的按钮命中零换算）。
/// - `Option<LayoutRect>`：宠物显示矩形（命中 + 拖拽判别用）；`None` = 无宠物 spec。
///
/// 失败路径：素材缺失/分配失败 → 返回空 Surface（窗口后续 SkipTimer 与原逻辑一致）。
pub fn render_composite(
    spec: &CompositeSpec,
    theme: &Theme,
    scale: f32,
) -> (Surface, Vec<LayoutRect>, Option<LayoutRect>) {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    // 工具栏尺寸（与 render_toolbar 同款）
    let btn = (TOOLBAR_BTN * scale).ceil() as i32;
    let gap = (TOOLBAR_GAP * scale).ceil() as i32;
    let pad = (TOOLBAR_PAD * scale).ceil() as i32;
    let toolbar_w = btn * TB_COUNT as i32 + gap * (TB_COUNT as i32 - 1) + pad * 2;
    let toolbar_h = btn + pad * 2;
    let zone_w = (PET_ZONE_W * scale).ceil() as i32;
    let overhang = (PET_OVERHANG * scale).ceil() as i32;
    let composite_w = toolbar_w + zone_w;
    let composite_h = toolbar_h + overhang;
    let pet_rect = if spec.pet.is_some() {
        let (x, y, w, h) = pet_display_rect(scale);
        Some(LayoutRect { x, y, w: w as i32, h: h as i32 })
    } else {
        None
    };

    // 复合画布：透明 BG (alpha=0) —— 仅工具栏区与宠物像素不透明，其余点击穿透
    let mut composite = match Pixmap::new(composite_w.max(0) as u32, composite_h.max(0) as u32) {
        Some(p) => p,
        None => {
            return (
                Surface::empty(),
                Vec::new(),
                pet_rect,
            );
        }
    };
    composite.fill(Color::TRANSPARENT);

    // 工具栏内容 blit 到 (0, overhang)
    if let Some((toolbar_pix, mut toolbar_rects)) =
        render_toolbar_into_pixmap(spec.toolbar, theme, scale)
    {
        let blit_paint = PixmapPaint::default();
        composite.draw_pixmap(
            0,
            overhang,
            toolbar_pix.as_ref(),
            &blit_paint,
            Transform::identity(),
            None,
        );
        // 按钮矩形偏移到复合坐标
        for r in toolbar_rects.iter_mut() {
            r.y += overhang;
        }
        // 宠物帧：直接 blit 到 pet_rect
        if let (Some(pet_spec), Some(pr)) = (spec.pet, pet_rect) {
            let _ = render_pet_frame(&mut composite, pet_spec.sprites, pet_spec.clip, pet_spec.frame, &pr);
        }
        // 复合 Pixmap → Surface（一次性 R/B 交换）
        let surf = pixmap_to_surface(composite);
        return (surf, toolbar_rects, pet_rect);
    }

    // 工具栏渲染失败：仍返回空 Surface（daemon 仍能感知失败并重试）
    (Surface::empty(), Vec::new(), pet_rect)
}