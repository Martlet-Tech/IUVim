//! 设置窗口（egui/eframe，**主线程**运行）。
//!
//! winit 事件循环只能在进程主线程创建（独立线程会 panic，实测 2026-08-17），
//! 故守护进程主线程 = eframe 事件循环宿主：轮询 `open_settings` 标志，
//! 语言栏菜单「设置」（管道 `OpenSettings`）置位后主线程 `run_settings` 弹窗
//! （阻塞至关窗，关窗后继续后台常驻轮询）。管道/共享段在独立线程，不受影响。
//!
//! 界面（25-settings-tabs.md）：固定 1024×800 不可缩放、标题栏无最大化；
//! 多标签页（常用/按键/外观/词库/高级/开发者）+ 底部「确定/取消/应用」。
//! 设置项（确定/应用 → 写 config.json → bump config_epoch 广播给会话进程）：
//! 外观=主题（浅色/深色）、高级=按键直通名单（passthrough_apps）、
//! 词库=用户库管理（列表 + 清除全部，暂挂到确定/应用）、按键=键位自定义（灰置占位，M7）、
//! 开发者（仅 dev 构建）=清除日志。绝不 panic：run_settings 包 `catch_unwind`。

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
            .with_inner_size([1024.0, 800.0])
            .with_min_inner_size([1024.0, 800.0])
            .with_max_inner_size([1024.0, 800.0]) // 锁死 1024×800
            .with_resizable(false) // 禁最大化/拉伸
            .with_maximize_button(false) // 标题栏只剩 最小化 + 关闭
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

/// 标签页。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Common,
    Keymap,
    Appearance,
    Dict,
    Advanced,
    #[cfg(any(debug_assertions, feature = "dev"))]
    Dev,
}

impl Tab {
    fn title(self) -> &'static str {
        match self {
            Tab::Common => "常用",
            Tab::Keymap => "按键",
            Tab::Appearance => "外观",
            Tab::Dict => "词库",
            Tab::Advanced => "高级",
            #[cfg(any(debug_assertions, feature = "dev"))]
            Tab::Dev => "开发者",
        }
    }
}

/// 全部标签（开发者仅 dev 构建，见 25-settings-tabs.md §4）。
fn tabs() -> Vec<Tab> {
    const BASE: [Tab; 5] = [
        Tab::Common,
        Tab::Keymap,
        Tab::Appearance,
        Tab::Dict,
        Tab::Advanced,
    ];
    #[cfg(any(debug_assertions, feature = "dev"))]
    {
        let mut v: Vec<Tab> = BASE.into_iter().collect();
        v.push(Tab::Dev);
        v
    }
    #[cfg(not(any(debug_assertions, feature = "dev")))]
    {
        BASE.into_iter().collect()
    }
}

/// 设置页 UI 状态。
struct SettingsApp {
    state: Arc<DaemonState>,
    /// 当前标签页。
    tab: Tab,
    /// 主题单选值（"light"/"dark"）。
    theme: String,
    /// 直通名单文本编辑（每行一个 exe 名）。
    passthrough: String,
    /// 「清除全部」二次确认。
    confirm_clear: bool,
    /// 清除暂挂：确定/应用才真正清空用户库（取消放弃）。
    pending_clear: bool,
    /// 清除日志结果（(成功, 失败)，开发者页展示）。
    #[cfg(any(debug_assertions, feature = "dev"))]
    log_clear: Option<(usize, usize)>,
    /// 操作反馈（保存成功/失败）。
    status: String,
}

impl SettingsApp {
    fn new(state: Arc<DaemonState>) -> Self {
        let cfg = state.config.lock().unwrap_or_else(|p| p.into_inner()).clone();
        SettingsApp {
            state,
            tab: Tab::Common,
            theme: cfg.theme,
            passthrough: cfg.passthrough_apps.join("\n"),
            confirm_clear: false,
            pending_clear: false,
            #[cfg(any(debug_assertions, feature = "dev"))]
            log_clear: None,
            status: String::new(),
        }
    }

    /// 顶部标签条：大号字体切换。
    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            for tab in tabs() {
                let selected = tab == self.tab;
                let label = egui::RichText::new(tab.title()).size(20.0);
                if ui.selectable_label(selected, label).clicked() {
                    self.tab = tab;
                    self.status.clear();
                }
            }
        });
        ui.add_space(6.0);
    }

    /// 底部公共按钮：确定 / 取消 / 应用（右对齐）。
    fn action_bar(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            if !self.status.is_empty() {
                ui.colored_label(
                    if self.status.contains("失败") {
                        egui::Color32::from_rgb(0xC0, 0x40, 0x40)
                    } else {
                        egui::Color32::from_rgb(0x20, 0x80, 0x40)
                    },
                    &self.status,
                );
            }
            // right_to_left：先加的靠右 → 确定最右（Windows 惯例）。
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("确定").clicked() {
                    self.apply();
                    let _ = ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if ui.button("取消").clicked() {
                    // 丢弃未保存改动（SettingsApp 状态随窗口销毁）。
                    let _ = ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
                if ui.button("应用").clicked() {
                    self.apply();
                }
            });
        });
        ui.add_space(4.0);
    }

    /// 按当前标签渲染内容。
    fn tab_content(&mut self, ui: &mut egui::Ui) {
        ui.add_space(8.0);
        match self.tab {
            Tab::Common => {
                ui.heading("常用");
                ui.add_space(4.0);
                ui.small("（暂无设置项）");
            }
            Tab::Keymap => self.keymap_tab(ui),
            Tab::Appearance => self.appearance_tab(ui),
            Tab::Dict => self.dict_tab(ui),
            Tab::Advanced => self.advanced_tab(ui),
            #[cfg(any(debug_assertions, feature = "dev"))]
            Tab::Dev => self.dev_tab(ui),
        }
    }

    /// 按键：键位自定义（M7 灰置占位）。
    fn keymap_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("按键");
        ui.add_space(4.0);
        ui.add_enabled_ui(false, |ui| {
            ui.label("键位自定义（M7 开放）");
            let _ = ui.button("翻页键…");
            let _ = ui.button("候选移动键…");
            ui.small("（规划中）");
        });
    }

    /// 外观：候选窗主题。
    fn appearance_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("外观");
        ui.add_space(4.0);
        ui.label("候选窗主题");
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.theme, "light".to_string(), "浅色");
            ui.radio_value(&mut self.theme, "dark".to_string(), "深色");
        });
        ui.small("更改在点击「确定」或「应用」后生效。");
    }

    /// 词库：用户库列表 + 清除全部（暂挂到确定/应用）。
    fn dict_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("用户库管理");
        ui.add_space(4.0);
        let (cover, block, lines) = self.user_dict_snapshot();
        ui.label(format!("覆盖/自造词 {cover} 条 · 屏蔽 {block} 条"));
        if cover + block > 0 {
            egui::ScrollArea::vertical()
                .max_height(420.0)
                .show(ui, |ui| {
                    for line in &lines {
                        ui.monospace(line);
                    }
                });
        } else {
            ui.small("（空用户库）");
        }
        ui.add_space(8.0);
        if self.pending_clear {
            ui.colored_label(
                egui::Color32::from_rgb(0xC0, 0x80, 0x00),
                "已标记清除：点「确定」或「应用」后生效；点「取消」放弃。",
            );
        }
        if self.confirm_clear {
            ui.horizontal(|ui| {
                ui.label("确认清除全部用户库？");
                if ui.button("确认清除").clicked() {
                    self.pending_clear = true;
                    self.confirm_clear = false;
                    self.status = "已标记清除全部用户库（确定/应用后生效）".into();
                }
                if ui.button("取消").clicked() {
                    self.confirm_clear = false;
                }
            });
        } else if ui.button("清除全部").clicked() {
            self.confirm_clear = true;
        }
    }

    /// 高级：按键直通名单。
    fn advanced_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("高级");
        ui.add_space(4.0);
        ui.label("按键直通应用（每行一个 exe 名，游戏场景输入法透明）：");
        ui.add(
            egui::TextEdit::multiline(&mut self.passthrough)
                .desired_rows(8)
                .desired_width(500.0)
                .hint_text("例如 notepad.exe"),
        );
        ui.small("命中进程 TSF 层全部按键放行（不建会话、无候选窗/预编辑）。");
    }

    /// 开发者：清除日志（仅 dev 构建）。
    #[cfg(any(debug_assertions, feature = "dev"))]
    fn dev_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("开发者");
        ui.add_space(4.0);
        ui.label("清除 %TEMP% 下的 iuv 日志（daemon / tsf / script / cleanup）：");
        ui.add_space(4.0);
        if ui.button("清除日志").clicked() {
            self.log_clear = Some(crate::log::clear_logs());
        }
        if let Some((ok, fail)) = self.log_clear {
            ui.add_space(4.0);
            if fail == 0 {
                ui.colored_label(
                    egui::Color32::from_rgb(0x20, 0x80, 0x40),
                    format!("已清除 {ok} 个日志文件"),
                );
            } else {
                ui.colored_label(
                    egui::Color32::from_rgb(0xC0, 0x80, 0x00),
                    format!("已清除 {ok} 个；{fail} 个被占用（进程正在写入）"),
                );
            }
        }
    }

    /// 用户库列表 + 清除全部。
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

    /// 确定/应用：写配置 + 广播 + （若暂挂清除）清空用户库。
    /// 用户库清除必须在此落盘：设置页不走管道，若只置 dirty，注销时 daemon 被硬杀
    /// 不触发退出强写盘 → 磁盘旧库残留、下次登录复活（2026-08-18 实测复活 bug）。
    fn apply(&mut self) {
        let mut msgs: Vec<String> = Vec::new();

        let theme = if self.theme == "dark" { "dark" } else { "light" };
        let apps: Vec<String> = self
            .passthrough
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect();
        match config::save_config(&theme, &apps) {
            Ok(()) => {
                {
                    let mut c = self.state.config.lock().unwrap_or_else(|p| p.into_inner());
                    c.theme = theme.to_string();
                    c.passthrough_apps = apps;
                }
                self.state.bump_config_epoch();
                msgs.push("配置已保存并广播 config_epoch（会话进程检测后重载）".into());
                log::log_line("[settings] 配置已保存 + config_epoch 已广播");
            }
            Err(e) => {
                msgs.push(format!("配置保存失败：{e}"));
                log::log_line(&format!("[settings] 配置保存失败: {e}"));
            }
        }

        if self.pending_clear {
            self.pending_clear = false;
            {
                let mut dict = self.state.dict.lock().unwrap_or_else(|p| p.into_inner());
                *dict = UserDict::empty();
            }
            self.state.publish();
            self.state.flush_now();
            msgs.push("已清除全部用户库（已落盘）".into());
        }

        self.status = msgs.join("；");
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

    /// 绘制 UI（root Ui 无外边距；多面板自上而下）。
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::Panel::top("tabs").show(ui, |ui| {
            self.tab_bar(ui);
        });
        egui::Panel::bottom("actions").show(ui, |ui| {
            self.action_bar(ui);
        });
        egui::CentralPanel::default().show(ui, |ui| {
            self.tab_content(ui);
        });
    }

    fn on_exit(&mut self) {
        *self.state.settings_ctx.lock().unwrap_or_else(|p| p.into_inner()) = None;
        log::log_line("[settings] 设置窗口已关闭");
    }
}
