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

use std::mem::size_of;
use std::sync::atomic::Ordering;
use std::sync::Arc;

use eframe::egui;
use iuv_data::UserDict;
use windows::core::PCWSTR;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetWindowRect, IsIconic, SetForegroundWindow, SetWindowPos, ShowWindow,
    SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SW_RESTORE,
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

/// 把已存在的设置窗挪到所在显示器工作区正中（每次打开调用一次，creator 回调时机：
/// 原生窗口已建、首帧未画 → 零闪烁）。物理像素全程运算（进程 PMv2，GetWindowRect/
/// 工作区均为物理值）天然 DPI 正确；基准 = 窗口实际落地的显示器（多屏跟随系统放置，
/// 不写死主屏）；工作区而非整屏 → 下沿不被任务栏压住。egui 0.36 ViewportCommand
/// 无居中命令，OuterPosition 是逻辑坐标还得换算 DPI——故走 Win32 直操（同聚焦套路）。
/// 找不到窗口静默返回（与聚焦函数同款防御）。
fn center_window_on_screen() {
    let wide: Vec<u16> = SETTINGS_TITLE.encode_utf16().chain(Some(0)).collect();
    // SAFETY: 标题为 NUL 结尾宽字符串；FindWindowW 纯查询。
    let hwnd = unsafe { FindWindowW(PCWSTR::null(), PCWSTR(wide.as_ptr())) }.unwrap_or_default();
    if hwnd.is_invalid() {
        return;
    }
    // SAFETY: GetWindowRect 读当前矩形。
    let mut rc = RECT::default();
    if unsafe { GetWindowRect(hwnd, &mut rc) }.is_err() {
        return;
    }
    let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
    // SAFETY: MonitorFromWindow 纯查询；MONITORINFO.cbSize 按约定先填。
    let hmon = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let mut mi = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetMonitorInfoW(hmon, &mut mi) }.as_bool() {
        let x = mi.rcWork.left + ((mi.rcWork.right - mi.rcWork.left) - w) / 2;
        let y = mi.rcWork.top + ((mi.rcWork.bottom - mi.rcWork.top) - h) / 2;
        // SAFETY: SetWindowPos 仅移动（NOSIZE|NOZORDER|NOACTIVATE），不抢焦点不改层级。
        unsafe {
            let _ = SetWindowPos(
                hwnd,
                None,
                x,
                y,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }
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
                center_window_on_screen();
                // 视觉基调：圆角 6px、呼吸间距、选中色对齐候选窗高亮蓝 #0078D7（产品一致感）。
                cc.egui_ctx.all_styles_mut(|style| {
                    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
                    for w in [
                        &mut style.visuals.widgets.noninteractive,
                        &mut style.visuals.widgets.inactive,
                        &mut style.visuals.widgets.hovered,
                        &mut style.visuals.widgets.active,
                        &mut style.visuals.widgets.open,
                    ] {
                        w.corner_radius = 6.into();
                    }
                    style.visuals.selection.bg_fill =
                        egui::Color32::from_rgb(0x00, 0x78, 0xD7);
                    style.visuals.selection.stroke =
                        egui::Stroke::new(1.0, egui::Color32::WHITE);
                });
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

/// 录入目标：某个功能（会话或全局）× 主/备槽。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureTarget {
    Session(iuv_core::SessionAction, Slot),
    Global(iuv_core::GlobalAction, Slot),
}

/// 槽位（主/备）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Slot {
    Primary,
    Secondary,
}

impl Slot {
    fn name(self) -> &'static str {
        match self {
            Slot::Primary => "主",
            Slot::Secondary => "备",
        }
    }
}

/// 取两槽的 &mut Combo 位置。
fn slot_combo_mut<'a>(slot: &'a mut iuv_core::TwoSlot, which: Slot) -> &'a mut Option<iuv_core::Combo> {
    match which {
        Slot::Primary => &mut slot.primary,
        Slot::Secondary => &mut slot.secondary,
    }
}

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
    /// 快捷键映射（41-keymap-settings.md §3）：会话内 7 + 全局 6，各主/备两槽。
    /// 会话组 TSF 消费（config_epoch 热载）；全局组 daemon RegisterHotKey 消费。
    keymap: iuv_core::Keymap,
    /// 录入模式目标（游戏式捕捉）：正在等待用户按下组合键的功能槽位。
    /// 点击录入框置位 → 从 egui 事件流捕获（41-keymap-settings.md §10.2 方案 A）→
    /// 回填 → 复位。
    capture: Option<CaptureTarget>,
    /// 录入模式提示（捕获中显示在框内）。
    capturing: bool,
    /// 最近一次冲突/校验警告（录入回填时置位，显示红字）。
    keymap_warn: Option<String>,
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

/// 卡片式分组容器（设置页视觉基调）：圆角描边 + 微底色 + 内边距，
/// 组间留白由调用方统一 `add_space(10)`。内容自动占满可用宽度。
fn card<R>(ui: &mut egui::Ui, add_contents: impl FnOnce(&mut egui::Ui) -> R) -> R {
    egui::Frame::new()
        .fill(ui.visuals().faint_bg_color)
        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
        .corner_radius(6)
        .inner_margin(egui::Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add_contents(ui)
        })
        .inner
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
            keymap: cfg.keymap,
            capture: None,
            capturing: false,
            keymap_warn: None,
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
                let label = egui::RichText::new(tab.title()).size(17.0);
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
                card(ui, |ui| {
                    ui.strong("新 TSF 实例初始状态");
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        ui.label("模式");
                        ui.radio_value(
                            &mut self.initial.mode,
                            iuv_core::InitialMode::Chinese,
                            "中文",
                        );
                        ui.radio_value(
                            &mut self.initial.mode,
                            iuv_core::InitialMode::English,
                            "英文",
                        );
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
                        ui.radio_value(
                            &mut self.initial.width,
                            iuv_core::WidthMode::Half,
                            "半角",
                        );
                        ui.radio_value(
                            &mut self.initial.width,
                            iuv_core::WidthMode::Full,
                            "全角",
                        );
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
                });
                ui.add_space(10.0);
                card(ui, |ui| {
                    ui.strong("每页候选数量");
                    ui.add_space(2.0);
                    egui::ComboBox::from_id_salt("page_size")
                        .selected_text(self.page_size.to_string())
                        .show_ui(ui, |ui| {
                            for n in [5usize, 6, 7, 8, 9] {
                                ui.selectable_value(&mut self.page_size, n, n.to_string());
                            }
                        });
                    ui.small("建议 ≤9 保证数字键可全选当前页。");
                });
            }
            Tab::Keymap => self.keymap_tab(ui),
            Tab::Appearance => self.appearance_tab(ui),
            Tab::Dict => self.dict_tab(ui),
            Tab::Advanced => self.advanced_tab(ui),
            #[cfg(any(debug_assertions, feature = "dev"))]
            Tab::Dev => self.dev_tab(ui),
        }
    }

    /// 按键：键位自定义（41-keymap-settings.md §5，游戏式录入）。
    /// 两卡片：会话内（7 项，TSF 键 sink）+ 全局（6 项，daemon RegisterHotKey）。
    /// 每项主/备两槽；点击槽位录入框 → WH_KEYBOARD_LL 捕获组合键。
    /// 2026-08-28：包 ScrollArea——13 行内容超出 640×480 固定窗口，无滚动则全局
    /// 卡片被挤出可视区（此前靠 Ctrl+- 缩放 UI 才能看到）。
    fn keymap_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("按键");
        ui.add_space(4.0);
        ui.small("点击录入框后按下组合键（游戏式，支持 Alt/Ctrl/Shift/Win 组合）；Esc 取消、Backspace 清除该槽。");
        ui.add_space(4.0);
        if let Some(warn) = self.keymap_warn.clone() {
            ui.colored_label(egui::Color32::from_rgb(0xC0, 0x40, 0x40), warn);
            ui.add_space(4.0);
        }
        egui::ScrollArea::vertical()
            .id_salt("keymap_scroll")
            .max_height(ui.available_height() - 12.0)
            .show(ui, |ui| {
                // —— 会话内快捷键（TSF 键 sink）——
                card(ui, |ui| {
                    ui.strong("输入会话内");
                    ui.small("仅无修饰/Shift 组合；Alt 组合不会到达输入法会话（机制限制），Ctrl 组合让给应用。");
                    ui.add_space(6.0);
                    let mut clicked: Option<CaptureTarget> = None;
                    for (label, action) in self.session_actions() {
                        let target = CaptureTarget::Session(action, Slot::Primary);
                        self.capture_row(ui, label, target, &mut clicked);
                    }
                    if let Some(t) = clicked {
                        self.start_capture(t);
                    }
                });
                ui.add_space(10.0);

                // —— 全局热键（daemon）——
                card(ui, |ui| {
                    ui.strong("全局快捷键（任意软件生效）");
                    ui.small("普通软件做法（daemon RegisterHotKey），Alt/Ctrl 随便绑；必须含修饰键。");
                    ui.add_space(6.0);
                    let mut clicked: Option<CaptureTarget> = None;
                    for (label, action) in self.global_actions() {
                        let target = CaptureTarget::Global(action, Slot::Primary);
                        self.capture_row(ui, label, target, &mut clicked);
                    }
                    if let Some(t) = clicked {
                        self.start_capture(t);
                    }
                    ui.add_space(4.0);
                    if ui.button("恢复默认键位").clicked() {
                        self.keymap = iuv_core::Keymap::default();
                        self.keymap_warn = None;
                    }
                });
            });
    }

    /// 会话动作列表（展示顺序即 UI 顺序）。
    fn session_actions(&self) -> Vec<(&'static str, iuv_core::SessionAction)> {
        use iuv_core::SessionAction::*;
        vec![
            ("翻上一页", PagePrev),
            ("翻下一页", PageNext),
            ("候选前移", CandidatePrev),
            ("候选后移", CandidateNext),
            ("调权（与左侧候选交换）", SwapLeft),
            ("调权（与右侧候选交换）", SwapRight),
            ("隐藏候选", HideCandidate),
        ]
    }

    /// 全局动作列表。
    fn global_actions(&self) -> Vec<(&'static str, iuv_core::GlobalAction)> {
        use iuv_core::GlobalAction::*;
        vec![
            ("中英切换", ToggleMode),
            ("全角/半角", ToggleWidth),
            ("简体/繁体", ToggleScript),
            ("中文标点", TogglePunct),
            ("打开设置", OpenSettings),
            ("显示/隐藏工具栏", ToggleToolbar),
        ]
    }

    /// 会话动作 → 槽位可变引用。
    fn keymap_slot_session(&mut self, a: iuv_core::SessionAction) -> &mut iuv_core::TwoSlot {
        match a {
            iuv_core::SessionAction::PagePrev => &mut self.keymap.page_prev,
            iuv_core::SessionAction::PageNext => &mut self.keymap.page_next,
            iuv_core::SessionAction::CandidatePrev => &mut self.keymap.candidate_prev,
            iuv_core::SessionAction::CandidateNext => &mut self.keymap.candidate_next,
            iuv_core::SessionAction::SwapLeft => &mut self.keymap.swap_left,
            iuv_core::SessionAction::SwapRight => &mut self.keymap.swap_right,
            iuv_core::SessionAction::HideCandidate => &mut self.keymap.hide_candidate,
        }
    }

    /// 全局动作 → 槽位可变引用。
    fn keymap_slot_global(&mut self, a: iuv_core::GlobalAction) -> &mut iuv_core::TwoSlot {
        match a {
            iuv_core::GlobalAction::ToggleMode => &mut self.keymap.toggle_mode,
            iuv_core::GlobalAction::ToggleWidth => &mut self.keymap.toggle_width,
            iuv_core::GlobalAction::ToggleScript => &mut self.keymap.toggle_script,
            iuv_core::GlobalAction::TogglePunct => &mut self.keymap.toggle_punct,
            iuv_core::GlobalAction::OpenSettings => &mut self.keymap.open_settings,
            iuv_core::GlobalAction::ToggleToolbar => &mut self.keymap.toggle_toolbar,
        }
    }

    /// 一行：功能名 + [主录入框] [备录入框]。渲染后把点击目标写入 `clicked`。
    /// 槽位值先拷贝（避免 self.keymap 与 self.capture 的借用冲突）。
    fn capture_row(
        &mut self,
        ui: &mut egui::Ui,
        label: &str,
        target: CaptureTarget,
        clicked: &mut Option<CaptureTarget>,
    ) {
        // 拷贝当前两槽组合（展示用）
        let (primary, secondary) = self.slot_combos(target);
        let mut hit: Option<CaptureTarget> = None;
        ui.horizontal(|ui| {
            ui.add_sized([130.0, 20.0], egui::Label::new(label));
            self.capture_slot_button(ui, target, Slot::Primary, primary, &mut hit);
            self.capture_slot_button(ui, target, Slot::Secondary, secondary, &mut hit);
        });
        if let Some(t) = hit {
            *clicked = Some(t);
        }
        ui.add_space(2.0);
    }

    /// 读某功能两槽组合（拷贝值；不借用 self.keymap）。
    fn slot_combos(
        &self,
        target: CaptureTarget,
    ) -> (Option<iuv_core::Combo>, Option<iuv_core::Combo>) {
        let slot = match target {
            CaptureTarget::Session(a, _) => self.keymap.session_slot(a),
            CaptureTarget::Global(a, _) => self.keymap.global_slot(a),
        };
        (slot.primary, slot.secondary)
    }

    /// 单个槽位录入按钮：显示当前键位（空 = "未设置"）；点击写入 `hit`（主/备）。
    /// 槽位值 `combo` 为拷贝值，展示后释放。
    fn capture_slot_button(
        &mut self,
        ui: &mut egui::Ui,
        target: CaptureTarget,
        which: Slot,
        combo: Option<iuv_core::Combo>,
        hit: &mut Option<CaptureTarget>,
    ) {
        let target = match target {
            CaptureTarget::Session(a, _) => CaptureTarget::Session(a, which),
            CaptureTarget::Global(a, _) => CaptureTarget::Global(a, which),
        };
        let is_capturing_this = self.capture == Some(target) && self.capturing;
        let text = if is_capturing_this {
            "按下组合键…（Esc 取消 / Backspace 清除）".to_string()
        } else {
            match combo {
                Some(c) => format!("{}（{}）", c.name(), which.name()),
                None => "未设置".to_string(),
            }
        };
        let btn = egui::Button::new(
            egui::RichText::new(text).color(if is_capturing_this {
                egui::Color32::from_rgb(0x00, 0x78, 0xD7)
            } else if combo.is_some() {
                egui::Color32::from_rgb(0x20, 0x80, 0x40)
            } else {
                egui::Color32::GRAY
            }),
        )
        .min_size(egui::vec2(170.0, 24.0));
        if ui.add(btn).clicked() {
            *hit = Some(target);
        }
    }

    /// 进入录入模式：仅置位目标（方案 A——按键从 egui 事件流捕获，无需钩子）。
    fn start_capture(&mut self, target: CaptureTarget) {
        self.capture = Some(target);
        self.capturing = true;
        self.keymap_warn = None;
        log::log_line(&format!("[capture] 进入录入模式（等待组合键）"));
    }

    /// 每帧从 egui 事件流消费按键：capturing 态下遇到 pressed 按键 → 处理 → 回填。
    /// 在 logic() 帧首调用。设置窗有焦点时必然收到（用户录入时焦点必在设置窗）。
    fn poll_capture(&mut self, ctx: &egui::Context) {
        if !self.capturing {
            return;
        }
        let target = self.capture;
        let events: Vec<egui::Event> = ctx.input(|i| i.events.clone());
        for ev in events {
            let egui::Event::Key {
                key,
                modifiers,
                pressed,
                repeat,
                ..
            } = ev
            else {
                continue;
            };
            if !pressed || repeat {
                continue;
            }
            let Some(outcome) = crate::capture::process_key_event(key, &modifiers) else {
                continue; // 纯修饰键等，继续等
            };
            // 捕获完成：复位 + 回填
            self.capturing = false;
            self.capture = None;
            if let Some(target) = target {
                self.apply_capture(target, outcome);
            }
            ctx.request_repaint();
            return;
        }
    }

    /// 应用捕获结果到槽位（含校验/冲突检测）。
    fn apply_capture(&mut self, target: CaptureTarget, outcome: crate::capture::CaptureOutcome) {
        use crate::capture::CaptureOutcome;
        match outcome {
            CaptureOutcome::Cancel => {
                self.keymap_warn = None; // Esc 取消：槽位不变
            }
            CaptureOutcome::Clear => {
                // 清除该槽（主/备）
                let c = self.slot_combo_mut(target);
                *c = None;
                self.keymap_warn = None;
            }
            CaptureOutcome::Rejected(combo) => {
                // 钩子层已拒（纯字母无修饰等）：给 UI 提示，录入会话已结束（槽位不变）
                self.keymap_warn = Some(format!(
                    "「{}」不可用：纯字母键无修饰会被拼音输入吞掉，请按带修饰的组合键。",
                    combo
                ));
                log::log_line(&format!("[settings] 录入被拒：{combo}（纯字母无修饰）"));
            }
            CaptureOutcome::Captured(combo) => {
                // 校验
                if let Err(msg) = self.validate_combo(target, &combo) {
                    log::log_line(&format!("[settings] 校验拒绝：{msg}"));
                    self.keymap_warn = Some(msg);
                    return;
                }
                let c = self.slot_combo_mut(target);
                *c = Some(combo);
                self.keymap_warn = None;
                log::log_line(&format!(
                    "[settings] 录入成功：{:?} → {}",
                    target, combo
                ));
            }
        }
    }

    /// 目标槽位的可变 Combo 引用。
    fn slot_combo_mut(&mut self, target: CaptureTarget) -> &mut Option<iuv_core::Combo> {
        match target {
            CaptureTarget::Session(a, which) => {
                let slot = self.keymap_slot_session(a);
                slot_combo_mut(slot, which)
            }
            CaptureTarget::Global(a, which) => {
                let slot = self.keymap_slot_global(a);
                slot_combo_mut(slot, which)
            }
        }
    }

    /// 组合校验（会话/全局红线 + 跨功能冲突）。Err → 拒绝并给红字。
    fn validate_combo(
        &self,
        target: CaptureTarget,
        combo: &iuv_core::Combo,
    ) -> Result<(), String> {
        // 会话内红线
        if let CaptureTarget::Session(a, _) = target {
            let label = self.session_label(a);
            if combo.alt {
                return Err(format!(
                    "{label}：Alt 组合不会到达输入法会话（WM_SYSKEYDOWN 不进 TSF 键 sink），运行时无效。"
                ));
            }
            if combo.ctrl {
                return Err(format!("{label}：Ctrl 组合让位给应用（冲突大），会话快捷键不可用。"));
            }
            if combo.base_is_letter() {
                return Err(format!(
                    "{label}：字母键是拼音输入空间，不能作为会话快捷键。"
                ));
            }
        }
        // 全局红线
        if let CaptureTarget::Global(a, _) = target {
            let label = self.global_label(a);
            if !combo.has_modifier() {
                return Err(format!(
                    "{label}：全局热键必须含修饰键（否则全系统劫持字母/数字）。"
                ));
            }
            if combo.name() == "Ctrl+Space" {
                return Err(format!(
                    "{label}：Ctrl+Space 是系统「输入法/非输入法切换」热键，建议不要占用。"
                ));
            }
        }
        // 跨功能冲突（排除自身槽位——目标槽本身即将被替换）
        let is_target = |c: &iuv_core::Combo| match target {
            CaptureTarget::Session(a, w) => {
                let s = self.keymap.session_slot(a);
                match w {
                    Slot::Primary => s.primary.as_ref() == Some(c),
                    Slot::Secondary => s.secondary.as_ref() == Some(c),
                }
            }
            CaptureTarget::Global(a, w) => {
                let s = self.keymap.global_slot(a);
                match w {
                    Slot::Primary => s.primary.as_ref() == Some(c),
                    Slot::Secondary => s.secondary.as_ref() == Some(c),
                }
            }
        };
        // 会话与全局两表统一查
        let (label, occupied) = self.find_conflict(combo, &is_target);
        if let Some(occ) = occupied {
            return Err(format!(
                "{label}：{combo} 已被「{occ}」占用，请换一个组合。"
            ));
        }
        Ok(())
    }

    /// 全表冲突查找：返回 (当前功能名, 占用方功能名)。排除 is_target 命中的槽位
    /// （即被替换的旧值）。
    fn find_conflict(
        &self,
        combo: &iuv_core::Combo,
        is_target: &dyn Fn(&iuv_core::Combo) -> bool,
    ) -> (String, Option<String>) {
        for (label, action) in self.session_actions() {
            let slot = self.keymap.session_slot(action);
            for c in slot.iter() {
                if c == combo && !is_target(c) {
                    return (label.to_string(), Some(label.to_string()));
                }
            }
        }
        for (label, action) in self.global_actions() {
            let slot = self.keymap.global_slot(action);
            for c in slot.iter() {
                if c == combo && !is_target(c) {
                    return (label.to_string(), Some(label.to_string()));
                }
            }
        }
        (String::new(), None)
    }

    fn session_label(&self, a: iuv_core::SessionAction) -> String {
        self.session_actions()
            .into_iter()
            .find(|(_, x)| *x == a)
            .map(|(l, _)| l.to_string())
            .unwrap_or_else(|| "会话功能".into())
    }

    fn global_label(&self, a: iuv_core::GlobalAction) -> String {
        self.global_actions()
            .into_iter()
            .find(|(_, x)| *x == a)
            .map(|(l, _)| l.to_string())
            .unwrap_or_else(|| "全局功能".into())
    }

    /// 外观：候选窗主题 + 布局方向。
    fn appearance_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("外观");
        ui.add_space(4.0);
        card(ui, |ui| {
            ui.strong("候选窗主题");
            ui.horizontal(|ui| {
                ui.radio_value(&mut self.theme, "light".to_string(), "浅色");
                ui.radio_value(&mut self.theme, "dark".to_string(), "深色");
            });
        });
        ui.add_space(10.0);
        card(ui, |ui| {
            ui.strong("候选窗布局");
            ui.horizontal(|ui| {
                ui.radio_value(
                    &mut self.orientation,
                    "vertical".to_string(),
                    "竖排（一列）",
                );
                ui.radio_value(
                    &mut self.orientation,
                    "horizontal".to_string(),
                    "横排（单行）",
                );
            });
            ui.small("更改在点击「确定」或「应用」后生效。");
        });
    }

    /// 词库：用户库列表 + 清除全部（暂挂到确定/应用）。
    fn dict_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("用户库管理");
        ui.add_space(4.0);
        card(ui, |ui| {
            let (cover, block, lines) = self.user_dict_snapshot();
            ui.label(format!("覆盖/自造词 {cover} 条 · 屏蔽 {block} 条"));
            if cover + block > 0 {
                egui::ScrollArea::vertical()
                    .max_height(260.0)
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
        });
    }

    /// 高级：按键直通名单 + 候选自绘应用（左右双卡片，各带「恢复默认」回填按钮）。
    fn advanced_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("高级");
        ui.add_space(4.0);
        ui.columns(2, |cols| {
            // 左：按键直通（纯单机游戏整进程隐身——该进程内无法输中文）
            card(&mut cols[0], |ui| {
                ui.strong("按键直通应用");
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .max_height(170.0)
                    .id_salt("passthrough_scroll")
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut self.passthrough)
                                .desired_width(ui.available_width())
                                .hint_text("例如 Cyberpunk2077.exe"),
                        );
                    });
                ui.small("命中进程全部按键放行（不建会话、无候选窗）——该进程内无法输中文。");
                ui.add_space(2.0);
                if ui.button("恢复默认名单").clicked() {
                    self.passthrough =
                        crate::config::DEFAULT_PASSTHROUGH_APPS.join("\n");
                }
                ui.small("默认 = 近五年 3A 单机大作");
            });
            // 右：候选自绘（要打中文的游戏——只让出候选窗绘制权）
            card(&mut cols[1], |ui| {
                ui.strong("候选自绘应用");
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .max_height(170.0)
                    .id_salt("candidate_owner_scroll")
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut self.candidate_owner)
                                .desired_width(ui.available_width())
                                .hint_text("例如 wow.exe"),
                        );
                    });
                ui.small("命中进程 iuv 不绘制候选窗（游戏自带候选栏场景），数据仍供其拉取。");
                ui.add_space(2.0);
                if ui.button("恢复默认名单").clicked() {
                    self.candidate_owner =
                        crate::config::DEFAULT_CANDIDATE_OWNER_APPS.join("\n");
                }
                ui.small("默认 = 预置知名游戏");
            });
        });
    }

    /// 开发者：清除日志 + 日志模块开关（仅 dev 构建）。
    #[cfg(any(debug_assertions, feature = "dev"))]
    fn dev_tab(&mut self, ui: &mut egui::Ui) {
        ui.heading("开发者");
        ui.add_space(4.0);
        card(ui, |ui| {
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
                .max_height(200.0)
                .show(ui, |ui| {
                    for (tag, desc) in LOG_MODULES {
                        let mut enabled =
                            !self.disabled_log.iter().any(|m| m == tag);
                        if ui.checkbox(&mut enabled, format!("{tag} — {desc}")).changed() {
                            if enabled {
                                self.disabled_log.retain(|m| m != tag);
                            } else if !self.disabled_log.iter().any(|m| m == tag) {
                                self.disabled_log.push((*tag).to_string());
                            }
                        }
                    }
                });
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
            keymap: self.keymap.clone(),
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
                    c.keymap = self.keymap.clone();
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
        // 录入轮询：捕获完成 → 校验回填（41-keymap-settings.md §5）。
        self.poll_capture(ctx);
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
        // 窗口关闭时若仍在录入：复位（方案 A 无钩子需卸载）
        self.capturing = false;
        self.capture = None;
        log::log_line("[settings] 设置窗口已关闭");
    }
}
