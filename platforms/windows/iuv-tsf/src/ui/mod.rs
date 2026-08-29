//! 候选窗抽象与 Effect → UiSnapshot 映射。
//! MVP 实现 = GdiCandidateWindow（已删）；M4 起 = CandwinCandidateWindow
//! （ULW 呈现 + iuv-ui 绘图，见 19-m4-cross-render.md），类型与快照自 iuv-ui 迁出。

pub mod candwin;
pub mod menu_window;
pub use candwin::CandwinCandidateWindow;
pub use menu_window::MenuWindow;
pub use iuv_ui::{effect_to_snapshot, CaretRect, UiSnapshot};

/// 候选窗抽象。M4 起实现 = CandwinCandidateWindow（iuv-ui 渲染 + ULW 呈现），
/// 见 `19-m4-cross-render.md`。COM 层零改动。
pub trait CandidateUi {
    fn show(&mut self, snap: &UiSnapshot, caret: CaretRect);
    fn update(&mut self, snap: &UiSnapshot);
    fn hide(&mut self);
    fn is_visible(&self) -> bool;
    /// 抑制显示：`candidate_owner_apps` 命中（app 自绘候选栏）时静默——show/update 空操作，
    /// 开启瞬间隐藏已显示窗口；false 恢复。引擎/元素/交互逻辑不受影响。
    fn set_suppressed(&mut self, suppressed: bool);
}
