//! Effect 应用（P2.2 从 text_service.rs 拆出）：`dispatch_effect` 自由函数 +
//! `TextService::dispatch` 薄包装。（自绘候选窗抑制判定随本地候选窗于
//! 2026-10-02 品质审查 D3 退役——抑制名单由 iuv-server 侧 app_suppressed 判定。）

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use iuv_core::Session;
use iuv_proto::KeyOutcome;

use crate::com::remote_host::{core_candidate, remote};
use crate::composition::Composition;
use crate::log::{log_line, perf_record_with, perf_tick};
use crate::session_bridge::apply_effect;
use crate::ui::CaretRect;
use crate::ui_element::CandidateElementHost;

use super::text_service::TextService;

/// 应用 Effect（契约 §7）：composition → 候选窗；end 则上屏/取消并清理会话。
impl TextService {
    pub(crate) fn dispatch(&self, effect: &iuv_core::Effect) {
        let t = perf_tick();
        // P4 服务端渲染：候选窗由 iuv-server 画，这里仅更新 composition/caret
        // 并在锚点变化时上报 CaretMoved。
        dispatch_effect(
            &self.session,
            &self.composition,
            &self.caret,
            &self.cand_elem,
            effect,
        );
        let caret = self.caret.get();
        if caret != self.caret_reported.get() {
            self.caret_reported.set(caret);
            if let Some(r) = crate::com::remote_host::remote() {
                r.sync_caret(caret);
            }
        }
        // M1 桌宠（docs/pet/M1-IMPLEMENTATION.md §2.1 + §4.4）：组合状态 transition
        // → Typing 信号。"正在打字" = composition 存在 + 未 end（持续会话中）。
        // （P4 前还有"候选非空"条件——服务端渲染后普通应用候选为空属常态，撤销。）
        // 边沿检测避免每键重复发。
        let composing_now = self.composition.borrow().is_some() && effect.end.is_none();
        if composing_now != self.was_typing.get() {
            self.was_typing.set(composing_now);
            self.notify_typing(composing_now);
        }
        perf_record_with("dispatch", t, || {
            format!(
                "cand={} all={}",
                effect.candidates.len(),
                effect.all_candidates.len()
            )
        });
    }

    /// M10：应用 KeyOutcome（以 `last_effect` 为基线组装 Effect，
    /// 复用既有 dispatch 渲染路径；会话结束 → 清基线 + 通知服务端 EndSession）。
    pub(crate) fn dispatch_outcome(&self, outcome: KeyOutcome) {
        let base = self.last_effect.borrow_mut().take();
        let (effect, ended) = merge_outcome(base, outcome);
        self.dispatch(&effect);
        if ended {
            self.last_effect.borrow_mut().take();
            if let Some(r) = remote() {
                r.end_session();
            }
        } else if self.composition.borrow().is_none() {
            // composition 已死（外部终止与在途键的竞态兜底）：会话无法延续，
            // 同步收尾防脑裂（下一键全新会话）。
            self.last_effect.borrow_mut().take();
            if let Some(r) = remote() {
                r.end_session();
            }
        } else {
            *self.last_effect.borrow_mut() = Some(effect);
        }
    }
}

/// KeyOutcome + 上帧 Effect 基线 → 完整 Effect（`Option = None` 沿用基线，49 §4.5.2 增量语义）。
/// 返回 `(effect, ended)`。候选窗点击回调与 TextService::dispatch_outcome 共用。
pub(crate) fn merge_outcome(
    base: Option<iuv_core::Effect>,
    outcome: KeyOutcome,
) -> (iuv_core::Effect, bool) {
    let mut e = base.unwrap_or_default();
    if let Some(c) = outcome.composition {
        e.composition = c;
    }
    if let Some(r) = outcome.reading {
        e.reading = r;
    }
    if let Some(v) = outcome.candidates {
        e.candidates = v.iter().map(core_candidate).collect();
    }
    if let Some(v) = outcome.all_candidates {
        e.all_candidates = v.iter().map(core_candidate).collect();
    }
    if let Some(p) = outcome.page {
        e.page = p;
    }
    if let Some(s) = outcome.selected {
        e.selected = s as usize;
    }
    e.end = outcome.end;
    let ended = e.end.is_some();
    (e, ended)
}

/// dispatch 的自由函数版：经 Rc 共享槽访问 session/composition/caret/cand_elem。
pub(crate) fn dispatch_effect(
    session: &Rc<RefCell<Option<Session>>>,
    composition: &Rc<RefCell<Option<Composition>>>,
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
                    apply_effect(comp, &mut caret_pos, effect)
                }
            }
            // composition 缺失（异常路径）：cand_elem 已在上方 sync，无事可做。
            None => {
                log_line("dispatch：composition 缺失，跳过渲染");
                false
            }
        }
    };
    caret.set(caret_pos);
    if ended {
        cand_elem.borrow_mut().end();
        *session.borrow_mut() = None;
        *composition.borrow_mut() = None;
        if degraded {
            log_line("dispatch：降级完成，会话已丢弃");
        }
    }
}
