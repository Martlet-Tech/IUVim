//! 桌宠形象素材装配（少女分层皮肤 + 外部皮肤目录 + L0 帧表回退）。
//!
//! # 三级装配（任一失败静默降级，daemon 绝不 panic）
//!
//! 1. **外部皮肤目录**：`<iuv_dir>/pet/skins/<skin_id>/` 下放 `skin.json` + 各图层 PNG。
//!    免重编译换装——这是为后续换装/换角色预留的扩展口（本次不实现管理 UI）。
//! 2. **内置默认皮肤**：`include_str!` / `include_bytes!` 内嵌（开箱可用、零外部依赖）。
//! 3. **L0 帧表回退**：像素狗 `assets/pet/default.png`（分层素材整体缺失时兜底）。
//!
//! # 皮肤描述单一数据源
//!
//! 内置与外部皮肤都走同一份 `skin.json` 反序列化，保证内外格式一致、
//! 「内置」只是把素材编译进二进制而已。
//!
//! §9 共享约定：零新依赖；解码用 tiny-skia 自带 `Pixmap::decode_png`。

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};

use iuv_core::{iuv_dir, FaceExpr, LayerId, PetSkin};
use iuv_ui::pet::slice_frames;
use iuv_ui::{LayerImages, PetSheetLayout, PetSprites};
use tiny_skia::Pixmap;

use crate::log::log_line;

/// 资产内嵌宏（与 toolbar_icons.rs 同款）：`concat!($env("CARGO_MANIFEST_DIR"), "/../../../assets/", $f)`。
macro_rules! asset {
    ($file:literal) => {
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../../assets/", $file)
    };
}

/// 当前内置皮肤标识（同时用作外部皮肤目录名）。
pub const DEFAULT_SKIN_ID: &str = "girl_default";

// ===== 内置少女皮肤（girl_default）=====

/// 皮肤描述（与 `assets/pet/girl_default/skin.json` 同一份文件）。
const GIRL_SKIN_JSON: &str = include_str!(asset!("pet/girl_default/skin.json"));

// 图层 PNG（呆毛 ahoge 为全透明占位，不入包；缺失层由渲染层自动跳过）
const GIRL_BODY: &[u8] = include_bytes!(asset!("pet/girl_default/body.png"));
const GIRL_HEAD: &[u8] = include_bytes!(asset!("pet/girl_default/head.png"));
const GIRL_HAIR_BACK: &[u8] = include_bytes!(asset!("pet/girl_default/hair_back.png"));
const GIRL_HAIR_FRONT: &[u8] = include_bytes!(asset!("pet/girl_default/hair_front.png"));
const GIRL_FACE_NORMAL: &[u8] = include_bytes!(asset!("pet/girl_default/face_normal.png"));
const GIRL_FACE_BLINK: &[u8] = include_bytes!(asset!("pet/girl_default/face_blink.png"));
const GIRL_FACE_SMILE: &[u8] = include_bytes!(asset!("pet/girl_default/face_smile.png"));
const GIRL_FACE_FOCUS: &[u8] = include_bytes!(asset!("pet/girl_default/face_focus.png"));
const GIRL_FACE_SURPRISED: &[u8] = include_bytes!(asset!("pet/girl_default/face_surprised.png"));
const GIRL_FACE_SLEEPY: &[u8] = include_bytes!(asset!("pet/girl_default/face_sleepy.png"));

// ===== L0 回退：像素狗帧表（M1 原始素材，保留作兜底）=====

const DEFAULT_SHEET: &[u8] = include_bytes!(asset!("pet/default.png"));

/// 像素狗帧表布局（@96dpi 基准）：6 列 × 5 行 × 16×16 = 30 帧。
const DEFAULT_LAYOUT: PetSheetLayout = PetSheetLayout {
    frame_w: 16,
    frame_h: 16,
    rows: 5,
    cols: 6,
};

/// 桌宠形象：皮肤描述 + 已解码分层素材 + L0 帧表兜底。
///
/// 由 daemon 启动时装配一次，经 `Arc` 交工具条线程独占。
pub struct PetArt {
    /// 皮肤描述（图层 z-order / 锚点 / 摆动参数 / 呼吸 / 眨眼）
    pub skin: PetSkin,
    /// 已解码的分层位图（分层路径）
    pub images: LayerImages,
    /// 像素狗帧表（L0 回退路径；分层不可用时启用）
    pub fallback: PetSprites,
}

impl PetArt {
    /// 是否走分层渲染路径（分层素材齐全）。
    ///
    /// `false` 时上层应改用 `fallback` 帧表渲染（L0 降级）。
    pub fn is_layered(&self) -> bool {
        !self.images.is_empty()
    }
}

/// 装配桌宠形象（三级降级，绝不 panic）。
pub fn load_pet_art() -> PetArt {
    // ① 外部皮肤目录（免重编译换装）
    if let Some(dir) = external_skin_dir(DEFAULT_SKIN_ID) {
        if let Some((skin, images)) = load_skin_dir(&dir) {
            log_line(&format!(
                "[pet] 已加载外部皮肤：{DEFAULT_SKIN_ID}（{} 图层）",
                skin.layers.len()
            ));
            return PetArt { skin, images, fallback: load_default_sprites() };
        }
        log_line(&format!(
            "[pet] 外部皮肤目录不可用（{}），回退内置皮肤",
            dir.display()
        ));
    }
    // ② 内置默认皮肤
    if let Some((skin, images)) = builtin_girl_art() {
        log_line(&format!(
            "[pet] 已装配内置少女皮肤：{DEFAULT_SKIN_ID}（{} 图层）",
            skin.layers.len()
        ));
        return PetArt { skin, images, fallback: load_default_sprites() };
    }
    // ③ L0 帧表兜底
    log_line("[pet] 分层素材缺失，回退 L0 帧表（像素狗）");
    PetArt {
        skin: PetSkin::builtin_girl_default(),
        images: LayerImages::empty(),
        fallback: load_default_sprites(),
    }
}

/// 外部皮肤目录：`<iuv_dir>/pet/skins/<skin_id>/`（不存在 → `None`）。
pub fn external_skin_dir(skin_id: &str) -> Option<PathBuf> {
    let dir = iuv_dir()?.join("pet").join("skins").join(skin_id);
    if dir.is_dir() {
        Some(dir)
    } else {
        None
    }
}

/// 从目录加载皮肤：`skin.json` + 各图层 PNG。
///
/// 任一环节失败（读文件/解析 JSON/解码 PNG）→ `None`，由调用方降级。
/// 图层缺失**不**导致整体失败：只要有任意一张可用即可（缺的层渲染时跳过）。
pub fn load_skin_dir(dir: &Path) -> Option<(PetSkin, LayerImages)> {
    let json = std::fs::read_to_string(dir.join("skin.json")).ok()?;
    let skin: PetSkin = serde_json::from_str(&json).ok()?;
    let mut images = LayerImages::empty();

    for layer in &skin.layers {
        // 表情层按 FaceExpr 逐张加载，不走通用图层文件名
        if layer.id == LayerId::Face {
            continue;
        }
        let path = dir.join(format!("{}.png", layer.id.file_stem()));
        if let Some(px) = decode_file(&path) {
            images.insert(layer.id, px);
        } else {
            log_line(&format!("[pet] 图层缺失，已跳过：{}", path.display()));
        }
    }
    for expr in FaceExpr::ALL {
        let path = dir.join(expr.file_name());
        if let Some(px) = decode_file(&path) {
            images.insert_face(expr, px);
        }
    }
    if images.is_empty() {
        return None;
    }
    Some((skin, images))
}

/// 内置少女皮肤装配。
fn builtin_girl_art() -> Option<(PetSkin, LayerImages)> {
    let skin: PetSkin = serde_json::from_str(GIRL_SKIN_JSON).ok()?;
    let mut images = LayerImages::empty();
    images.insert(LayerId::Body, decode(GIRL_BODY)?);
    images.insert(LayerId::Head, decode(GIRL_HEAD)?);
    images.insert(LayerId::HairBack, decode(GIRL_HAIR_BACK)?);
    images.insert(LayerId::HairFront, decode(GIRL_HAIR_FRONT)?);
    images.insert_face(FaceExpr::Normal, decode(GIRL_FACE_NORMAL)?);
    images.insert_face(FaceExpr::Blink, decode(GIRL_FACE_BLINK)?);
    images.insert_face(FaceExpr::Smile, decode(GIRL_FACE_SMILE)?);
    images.insert_face(FaceExpr::Focus, decode(GIRL_FACE_FOCUS)?);
    images.insert_face(FaceExpr::Surprised, decode(GIRL_FACE_SURPRISED)?);
    images.insert_face(FaceExpr::Sleepy, decode(GIRL_FACE_SLEEPY)?);
    Some((skin, images))
}

/// `PetSprites` 装配失败时使用的空集。
pub fn empty_sprites() -> PetSprites {
    PetSprites::new(Vec::new(), HashMap::new())
}

/// 装配 L0 回退帧表（像素狗）。
///
/// 解码失败 / 切割失败 → 空集（上层据此不画宠物，工具栏不受影响）。
pub fn load_default_sprites() -> PetSprites {
    let sheet = match Pixmap::decode_png(DEFAULT_SHEET) {
        Ok(p) => p,
        Err(e) => {
            log_line(&format!("[pet] 回退帧表解码失败：{e:?}"));
            return empty_sprites();
        }
    };
    let frames = slice_frames(&sheet, &DEFAULT_LAYOUT);
    if frames.is_empty() {
        log_line("[pet] 回退帧表切割为空");
        return empty_sprites();
    }
    let cols = DEFAULT_LAYOUT.cols as usize;
    let mut clips: HashMap<iuv_core::PetClip, Range<usize>> = HashMap::new();
    clips.insert(iuv_core::PetClip::Idle, 0..cols);
    clips.insert(iuv_core::PetClip::ModeCn, 0..cols);
    clips.insert(iuv_core::PetClip::ModeEn, 0..cols);
    clips.insert(iuv_core::PetClip::Typing, cols..2 * cols);
    clips.insert(iuv_core::PetClip::React, 3 * cols..3 * cols + 2.min(cols));
    clips.insert(iuv_core::PetClip::Width, 3 * cols..3 * cols + 2.min(cols));
    clips.insert(iuv_core::PetClip::Script, 3 * cols..3 * cols + 2.min(cols));
    clips.insert(iuv_core::PetClip::Punct, 3 * cols..3 * cols + 2.min(cols));
    PetSprites::new(frames, clips)
}

/// 解码内嵌 PNG 字节。
fn decode(bytes: &[u8]) -> Option<Pixmap> {
    Pixmap::decode_png(bytes).ok()
}

/// 解码磁盘 PNG 文件（外部皮肤目录用）。
fn decode_file(path: &Path) -> Option<Pixmap> {
    let bytes = std::fs::read(path).ok()?;
    Pixmap::decode_png(&bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_girl_art_loads_all_layers() {
        let (skin, images) = builtin_girl_art().expect("内置少女皮肤必须可装配");
        assert_eq!(skin.id, DEFAULT_SKIN_ID);
        assert_eq!(skin.design_size, (224, 256));
        assert!(!images.is_empty());
        // 核心图层全在
        for id in [LayerId::Body, LayerId::Head, LayerId::HairBack, LayerId::HairFront] {
            assert!(images.get(id).is_some(), "{id:?} 图层必须存在");
        }
        // 全部表情都在（缺失时渲染层会回退 Normal，但内置包应当提供齐全）
        for expr in FaceExpr::ALL {
            assert!(images.face(expr).is_some(), "{expr:?} 表情必须存在");
        }
    }

    #[test]
    fn builtin_layer_sizes_match_design_size() {
        let (skin, images) = builtin_girl_art().expect("内置皮肤可装配");
        let (dw, dh) = skin.design_size;
        for id in [LayerId::Body, LayerId::Head, LayerId::HairBack, LayerId::HairFront] {
            let px = images.get(id).expect("图层存在");
            assert_eq!(
                (px.width(), px.height()),
                (dw, dh),
                "{id:?} 尺寸必须等于 design_size（分层对齐的前提）"
            );
        }
        for expr in FaceExpr::ALL {
            let px = images.face(expr).expect("表情存在");
            assert_eq!((px.width(), px.height()), (dw, dh), "{expr:?} 尺寸必须一致");
        }
    }

    #[test]
    fn builtin_skin_layers_have_no_face_layer_gap() {
        // 表情层在 skin.layers 中存在，但素材按 FaceExpr 加载
        let (skin, images) = builtin_girl_art().expect("内置皮肤可装配");
        assert!(skin.layer(LayerId::Face).is_some(), "皮肤描述应含表情层");
        assert!(
            images.get(LayerId::Face).is_none(),
            "表情素材应存在 faces 槽位而非 layers"
        );
        assert!(images.face(FaceExpr::Normal).is_some());
    }

    #[test]
    fn load_pet_art_is_layered_with_builtin_assets() {
        let art = load_pet_art();
        assert!(art.is_layered(), "内置素材齐全时应走分层路径");
        assert!(!art.fallback.is_empty(), "L0 回退帧表应始终可用");
    }

    #[test]
    fn fallback_sprites_are_usable() {
        let sprites = load_default_sprites();
        assert!(!sprites.is_empty());
        assert!(sprites.clip_len(iuv_core::PetClip::Idle) >= 1);
        assert!(sprites.clip_len(iuv_core::PetClip::Typing) >= 1);
    }

    #[test]
    fn empty_sprites_has_no_frames() {
        let s = empty_sprites();
        assert!(s.is_empty());
    }

    #[test]
    fn load_skin_dir_missing_dir_returns_none() {
        // 不存在的目录 → None（调用方降级）
        let missing = std::env::temp_dir().join("iuv_pet_skin_that_does_not_exist");
        assert!(load_skin_dir(&missing).is_none());
    }
}
