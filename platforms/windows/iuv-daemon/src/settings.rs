//! 设置窗口（egui/eframe，**主线程**运行）。
//!
//! winit 事件循环只能在进程主线程创建（独立线程会 panic，实测 2026-08-17），
//! 故守护进程主线程 = eframe 事件循环宿主：轮询 `open_settings` 标志，
//! 语言栏菜单「设置」（管道 `OpenSettings`）置位后主线程 `run_settings` 弹窗
//! （阻塞至关窗，关窗后继续后台常驻轮询）。管道/共享段在独立线程，不受影响。
//!
//! 设置项（保存 → 写 config.json → bump config_epoch 广播给会话进程）：
//! 主题（浅色/深色）、按键直通名单（passthrough_apps）、用户库管理（列表 + 清除全部）、
//! 键位自定义（**灰置占位，M7**）。绝不 panic：run_settings 包 `catch_unwind`。

use std::sync::atomic::Ordering;
use std::sync::Arc;

use eframe::egui;
use iuv_data::UserDict;

use crate::config;
use crate::log;
use crate::state::DaemonState;

/// eframe 窗口主体（阻塞直到窗口关闭；主线程调用）。返回 Ok(()) = 正常关闭。
pub fn run_settings(state: &Arc<DaemonState>) -> Result<(), String> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([420.0, 560.0])
            .with_min_inner_size([360.0, 420.0])
            .with_title("iuv 设置"),
        ..Default::default()
    };
    let state = state.clone();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        eframe::run_native(
            "iuv 设置",
            options,
            Box::new(move |cc| {
                install_cjk_font(&cc.egui_ctx);
                Ok(Box::new(SettingsApp::new(state)))
            }),
        )
    }));
    match r {
        Ok(Ok(())) => Ok(()),
        Ok(Err(e)) => {
            log::log_line(&format!("[settings] eframe 运行错误: {e}"));
            Err(e.to_string())
        }
        Err(_) => {
            log::log_line("[settings] eframe 主线程 panic（已捕获）");
            Err("eframe panic".into())
        }
    }
}

/// 注入系统中文字体（egui 内置字体不含 CJK 字形——中文显示为豆腐块，2026-08-17 实测）。
/// Proportional 与 Monospace 两族末尾追加为 fallback（设置页用户库列表用 monospace）。
/// 找不到字体 → 记日志，维持默认字体（仅异常环境中文乱码）。
fn install_cjk_font(ctx: &egui::Context) {
    let Some(bytes) = load_cjk_font_bytes() else {
        log::log_line("[settings] 未找到系统中文字体，设置页用默认字体（中文可能乱码）");
        return;
    };
    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert("iuv-cjk".to_owned(), egui::FontData::from_owned(bytes).into());
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts.families.entry(family).or_default().push("iuv-cjk".to_owned());
    }
    ctx.set_fonts(fonts);
    log::log_line("[settings] 已注入系统中文字体（微软雅黑等）");
}

/// 定位系统中文字体文件（WINDIR\Fonts 候选，取第一个存在；TTC 由 ab_glyph 取首 face）。
fn load_cjk_font_bytes() -> Option<Vec<u8>> {
    let windir = std::env::var("WINDIR").ok()?;
    for name in ["msyh.ttc", "msyh.ttf", "Deng.ttf", "simsun.ttc", "simhei.ttf"] {
        let path = std::path::PathBuf::from(&windir).join("Fonts").join(name);
        match std::fs::read(&path) {
            Ok(bytes) => {
                log::log_line(&format!("[settings] 使用中文字体：{path:?}"));
                return Some(bytes);
            }
            Err(_) => continue,
        }
    }
    None
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

    /// 「清除全部」：空库 + 发布 + 立即写盘。
    /// 必须立即落盘：设置页不走管道（管道路径 publish 后紧接 flush_now），若只置
    /// dirty，注销时 daemon 被硬杀不触发退出强写盘 → 磁盘旧库残留、下次登录复活
    /// （2026-08-18 实测：清除两次注销两次均复活）。用户点确认即持久化。
    fn clear_all(&mut self) {
        {
            let mut dict = self.state.dict.lock().unwrap_or_else(|p| p.into_inner());
            *dict = UserDict::empty();
        }
        self.state.publish();
        self.state.flush_now();
        self.confirm_clear = false;
        self.status = "已清除全部用户库（已落盘）".into();
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