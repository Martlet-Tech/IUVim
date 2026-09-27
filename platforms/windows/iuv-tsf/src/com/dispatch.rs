//! Effect 应用（P2.2 从 text_service.rs 拆出）：`dispatch_effect` 自由函数 +
//! `TextService::dispatch` 薄包装 + 自绘候选窗抑制判定。

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use iuv_core::Session;
use iuv_proto::KeyOutcome;

use crate::com::remote_host::{
    backend_config, core_candidate, core_page, core_session_end, remote, use_server,
};
use crate::composition::Composition;
use crate::log::{self, log_line, perf_record_with, perf_tick};
use crate::session_bridge::{apply_effect, is_passthrough_app};
use crate::ui::{CandidateUi, CandwinCandidateWindow, CaretRect};
use crate::ui_element::CandidateElementHost;

use super::text_service::TextService;

/// 应用 Effect（契约 §7）：composition → 候选窗；end 则上屏/取消并清理会话。
impl TextService {
    pub(crate) fn dispatch(&self, effect: &iuv_core::Effect) {
        let t = perf_tick();
        // P4 服务端渲染：远端模式本地候选窗不画（iuv-server 画），仅更新
        // composition/caret 并在锚点变化时上报 CaretMoved。
        let remote = crate::com::remote_host::use_server();
        dispatch_effect(
            &self.session,
            &self.composition,
            &self.ui,
            &self.caret,
            &self.cand_elem,
            effect,
            !remote,
        );
        if remote {
            let caret = self.caret.get();
            if caret != self.caret_reported.get() {
                self.caret_reported.set(caret);
                if let Some(r) = crate::com::remote_host::remote() {
                    r.sync_caret(caret);
                }
            }
        }
        // M1 桌宠（docs/pet/M1-IMPLEMENTATION.md §2.1 + §4.4）：组合状态 transition
        // → Typing 信号。"正在打字" = composition 存在 + 未 end（持续会话中）。
        // （P4 前还有"候选非空"条件——服务端渲染后普通应用候选为空属常态，撤销。）
        // 边沿检测避免每键重复发。
        let composing_now = self.composition.borrow().is_some() && effect.end.is_none();
        if composing_now != self.was_typing.get() {
            self.was_typing.set(composing_now);
            if let Some(client) = self.daemon.borrow().as_ref() {
                let (pid, tid) = self.instance_id();
                client.typing(pid, tid, composing_now);
            }
        }
        perf_record_with("dispatch", t, || {
            format!(
                "cand={} all={}",
                effect.candidates.len(),
                effect.all_candidates.len()
            )
        });
    }

    /// M10 远端模式：应用 KeyOutcome（以 `last_effect` 为基线组装 Effect，
    /// 复用既有 dispatch 渲染路径；会话结束 → 清基线 + 通知服务端 EndSession）。
    pub(crate) fn dispatch_outcome(&self, outcome: KeyOutcome) {
        let base = self.last_effect.borrow_mut().take();
        let (effect, ended) = merge_outcome(base, outcome);
        self.dispatch(&effect);
        if ended {
            self.last_effect.borrow_mut().take();
            if use_server() {
                if let Some(r) = remote() {
                    r.end_session();
                }
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
        e.page = core_page(p);
    }
    if let Some(s) = outcome.selected {
        e.selected = s as usize;
    }
    e.end = outcome.end.map(core_session_end);
    let ended = e.end.is_some();
    (e, ended)
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
    render_locally: bool,
) {
    // TSF 候选 UI 元素同步（与自绘窗平行）：候选非空 → Begin/Update；空 → End。
    // effect.end 的提交/取消路径统一走 ended 分支 End，这里跳过避免多余一次 Update。
    if effect.end.is_none() {
        let snap = crate::ui::effect_to_snapshot(effect);
        cand_elem.borrow_mut().sync(&snap);
    }
    // M10：模式感知配置（local=engine.config / remote=客户端副本）。
    let cfg = backend_config();
    let orientation = cfg
        .as_ref()
        .map(|c| c.candidate_orientation)
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
                    apply_effect(
                        comp,
                        &mut *ui_guard,
                        &mut caret_pos,
                        effect,
                        orientation,
                        render_locally,
                    )
                }
            }
            // composition 缺失（异常路径）：仅更新候选窗并继续。
            None => {
                log_line("dispatch：composition 缺失，仅更新候选窗");
                if !render_locally {
                    false // P4 服务端渲染：本地窗不画（match 臂值，非 return）
                } else {
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
        }
    };
    caret.set(caret_pos);
    // 自绘候选窗抑制（candidate_owner_apps 名单驱动，2026-08-20 弃矩形启发式）：
    // 命中进程（如 WoW 自绘游戏内候选栏）→ 抑制自绘窗（避免双候选栏）；默认空 = 恒自绘。
    // 名单空时零开销（不查进程名）。候选 UI 元素同步不受影响（游戏桥仍可拉取候选数据）。
    let suppress = cfg
        .map(|c| c.candidate_owner_apps)
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
        assert!(should_suppress_candidate_window(
            &["wow.exe".into()],
            "WoW.exe"
        ));
        // 未命中名单 = 恒自绘
        assert!(!should_suppress_candidate_window(
            &["wow.exe".into()],
            "weixin.exe"
        ));
    }
}
