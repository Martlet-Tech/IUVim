//! 远端协作（P2.2 从 text_service.rs 拆出；M10 ② daemon→server 全量迁移 +
//! ③ 本地模式删除后仅存远端路径）：主题收敛 + 焦点/打字信号。
//! 均挂 `impl TextService`（32-status-toolbar.md §4/§5 + 22-m6-daemon.md）。

use crate::log::{self, log_line};
use crate::session_bridge::is_passthrough_app;

use super::text_service::TextService;

impl TextService {
    /// 按键路径唯一轮询点（route_key 触发）：服务端配置推送的实例侧主题收敛。
    /// 成本 = 读一个 u32 原子量 + 纪元比对；零 SHM/IPC/文件读（P4 契约）。
    pub(crate) fn daemon_poll_tick(&self) {
        self.apply_remote_theme_tick();
    }

    /// 远端主题收敛（按键路径触发）：服务端推送泵刷新配置副本时自增纪元，
    /// 实例比对纪元落后才读副本切主题（绝大多数键 = 两次原子比较后直接返回）。
    fn apply_remote_theme_tick(&self) {
        let Some(remote) = crate::com::remote_host::remote() else {
            return;
        };
        let epoch = remote.config_epoch();
        if epoch == self.remote_theme_epoch.get() {
            return;
        }
        let cfg = remote.config();
        let theme = match cfg.theme {
            iuv_core::ThemeChoice::Light => iuv_ui::theme_light(),
            iuv_core::ThemeChoice::Dark => iuv_ui::theme_dark(),
        };
        self.ui.borrow_mut().set_theme(theme);
        self.remote_theme_epoch.set(epoch);
        log_line(&format!(
            "[backend] 实例主题收敛：epoch={epoch} theme={:?}",
            cfg.theme
        ));
    }

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
        if let Some(r) = crate::com::remote_host::remote() {
            r.focus_changed(true);
        }
    }

    pub(crate) fn notify_focus_lost(&self) {
        if let Some(r) = crate::com::remote_host::remote() {
            r.focus_changed(false);
        }
    }

    pub(crate) fn notify_typing(&self, active: bool) {
        if let Some(r) = crate::com::remote_host::remote() {
            r.send_typing(active);
        }
    }
}
