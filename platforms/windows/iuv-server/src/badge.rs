//! 服务端自绘状态徽标（57 号）：`中`/`英`/`A` 三态光标处徽标，对齐小狼毫观感。
//!
//! 持有者 = 本进程（TSF 薄客户端只报信息：按键经 `Key`/`RawKey`、四态经
//! `ImeState`、光标经 `CaretMoved`——弹/隐/淡出决策全在连接层 [`crate::lib`]，
//! 本模块只是执行者：渲染（[`iuv_ui::render_badge`]）+ 定位（caret 下沿）+
//! 自动淡出计时。
//!
//! 结构 = [`BadgeHandle`]（连接持有的命令发送端）+ 每连接一个 UI 线程
//! （命令循环 + 消息泵；窗口建在本线程满足 Win32 线程亲和）——与
//! [`crate::candwin`] 同款模式（WakeEvent 唤醒 / ULW / UpdateLayeredWindow
//! per-pixel alpha / 不抢焦点）。徽标不接收任何鼠标交互（命中等一概穿透）。

use std::mem::size_of;
use std::sync::mpsc::{self, Sender, TryRecvError};
use std::sync::Arc;

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::Foundation::{WAIT_EVENT, WAIT_OBJECT_0};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, KillTimer, MsgWaitForMultipleObjectsEx, PeekMessageW, SetTimer, ShowWindow,
    TranslateMessage, HTTRANSPARENT, MA_NOACTIVATE, MSG, MWMO_INPUTAVAILABLE, PM_REMOVE,
    QS_ALLINPUT, SW_HIDE, SW_SHOWNA, WM_ERASEBKGND, WM_MOUSEACTIVATE, WM_NCHITTEST, WM_PAINT,
    WM_TIMER,
};

use iuv_ui::{render_badge, CaretRect, TextRenderer, Theme};

use crate::candwin::WakeEvent;

const CLASS_NAME: PCWSTR = w!("IuvServerBadgeWindow");

/// 徽标自动淡出时长（ms）：对齐主流输入法状态提示节奏。决策层收到任意按键
/// 会提前隐藏，本计时只是无按键时的兜底自隐。
const AUTO_HIDE_MS: u32 = 1200;
const TIMER_ID: usize = 1;
/// caret 下沿与徽标的间隙（px 物理；与候选窗 CARET_GAP 同值）。
const CARET_GAP: i32 = 2;

/// 徽标命令（连接线程 → UI 线程；latest-wins——每条命令自足，按序应用最后一条即最新）。
pub(crate) enum BadgeCmd {
    /// 弹出：glyph（`中`/`英`/`A`）+ 锚点。重复 Show = 换内容/换位/重置淡出计时。
    Show {
        glyph: &'static str,
        caret: CaretRect,
    },
    Hide,
}

/// 连接持有的徽标句柄（clone 廉价；全部 sender drop → UI 线程退出并销毁窗口）。
pub(crate) struct BadgeHandle {
    tx: Sender<BadgeCmd>,
    wake: Arc<WakeEvent>,
}

impl BadgeHandle {
    /// 启动 UI 线程（每连接一个；隐藏态零渲染开销）。
    pub(crate) fn spawn(theme: Theme) -> BadgeHandle {
        let (tx, rx) = mpsc::channel::<BadgeCmd>();
        let wake = Arc::new(WakeEvent::new());
        let spawned = std::thread::Builder::new()
            .name("iuv-server-badge".into())
            .spawn({
                let wake = wake.clone();
                move || run_ui_thread(rx, theme, wake)
            });
        if let Err(e) = spawned {
            iuv_win::logger::log_line(&format!("[badge] UI 线程创建失败（{e}）→ 该连接无徽标"));
        }
        BadgeHandle { tx, wake }
    }

    /// 发命令 + 唤醒 UI 线程（UI 线程已退出/未建 → 静默丢弃）。
    pub(crate) fn send(&self, cmd: BadgeCmd) {
        let _ = self.tx.send(cmd);
        self.wake.set();
    }
}

fn run_ui_thread(rx: mpsc::Receiver<BadgeCmd>, theme: Theme, wake: Arc<WakeEvent>) {
    let mut wnd = ServerBadge::new(theme);
    // 建窗 + 字体渲染器预热在连接建立时完成（同 candwin：惰性首显会把字体库
    // 装配压进切换关键路径）。
    wnd.ensure_window();
    loop {
        let mut disconnected = false;
        loop {
            match rx.try_recv() {
                Ok(cmd) => match cmd {
                    BadgeCmd::Show { glyph, caret } => wnd.show(glyph, caret),
                    BadgeCmd::Hide => wnd.hide(),
                },
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        if disconnected {
            wnd.hide();
            return;
        }
        // 等唤醒（命令事件 / 窗口消息——WM_TIMER 淡出经 PeekMessage 分发）。
        // SAFETY: 事件句柄 Arc 存活；单事件等待。
        let w = unsafe {
            MsgWaitForMultipleObjectsEx(Some(&[wake.0]), u32::MAX, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
        };
        if w == WAIT_EVENT(WAIT_OBJECT_0.0 + 1) {
            // SAFETY: 本线程创建的窗口；标准消息泵。
            unsafe {
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
    }
}

/// ULW 自绘状态徽标（服务端版）。
struct ServerBadge {
    layered: iuv_win::LayeredWindow,
    visible: bool,
    theme: Theme,
    text: Option<TextRenderer>,
    ulw: iuv_win::UlwSurface,
    /// 本帧 DPI（caret 所在显示器；0 = 未知 → 96）。
    dpi: u32,
}

impl ServerBadge {
    fn new(theme: Theme) -> Self {
        ServerBadge {
            layered: iuv_win::LayeredWindow::new(),
            visible: false,
            theme,
            text: None,
            ulw: iuv_win::UlwSurface::new(),
            dpi: 0,
        }
    }

    fn scale(&self) -> f32 {
        (if self.dpi == 0 { 96 } else { self.dpi }) as f32 / 96.0
    }

    fn ensure_window(&mut self) {
        if self.layered.is_created() {
            return;
        }
        // SAFETY: TOPMOST|TOOLWINDOW|NOACTIVATE 保证置顶且不抢焦点；
        // LAYERED = per-pixel alpha 合成（UpdateLayeredWindow 前置条件）；
        // TRANSPARENT = 整窗点击穿透（徽标纯展示，绝不拦输入）。
        let outer = self as *mut Self;
        self.layered.create(
            CLASS_NAME,
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(
                windows::Win32::UI::WindowsAndMessaging::WS_EX_TOPMOST.0
                    | windows::Win32::UI::WindowsAndMessaging::WS_EX_TOOLWINDOW.0
                    | windows::Win32::UI::WindowsAndMessaging::WS_EX_NOACTIVATE.0
                    | windows::Win32::UI::WindowsAndMessaging::WS_EX_LAYERED.0
                    | windows::Win32::UI::WindowsAndMessaging::WS_EX_TRANSPARENT.0,
            ),
            wnd_proc,
            outer,
            "server-badge",
        );
        if self.layered.is_created() {
            self.text = Some(TextRenderer::new());
        }
    }

    fn show(&mut self, glyph: &'static str, caret: CaretRect) {
        if self.layered.hwnd.is_invalid() {
            self.ensure_window();
            if self.layered.hwnd.is_invalid() {
                return; // 建窗失败：静默降级
            }
        }
        self.dpi = dpi_for_caret(caret);
        // 先取标量再借渲染器：render_badge 同时要 &theme 与 &mut TextRenderer。
        let (theme, scale) = (self.theme, self.scale());
        let surf = {
            let text = match self.text.as_mut() {
                Some(t) => t,
                None => return,
            };
            render_badge(glyph, &theme, scale, text)
        };
        if surf.w == 0 || surf.h == 0 {
            return;
        }
        // 定位：caret 下沿 + GAP；越界内收到 caret 所在显示器工作区。
        let area = work_area_for_caret(caret);
        let (w, h) = (surf.w as i32, surf.h as i32);
        let x = caret.x.clamp(area.left, (area.right - w).max(area.left));
        let below = caret.y + caret.h + CARET_GAP;
        let y = if below + h <= area.bottom {
            below
        } else {
            // 下方放不下 → 光标上方（贴近光标优先于贴边）
            (caret.y - h).max(area.top)
        };
        iuv_win::logger::log_line(&format!(
            "[badge] show：glyph={glyph} 锚点 ({},{}) → 窗口 ({x},{y})",
            caret.x, caret.y
        ));
        // SAFETY: ULW 上传像素 + 定位一次性完成。
        let _ = self
            .ulw
            .upload(self.layered.hwnd, &surf, x, y, w, h, "[server-badge]");
        // SAFETY: SW_SHOWNA 显示但不激活。
        let _ = unsafe { ShowWindow(self.layered.hwnd, SW_SHOWNA) };
        self.visible = true;
        // 重置淡出计时（重复 Show = 重新计时）。
        // SAFETY: hwnd 有效；SetTimer 仅配额限制，失败则无兜底自隐（决策层 Key 隐仍有效）。
        unsafe {
            let _ = SetTimer(Some(self.layered.hwnd), TIMER_ID, AUTO_HIDE_MS, None);
        }
    }

    fn hide(&mut self) {
        if self.visible {
            iuv_win::logger::log_line("[badge] hide");
        }
        self.visible = false;
        if !self.layered.hwnd.is_invalid() {
            // SAFETY: hwnd 有效； KillTimer 无定时器时无害。
            unsafe {
                let _ = KillTimer(Some(self.layered.hwnd), TIMER_ID);
            }
            // SAFETY: 隐藏徽标窗。
            let _ = unsafe { ShowWindow(self.layered.hwnd, SW_HIDE) };
        }
    }
}

/// caret 所在显示器有效 DPI（进程需 PMv2；查询失败回退 96）。与 candwin 同源
///（单点收敛候选：两处各 ~20 行纯查询，抽象收益低，2026-10-16 收敛判断保留）。
fn dpi_for_caret(caret: CaretRect) -> u32 {
    // SAFETY: MonitorFromPoint/GetDpiForMonitor 纯查询，无资源。
    let monitor = unsafe {
        MonitorFromPoint(
            POINT {
                x: caret.x,
                y: caret.y,
            },
            MONITOR_DEFAULTTONEAREST,
        )
    };
    let mut dpi = 0u32;
    // SAFETY: 输出指针有效（windows 0.62 签名带第四个 *mut u32 reserved 位）。
    if unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut 0u32 as *mut u32) }
        .is_err()
        || dpi == 0
    {
        return 96;
    }
    dpi
}

/// caret 所在显示器工作区（失败兜底近乎全屏）。
fn work_area_for_caret(caret: CaretRect) -> iuv_ui::layout::Area {
    // SAFETY: MonitorFromPoint/GetMonitorInfoW 纯查询；输出缓冲已初始化。
    let monitor = unsafe {
        MonitorFromPoint(
            POINT {
                x: caret.x,
                y: caret.y,
            },
            MONITOR_DEFAULTTONEAREST,
        )
    };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        iuv_ui::layout::Area {
            left: info.rcWork.left,
            top: info.rcWork.top,
            right: info.rcWork.right,
            bottom: info.rcWork.bottom,
        }
    } else {
        iuv_ui::layout::Area {
            left: 0,
            top: 0,
            right: 32767,
            bottom: 32767,
        }
    }
}

/// 类窗口过程：徽标纯展示——只处理自隐计时与穿透，无任何鼠标交互。
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        // SAFETY: BeginPaint 仅允许在 WM_PAINT 内调用；ULW 内容不走窗口 DC。
        WM_PAINT => {
            let mut ps = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
            let hdc = windows::Win32::Graphics::Gdi::BeginPaint(hwnd, &mut ps);
            if !hdc.is_invalid() {
                let _ = windows::Win32::Graphics::Gdi::EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        // 整窗穿透（WS_EX_TRANSPARENT 已覆盖，此为双保险）。
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        // 淡出兜底自隐：决策层的显式 Hide 会 KillTimer，本路径只在无按键超时时走。
        WM_TIMER => {
            if wparam.0 == TIMER_ID {
                if let Some(wnd) =
                    unsafe { iuv_win::LayeredWindow::get_self_mut::<ServerBadge>(hwnd) }
                {
                    wnd.hide();
                }
            }
            LRESULT(0)
        }
        _ => iuv_win::LayeredWindow::default_wnd_proc(hwnd, msg, wparam, lparam),
    }
}
