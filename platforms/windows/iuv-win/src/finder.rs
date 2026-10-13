//! 捕捉态（54-2 Step 3）：取景框 + 目标描边 + 提示条，三件装饰件跑在**独立线程**。
//!
//! ## 设计要点
//! - **不抢焦点、不用 `SetCapture`、不用 hook**：一个 ~16ms 定时器轮询
//!   `GetCursorPos` + `GetAsyncKeyState`。左键确认、右键/`Esc` 取消。
//!   这样 NOACTIVATE 铁律保住（点击不打断打字），`Esc` 只在捕捉态内处理。
//! - **三窗都点击穿透**（`WS_EX_TRANSPARENT`）：不挡底下窗口的关闭按钮；
//!   鼠标不参与交互，全走轮询。
//! - **自己排除出枚举**：三窗与本进程同 PID，`window_probe` 按 PID 过滤——
//!   否则最上层永远是自家装饰件，会「自己高亮自己」。
//! - **重绘只在变化时**：取景框仅在光标换显示器时重绘；描边仅在目标或尺寸变化时
//!   重绘；提示条**只 `SetWindowPos` 移动、不重绘**（仅文本变化才重渲染）。
//! - 判定在捕捉期完成：不可挂载的目标当场用警示色 + 文案标出（见 `TargetKind`）。
//!
//! 全部失败静默降级（记日志，不 panic）。

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use iuv_ui::{
    render_finder_frame, render_target_label, render_target_outline, TargetKind, TextRenderer,
    Theme,
};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, VIRTUAL_KEY, VK_ESCAPE, VK_LBUTTON, VK_RBUTTON,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetCursorPos, GetMessageW, KillTimer, PostMessageW, PostQuitMessage,
    SetTimer, SetWindowPos, ShowWindow, TranslateMessage, HWND_TOPMOST, MSG, SWP_NOACTIVATE,
    SWP_NOSIZE, SW_HIDE, SW_SHOWNA, WINDOW_EX_STYLE, WM_APP, WM_TIMER, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT,
};

use crate::logger::log_line;
use crate::popup::LayeredWindow;
use crate::ulw::UlwSurface;
use crate::window_probe::{monitor_rect_at, probe_at, Mountability, ProbeRect, WindowProbe};

const CLASS_FRAME: PCWSTR = w!("IuvFinderFrame");
const CLASS_OUTLINE: PCWSTR = w!("IuvFinderOutline");
const CLASS_LABEL: PCWSTR = w!("IuvFinderLabel");

/// 外部请求取消（跨线程 `PostMessage` 到取景框窗口）。
const WM_APP_CANCEL: u32 = WM_APP + 61;
const TIMER_ID: usize = 1;
/// 轮询间隔（ms）。16 ≈ 60fps，肉眼无感；只在目标变化时才重绘，故开销可忽略。
const POLL_MS: u32 = 16;
/// 提示条相对光标的偏移（px，@96dpi 语义）。
const LABEL_OFFSET: i32 = 18;
/// 目标描边相对目标窗口外扩（px）。
const OUTLINE_GROW: i32 = 2;

/// 三件装饰件共用的窗口扩展样式：置顶、工具窗、**不抢焦点**、ULW 分层、**点击穿透**
/// （不挡底下窗口的关闭按钮；鼠标交互全走轮询）。
const EX_DECOR: WINDOW_EX_STYLE = WINDOW_EX_STYLE(
    WS_EX_TOPMOST.0
        | WS_EX_TOOLWINDOW.0
        | WS_EX_NOACTIVATE.0
        | WS_EX_LAYERED.0
        | WS_EX_TRANSPARENT.0,
);

static ACTIVE: AtomicBool = AtomicBool::new(false);
static FRAME_HWND: AtomicUsize = AtomicUsize::new(0);

/// 捕捉结果。
pub enum CaptureOutcome {
    /// 用户确认选中的目标窗口
    Picked(WindowProbe),
    /// 取消（右键 / `Esc` / 外部 `cancel()`）
    Cancelled,
}

/// 是否正在捕捉。
pub fn is_active() -> bool {
    ACTIVE.load(Ordering::SeqCst)
}

/// 请求取消捕捉（跨线程安全；未在捕捉时无副作用）。
pub fn cancel() {
    let h = FRAME_HWND.load(Ordering::SeqCst);
    if h != 0 {
        // SAFETY: PostMessageW 跨线程投递到取景框窗口（其线程的消息循环消费）。
        let _ =
            unsafe { PostMessageW(Some(HWND(h as *mut _)), WM_APP_CANCEL, WPARAM(0), LPARAM(0)) };
    }
}

/// 进入捕捉模式。已在捕捉中 → 返回 false（幂等，不叠第二个）。
/// `on_done` 在捕捉线程结束前调用一次（消费方据此建档案）。
pub fn start<F>(theme: Theme, on_done: F) -> bool
where
    F: FnOnce(CaptureOutcome) + Send + 'static,
{
    if ACTIVE.swap(true, Ordering::SeqCst) {
        return false;
    }
    let spawned = std::thread::Builder::new()
        .name("iuv-finder".into())
        .spawn(move || {
            let outcome = run_capture(theme);
            ACTIVE.store(false, Ordering::SeqCst);
            FRAME_HWND.store(0, Ordering::SeqCst);
            on_done(outcome);
        });
    if spawned.is_err() {
        log_line("[finder] 捕捉线程创建失败");
        ACTIVE.store(false, Ordering::SeqCst);
        return false;
    }
    true
}

/// 捕捉线程主体：建三窗 → 起定时器 → 消息循环 → 返回结果。
fn run_capture(theme: Theme) -> CaptureOutcome {
    let Some(mut f) = Finder::new(theme) else {
        return CaptureOutcome::Cancelled;
    };
    FRAME_HWND.store(f.frame.hwnd.0 as usize, Ordering::SeqCst);
    // SAFETY: 定时器投递到取景框窗口（本线程消息循环消费）。
    unsafe { SetTimer(Some(f.frame.hwnd), TIMER_ID, POLL_MS, None) };
    f.refresh(0, 0, true); // 首帧：立即出装饰件（不等鼠标动）
    log_line("[finder] 进入捕捉态");

    loop {
        let mut msg = MSG::default();
        // SAFETY: 标准消息循环；WM_QUIT 时 GetMessageW 返回 0（BOOL.0）。
        let r = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if r.0 <= 0 {
            break;
        }
        // SAFETY: TranslateMessage/DispatchMessage 标准配对。
        unsafe {
            let _ = TranslateMessage(&msg);
            let _ = DispatchMessageW(&msg);
        }
        if f.outcome.is_some() {
            break;
        }
    }
    let outcome = f.outcome.take().unwrap_or(CaptureOutcome::Cancelled);
    log_line("[finder] 退出捕捉态");
    outcome
}

/// 光标屏幕坐标（失败 None）。
fn cursor_screen() -> Option<(i32, i32)> {
    let mut pt = POINT::default();
    // SAFETY: pt 为栈上可写；GetCursorPos 纯查询。
    unsafe { GetCursorPos(&mut pt) }.ok()?;
    Some((pt.x, pt.y))
}

/// 按键是否按下（`GetAsyncKeyState` 高位）。
fn key_down(vk: VIRTUAL_KEY) -> bool {
    // SAFETY: GetAsyncKeyState 纯查询。
    (unsafe { GetAsyncKeyState(vk.0 as i32) } as u16 & 0x8000) != 0
}

fn kind_of(m: Mountability) -> TargetKind {
    match m {
        Mountability::Ok => TargetKind::Mountable,
        Mountability::FullscreenOverlay | Mountability::Elevated => TargetKind::Unmountable,
    }
}

fn mount_label(m: Mountability) -> &'static str {
    match m {
        Mountability::Ok => "可挂载",
        Mountability::FullscreenOverlay => "全屏覆盖 · 不可挂载",
        Mountability::Elevated => "不可挂载（权限不足）",
    }
}

/// 捕捉会话状态（**只在本线程触碰**；经 `GWLP_USERDATA` 从 wnd_proc 取回）。
struct Finder {
    frame: LayeredWindow,
    frame_ulw: UlwSurface,
    outline: LayeredWindow,
    outline_ulw: UlwSurface,
    label: LayeredWindow,
    label_ulw: UlwSurface,
    text: TextRenderer,
    theme: Theme,
    scale: f32,
    /// 首次轮询前必须等到所有键松开——否则「点工具栏按钮那一下」会被当成确认。
    armed: bool,
    mon: Option<ProbeRect>,
    /// 当前目标：(hwnd, rect, 分级)
    target: Option<(isize, ProbeRect, TargetKind)>,
    label_text: String,
    /// 最近一次上屏的提示条尺寸（提示条只移动不重绘，尺寸稳定；夹屏用）。
    label_size_px: (i32, i32),
    outcome: Option<CaptureOutcome>,
}

impl Finder {
    fn new(theme: Theme) -> Option<Box<Finder>> {
        let mut f = Box::new(Finder {
            frame: LayeredWindow::new(),
            frame_ulw: UlwSurface::new(),
            outline: LayeredWindow::new(),
            outline_ulw: UlwSurface::new(),
            label: LayeredWindow::new(),
            label_ulw: UlwSurface::new(),
            text: TextRenderer::new(),
            theme,
            scale: 1.0,
            armed: false,
            mon: None,
            target: None,
            label_text: String::new(),
            label_size_px: (0, 0),
            outcome: None,
        });
        // SAFETY: f 是 Box（地址稳定），三窗的 GWLP_USERDATA 都指向它；LayeredWindow::Drop
        // 先清零 GWLP_USERDATA 再销毁窗口，故 wnd_proc 取回的指针不悬垂。
        let p: *mut Finder = &mut *f;
        if !f
            .frame
            .create(CLASS_FRAME, EX_DECOR, finder_wnd_proc, p, "[finder]")
        {
            return None;
        }
        if !f
            .outline
            .create(CLASS_OUTLINE, EX_DECOR, finder_wnd_proc, p, "[finder]")
            || !f
                .label
                .create(CLASS_LABEL, EX_DECOR, finder_wnd_proc, p, "[finder]")
        {
            return None;
        }
        let dpi = f.frame.dpi();
        f.scale = if dpi >= 96 { dpi as f32 / 96.0 } else { 1.0 };
        Some(f)
    }

    /// 一次轮询：更新三件装饰件 + 处理输入。返回 Some = 会话结束。
    fn refresh(&mut self, cx: i32, cy: i32, first: bool) -> Option<CaptureOutcome> {
        // 1) 取景框：跟随光标所在显示器（仅换屏时重绘）
        if let Some(mon) = monitor_rect_at(cx, cy) {
            if self.mon != Some(mon) {
                let surf = render_finder_frame(
                    mon.w.max(1) as u32,
                    mon.h.max(1) as u32,
                    self.scale,
                    &self.theme,
                );
                self.frame_ulw.upload(
                    self.frame.hwnd,
                    &surf,
                    mon.x,
                    mon.y,
                    mon.w,
                    mon.h,
                    "[finder]",
                );
                // SAFETY: 显示不抢焦点（SW_SHOWNA）。
                let _ = unsafe { ShowWindow(self.frame.hwnd, SW_SHOWNA) };
                self.mon = Some(mon);
            }
        }
        // 2) 目标描边 + 提示条文本（仅目标或尺寸变化时重绘）
        let probe = probe_at(cx, cy);
        let key = probe.as_ref().map(|p| (p.hwnd, p.rect));
        if first || key != self.target.map(|(h, r, _)| (h, r)) {
            match &probe {
                Some(p) => {
                    let kind = kind_of(p.mountability);
                    let (w, h) = (p.rect.w + OUTLINE_GROW * 2, p.rect.h + OUTLINE_GROW * 2);
                    let surf = render_target_outline(
                        w.max(1) as u32,
                        h.max(1) as u32,
                        self.scale,
                        kind,
                        &self.theme,
                    );
                    self.outline_ulw.upload(
                        self.outline.hwnd,
                        &surf,
                        p.rect.x - OUTLINE_GROW,
                        p.rect.y - OUTLINE_GROW,
                        w,
                        h,
                        "[finder]",
                    );
                    // SAFETY: SW_SHOWNA 显示不抢焦点。
                    let _ = unsafe { ShowWindow(self.outline.hwnd, SW_SHOWNA) };
                    self.target = Some((p.hwnd, p.rect, kind));
                    self.set_label(p, kind);
                }
                None => {
                    // SAFETY: SW_HIDE 隐藏。
                    let _ = unsafe { ShowWindow(self.outline.hwnd, SW_HIDE) };
                    let _ = unsafe { ShowWindow(self.label.hwnd, SW_HIDE) };
                    self.target = None;
                }
            }
        }
        // 3) 提示条跟随光标（只移动，不重绘；夹到显示器内防溢出屏幕）
        if probe.is_some() {
            let (lx, ly) = self.label_pos(cx, cy);
            // SAFETY: 只改位置（SWP_NOSIZE），不激活窗口。
            let _ = unsafe {
                SetWindowPos(
                    self.label.hwnd,
                    Some(HWND_TOPMOST),
                    lx,
                    ly,
                    0,
                    0,
                    SWP_NOSIZE | SWP_NOACTIVATE,
                )
            };
        }
        // 4) 输入（轮询；不抢焦点、不 hook）
        let lmb = key_down(VK_LBUTTON);
        let rmb = key_down(VK_RBUTTON);
        let esc = key_down(VK_ESCAPE);
        if !self.armed {
            // 等所有键松开再武装，避免把「点工具栏按钮那一下」当成确认
            if !lmb && !rmb && !esc {
                self.armed = true;
            }
            return None;
        }
        if esc || rmb {
            return Some(CaptureOutcome::Cancelled);
        }
        if lmb {
            // 有目标才算确认；空处点击忽略（继续捕捉）
            if let Some(p) = probe {
                return Some(CaptureOutcome::Picked(p));
            }
        }
        None
    }

    /// 提示条位置：光标右下偏移，夹到显示器矩形内。
    fn label_pos(&self, cx: i32, cy: i32) -> (i32, i32) {
        let (mut x, mut y) = (cx + LABEL_OFFSET, cy + LABEL_OFFSET);
        if let Some(mon) = self.mon {
            let (lw, lh) = self.label_size();
            if x + lw > mon.right() {
                x = (cx - LABEL_OFFSET - lw).max(mon.x);
            }
            if y + lh > mon.bottom() {
                y = (cy - LABEL_OFFSET - lh).max(mon.y);
            }
        }
        (x, y)
    }

    fn label_size(&self) -> (i32, i32) {
        self.label_size_px
    }

    /// 渲染并上屏提示条（仅文本变化时）。
    fn set_label(&mut self, p: &WindowProbe, kind: TargetKind) {
        let text = format!(
            "{} · {} · {}×{} · {}",
            p.exe_name(),
            p.class_name,
            p.rect.w,
            p.rect.h,
            mount_label(p.mountability)
        );
        if text == self.label_text {
            return;
        }
        let surf = render_target_label(&text, kind, &self.theme, self.scale, &mut self.text);
        self.label_size_px = (surf.w as i32, surf.h as i32);
        if self.label_ulw.upload(
            self.label.hwnd,
            &surf,
            0,
            0,
            surf.w as i32,
            surf.h as i32,
            "[finder]",
        ) {
            // SAFETY: SW_SHOWNA 显示不抢焦点。
            let _ = unsafe { ShowWindow(self.label.hwnd, SW_SHOWNA) };
        }
        self.label_text = text;
    }

    /// 收尾：停定时器 + 隐藏三窗。
    fn shutdown(&mut self) {
        // SAFETY: 与 SetTimer 同窗口配对。
        let _ = unsafe { KillTimer(Some(self.frame.hwnd), TIMER_ID) };
        // SAFETY: SW_HIDE 隐藏。
        unsafe {
            let _ = ShowWindow(self.frame.hwnd, SW_HIDE);
            let _ = ShowWindow(self.outline.hwnd, SW_HIDE);
            let _ = ShowWindow(self.label.hwnd, SW_HIDE);
        }
    }
}

/// 三窗共用窗口过程：只处理定时器与取消；其余转发默认（无鼠标/键盘消息——全走轮询）。
unsafe extern "system" fn finder_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TIMER => {
            // SAFETY: hwnd 由本线程创建；Drop 先清零 GWLP_USERDATA。
            if let Some(f) = unsafe { LayeredWindow::get_self_mut::<Finder>(hwnd) } {
                let outcome = match cursor_screen() {
                    Some((cx, cy)) => f.refresh(cx, cy, false),
                    None => None,
                };
                if let Some(o) = outcome {
                    f.outcome = Some(o);
                    f.shutdown();
                    // SAFETY: 通知本线程消息循环退出。
                    unsafe { PostQuitMessage(0) };
                }
            }
            LRESULT(0)
        }
        WM_APP_CANCEL => {
            // SAFETY: 同上。
            if let Some(f) = unsafe { LayeredWindow::get_self_mut::<Finder>(hwnd) } {
                f.outcome = Some(CaptureOutcome::Cancelled);
                f.shutdown();
            }
            // SAFETY: 通知本线程消息循环退出。
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { LayeredWindow::default_wnd_proc(hwnd, msg, wparam, lparam) },
    }
}
