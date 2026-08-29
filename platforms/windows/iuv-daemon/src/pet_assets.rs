//! 桌宠精灵素材（M1 桌宠骨架 · daemon 侧资产装配）。
//!
//! 决策（docs/pet/M1-IMPLEMENTATION.md §1.1）：**不建缩放/转换工具**——源图即最终素材，
//! 编译期 `include_bytes!` 内嵌（与 `toolbar_icons.rs` 同模式），运行时 `Pixmap::decode_png`
//! 解码 + `iuv_ui::pet::slice_frames` 切割。素材文件落地在 `assets/pet/default.png`，
//! 许可与版权见 `assets/pet/LICENSE.md`（CC0 默认宠）。
//!
//! 失败降级：素材缺失 / 解码失败 / 切割失败 → 返回 `PetSprites::default()`（空帧表），
//! 上层 `ToolbarWindow` 据此决定"宠物区留空、工具栏区不受影响"——daemon 绝不 panic。
//!
//! §9 共享约定：零新依赖；布局常量 Rust `const` 内嵌，不引入 toml。

use std::collections::HashMap;
use std::ops::Range;

use iuv_core::PetClip;
use iuv_ui::{pet::slice_frames, PetSheetLayout, PetSprites};
use tiny_skia::Pixmap;

use crate::log::log_line;

/// 资产内嵌宏（与 toolbar_icons.rs 同款）：`concat!($env("CARGO_MANIFEST_DIR"), "/../../../assets/", $f)`。
macro_rules! asset {
    ($file:literal) => {
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../../assets/", $file)
    };
}

/// 默认宠帧表（assets/pet/default.png）：行 = 动画、列 = 帧数，行优先切割。
///
/// 与 `assets/pet/LICENSE.md` §4 "M1 帧表布局约定" 一一对应——
/// 任何对外变更（替换默认宠 / 改帧表规格）必须同步更新该文件。
const DEFAULT_SHEET: &[u8] = include_bytes!(asset!("pet/default.png"));

/// 帧表布局（@96dpi 基准）：6 列 × 5 行 × 16×16 = 30 帧。改这里时同步更新 LICENSE.md。
const DEFAULT_LAYOUT: PetSheetLayout = PetSheetLayout {
    frame_w: 16,
    frame_h: 16,
    rows: 5,
    cols: 6,
};

/// `PetSprites` 装配失败时使用的空集（None 帧表）—— 渲染层据此决定留空。
pub fn empty_sprites() -> PetSprites {
    PetSprites::new(Vec::new(), HashMap::new())
}

/// 装配默认宠 `PetSprites`（进程启动一次，失败 → 空集，工具栏宠物区留空）。
///
/// 解码 PNG → 切割帧 → 构造 `PetSprites`：clip → frames 区间映射（M1 默认映射见下）。
/// 全部失败降级不 panic：
/// - `Pixmap::decode_png` 失败 → 记日志返回空集
/// - `slice_frames` 失败（尺寸不整除等）→ 记日志返回空集
/// - 帧总数为 0 → 返回空集
pub fn load_default_sprites() -> PetSprites {
    let sheet = match Pixmap::decode_png(DEFAULT_SHEET) {
        Ok(p) => p,
        Err(e) => {
            log_line(&format!("[pet] 默认宠帧表解码失败：{e:?}（工具栏宠物区留空）"));
            return empty_sprites();
        }
    };
    // tiny-skia 0.12 Pixmap::decode_png 像素内存序 = RGBA（与 PetSprites 期望一致，
    // 后续渲染层 `render_pet_frame` 用的也是 RGBA Pixmap）；无需 R/B 交换。

    let frames = slice_frames(&sheet, &DEFAULT_LAYOUT);
    if frames.is_empty() {
        log_line(&format!(
            "[pet] 帧表切割为空（sheet {}x{}, 期望 {}x{} 帧）→ 工具栏宠物区留空",
            sheet.width(),
            sheet.height(),
            DEFAULT_LAYOUT.cols,
            DEFAULT_LAYOUT.rows
        ));
        return empty_sprites();
    }
    log_line(&format!(
        "[pet] 默认宠已装配：{} 帧（{}x{} 网格 × {}x{} px）",
        frames.len(),
        DEFAULT_LAYOUT.cols,
        DEFAULT_LAYOUT.rows,
        DEFAULT_LAYOUT.frame_w,
        DEFAULT_LAYOUT.frame_h
    ));

    // 帧区间映射（行优先切片；区间半开 [start, end)）：
    //   row 0 = idle（6 帧）     → Idle, ModeCn, ModeEn（Idle/英文回退）
    //   row 1 = walk（6 帧）     → Typing
    //   row 2 = run（6 帧）       → 预留（M1 未映射）
    //   row 3 = jump（6 帧）      → React, Width, Script, Punct（四态一闪共用）
    //   row 4 = attack（6 帧）    → 预留（M1 未映射）
    let cols = DEFAULT_LAYOUT.cols as usize;
    let mut clips: HashMap<PetClip, Range<usize>> = HashMap::new();
    clips.insert(PetClip::Idle, 0..cols);
    clips.insert(PetClip::ModeCn, 0..cols);
    clips.insert(PetClip::ModeEn, 0..cols);
    clips.insert(PetClip::Typing, cols..2 * cols);
    clips.insert(PetClip::React, 3 * cols..3 * cols + 2.min(cols)); // 一次性跳 2 帧
    clips.insert(PetClip::Width, 3 * cols..3 * cols + 2.min(cols));
    clips.insert(PetClip::Script, 3 * cols..3 * cols + 2.min(cols));
    clips.insert(PetClip::Punct, 3 * cols..3 * cols + 2.min(cols));

    PetSprites::new(frames, clips)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实素材存在时 `load_default_sprites` 成功；clip 映射覆盖核心动画。
    /// 全部失败路径在 daemon 集成测试覆盖（手测：删 default.png / 改 PNG 头部）。
    #[test]
    fn load_default_sprites_succeeds_with_real_asset() {
        let sprites = load_default_sprites();
        // 至少 30 帧 + 8 clip 区间
        assert!(sprites.clip_len(PetClip::Idle) >= 1, "Idle 必有");
        assert!(sprites.clip_len(PetClip::Typing) >= 1, "Typing 必有");
        assert!(sprites.clip_len(PetClip::React) >= 1, "React 必有");
        // React 一次性只取 2 帧（jump 行的前 2 帧）
        let react_len = sprites.clip_len(PetClip::React);
        assert!(react_len <= 2, "React 一次性 2 帧上限，实际 {react_len}");
    }

    #[test]
    fn empty_sprites_returns_no_frames() {
        let s = empty_sprites();
        assert!(s.is_empty());
        assert_eq!(s.clip_len(PetClip::Idle), 0);
        assert_eq!(s.clip_len(PetClip::Typing), 0);
    }

    /// 默认 layout 总帧数 = cols × rows（30）。
    #[test]
    fn layout_total_matches_30() {
        assert_eq!(DEFAULT_LAYOUT.cols, 6);
        assert_eq!(DEFAULT_LAYOUT.rows, 5);
        assert_eq!(DEFAULT_LAYOUT.frame_w, 16);
        assert_eq!(DEFAULT_LAYOUT.frame_h, 16);
        assert_eq!(DEFAULT_LAYOUT.total(), 30);
    }
}
