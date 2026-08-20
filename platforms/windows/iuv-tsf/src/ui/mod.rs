//! 候选窗抽象与 Effect → UiSnapshot 映射。W0 完整实现，冻结。
//! MVP 实现 = GdiCandidateWindow（Agent E）；M4 起 = CandwinCandidateWindow
//! （ULW 呈现 + iuv-ui 绘图，见 19-m4-cross-render.md），类型与快照自 iuv-ui 迁出。

pub mod candwin;
pub mod menu_window;
pub mod ulw;
pub use candwin::CandwinCandidateWindow;
pub use menu_window::MenuWindow;
pub use iuv_core::Orientation;
pub use iuv_ui::{effect_to_snapshot, CaretRect, UiSnapshot};

/// 候选窗抽象。M4 起实现 = CandwinCandidateWindow（iuv-ui 渲染 + ULW 呈现），
/// 见 `19-m4-cross-render.md`。COM 层零改动。
pub trait CandidateUi {
    fn show(&mut self, snap: &UiSnapshot, caret: CaretRect);
    fn update(&mut self, snap: &UiSnapshot);
    fn move_to(&mut self, caret: CaretRect);
    fn hide(&mut self);
    fn is_visible(&self) -> bool;
    /// 抑制显示：`candidate_owner_apps` 命中（app 自绘候选栏）时静默——show/update 空操作，
    /// 开启瞬间隐藏已显示窗口；false 恢复。引擎/元素/交互逻辑不受影响。
    fn set_suppressed(&mut self, suppressed: bool);
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
    fn set_suppressed(&mut self, _suppressed: bool) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use iuv_core::{Candidate, CandidateKind, Effect, PageInfo};

    #[test]
    fn effect_to_snapshot_maps_fields() {
        let mut e = Effect::default();
        e.reading = "ni'hao".into();
        let cands = vec![
            Candidate::new("你好", CandidateKind::Word, "nihao", 1, 2),
            Candidate::new("泥嚎", CandidateKind::Word, "nihao", 2, 2),
        ];
        e.candidates = cands.clone();
        e.all_candidates = cands;
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
        assert_eq!(snap.all_candidates, vec!["你好", "泥嚎"]);
        assert_eq!(snap.selected, 1);
        assert_eq!(snap.page.page_count, 2);
    }
}
