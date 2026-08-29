//! daemon 协作（P2.2 从 text_service.rs 拆出）：实例注册/重注册 + 配置热载。
//! 均挂 `impl TextService`（32-status-toolbar.md §4/§5 + 22-m6-daemon.md）。

use std::sync::Arc;

use iuv_core::Engine;

use crate::log::{self, log_line};
use crate::session_bridge::is_passthrough_app;

use super::engine_host::engine;
use super::text_service::TextService;

impl TextService {
    /// M6 daemon 轮询（route_key 按键路径唯一触发点）：共享段版本/纪元检测
    /// （用户库/配置热载）+ 离线→在线翻转重发激活（§4.4 自愈）。
    /// 成本 = 读两个 u32 原子量；引擎未就绪/daemon 客户端缺失时静默跳过。
    /// 2026-08-21 决策：**不挂轮询定时器**——daemon 异常重启的自愈靠事件驱动
    /// （Activate 重发激活 + 本函数按键路径），零交互盲区以注销/重启规避
    /// （正式使用不重启 daemon；对齐小狼毫纯事件驱动架构）。
    pub(crate) fn daemon_poll_tick(&self) {
        let Some(engine) = engine() else { return };
        if let Some(client) = self.daemon.borrow().as_ref() {
            client.poll(
                &engine,
                |engine| self.apply_config_hot_reload(engine),
                || self.signal_focus_gained(),
            );
        }
    }

    /// 激活上报（40-toolbar-show-hide-governance.md 纯信号模型）：实例获得焦点 /
    /// TIP 激活 / daemon 上线翻转——「激活 + 当前四态」经信号通道发 daemon，
    /// 由其绑定并渲染工具栏。passthrough 进程不上报（iuv 完全透明）。
    pub(crate) fn signal_focus_gained(&self) {
        let cfg = iuv_core::Config::load();
        let passthrough = !cfg.passthrough_apps.is_empty()
            && is_passthrough_app(&log::module_name(), &cfg.passthrough_apps);
        if passthrough {
            log_line("[toolbar] passthrough 进程：不上报工具栏信号（iuv 完全透明）");
            return;
        }
        let Some(client) = self.daemon.borrow().as_ref().cloned() else {
            return;
        };
        let (pid, tid) = self.instance_id();
        client.focus_gained(pid, tid, self.runtime_snapshot());
    }

    /// M6 配置热载（config_epoch 变化触发，DaemonClient::poll 回调）：
    /// 重载 config.json → 引擎配置（page_size/passthrough_apps/theme/keymap 等读取点随新值生效）
    /// + 候选窗主题即时切换（set_theme，下帧 paint 生效）。
    /// 会话快捷键（keymap）热载：route_key 每键读 `engine.config().keymap` 查表，
    /// set_config 替换后即生效（41-keymap-settings.md §2；全局热键由 daemon 侧重注册）。
    pub(crate) fn apply_config_hot_reload(&self, engine: &Arc<Engine>) {
        let cfg = iuv_core::Config::load();
        let keymap_changed = cfg.keymap != engine.config().keymap;
        engine.set_config(cfg.clone());
        // 日志模块禁用集热载（26-log-modules.md）：随 config_epoch 生效。
        crate::log::set_log_modules_disabled(&cfg.disabled_log_modules);
        // 性能埋点热载（config_epoch 触发）：排查时置 true 即时生效，无需重载。
        crate::log::configure_perf_probe(cfg.perf_probe);
        let theme = match cfg.theme {
            iuv_core::ThemeChoice::Light => iuv_ui::theme_light(),
            iuv_core::ThemeChoice::Dark => iuv_ui::theme_dark(),
        };
        self.ui.borrow_mut().set_theme(theme);
        log_line(&format!(
            "[daemon] 配置热载：theme={:?} passthrough_apps={} keymap{}",
            cfg.theme,
            cfg.passthrough_apps.len(),
            if keymap_changed { "变化（会话键已生效）" } else { "不变" }
        ));
    }
}
