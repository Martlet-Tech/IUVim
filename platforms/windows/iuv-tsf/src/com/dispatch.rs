//! Effect 应用（P2.2 从 text_service.rs 拆出）：`dispatch_effect` 自由函数 +
//! `TextService::dispatch` 薄包装 + 自绘候选窗抑制判定。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use iuv_core::Session;

use crate::composition::Composition;
use crate::com::engine_host::engine;
use crate::log::{self, log_line};
use crate::session_bridge::{apply_effect, is_passthrough_app};
use crate::ui::{CandidateUi, CandwinCandidateWindow, CaretRect};
use crate::ui_element::CandidateElementHost;

use super::text_service::TextService;

/// 应用 Effect（契约 §7）：composition → 候选窗；end 则上屏/取消并清理会话。
impl TextService {
    pub(crate) fn dispatch(&self, effect: &iuv_core::Effect) {
        dispatch_effect(
            &self.session,
            &self.composition,
            &self.ui,
            &self.caret,
            &self.cand_elem,
            effect,
        )
    }
}

/// dispatch 的自由函数版：候选窗点击回调（同线程）与 TextService 共用同一路径。
/// 经 Rc 共享槽访问 session/composition/ui/caret/cand_elem；orientation 取自引擎配置。
pub(crate) fn dispatch_effect(
    session: &Rc<RefCell<Option<Session>>>,
    composition: &Rc<RefCell<Option<Composition>>>,
    ui: &Rc<RefCell<CandwinCandidateWindow>>,
    caret: &Rc<Cell<CaretRect>>,
    cand_elem: &Rc<RefCell<CandidateElementHost>>,
    effect: &iuv_core::Effect,
) {
    // TSF 候选 UI 元素同步（与自绘窗平行）：候选非空 → Begin/Update；空 → End。
    // effect.end 的提交/取消路径统一走 ended 分支 End，这里跳过避免多余一次 Update。
    if effect.end.is_none() {
        let snap = crate::ui::effect_to_snapshot(effect);
        cand_elem.borrow_mut().sync(&snap);
    }
    let orientation = engine()
        .map(|e| e.config().candidate_orientation)
        .unwrap_or_default();
    let mut caret_pos = caret.get();
    let mut degraded = false;
    let ended = {
        let comp = composition.borrow();
        match comp.as_ref() {
            Some(comp) => {
                // 外部终止（OnCompositionTerminated）降级：丢弃会话，
                // 文档残留文本由用户自行清理，下一键重新开会话（透明放行避免 0x8000FFFF 卡死）。
                if comp.terminated() {
                    log_line("dispatch：composition 被外部终止，降级丢弃会话");
                    degraded = true;
                    true
                } else {
                    let mut ui_guard = ui.borrow_mut();
                    apply_effect(comp, &mut *ui_guard, &mut caret_pos, effect, orientation)
                }
            }
            // composition 缺失（异常路径）：仅更新候选窗并继续。
            None => {
                log_line("dispatch：composition 缺失，仅更新候选窗");
                let mut snap = crate::ui::effect_to_snapshot(effect);
                snap.orientation = orientation;
                let mut ui_guard = ui.borrow_mut();
                if snap.candidates.is_empty() && snap.reading.is_empty() {
                    ui_guard.hide();
                } else if ui_guard.is_visible() {
                    ui_guard.update(&snap);
                } else {
                    ui_guard.show(&snap, caret_pos);
                }
                effect.end.is_some()
            }
        }
    };
    caret.set(caret_pos);
    // 自绘候选窗抑制（candidate_owner_apps 名单驱动，2026-08-20 弃矩形启发式）：
    // 命中进程（如 WoW 自绘游戏内候选栏）→ 抑制自绘窗（避免双候选栏）；默认空 = 恒自绘。
    // 名单空时零开销（不查进程名）。候选 UI 元素同步不受影响（游戏桥仍可拉取候选数据）。
    let suppress = engine()
        .map(|e| e.config().candidate_owner_apps)
        .map(|apps| should_suppress_candidate_window(&apps, &log::module_name()))
        .unwrap_or(false);
    ui.borrow_mut().set_suppressed(suppress);
    if ended {
        ui.borrow_mut().hide();
        cand_elem.borrow_mut().end();
        *session.borrow_mut() = None;
        *composition.borrow_mut() = None;
        if degraded {
            log_line("dispatch：降级完成，会话已丢弃");
        }
    }
}

/// 自绘候选窗抑制判定（candidate_owner_apps 名单驱动，2026-08-20 弃矩形启发式）：
/// 名单空 = 恒自绘（false，零开销）；命中进程名 = 抑制自绘窗（true，app 自绘候选栏）。
fn should_suppress_candidate_window(apps: &[String], exe: &str) -> bool {
    !apps.is_empty() && is_passthrough_app(exe, apps)
}

#[cfg(test)]
mod tests {
    use super::should_suppress_candidate_window;

    #[test]
    fn suppress_only_for_listed_apps() {
        // 空名单 = 恒自绘（微信/notepad/WinTerm 等主流应用不误伤）
        assert!(!should_suppress_candidate_window(&[], "weixin.exe"));
        // 命中名单（大小写不敏感精确匹配）= 抑制（WoW 游戏自绘候选栏）
        assert!(should_suppress_candidate_window(&["wow.exe".into()], "WoW.exe"));
        // 未命中名单 = 恒自绘
        assert!(!should_suppress_candidate_window(&["wow.exe".into()], "weixin.exe"));
    }
}