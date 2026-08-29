//! 宠物精灵帧渲染（M1 桌宠骨架 · 渲染层）。
//!
//! 管线五段（docs/pet/M1-IMPLEMENTATION.md §4）：
//! ① `include_bytes!` 内嵌帧表（daemon `pet_assets.rs`）
//! ② `Pixmap::decode_png` → 单张 Pixmap（tiny-skia 自带，零新依赖）
//! ③ `slice_frames(sheet, layout)` 行优先切割，强校验（尺寸整除/行列为 0 → 空 Vec）
//! ④ `PetSprites::frame(clip, idx)` 查帧缓存（缺 clip → Idle；越界 → 首帧）
//! ⑤ `render_pet_frame` → 宠物区 Surface → `render_composite` 合成
//!
//! **Surface 契约**：产物像素是 **premultiplied BGRA**（Windows ULW DIB 直供）；
//! tiny-skia Pixmap = premultiplied RGBA——`render_pet_frame` 在合成时交换 R/B。
//! 全部公开函数不 panic：素材缺失/分配失败静默降级。

use std::collections::HashMap;
use std::ops::Range;

use iuv_core::{FaceExpr, LayerId, PetAnim, PetClip, PetSkin};
use tiny_skia::{FilterQuality, Pixmap, PixmapPaint, Transform};

use crate::layout::Rect as LayoutRect;

/// 精灵帧表布局（行优先切割：`row * cols + col`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PetSheetLayout {
    /// 单帧宽（像素）
    pub frame_w: u32,
    /// 单帧高（像素）
    pub frame_h: u32,
    /// 行数（动画数）
    pub rows: u32,
    /// 列数（每动画帧数；允许不同行不同列数 → 实际用 `clips` 映射精确控制）
    pub cols: u32,
}

impl PetSheetLayout {
    /// 总帧数 = rows × cols
    pub fn total(&self) -> u32 {
        self.rows * self.cols
    }
}

/// 帧表行优先切割（强校验）：
/// - `frame_w == 0` / `frame_h == 0` / `rows == 0` / `cols == 0` → 空 Vec
/// - `sheet.w() % frame_w != 0` 或 `sheet.h() % frame_h != 0` → 空 Vec
/// - `sheet.data().is_empty()` → 空 Vec
///
/// 不 panic。返回的 Vec 长度 = `rows * cols`（成功时）。
pub fn slice_frames(sheet: &Pixmap, layout: &PetSheetLayout) -> Vec<Pixmap> {
    if layout.frame_w == 0
        || layout.frame_h == 0
        || layout.rows == 0
        || layout.cols == 0
        || sheet.width() == 0
        || sheet.height() == 0
        || sheet.data().is_empty()
    {
        return Vec::new();
    }
    if !sheet.width().is_multiple_of(layout.frame_w)
        || !sheet.height().is_multiple_of(layout.frame_h)
    {
        return Vec::new();
    }
    let mut out = Vec::with_capacity((layout.rows * layout.cols) as usize);
    for row in 0..layout.rows {
        for col in 0..layout.cols {
            let Some(px) =
                copy_subpixmap(sheet, col * layout.frame_w, row * layout.frame_h, layout.frame_w, layout.frame_h)
            else {
                return Vec::new();
            };
            out.push(px);
        }
    }
    out
}

/// 从 sheet 中复制指定矩形到新 Pixmap（premultiplied RGBA 内存）。
/// 越界 / 分配失败 → None。
fn copy_subpixmap(src: &Pixmap, x: u32, y: u32, w: u32, h: u32) -> Option<Pixmap> {
    if w == 0 || h == 0 {
        return None;
    }
    if x + w > src.width() || y + h > src.height() {
        return None;
    }
    let mut dst = Pixmap::new(w, h)?;
    // 按行复制（tiny-skia Pixmap 内存布局 = 行优先、长度 = w*h*4）
    let src_stride = (src.width() * 4) as usize;
    let dst_stride = (w * 4) as usize;
    for row in 0..h as usize {
        let s_off = (y as usize + row) * src_stride + (x as usize) * 4;
        let d_off = row * dst_stride;
        // SAFETY: 边界已校验（x+w ≤ src.w、y+h ≤ src.h；dst 全新分配 w*h*4）
        unsafe {
            std::ptr::copy_nonoverlapping(src.data().as_ptr().add(s_off), dst.data_mut().as_mut_ptr().add(d_off), dst_stride);
        }
    }
    Some(dst)
}

/// 精灵帧缓存：把切割好的帧按 `PetClip` 映射分组。
///
/// 构造：`PetSprites::new(frames, clips)` —— `clips` 是"哪个 clip 用 frames 哪段区间"的映射。
/// 缺失 clip → `Idle`（再缺 → `None`）；越界 frame 索引 → clamp 到首帧。
pub struct PetSprites {
    /// 帧表（已切好的 Pixmap 序列；M1 单一帧表）
    pub frames: Vec<Pixmap>,
    /// clip → frames 区间
    pub clips: HashMap<PetClip, Range<usize>>,
}

impl PetSprites {
    /// 构造（clips 范围超出 frames 长度 → 自动 clamp 防御）。
    pub fn new(frames: Vec<Pixmap>, clips: HashMap<PetClip, Range<usize>>) -> Self {
        let n = frames.len();
        let clips = clips
            .into_iter()
            .map(|(k, mut r)| {
                r.start = r.start.min(n);
                r.end = r.end.min(n).max(r.start);
                (k, r)
            })
            .collect();
        PetSprites { frames, clips }
    }

    /// 按 clip + 帧索引查帧。
    ///
    /// 查帧策略：
    /// 1. clip 存在 + 区间非空 → 优先返回该区间
    /// 2. clip 不存在或区间空 → 回退 `Idle`（静止 → clamp 首帧）
    /// 3. `Idle` 也无 → 返回 `None`（让上层留空）
    /// 4. 帧索引处理（由 clip 语义区分，M1-IMPLEMENTATION §4.2「越界 → 首帧」
    ///    仅指"缺失回退 Idle 后的静止帧"，循环动画需 mod wrap）：
    ///    - 循环动画（Typing/React/Width/Script/Punct）：`idx % len` 无限循环，
    ///      保证打字律动/一次性动画播完自然从头重放（QA P1 修复）。
    ///    - 静止 clip（Idle/ModeCn/ModeEn）：越界 clamp 首帧（零 tick 冻结语义）。
    pub fn frame(&self, clip: PetClip, idx: u32) -> Option<&Pixmap> {
        // 主查找：clip 区间
        if let Some(r) = self.clips.get(&clip) {
            if !r.is_empty() {
                let i = if clip_is_loop(clip) {
                    pick_wrap_in(r, idx)
                } else {
                    pick_clamp_in(r, idx)
                };
                return self.frames.get(i);
            }
        }
        // 回退 Idle（静止 → clamp 首帧；任务书 §4.2「越界 clamp 到 0」）
        if let Some(r) = self.clips.get(&PetClip::Idle) {
            if !r.is_empty() {
                let i = pick_clamp_in(r, idx);
                return self.frames.get(i);
            }
        }
        None
    }

    /// clip 的有效帧数（缺失回退与 `frame` 一致：clip 缺 → Idle；Idle 也缺 → 0）。
    pub fn clip_len(&self, clip: PetClip) -> u32 {
        if let Some(r) = self.clips.get(&clip) {
            if !r.is_empty() {
                return r.len() as u32;
            }
        }
        if let Some(r) = self.clips.get(&PetClip::Idle) {
            if !r.is_empty() {
                return r.len() as u32;
            }
        }
        0
    }

    /// 是否有任何可用帧（false = 整个 sprite 全缺）
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
            || self.clips.values().all(|r| r.is_empty())
    }
}

/// 循环动画判定：需要 `idx % len` wrap 的 clip。
/// 静止 clip（Idle/ModeCn/ModeEn）是"冻结帧"语义 → clamp 首帧；
/// 其余（Typing 律动 / React/Width/Script/Punct 一次性）循环重放。
fn clip_is_loop(clip: PetClip) -> bool {
    matches!(
        clip,
        PetClip::Typing | PetClip::React | PetClip::Width | PetClip::Script | PetClip::Punct
    )
}

/// 在区间 [start, end) 内取第 idx 个：**mod wrap**（循环动画用；idx 无限增长回绕）。
/// len == 0（防御，`PetSprites::new` 已保证非空）→ 返回 start。
#[inline]
fn pick_wrap_in(r: &Range<usize>, idx: u32) -> usize {
    let len = r.len() as u32;
    if len == 0 {
        r.start
    } else {
        r.start + (idx % len) as usize
    }
}

/// 在区间 [start, end) 内取第 idx 个：**clamp 首帧**（静止 clip 用；越界取区间首帧）。
/// len == 0（防御）→ 返回 start。
#[inline]
fn pick_clamp_in(r: &Range<usize>, idx: u32) -> usize {
    let len = r.len() as u32;
    if len == 0 {
        r.start
    } else {
        r.start + (idx.min(len - 1)) as usize
    }
}

/// 把单帧 `src`（premultiplied RGBA，等比）缩放并 blit 到 `canvas`（premultiplied RGBA）
/// 上 (x, y) 位置。返回 true = 至少写入一个像素。
///
/// 内部：分配一个 dw×dh 的临时 Pixmap → `draw_pixmap(from_scale)` → `draw_pixmap(identity)`
/// 复制到 `canvas` 的 (x, y)。与 `toolbar.rs::draw_icon_scaled` 同款手法（from_scale 而非
/// from_bbox：源坐标 (0..iw) 映射到 (0..iw*scale)，目标画布只有 dw 大小 → 只采样源图左上
/// 角 1 像素的 from_bbox 漏洞）。
///
/// 全部失败（分配失败/越界）→ 返回 false，不 panic。
fn blit_frame_scaled(
    canvas: &mut Pixmap,
    src: &Pixmap,
    x: i32,
    y: i32,
    dst_w: u32,
    dst_h: u32,
) -> bool {
    if dst_w == 0 || dst_h == 0 || src.width() == 0 || src.height() == 0 {
        return false;
    }
    if x < 0 || y < 0 {
        return false;
    }
    if x as u32 + dst_w > canvas.width() || y as u32 + dst_h > canvas.height() {
        return false;
    }
    let iw = src.width() as f32;
    let ih = src.height() as f32;
    let sx = dst_w as f32 / iw;
    let sy = dst_h as f32 / ih;
    let Some(mut scaled) = Pixmap::new(dst_w, dst_h) else {
        return false;
    };
    let paint = PixmapPaint {
        opacity: 1.0,
        quality: FilterQuality::Bilinear,
        ..Default::default()
    };
    scaled.draw_pixmap(
        0,
        0,
        src.as_ref(),
        &paint,
        Transform::from_scale(sx, sy),
        None,
    );
    canvas.draw_pixmap(
        x,
        y,
        scaled.as_ref(),
        &PixmapPaint::default(),
        Transform::identity(),
        None,
    );
    true
}

/// 渲染宠物单帧到 `canvas`（premultiplied RGBA，**非** Surface）。
///
/// - 缺帧（`sprites.frame()` 返回 None）→ 不写入，返回 false（调用方应保留目标区域
///   为透明/原色，不 panic）。
/// - 缩放失败 / 越界 → 返回 false。
/// - 成功写入 → 返回 true。
pub fn render_pet_frame(
    canvas: &mut Pixmap,
    sprites: &PetSprites,
    clip: PetClip,
    frame: u32,
    dst: &LayoutRect,
) -> bool {
    let Some(src) = sprites.frame(clip, frame) else {
        return false;
    };
    blit_frame_scaled(
        canvas,
        src,
        dst.x,
        dst.y,
        dst.w.max(0) as u32,
        dst.h.max(0) as u32,
    )
}

/// 命中测试：把 (px, py) 经目标矩形 `dst` 逆缩放到源 sprite 帧 (sx, sy)，
/// 读取该点 alpha。
///
/// 用途：daemon 复合窗口 WM_NCHITTEST 区分"宠物像素点（可点击）"与"宠物区透明点
/// （穿透）"——纯函数，可在 daemon 抽出便于单测。
///
/// 越界 / 缺帧 → 0（不命中）。
/// `alpha_threshold`：调用方传 0x20（§5.2 M1 约定）。
pub fn pet_alpha_at(
    sprites: &PetSprites,
    clip: PetClip,
    frame: u32,
    dst: &LayoutRect,
    px: f32,
    py: f32,
) -> u8 {
    if dst.w <= 0 || dst.h <= 0 {
        return 0;
    }
    let Some(src) = sprites.frame(clip, frame) else {
        return 0;
    };
    let local_x = px - dst.x as f32;
    let local_y = py - dst.y as f32;
    if local_x < 0.0 || local_y < 0.0 || local_x >= dst.w as f32 || local_y >= dst.h as f32 {
        return 0;
    }
    let iw = src.width() as f32;
    let ih = src.height() as f32;
    // 逆缩放 → 源坐标
    let sx = (local_x * iw / dst.w as f32).floor() as i32;
    let sy = (local_y * ih / dst.h as f32).floor() as i32;
    if sx < 0 || sy < 0 || sx >= src.width() as i32 || sy >= src.height() as i32 {
        return 0;
    }
    let idx = (sy as usize * src.width() as usize + sx as usize) * 4;
    // tiny-skia Pixmap 内存序 = RGBA premultiplied；alpha 在 byte[3]
    if idx + 3 >= src.data().len() {
        return 0;
    }
    src.data()[idx + 3]
}

// ===== 分层渲染（少女形象 · Live2D 式） =====

/// 已解码的分层素材（每图层一张位图 + 每种表情一张位图）。
///
/// **关键约定**：所有图层位图尺寸**相同**（= `PetSkin::design_size`），透明区域表示该层
/// 无内容。全画布尺寸让各层天然对齐、锚点语义统一，绕锚点旋转时不会错位——这正是
/// Live2D 分层素材的通行做法。
#[derive(Clone, Debug, Default)]
pub struct LayerImages {
    layers: [Option<Pixmap>; LayerId::COUNT],
    faces: [Option<Pixmap>; FaceExpr::COUNT],
}

impl LayerImages {
    /// 空素材集（渲染时静默跳过全部图层）。
    pub fn empty() -> Self {
        LayerImages {
            layers: std::array::from_fn(|_| None),
            faces: std::array::from_fn(|_| None),
        }
    }

    /// 放入图层位图（同一 id 重复放入覆盖旧值）。
    pub fn insert(&mut self, id: LayerId, px: Pixmap) {
        self.layers[id.index()] = Some(px);
    }

    /// 放入表情位图。
    pub fn insert_face(&mut self, expr: FaceExpr, px: Pixmap) {
        self.faces[expr.index()] = Some(px);
    }

    /// 取图层位图（缺失 → `None`）。
    pub fn get(&self, id: LayerId) -> Option<&Pixmap> {
        self.layers[id.index()].as_ref()
    }

    /// 取表情位图（缺失 → `None`）。
    pub fn face(&self, expr: FaceExpr) -> Option<&Pixmap> {
        self.faces[expr.index()].as_ref()
    }

    /// 是否一张素材都没有。
    pub fn is_empty(&self) -> bool {
        self.layers.iter().all(|l| l.is_none()) && self.faces.iter().all(|f| f.is_none())
    }
}

/// 把单个图层缩放到目标矩形，并绕**归一化锚点**旋转后合成到 `canvas`。
///
/// 变换矩阵（右乘链：先缩放 → 把锚点平移到原点 → 旋转 → 平移到目标锚点位置）：
/// ```text
/// T = translate(pivot) ∘ rotate(deg) ∘ translate(-anchor·dst_size) ∘ scale(s)
/// ```
///
/// tiny-skia 的 `from_rotate` 绕原点旋转，故用"平移—旋转—平移"三明治把支点移到锚点。
///
/// `deg == 0` 时跳过旋转（绝大多数帧的常见路径），省一次矩阵分解。
///
/// **零 panic**：尺寸非法 / 旋转失败 / 参数非有限 → 返回 `false`。
pub fn blit_layer(
    canvas: &mut Pixmap,
    src: &Pixmap,
    dst: &LayoutRect,
    anchor: (f32, f32),
    deg: f32,
) -> bool {
    if dst.w <= 0 || dst.h <= 0 || src.width() == 0 || src.height() == 0 {
        return false;
    }
    let dst_w = dst.w as f32;
    let dst_h = dst.h as f32;
    let sx = dst_w / src.width() as f32;
    let sy = dst_h / src.height() as f32;
    if !sx.is_finite() || !sy.is_finite() || sx <= 0.0 || sy <= 0.0 {
        return false;
    }

    let ax = anchor.0.clamp(0.0, 1.0);
    let ay = anchor.1.clamp(0.0, 1.0);
    let pivot_x = dst.x as f32 + ax * dst_w;
    let pivot_y = dst.y as f32 + ay * dst_h;

    // 注意 tiny-skia 的 `post_*` 是**左乘**：新变换套在最外层、最后作用。
    // 故按"作用顺序"倒着链接：scale → 平移锚点到原点 → 旋转 → 平移到目标锚点位置。
    let scaled = Transform::from_scale(sx, sy)
        .post_translate(-ax * dst_w, -ay * dst_h);
    let rotated = if deg == 0.0 {
        scaled
    } else {
        scaled.post_rotate(deg)
    };
    let t = rotated.post_translate(pivot_x, pivot_y);

    let paint = PixmapPaint {
        opacity: 1.0,
        quality: FilterQuality::Bilinear,
        ..Default::default()
    };
    // 位置已包含在 transform 内，故 x/y 传 0
    canvas.draw_pixmap(0, 0, src.as_ref(), &paint, t, None);
    true
}

/// 分层合成：按 `PetSkin` 的 z-order 逐层绘制，并抽出宠物区的 alpha 位图。
///
/// - 图层素材缺失 → **跳过该层**（而非整体失败），保证缺素材时仍能看到部分形象
/// - 表情层用 `expr` 对应素材；该表情缺失时依次回退 `FaceExpr::Normal`、整张 `Face` 层
/// - 呼吸：`anim.breath_offset()` 换算成像素后**整体**同步偏移（所有层一致，避免头身脱节）
///
/// 返回宠物区的 alpha 位图（长度 `dst.w * dst.h`），供 daemon 做点击命中。
/// 一张都没画上 → `None`。
pub fn render_pet_layered(
    canvas: &mut Pixmap,
    skin: &PetSkin,
    images: &LayerImages,
    expr: FaceExpr,
    anim: &PetAnim,
    dst: &LayoutRect,
) -> Option<Vec<u8>> {
    if dst.w <= 0 || dst.h <= 0 {
        return None;
    }
    let breath_px = (anim.breath_offset() * dst.h as f32).round() as i32;

    let mut drew_any = false;
    for layer in &skin.layers {
        let src = if layer.id == LayerId::Face {
            images
                .face(expr)
                .or_else(|| images.face(FaceExpr::Normal))
                .or_else(|| images.get(LayerId::Face))
        } else {
            images.get(layer.id)
        };
        let Some(src) = src else { continue };
        let layer_dst = LayoutRect { x: dst.x, y: dst.y + breath_px, w: dst.w, h: dst.h };
        if blit_layer(canvas, src, &layer_dst, layer.anchor, anim.layer_angle(layer.id)) {
            drew_any = true;
        }
    }
    if !drew_any {
        return None;
    }
    extract_alpha_mask(canvas, dst)
}

/// 从画布的 `dst` 矩形抽取 alpha 通道（长度 `w*h`），供命中测试 O(1) 查表。
fn extract_alpha_mask(canvas: &Pixmap, dst: &LayoutRect) -> Option<Vec<u8>> {
    if dst.w <= 0 || dst.h <= 0 {
        return None;
    }
    let (w, h) = (dst.w as u32, dst.h as u32);
    let mut mask = Vec::with_capacity((w * h) as usize);
    let cw = canvas.width();
    let ch = canvas.height();
    for y in 0..h {
        for x in 0..w {
            let px = dst.x + x as i32;
            let py = dst.y + y as i32;
            if px < 0 || py < 0 || px >= cw as i32 || py >= ch as i32 {
                mask.push(0);
                continue;
            }
            let idx = ((py as u32 * cw + px as u32) * 4 + 3) as usize;
            mask.push(if idx < canvas.data().len() { canvas.data()[idx] } else { 0 });
        }
    }
    Some(mask)
}

/// 在 alpha mask 上做命中测试（`threshold` 沿用既有 `PET_HIT_ALPHA = 0x20`）。
///
/// **所见即所点**：mask 直接从合成结果抽取，天然支持任意旋转/缩放变换，无需像
/// [`pet_alpha_at`] 那样按矩形逆算（分层后各层变换各异，逆算已不可行）。
///
/// 越界 / mask 尺寸与 (w, h) 不匹配 → `false`。
pub fn pet_mask_hit(mask: &[u8], w: i32, h: i32, px: f32, py: f32, threshold: u8) -> bool {
    if w <= 0 || h <= 0 || mask.len() != (w as usize) * (h as usize) {
        return false;
    }
    let x = px.floor() as i32;
    let y = py.floor() as i32;
    if x < 0 || y < 0 || x >= w || y >= h {
        return false;
    }
    mask[(y as usize) * (w as usize) + x as usize] >= threshold
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 构造一张测试帧表：每帧 = 4×4 像素，4 帧（2×2 排列）。
    /// 每个像素位置 (col, row) 的 alpha = (col + row) > 0 ? 0xFF : 0（最上行 col=0 透明）
    /// 实际测试用：所有像素统一 alpha=0xFF（方便后续断言）。
    fn make_sheet(cols: u32, rows: u32) -> Pixmap {
        let w = cols * 4;
        let h = rows * 4;
        let mut p = Pixmap::new(w, h).unwrap();
        p.fill(tiny_skia::Color::from_rgba8(0xFF, 0xAA, 0x55, 0xFF));
        p
    }

    #[test]
    fn slice_frames_legit_layout_yields_full_grid() {
        let sheet = make_sheet(3, 2);
        let layout = PetSheetLayout {
            frame_w: 4,
            frame_h: 4,
            rows: 2,
            cols: 3,
        };
        let frames = slice_frames(&sheet, &layout);
        assert_eq!(frames.len(), 6);
        for f in &frames {
            assert_eq!(f.width(), 4);
            assert_eq!(f.height(), 4);
        }
    }

    #[test]
    fn slice_frames_non_divisible_returns_empty() {
        // 8x8 / 3x3 = 不整除
        let sheet = Pixmap::new(8, 8).unwrap();
        let layout = PetSheetLayout {
            frame_w: 3,
            frame_h: 3,
            rows: 2,
            cols: 2,
        };
        assert!(slice_frames(&sheet, &layout).is_empty());
    }

    #[test]
    fn slice_frames_zero_dims_return_empty() {
        let sheet = Pixmap::new(8, 8).unwrap();
        for layout in [
            PetSheetLayout { frame_w: 0, frame_h: 4, rows: 2, cols: 2 },
            PetSheetLayout { frame_w: 4, frame_h: 0, rows: 2, cols: 2 },
            PetSheetLayout { frame_w: 4, frame_h: 4, rows: 0, cols: 2 },
            PetSheetLayout { frame_w: 4, frame_h: 4, rows: 2, cols: 0 },
        ] {
            assert!(slice_frames(&sheet, &layout).is_empty(), "layout={layout:?}");
        }
    }

    #[test]
    fn slice_frames_zero_sheet_dims_return_empty() {
        // 0×0 sheet 在 tiny-skia 0.12 中 Pixmap::new 不允许（返回 None），
        // 故通过 frame_w/h=0 的等价路径覆盖"无效输入"分支：
        let sheet = Pixmap::new(8, 8).unwrap();
        let layout = PetSheetLayout {
            frame_w: 0,
            frame_h: 4,
            rows: 2,
            cols: 2,
        };
        assert!(slice_frames(&sheet, &layout).is_empty(), "frame_w=0 → 空");
    }

    fn make_sprites(frames_count: usize) -> PetSprites {
        let frames: Vec<Pixmap> = (0..frames_count)
            .map(|_| {
                let mut p = Pixmap::new(4, 4).unwrap();
                p.fill(tiny_skia::Color::from_rgba8(0xFF, 0x00, 0x00, 0xFF));
                p
            })
            .collect();
        let mut clips = HashMap::new();
        if frames_count >= 1 {
            clips.insert(PetClip::Idle, 0..1.min(frames_count));
        }
        PetSprites::new(frames, clips)
    }

    #[test]
    fn frame_returns_present_clip() {
        let mut s = make_sprites(2);
        s.clips.insert(PetClip::Typing, 1..2);
        let f = s.frame(PetClip::Typing, 0).expect("Typing 帧存在");
        assert_eq!(f.width(), 4);
    }

    #[test]
    fn frame_missing_clip_falls_back_to_idle() {
        let mut s = make_sprites(2);
        s.clips.insert(PetClip::Typing, 1..2);
        // React 不在 clips → 回退 Idle
        let f = s.frame(PetClip::React, 0).expect("回退 Idle");
        assert_eq!(f.width(), 4);
    }

    #[test]
    fn frame_static_clip_out_of_bounds_clamps_to_first() {
        // 静止 clip（Idle）：越界 clamp 首帧（任务书 §4.2「越界 → 首帧」）。
        // 注意：循环动画（Typing）越界是 mod wrap（见 `frame_typing_loop_wraps_within_clip_len`），
        // 二者语义不同——本测试验证"静止冻结帧"分支。
        let mut s = make_sprites(4);
        s.clips.insert(PetClip::Idle, 0..4); // 4 帧 Idle（理论上静止只播帧 0）
        let first = s.frame(PetClip::Idle, 0).expect("首帧").data().to_vec();
        let clamped = s.frame(PetClip::Idle, 100).expect("clamp 不应 None").data().to_vec();
        assert_eq!(first, clamped, "静止 Idle 越界 idx=100 应回到首帧（clamp 而非 wrap）");
    }

    /// M1 §4.2 + §3.3：Typing 是循环律动，frame 须 wrap（mod clip_len）而非 clamp。
    /// 若用 clamp-to-last，长时间打字后动画冻结在末帧。
    /// 构造 3 帧 Typing clip（红/绿/蓝区分），验证 idx=3 应回到 idx=0。
    #[test]
    fn frame_typing_loop_wraps_within_clip_len() {
        let frames: Vec<Pixmap> = (0..3u8)
            .map(|i| {
                let mut p = Pixmap::new(4, 4).unwrap();
                let c = match i {
                    0 => (0xFF, 0x00, 0x00), // 红
                    1 => (0x00, 0xFF, 0x00), // 绿
                    _ => (0x00, 0x00, 0xFF), // 蓝
                };
                p.fill(tiny_skia::Color::from_rgba8(c.0, c.1, c.2, 0xFF));
                p
            })
            .collect();
        let mut clips = HashMap::new();
        clips.insert(PetClip::Idle, 0..1);
        clips.insert(PetClip::Typing, 0..3);
        let s = PetSprites::new(frames, clips);
        let f0 = s.frame(PetClip::Typing, 0).expect("idx 0").data().to_vec();
        let f1 = s.frame(PetClip::Typing, 1).expect("idx 1").data().to_vec();
        let f2 = s.frame(PetClip::Typing, 2).expect("idx 2").data().to_vec();
        // 三帧必须不同（测试前提）
        assert_ne!(f0, f1, "idx 0/1 必须不同帧");
        assert_ne!(f1, f2, "idx 1/2 必须不同帧");
        // 循环：idx=3 应回到 idx=0（首帧红）
        let wrapped = s.frame(PetClip::Typing, 3).expect("idx 3").data().to_vec();
        assert_eq!(f0, wrapped, "Typing 循环：idx=len 必须回到首帧（mod），而非卡在末帧");
        // 验证正方向：idx=4 = idx=1（绿）
        let f4 = s.frame(PetClip::Typing, 4).expect("idx 4").data().to_vec();
        assert_eq!(f1, f4, "Typing 循环：idx=4 = idx=1");
    }

    #[test]
    fn frame_all_missing_returns_none() {
        // frames 0 个 + clips 空 → None
        let s = PetSprites::new(Vec::new(), HashMap::new());
        assert!(s.frame(PetClip::Idle, 0).is_none());
        assert!(s.frame(PetClip::Typing, 0).is_none());
    }

    #[test]
    fn clip_len_returns_range_length() {
        let mut s = make_sprites(10);
        s.clips.insert(PetClip::Typing, 0..3);
        assert_eq!(s.clip_len(PetClip::Typing), 3);
        assert_eq!(s.clip_len(PetClip::Idle), 1);
    }

    #[test]
    fn clip_len_missing_falls_back_to_idle_len() {
        let mut s = make_sprites(3);
        s.clips.insert(PetClip::Idle, 0..3);
        // React 缺 → 回退 Idle = 3
        assert_eq!(s.clip_len(PetClip::React), 3);
    }

    #[test]
    fn clip_len_all_missing_returns_zero() {
        let s = PetSprites::new(Vec::new(), HashMap::new());
        assert_eq!(s.clip_len(PetClip::Idle), 0);
    }

    #[test]
    fn render_pet_frame_writes_to_canvas() {
        // 8x8 中心不透明、四周透明（模拟精灵）
        let mut src = Pixmap::new(8, 8).unwrap();
        src.fill(tiny_skia::Color::TRANSPARENT);
        let mut paint = tiny_skia::Paint::default();
        paint.set_color_rgba8(0xAA, 0xBB, 0xCC, 0xFF);
        if let Some(rect) = tiny_skia::Rect::from_xywh(2.0, 2.0, 4.0, 4.0) {
            src.fill_rect(rect, &paint, tiny_skia::Transform::identity(), None);
        }
        let mut sprites = PetSprites::new(vec![src], HashMap::new());
        sprites.clips.insert(PetClip::Idle, 0..1);

        // 16x16 画布，把帧缩放绘到 (0,0,16,16)
        let mut canvas = Pixmap::new(16, 16).unwrap();
        let dst = LayoutRect { x: 0, y: 0, w: 16, h: 16 };
        assert!(render_pet_frame(&mut canvas, &sprites, PetClip::Idle, 0, &dst));
        // 中心 alpha > 0（缩放后像素已绘制）
        let center_idx = (8 * 16 + 8) * 4 + 3; // 中心像素 alpha
        assert!(canvas.data()[center_idx] > 0, "中心像素应被绘制");
    }

    #[test]
    fn render_pet_frame_missing_returns_false() {
        let sprites = PetSprites::new(Vec::new(), HashMap::new());
        let mut canvas = Pixmap::new(8, 8).unwrap();
        let dst = LayoutRect { x: 0, y: 0, w: 8, h: 8 };
        assert!(!render_pet_frame(&mut canvas, &sprites, PetClip::Idle, 0, &dst));
    }

    #[test]
    fn pet_alpha_at_inside_returns_alpha() {
        let mut src = Pixmap::new(4, 4).unwrap();
        src.fill(tiny_skia::Color::TRANSPARENT);
        let mut paint = tiny_skia::Paint::default();
        paint.set_color_rgba8(0x00, 0x00, 0x00, 0xFF);
        if let Some(rect) = tiny_skia::Rect::from_xywh(1.0, 1.0, 2.0, 2.0) {
            src.fill_rect(rect, &paint, tiny_skia::Transform::identity(), None);
        }
        let mut sprites = PetSprites::new(vec![src], HashMap::new());
        sprites.clips.insert(PetClip::Idle, 0..1);

        let dst = LayoutRect { x: 0, y: 0, w: 16, h: 16 };
        // 中心 = (8, 8) → 缩放后落在源 (2, 2)（不透明区）
        let a = pet_alpha_at(&sprites, PetClip::Idle, 0, &dst, 8.0, 8.0);
        assert!(a > 0, "中心 alpha 应 >0，实际 {a}");
        // 左上角 (1, 1) → 源 (0, 0)（透明）
        let a_corner = pet_alpha_at(&sprites, PetClip::Idle, 0, &dst, 1.0, 1.0);
        assert_eq!(a_corner, 0, "左上角 alpha = 0");
    }

    #[test]
    fn pet_alpha_at_outside_returns_zero() {
        let mut src = Pixmap::new(4, 4).unwrap();
        src.fill(tiny_skia::Color::from_rgba8(0x00, 0x00, 0x00, 0xFF));
        let mut sprites = PetSprites::new(vec![src], HashMap::new());
        sprites.clips.insert(PetClip::Idle, 0..1);

        let dst = LayoutRect { x: 0, y: 0, w: 8, h: 8 };
        // 矩形外
        assert_eq!(pet_alpha_at(&sprites, PetClip::Idle, 0, &dst, 100.0, 100.0), 0);
        // 矩形内但无帧（先清 clips）
        let sprites_empty = PetSprites::new(Vec::new(), HashMap::new());
        assert_eq!(pet_alpha_at(&sprites_empty, PetClip::Idle, 0, &dst, 4.0, 4.0), 0);
    }

    // ===== 分层渲染（少女形象） =====

    use iuv_core::PetLayer;

    /// 构造纯色位图（premultiplied RGBA）。
    fn solid(w: u32, h: u32, r: u8, g: u8, b: u8, a: u8) -> Pixmap {
        let mut p = Pixmap::new(w, h).unwrap();
        p.fill(tiny_skia::Color::from_rgba8(r, g, b, a));
        p
    }

    /// 测试用皮肤：关闭呼吸（breath_amp=0）以保证渲染结果确定。
    fn test_skin(layers: Vec<PetLayer>) -> PetSkin {
        PetSkin {
            id: "test".to_string(),
            design_size: (8, 8),
            layers,
            breath_amp: 0.0,
            breath_period_ms: 3500,
            blink_interval_ms: (2600, 6400),
        }
    }

    fn no_swing(id: LayerId, anchor: (f32, f32)) -> PetLayer {
        PetLayer { id, anchor, spring: None }
    }

    #[test]
    fn layer_images_insert_and_get() {
        let mut imgs = LayerImages::empty();
        assert!(imgs.is_empty());
        assert!(imgs.get(LayerId::Body).is_none());
        imgs.insert(LayerId::Body, solid(4, 4, 255, 0, 0, 255));
        assert!(!imgs.is_empty());
        assert_eq!(imgs.get(LayerId::Body).unwrap().width(), 4);
        assert!(imgs.get(LayerId::Head).is_none(), "未放入的图层应缺失");

        imgs.insert_face(FaceExpr::Smile, solid(4, 4, 0, 255, 0, 255));
        assert!(imgs.face(FaceExpr::Smile).is_some());
        assert!(imgs.face(FaceExpr::Normal).is_none());
    }

    #[test]
    fn blit_layer_scales_source_to_dst() {
        let src = solid(8, 8, 0, 0, 255, 255);
        let mut canvas = Pixmap::new(16, 16).unwrap();
        let dst = LayoutRect { x: 0, y: 0, w: 16, h: 16 };
        assert!(blit_layer(&mut canvas, &src, &dst, (0.5, 0.5), 0.0));
        // 中心与四角都应有内容（等比铺满）
        for (x, y) in [(8, 8), (1, 1), (14, 14)] {
            let idx = ((y * 16 + x) * 4 + 3) as usize;
            assert_eq!(canvas.data()[idx], 255, "({x},{y}) 应被铺满");
        }
    }

    #[test]
    fn blit_layer_rotation_actually_rotates_around_anchor() {
        // 铺满的正方形绕中心旋转 45° → 变成菱形：中心仍有内容，角落空出来
        let src = solid(8, 8, 0, 0, 255, 255);
        let mut canvas = Pixmap::new(16, 16).unwrap();
        let dst = LayoutRect { x: 0, y: 0, w: 16, h: 16 };
        assert!(blit_layer(&mut canvas, &src, &dst, (0.5, 0.5), 45.0));
        let center = ((8 * 16 + 8) * 4 + 3) as usize;
        let corner = ((1 * 16 + 1) * 4 + 3) as usize;
        assert!(canvas.data()[center] > 200, "旋转后中心应仍有内容");
        assert!(canvas.data()[corner] < 128, "旋转后角落应空出，实际 {}", canvas.data()[corner]);
    }

    #[test]
    fn blit_layer_rejects_invalid_without_panic() {
        let src = solid(8, 8, 255, 0, 0, 255);
        let mut canvas = Pixmap::new(16, 16).unwrap();
        // 零/负尺寸
        assert!(!blit_layer(&mut canvas, &src, &LayoutRect { x: 0, y: 0, w: 0, h: 8 }, (0.5, 0.5), 0.0));
        assert!(!blit_layer(&mut canvas, &src, &LayoutRect { x: 0, y: 0, w: 8, h: 0 }, (0.5, 0.5), 0.0));
        assert!(!blit_layer(&mut canvas, &src, &LayoutRect { x: 0, y: 0, w: -8, h: 8 }, (0.5, 0.5), 0.0));
        // 越界 dst 也不应 panic（由 tiny-skia 裁剪）
        let _ = blit_layer(&mut canvas, &src, &LayoutRect { x: 100, y: 100, w: 16, h: 16 }, (0.5, 0.5), 30.0);
    }

    #[test]
    fn render_pet_layered_respects_z_order() {
        // Body(红) 在下、Head(蓝) 在上 → 上层覆盖下层
        let skin = test_skin(vec![
            no_swing(LayerId::Body, (0.5, 1.0)),
            no_swing(LayerId::Head, (0.5, 0.95)),
        ]);
        let mut imgs = LayerImages::empty();
        imgs.insert(LayerId::Body, solid(8, 8, 255, 0, 0, 255));
        imgs.insert(LayerId::Head, solid(8, 8, 0, 0, 255, 255));

        let mut canvas = Pixmap::new(16, 16).unwrap();
        let dst = LayoutRect { x: 0, y: 0, w: 16, h: 16 };
        let anim = PetAnim::new(&skin);
        assert!(render_pet_layered(&mut canvas, &skin, &imgs, FaceExpr::Normal, &anim, &dst).is_some());

        let idx = (8 * 16 + 8) * 4;
        let (r, b) = (canvas.data()[idx], canvas.data()[idx + 2]);
        assert!(b > 200 && r < 64, "上层 Head(蓝) 应覆盖 Body(红)，实际 r={r} b={b}");
    }

    #[test]
    fn render_pet_layered_skips_missing_layers() {
        // 只提供 Body，skin 里还有 Head → 应跳过缺失层并照常画出 Body
        let skin = test_skin(vec![
            no_swing(LayerId::Body, (0.5, 1.0)),
            no_swing(LayerId::Head, (0.5, 0.95)),
        ]);
        let mut imgs = LayerImages::empty();
        imgs.insert(LayerId::Body, solid(8, 8, 255, 0, 0, 255));

        let mut canvas = Pixmap::new(16, 16).unwrap();
        let dst = LayoutRect { x: 0, y: 0, w: 16, h: 16 };
        let anim = PetAnim::new(&skin);
        assert!(render_pet_layered(&mut canvas, &skin, &imgs, FaceExpr::Normal, &anim, &dst).is_some());
        let idx = (8 * 16 + 8) * 4;
        assert!(canvas.data()[idx] > 200, "Body(红) 应被画出");
    }

    #[test]
    fn render_pet_layered_returns_none_when_nothing_drawn() {
        let skin = test_skin(vec![no_swing(LayerId::Body, (0.5, 1.0))]);
        let imgs = LayerImages::empty(); // 完全没有素材
        let mut canvas = Pixmap::new(16, 16).unwrap();
        let dst = LayoutRect { x: 0, y: 0, w: 16, h: 16 };
        let anim = PetAnim::new(&skin);
        assert!(render_pet_layered(&mut canvas, &skin, &imgs, FaceExpr::Normal, &anim, &dst).is_none());
    }

    #[test]
    fn render_pet_layered_mask_matches_drawn_pixels() {
        // 左半透明、右半不透明的图层 → mask 应如实反映
        let mut src = solid(8, 8, 0, 0, 255, 255);
        for y in 0..8u32 {
            for x in 0..4u32 {
                let i = ((y * 8 + x) * 4 + 3) as usize;
                src.data_mut()[i] = 0;
            }
        }
        let skin = test_skin(vec![no_swing(LayerId::Body, (0.5, 1.0))]);
        let mut imgs = LayerImages::empty();
        imgs.insert(LayerId::Body, src);

        let mut canvas = Pixmap::new(16, 16).unwrap();
        let dst = LayoutRect { x: 0, y: 0, w: 16, h: 16 };
        let anim = PetAnim::new(&skin);
        let mask = render_pet_layered(&mut canvas, &skin, &imgs, FaceExpr::Normal, &anim, &dst)
            .expect("应返回 mask");
        assert_eq!(mask.len(), 16 * 16, "mask 长度应等于 dst 面积");
        // 左半 (x=2) 透明、右半 (x=13) 不透明
        assert!(pet_mask_hit(&mask, 16, 16, 2.0, 8.0, 0x20) == false, "左侧应透明");
        assert!(pet_mask_hit(&mask, 16, 16, 13.0, 8.0, 0x20), "右侧应命中");
    }

    #[test]
    fn render_pet_layered_face_falls_back_to_normal() {
        // 请求 Smile 但只提供 Normal → 回退到 Normal 并正常绘制
        let skin = test_skin(vec![no_swing(LayerId::Face, (0.5, 0.5))]);
        let mut imgs = LayerImages::empty();
        imgs.insert_face(FaceExpr::Normal, solid(8, 8, 0, 255, 0, 255));

        let mut canvas = Pixmap::new(16, 16).unwrap();
        let dst = LayoutRect { x: 0, y: 0, w: 16, h: 16 };
        let anim = PetAnim::new(&skin);
        assert!(render_pet_layered(&mut canvas, &skin, &imgs, FaceExpr::Smile, &anim, &dst).is_some());
        let idx = (8 * 16 + 8) * 4;
        assert!(canvas.data()[idx + 1] > 200, "应回退绘制 Normal(绿)");
    }

    #[test]
    fn render_pet_layered_applies_breath_offset() {
        // 开启呼吸：推进到相位 90°（偏移最大）后，整体应向下平移
        let mut skin = test_skin(vec![no_swing(LayerId::Body, (0.5, 1.0))]);
        skin.breath_amp = 0.25; // 放大幅度便于观测
        let mut imgs = LayerImages::empty();
        // 只有上半部分有内容，便于观察垂直位移
        let mut src = solid(8, 8, 255, 0, 0, 255);
        for y in 4..8u32 {
            for x in 0..8u32 {
                let i = ((y * 8 + x) * 4 + 3) as usize;
                src.data_mut()[i] = 0;
            }
        }
        imgs.insert(LayerId::Body, src);

        let dst = LayoutRect { x: 0, y: 0, w: 16, h: 16 };
        let mut anim = PetAnim::new(&skin);

        let mut canvas_a = Pixmap::new(16, 16).unwrap();
        render_pet_layered(&mut canvas_a, &skin, &imgs, FaceExpr::Normal, &anim, &dst);
        let top_row_alpha = |c: &Pixmap| {
            let idx = ((1 * 16 + 8) * 4 + 3) as usize;
            c.data()[idx]
        };
        let before = top_row_alpha(&canvas_a);

        // 推进约四分之一周期 → 呼吸偏移接近最大值
        for _ in 0..(3500 / 4 / 16) {
            anim.step(16, 0);
        }
        let mut canvas_b = Pixmap::new(16, 16).unwrap();
        render_pet_layered(&mut canvas_b, &skin, &imgs, FaceExpr::Normal, &anim, &dst);
        let after = top_row_alpha(&canvas_b);

        assert!(anim.breath_offset() > 0.0, "应处于呼吸正相位");
        assert!(
            after < before,
            "呼吸正相位时内容应下移、顶部腾空：before={before} after={after}"
        );
    }

    #[test]
    fn pet_mask_hit_basics() {
        let mask = vec![0u8, 255, 0, 255]; // 2x2
        assert!(!pet_mask_hit(&mask, 2, 2, 0.0, 0.0, 0x20), "透明点不命中");
        assert!(pet_mask_hit(&mask, 2, 2, 1.0, 0.0, 0x20), "不透明点命中");
        assert!(pet_mask_hit(&mask, 2, 2, 1.9, 1.9, 0x20), "右下角命中");
        // 越界与尺寸不匹配
        assert!(!pet_mask_hit(&mask, 2, 2, 2.0, 0.0, 0x20), "越界不命中");
        assert!(!pet_mask_hit(&mask, 2, 2, -1.0, 0.0, 0x20));
        assert!(!pet_mask_hit(&mask, 3, 2, 1.0, 0.0, 0x20), "尺寸不匹配不命中");
        assert!(!pet_mask_hit(&mask, 0, 0, 0.0, 0.0, 0x20), "零尺寸不命中");
    }
}
