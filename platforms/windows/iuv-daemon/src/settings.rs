//! 设置窗口（egui/eframe，独立线程运行）。
//!
//! eframe 自带 winit 事件循环独占所在线程，故守护进程主线程跑 Win32 托盘消息循环、
//! 设置窗口在**独立线程**跑 `eframe::run_native`。跨线程联动：
//! - 托盘菜单「打开设置」→ `open()`：无窗口则 spawn 线程；有窗口则
//!   `ViewportCommand::Visible(true)` + `request_repaint()` 唤出；
//! - 托盘「退出」→ 主线程置 `close_settings` → 设置线程检测到后
//!   `ViewportCommand::Close` 关闭窗口 → run_native 返回 → 线程结束（清 settings_ctx）。
//!
//! 设置项（保存 → 写 config.json → bump config_epoch 广播给会话进程）：
//! 主题（浅色/深色）、按键直通名单（passthrough_apps）、用户库管理（列表 + 清除全部）、
//! 键位自定义（**灰置占位，M7**）。绝不 panic：线程体包 `catch_unwind`，失败只记日志。

use std::sync::atomic::Ordering;
use std::sync::Arc;

use eframe::egui;
use iuv_data::UserDict;

use crate::config;
use crate::log;
use crate::state::DaemonState;

/// 设置窗口线程名。
const THREAD_NAME: &str = "iuv-settings";

/// 打开/唤出设置窗口（托盘菜单入口；可在主线程调用）。
pub fn open(state: &Arc<DaemonState>) {
    let has_window = state
        .settings_ctx
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .is_some();
    if has_window {
        // 已有窗口：显示（可能被最小化）+ 重绘。
        if let Some(ctx) = state
            .settings_ctx
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            let _ = ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.request_repaint();
        }
        return;
    }
    spawn(state.clone());
}

/// 起设置窗口线程。线程体绝不 panic（catch_unwind 兜底，失败记日志）。
fn spawn(state: Arc<DaemonState>) {
    let spawned = std::thread::Builder::new()
        .name(THREAD_NAME.to_string())
        .spawn(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = run_settings(&state);
            }));
            if r.is_err() {
                log::log_line("[settings] 设置窗口线程 panic（已捕获）");
            }
            // 线程结束：清 ctx（供 open() 判断重建窗口）。
            *state.settings_ctx.lock().unwrap_or_else(|p| p.into_inner()) = None;
        });
    match spawned {
        Ok(_) => log::log_line("[settings] 设置窗口线程已启动"),
        Err(e) => log::log_line(&format!("[settings] 启动设置窗口线程失败: {e}")),
    }
}

/// eframe 窗口主体（阻塞直到窗口关闭）。
fn run_settings(state: &Arc<DaemonState>) -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([420.0, 560.0])
            .with_min_inner_size([360.0, 420.0])
            .with_title("iuv 设置"),
        ..Default::default()
    };
    let state = state.clone();
    eframe::run_native(
        "iuv 设置",
        options,
        Box::new(move |_cc| Ok(Box::new(SettingsApp::new(state)))),
    )
}

/// 设置页 UI 状态。
struct SettingsApp {
    state: Arc<DaemonState>,
    /// 主题单选值（"light"/"dark"）。
    theme: String,
    /// 直通名单文本编辑（每行一个 exe 名）。
    passthrough: String,
    /// 「清除全部」二次确认。
    confirm_clear: bool,
    /// 操作反馈（保存成功/失败）。
    status: String,
}

impl SettingsApp {
    fn new(state: Arc<DaemonState>) -> Self {
        let cfg = state.config.lock().unwrap_or_else(|p| p.into_inner()).clone();
        SettingsApp {
            state,
            theme: cfg.theme,
            passthrough: cfg.passthrough_apps.join("\n"),
            confirm_clear: false,
            status: String::new(),
        }
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        ui.heading("iuv 设置");
        ui.add_space(4.0);

        // ---- 主题 ----
        ui.label("候选窗主题");
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.theme, "light".to_string(), "浅色");
            ui.radio_value(&mut self.theme, "dark".to_string(), "深色");
        });
        ui.add_space(8.0);

        // ---- 直通名单 ----
        ui.label("按键直通应用（每行一个 exe 名，游戏场景输入法透明）：");
        ui.add(
            egui::TextEdit::multiline(&mut self.passthrough)
                .desired_rows(5)
                .desired_width(360.0)
                .hint_text("例如 notepad.exe"),
        );
        ui.add_space(8.0);

        // ---- 键位自定义（M7 灰置占位）----
        ui.add_enabled_ui(false, |ui| {
            ui.label("键位自定义（M7 开放）");
            let _ = ui.button("翻页键…");
            let _ = ui.button("候选移动键…");
            ui.small("（规划中）");
        });
        ui.add_space(8.0);

        // ---- 用户库管理 ----
        ui.separator();
        ui.heading("用户库管理");
        self.user_dict_section(ui);
        ui.add_space(8.0);
        ui.separator();

        // ---- 操作反馈 + 保存/关闭 ----
        if !self.status.is_empty() {
            ui.colored_label(
                if self.status.starts_with("保存失败") || self.status.starts_with("清除失败") {
                    egui::Color32::from_rgb(0xC0, 0x40, 0x40)
                } else {
                    egui::Color32::from_rgb(0x20, 0x80, 0x40)
                },
                &self.status,
            );
        }
        ui.horizontal(|ui| {
            if ui.button("保存").clicked() {
                self.save();
            }
            if ui.button("关闭").clicked() {
                let _ = ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        });
    }

    /// 用户库列表 + 清除全部。
    fn user_dict_section(&mut self, ui: &mut egui::Ui) {
        let (cover, block, lines) = self.user_dict_snapshot();
        ui.label(format!("覆盖/自造词 {cover} 条 · 屏蔽 {block} 条"));
        if cover + block > 0 {
            egui::ScrollArea::vertical()
                .max_height(160.0)
                .show(ui, |ui| {
                    for line in &lines {
                        ui.monospace(line);
                    }
                });
        } else {
            ui.small("（空用户库）");
        }
        if self.confirm_clear {
            ui.horizontal(|ui| {
                ui.label("确认清除全部用户库？");
                if ui.button("确认清除").clicked() {
                    self.clear_all();
                }
                if ui.button("取消").clicked() {
                    self.confirm_clear = false;
                }
            });
        } else if ui.button("清除全部").clicked() {
            self.confirm_clear = true;
        }
    }

    /// 快照用户库 → (覆盖数, 屏蔽数, 显示行)。
    fn user_dict_snapshot(&self) -> (usize, usize, Vec<String>) {
        let dict = self.state.dict.lock().unwrap_or_else(|p| p.into_inner());
        let mut lines = Vec::new();
        for (code, word, adj) in dict.cover_iter() {
            lines.push(format!("{code}\t{word}\t权重 {adj}"));
        }
        for (code, word) in dict.block_iter() {
            lines.push(format!("{code}\t{word}\t[屏蔽]"));
        }
        (dict.cover_count(), dict.block_count(), lines)
    }

    /// 「清除全部」：空库 + 发布 + 写盘。
    fn clear_all(&mut self) {
        {
            let mut dict = self.state.dict.lock().unwrap_or_else(|p| p.into_inner());
            *dict = UserDict::empty();
        }
        self.state.publish();
        self.confirm_clear = false;
        self.status = "已清除全部用户库（发布 + 待写盘）".into();
    }

    /// 保存：解析名单 → 写 config.json → bump config_epoch → 更新内存配置。
    fn save(&mut self) {
        let theme = if self.theme == "dark" { "dark" } else { "light" };
        let apps: Vec<String> = self
            .passthrough
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect();
        match config::save_config(theme, &apps) {
            Ok(()) => {
                {
                    let mut c = self.state.config.lock().unwrap_or_else(|p| p.into_inner());
                    c.theme = theme.to_string();
                    c.passthrough_apps = apps;
                }
                self.state.bump_config_epoch();
                self.status = "已保存并广播 config_epoch（会话进程检测后重载）".into();
                log::log_line("[settings] 配置已保存 + config_epoch 已广播");
            }
            Err(e) => {
                self.status = format!("保存失败：{e}");
                log::log_line(&format!("[settings] 配置保存失败: {e}"));
            }
        }
    }
}

impl eframe::App for SettingsApp {
    /// 每帧先跑（窗口隐藏时也调用）：注册 ctx 供主线程 Close/唤出；检测退出信号。
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        *self.state.settings_ctx.lock().unwrap_or_else(|p| p.into_inner()) = Some(ctx.clone());
        if self.state.close_settings.swap(false, Ordering::AcqRel) {
            let _ = ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    /// 绘制 UI（root Ui 无外边距，包一层 CentralPanel）。
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| {
            let ctx = ui.ctx().clone();
            self.settings_ui(ui, &ctx);
        });
    }

    fn on_exit(&mut self) {
        *self.state.settings_ctx.lock().unwrap_or_else(|p| p.into_inner()) = None;
        log::log_line("[settings] 设置窗口已关闭");
    }
}