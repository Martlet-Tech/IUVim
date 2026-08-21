//! daemon 协作（P2.2 从 text_service.rs 拆出）：实例注册/重注册 + 配置热载。
//! 均挂 `impl TextService`（32-status-toolbar.md §4/§5 + 22-m6-daemon.md）。

use std::sync::Arc;

use iuv_core::Engine;

use crate::log::{self, log_line};
use crate::session_bridge::is_passthrough_app;

use super::text_service::TextService;

impl TextService {
    /// 向 daemon 注册实例 + 通知 active（Activate 时；passthrough 进程不注册，iuv 完全透明）。
    /// Register 仅首 Activate 发一次（防重复）；`Active{true}` 每次 Activate 都发（daemon 判
    /// 「iuv 被选中」→ 看板显示；Deactivate 发 false 隐藏）。Register 失败 = daemon 离线
    /// （静默；poll 在线翻转后重注册，§4.4）。
    pub(crate) fn register_instance(&self) {
        let cfg = iuv_core::Config::load();
        let passthrough = !cfg.passthrough_apps.is_empty()
            && is_passthrough_app(&log::module_name(), &cfg.passthrough_apps);
        if passthrough {
            log_line("[toolbar] passthrough 进程：不注册工具栏实例（iuv 完全透明）");
            return;
        }
        let Some(client) = self.daemon.borrow().as_ref().cloned() else {
            return;
        };
        let (pid, tid) = self.instance_id();
        if !self.registered.get() {
            if client.register(pid, tid, self.runtime_snapshot()) {
                self.registered.set(true);
                log_line(&format!("[toolbar] 实例注册（{pid}:{tid}）"));
            }
        }
        client.set_active(pid, tid, true);
    }

    /// daemon 重启恢复重注册（§4.4：poll 检测离线→在线翻转后调用）：
    /// daemon 重启清空实例表，本进程仍在运行（registered 仍 true）→ 强制重新 Register。
    pub(crate) fn re_register_instance(&self) {
        self.registered.set(false);
        self.register_instance();
    }

    /// M6 配置热载（config_epoch 变化触发，DaemonClient::poll 回调）：
    /// 重载 config.json → 引擎配置（page_size/passthrough_apps/主题等读取点随新值生效）
    /// + 候选窗主题即时切换（set_theme，下帧 paint 生效）。
    /// 键位（keymap）热载为 M7 范畴（TSF 键映射装配不热切），keymap 变化仅记日志。
    pub(crate) fn apply_config_hot_reload(&self, engine: &Arc<Engine>) {
        let cfg = iuv_core::Config::load();
        let keymap_changed = cfg.keymap != engine.config().keymap;
        engine.set_config(cfg.clone());
        // 日志模块禁用集热载（26-log-modules.md）：随 config_epoch 生效。
        crate::log::set_log_modules_disabled(&cfg.disabled_log_modules);
        let theme = match cfg.theme {
            iuv_core::ThemeChoice::Light => iuv_ui::theme_light(),
            iuv_core::ThemeChoice::Dark => iuv_ui::theme_dark(),
        };
        self.ui.borrow_mut().set_theme(theme);
        log_line(&format!(
            "[daemon] 配置热载：theme={:?} passthrough_apps={} keymap{}",
            cfg.theme,
            cfg.passthrough_apps.len(),
            if keymap_changed {
                "变化（键位热载 M7）"
            } else {
                "不变"
            }
        ));
    }
}