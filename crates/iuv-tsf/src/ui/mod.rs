//! 候选窗抽象与 Effect → UiSnapshot 映射。W0 完整实现，冻结。
//! MVP 实现 = GdiCandidateWindow（Agent E）；M4 增加 RemoteCandidateWindow。

use iuv_core::{Effect, PageInfo};

/// 【Agent E】GDI 候选窗实现（属主矩阵 01-contract.md §6 已列）。
/// 注：W0 骨架遗漏本声明与 re-export，属契约缺陷，由 Agent E 补接线（待主智能体追认）。
pub mod gdi;
pub use gdi::GdiCandidateWindow;
pub use iuv_core::Orientation;

/// 光标矩形（屏幕坐标）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CaretRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// 候选窗 UI 快照。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UiSnapshot {
    /// "ni'hao"
    pub reading: String,
    /// 页内候选文本
    pub candidates: Vec<String>,
    pub selected: usize,
    pub page: PageInfo,
    /// 布局方向（竖排/横排）；effect_to_snapshot 默认竖排，TSF 侧从 config 填
    pub orientation: Orientation,
}

/// Effect → UiSnapshot：取 effect.reading / 页内候选 text / selected / page。
pub fn effect_to_snapshot(e: &Effect) -> UiSnapshot {
    UiSnapshot {
        reading: e.reading.clone(),
        candidates: e.candidates.iter().map(|c| c.text.clone()).collect(),
        selected: e.selected,
        page: e.page.clone(),
        orientation: Orientation::default(),
    }
}

/// 候选窗抽象。M4 增加 RemoteCandidateWindow（IPC 转发 Tauri helper），COM 层零改动。
pub trait CandidateUi {
    fn show(&mut self, snap: &UiSnapshot, caret: CaretRect);
    fn update(&mut self, snap: &UiSnapshot);
    fn move_to(&mut self, caret: CaretRect);
    fn hide(&mut self);
    fn is_visible(&self) -> bool;
}

/// 空实现桩：Agent D 在 Agent E 完成前用它联调管线。
pub struct NullCandidateUi;

impl CandidateUi for NullCandidateUi {
    fn show(&mut self, _snap: &UiSnapshot, _caret: CaretRect) {}
    fn update(&mut self, _snap: &UiSnapshot) {}
    fn move_to(&mut self, _caret: CaretRect) {}
    fn hide(&mut self) {}
    fn is_visible(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iuv_core::{Candidate, CandidateKind};

    #[test]
    fn effect_to_snapshot_maps_fields() {
        let mut e = Effect::default();
        e.reading = "ni'hao".into();
        e.candidates = vec![
            Candidate::new("你好", CandidateKind::Word, "nihao", 1, 2),
            Candidate::new("泥嚎", CandidateKind::Word, "nihao", 2, 2),
        ];
        e.selected = 1;
        e.page = PageInfo {
            page: 0,
            page_count: 2,
            page_size: 5,
            total: 7,
        };
        let snap = effect_to_snapshot(&e);
        assert_eq!(snap.reading, "ni'hao");
        assert_eq!(snap.candidates, vec!["你好", "泥嚎"]);
        assert_eq!(snap.selected, 1);
        assert_eq!(snap.page.page_count, 2);
    }
}
