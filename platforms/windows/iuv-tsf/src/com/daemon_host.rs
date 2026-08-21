//! daemon 协作（P2.2 从 text_service.rs 拆出）：实例注册/重注册 + 配置热载。
//! 均挂 `impl TextService`（32-status-toolbar.md §4/§5 + 22-m6-daemon.md）。

use std::sync::Arc;

use iuv_core::Engine;

use crate::log::{self, log_line};
use crate::session_bridge::is_passthrough_app;

use super::engine_host::engine;
use super::text_service::TextService;

impl TextService {
    /// M6 daemon 轮询（按键路径 route_key 与 ctl 隐藏窗 2s 定时器**共用**）：
    /// 共享段版本/纪元检测（用户库/配置热载）+ 离线→在线翻转重注册（§4.4 自愈）。
    /// 成本 = 读两个 u32 原子量；引擎未就绪/daemon 客户端缺失时静默跳过。
    /// 2026-08-21 起定时器承载自愈——修「daemon 重启窗口期内打开的应用 Register 失败
    /// 后无重试、直到打字才恢复」（此前唯一触发点在按键路径，日志实测盲区）。
    pub(crate) fn daemon_poll_tick(&self) {
        let Some(engine) = engine() else { return };
        if let Some(client) = self.daemon.borrow().as_ref() {
            client.poll(&engine, |engine| self.apply_config_hot_reload(engine), || {
                self.register_instance()
            });
        }
    }

    /// 向 daemon 注册实例 + 通知 active（Activate 时；passthrough 进程不注册，iuv 完全透明）。
    /// **每次 Activate 都发 Register**（daemon 侧 `instances.insert` 幂等覆盖）：daemon 重启
    /// 清空实例表后，焦点切回任意 iuv 应用即自愈重建（§4.4），无需等按键触发的 poll——
    /// 旧实现的 registered 门导致重启后只发 Active（被 daemon 对未知实例丢弃），工具栏
    /// 「显示/隐藏」点了没反应，直到打字才恢复（2026-08-21 日志实测修复）。
    /// Register 失败 = daemon 离线（静默；poll 在线翻转 / 下次 Activate 重试，§4.4）。
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
        if client.register(pid, tid, self.runtime_snapshot()) {
            log_line(&format!("[toolbar] 实例注册（{pid}:{tid}）"));
        }
        client.set_active(pid, tid, true);
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