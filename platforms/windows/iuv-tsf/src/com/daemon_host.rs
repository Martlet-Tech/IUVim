//! 远端协作（P2.2 从 text_service.rs 拆出；M10 ② daemon→server 全量迁移 +
//! ③ 本地模式删除后仅存远端路径）：焦点/打字信号。
//! 均挂 `impl TextService`（32-status-toolbar.md §4/§5 + 22-m6-daemon.md）。
//! （主题收敛随本地候选窗于 2026-10-02 品质审查 D3 退役——服务端自渲染，
//! 客户端无窗可切主题。）

use crate::log::{self, log_line};
use crate::session_bridge::is_passthrough_app;

use super::text_service::TextService;

impl TextService {
    /// 激活上报（40-toolbar-show-hide-governance.md 纯信号模型）：实例获得焦点 /
    /// TIP 激活——「激活 + 当前四态」经 transport 发 iuv-server，由其绑定并渲染
    /// 工具栏。passthrough 进程不上报（iuv 完全透明）。
    pub(crate) fn signal_focus_gained(&self) {
        let cfg = iuv_core::Config::load();
        let passthrough = !cfg.passthrough_apps.is_empty()
            && is_passthrough_app(&log::module_name(), &cfg.passthrough_apps);
        if passthrough {
            log_line("[toolbar] passthrough 进程：不上报工具栏信号（iuv 完全透明）");
            return;
        }
        // 焦点真值记录（remote 未就绪也记——重连/首连回放靠它，见 remote_host）。
        crate::com::remote_host::note_focus(true);
        if let Some(r) = crate::com::remote_host::remote() {
            r.focus_changed(true);
        }
    }

    pub(crate) fn notify_focus_lost(&self) {
        crate::com::remote_host::note_focus(false);
        if let Some(r) = crate::com::remote_host::remote() {
            r.focus_changed(false);
        }
    }

    /// 实例停用上报（TSF Deactivate / 实例 Drop）：强于失焦——服务端解绑工具栏
    /// 不受设置窗粘性抑制（切到别的输入法 = 用户明确弃用 iuv，热键前提不成立）。
    pub(crate) fn notify_instance_deactivated(&self) {
        crate::com::remote_host::note_focus(false);
        if let Some(r) = crate::com::remote_host::remote() {
            r.instance_deactivated();
        }
    }

    pub(crate) fn notify_typing(&self, active: bool) {
        if let Some(r) = crate::com::remote_host::remote() {
            r.send_typing(active);
        }
    }
}
