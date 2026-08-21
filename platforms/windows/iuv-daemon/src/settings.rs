//! 设置窗口（egui/eframe，**主线程**运行）。
//!
//! winit 事件循环只能在进程主线程创建（独立线程会 panic，实测 2026-08-17），
//! 故守护进程主线程 = eframe 事件循环宿主：轮询 `open_settings` 标志，
//! 语言栏菜单「设置」（管道 `OpenSettings`）置位后主线程 `run_settings` 弹窗
//! （阻塞至关窗，关窗后继续后台常驻轮询）。管道/共享段在独立线程，不受影响。
//!
//! 界面（25-settings-tabs.md）：固定 640×480 不可缩放、标题栏无最大化；
//! 多标签页（常用/按键/外观/词库/高级/开发者）+ 底部「确定/取消/应用」。
//! 设置项（确定/应用 → 写 config.json → bump config_epoch 广播给会话进程）：
//! 常用=新 TSF 实例初始状态（模式/标点/宽度/字形）+ 每页候选数下拉、外观=主题（浅色/深色）+ 候选窗布局（竖排/横排）、
//! 高级=按键直通名单（passthrough_apps）+ 候选自绘应用（candidate_owner_apps）、词库=用户库管理（列表 + 清除全部，暂挂到确定/应用）、
//! 按键=键位自定义（灰置占位，M7）、开发者（仅 dev 构建）=清除日志。绝不 panic：
//! run_settings 包 `catch_unwind`。

use std::sync::atomic::Ordering;
use std::sync::Arc;

use eframe::egui;
use iuv_data::UserDict;
use windows::core::PCWSTR;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, IsIconic, SetForegroundWindow, ShowWindow, SW_RESTORE,
};

use crate::config::{self, DaemonConfig};
use crate::log;
use crate::state::DaemonState;

/// 设置窗标题（eframe viewport 标题；`FindWindowW` 按此查找，两处必须一致）。
const SETTINGS_TITLE: &str = "iuv 设置";

/// 把已存在的设置窗还原/置前（齿轮重复点击）：最小化 → `SW_RESTORE`（还原+激活
/// 一步到位）；非最小化 → `SetForegroundWindow` 置前。纯 Win32、**不依赖 egui 帧
/// 循环**——最小化窗口无 WM_PAINT → winit 不派发 RedrawRequested → logic()/
/// ViewportCommand 停摆（2026-08-22 实测五次点击零反应）。任务栏点击还原窗口走
/// 的就是同款系统路径。跨线程合法（托盘应用标准手法）；前台锁不拦——工具栏是
/// 本进程窗口、刚收过点击。
/// 返回是否找到窗口（首开的字体注入间隙 ~300ms 内可能尚未建窗，调用方记日志忽略）。
pub fn focus_existing_window() -> bool {
    let wide: Vec<u16> = SETTINGS_TITLE.encode_utf16().chain(Some(0)).collect();
    // SAFETY: 标题为 NUL 结尾宽字符串；FindWindowW 纯查询。
    let hwnd = unsafe { FindWindowW(PCWSTR::null(), PCWSTR(wide.as_ptr())) }.unwrap_or_default();
    if hwnd.is_invalid() {
        return false;
    }
    // SAFETY: IsIconic 纯查询。
    if unsafe { IsIconic(hwnd) }.as_bool() {
        // SAFETY: ShowWindow 投递显示状态变更给属主线程序列化执行；SW_RESTORE 只负责
        // 翻最小化状态——跨线程调用时其隐式"激活"可能被静默跳过（实测还原后仍被
        // 前台窗口压住），置前交给下面的 SetForegroundWindow。两次调用均为同步系统
        // 调用（状态变更在返回前完成，不依赖属主线程序列化通知），顺序安全。
        unsafe {
            let _ = ShowWindow(hwnd, SW_RESTORE);
        }
    }
    // 无条件置前+聚焦（非最小化路径同样需要：被遮挡时拉到最前面）。
    // SAFETY: 本进程刚收过工具栏点击，前台锁允许设前台。
    unsafe {
        let _ = SetForegroundWindow(hwnd);
    }
    true
}

/// eframe 窗口主体（阻塞直到窗口关闭；主线程调用）。返回 Ok(()) = 正常关闭。
pub fn run_settings(state: &Arc<DaemonState>) -> Result<(), String> {
    const WIDTH: f32 = 640.0;
    const HEIGHT: f32 = 480.0;

    let options = eframe::NativeOptions {

        viewport: egui::ViewportBuilder::default()
            .with_inner_size([WIDTH, HEIGHT])
            .with_min_inner_size([WIDTH, HEIGHT])
            .with_max_inner_size([WIDTH, HEIGHT]) // 锁死 640×480
            .with_resizable(false) // 禁最大化/拉伸
            .with_maximize_button(false) // 标题栏只剩 最小化 + 关闭
            .with_title(SETTINGS_TITLE),
        ..Default::default()
    };
    let state = state.clone();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        eframe::run_native(
            SETTINGS_TITLE,
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

/// 日志模块目录（tag, 说明）——开发者标签开关（26-log-modules.md）。
/// TSF 侧：uielem/key/commit/caret/candwin/menuwin/daemon；
/// daemon 侧：main/pipe/settings/state。tag 须与 log_line 消息前缀 `[tag]` 一致。
#[cfg(any(debug_assertions, feature = "dev"))]
const LOG_MODULES: &[(&str, &str)] = &[
    ("uielem", "TSF 候选 UIElement 桥（最高频）"),
    ("key", "TSF 按键记录（每键一行）"),
    ("commit", "上屏记录"),
    ("caret", "光标量取"),
    ("candwin", "候选窗窗口层"),
    ("menuwin", "语言栏右键菜单"),
    ("punct", "中文标点直接上屏"),
    ("daemon", "TSF 侧 daemon_client"),
    ("main", "守护进程主循环"),
    ("pipe", "守护进程管道"),
    ("settings", "守护进程设置页"),
    ("state", "守护进程状态"),
];

/// 设置页 UI 状态。
struct SettingsApp {
    state: Arc<DaemonState>,
    /// 当前标签页。
    tab: Tab,
    /// 主题单选值（"light"/"dark"）。
    theme: String,
    /// 候选窗布局单选值（"vertical"/"horizontal"）。
    orientation: String,
    /// 每页候选数（常用页下拉，[5,6,7,8,9]）。
    page_size: usize,
    /// 新 TSF 实例初始状态（中/英、半/全角、简/繁、标点风格；复用 iuv-core 类型）。
    initial: iuv_core::ImeState,
    /// 直通名单文本编辑（每行一个 exe 名）。
    passthrough: String,
    /// 候选自绘名单文本编辑（每行一个 exe 名）。
    candidate_owner: String,
    /// 禁用日志模块集（denylist，勾掉某模块即加入；默认空 = 全记录）。
    disabled_log: Vec<String>,
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
            orientation: cfg.candidate_orientation,
            page_size: cfg.page_size,
            initial: cfg.initial_state,
            passthrough: cfg.passthrough_apps.join("\n"),
            candidate_owner: cfg.candidate_owner_apps.join("\n"),
            disabled_log: cfg.disabled_log_modules.clone(),
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
                ui.label("新 TSF 实例初始状态");
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    ui.label("模式");
                    ui.radio_value(&mut self.initial.mode, iuv_core::InitialMode::Chinese, "中文");
                    ui.radio_value(&mut self.initial.mode, iuv_core::InitialMode::English, "英文");
                });
                let mut punct_en = self.initial.punct == iuv_core::PunctMode::English;
                if ui.checkbox(&mut punct_en, "中文状态使用英文标点").changed() {
                    self.initial.punct = if punct_en {
                        iuv_core::PunctMode::English
                    } else {
                        iuv_core::PunctMode::Chinese
                    };
                }
                ui.horizontal(|ui| {
                    ui.label("宽度");
                    ui.radio_value(&mut self.initial.width, iuv_core::WidthMode::Half, "半角");
                    ui.radio_value(&mut self.initial.width, iuv_core::WidthMode::Full, "全角");
                });
                ui.horizontal(|ui| {
                    ui.label("字形");
                    ui.radio_value(
                        &mut self.initial.script,
                        iuv_core::ScriptMode::Simplified,
                        "简体",
                    );
                    ui.radio_value(
                        &mut self.initial.script,
                        iuv_core::ScriptMode::Traditional,
                        "繁体",
                    );
                });
                ui.small("「初始状态」定义切换/新开软件时输入法的默认态：模式（中/英）、标点、半角/全角与简体/繁体均已生效；");
                ui.small("繁体 = 简体词库 + 运行时简→繁转换（s2t 通用繁体，数据文件 iuv.opencc 缺失时降级简体）。");
                ui.add_space(12.0);
                ui.separator();
                ui.add_space(4.0);
                ui.label("每页候选数量");
                egui::ComboBox::from_id_salt("page_size")
                    .selected_text(self.page_size.to_string())
                    .show_ui(ui, |ui| {
                        for n in [5usize, 6, 7, 8, 9] {
                            ui.selectable_value(&mut self.page_size, n, n.to_string());
                        }
                    });
                ui.small("建议 ≤9 保证数字键可全选当前页。");
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

    /// 外观：候选窗主题 + 布局方向。
    fn appearance_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("外观");
        ui.add_space(4.0);
        ui.label("候选窗主题");
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.theme, "light".to_string(), "浅色");
            ui.radio_value(&mut self.theme, "dark".to_string(), "深色");
        });
        ui.add_space(8.0);
        ui.label("候选窗布局");
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.orientation, "vertical".to_string(), "竖排（一列）");
            ui.radio_value(&mut self.orientation, "horizontal".to_string(), "横排（单行）");
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

    /// 高级：按键直通名单 + 候选自绘应用。
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
        ui.add_space(10.0);
        ui.label("候选自绘应用（每行一个 exe 名，命中则 iuv 不绘制候选窗，由应用自绘）：");
        ui.add(
            egui::TextEdit::multiline(&mut self.candidate_owner)
                .desired_rows(8)
                .desired_width(500.0)
                .hint_text("例如 wow.exe"),
        );
        ui.small("命中进程 iuv 抑制自绘候选窗（游戏内自绘候选栏场景）；默认预置 wow.exe，其他应用自行添加。");
    }

    /// 开发者：清除日志 + 日志模块开关（仅 dev 构建）。
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

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(4.0);
        ui.label("日志模块（勾选 = 记录该模块；改动点「确定/应用」生效并热载）");
        ui.add_space(4.0);
        egui::ScrollArea::vertical()
            .max_height(300.0)
            .show(ui, |ui| {
                for (tag, desc) in LOG_MODULES {
                    let mut enabled = !self.disabled_log.iter().any(|m| m == tag);
                    if ui.checkbox(&mut enabled, format!("{tag} — {desc}")).changed() {
                        if enabled {
                            self.disabled_log.retain(|m| m != tag);
                        } else if !self.disabled_log.iter().any(|m| m == tag) {
                            self.disabled_log.push((*tag).to_string());
                        }
                    }
                }
            });
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
        let orientation = if self.orientation == "horizontal" {
            "horizontal"
        } else {
            "vertical"
        };
        let apps: Vec<String> = self
            .passthrough
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect();
        let cand_owners: Vec<String> = self
            .candidate_owner
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .map(String::from)
            .collect();
        // 日志模块禁用集：先本进程生效（daemon 自身 log_line），再随 config_epoch 热载到 TSF。
        log::set_log_modules_disabled(&self.disabled_log);
        match config::save_config(&DaemonConfig {
            theme: theme.to_string(),
            candidate_orientation: orientation.to_string(),
            page_size: self.page_size,
            initial_state: self.initial.clone(),
            passthrough_apps: apps.clone(),
            candidate_owner_apps: cand_owners.clone(),
            disabled_log_modules: self.disabled_log.clone(),
        }) {
            Ok(()) => {
                {
                    let mut c = self.state.config.lock().unwrap_or_else(|p| p.into_inner());
                    c.theme = theme.to_string();
                    c.candidate_orientation = orientation.to_string();
                    c.page_size = self.page_size;
                    c.initial_state = self.initial.clone();
                    c.passthrough_apps = apps;
                    c.candidate_owner_apps = cand_owners;
                    c.disabled_log_modules = self.disabled_log.clone();
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
    /// 每帧先跑（窗口隐藏时也调用）：检测退出信号。
    /// 聚焦不走这里——最小化窗口无 WM_PAINT → 无帧 → 本函数停摆，改走
    /// `focus_existing_window()` 的 Win32 直操路径。
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
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
        log::log_line("[settings] 设置窗口已关闭");
    }
}
