//! 浮动工具栏宿主（32-status-toolbar.md §6）：daemon 独立 win32 消息泵线程承载。
//!
//! daemon 持有**全局唯一**工具栏窗口（看板）：渲染当前前台 iuv 实例的四态，点击
//! 经反向控制通道（§4.2 按需连接 per-实例管道）驱动 TSF 应用、回 StateResult 更新。
//!
//! 线程模型：工具条窗口跑在**独立线程**（GetMessage 循环）——daemon 主线程被 eframe
//! 设置窗占用时工具栏保持响应；管道线程改共享态后 `PostMessage(WM_APP_REFRESH)` 唤醒
//! reconcile。窗口必须由创建线程使用（Win32 纪律），全部 ToolbarWindow 状态只在该线程
//! 触碰；跨线程经 `Arc<Mutex<Shared>>` + `PostMessage`。
//!
//! 前台看板判定（§6.2）：定时器 ~250ms `GetForegroundWindow` + `GetWindowThreadProcessId`
//! → 前台 pid:tid 命中「active 实例」→ focused = 该实例、渲染其四态；否则隐藏。轮询
//! 兜底天然覆盖切 app/切输入法/实例死亡/失焦时序竞态；TSF `Active` 通知用于即时性。
//!
//! 持久化（§6.3）：独立 `toolbar.json`（`%LOCALAPPDATA%\iuv\`，显示偏好 + 位置），
//! 写盘复用「临时文件 + rename 原子替换」，失败不阻断（内存态已生效）。首次无位置 →
//! 主屏右下角（§7 决策点 6）。
//!
//! 交互（§6.6）：不抢焦点（NOACTIVATE + 点击穿透圆角外空白）、拖拽空白区移动 + 持久化、
//! 悬停 tooltip、**无自身右键菜单**（显隐唯一入口 = 语言栏「中/英」右键菜单）。
//! 全部失败静默降级（记日志，不 panic；守护进程硬性约定）。

use std::collections::HashMap;
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use iuv_data::{
    ctl_pipe_name, CtlClient, CtlCmd, CtlResult, PipeClient, Request, ToolbarState,
    CTL_FIELD_MODE, CTL_FIELD_PUNCT, CTL_FIELD_SCRIPT, CTL_FIELD_WIDTH,
};
use iuv_ui::{
    hit_test, render_toolbar, render_tooltip, theme_dark, theme_light, TextRenderer, Theme,
    ToolbarIcons, ToolbarSpec, TB_GEAR, TB_LOGO, TB_MODE, TB_PUNCT, TB_SCRIPT, TB_WIDTH,
};
use iuv_ui::layout::Rect;
use iuv_win::UlwSurface;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    GetDC, GetDeviceCaps, GetMonitorInfoW, MonitorFromPoint, ReleaseDC, LOGPIXELSY, MONITORINFO,
    MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TRACKMOUSEEVENT, TME_LEAVE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos,
    GetForegroundWindow, GetMessageW, GetWindowLongPtrW, GetWindowRect, GetWindowThreadProcessId,
    PostMessageW, PostThreadMessageW, RegisterClassExW, SetTimer, SetWindowLongPtrW, SetWindowPos,
    ShowWindow, TranslateMessage, WM_APP, WM_QUIT, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, HTCLIENT,
    HTTRANSPARENT, MA_NOACTIVATE, MSG, SW_HIDE, SW_SHOWNA, SWP_NOACTIVATE, SWP_NOCOPYBITS,
    SWP_NOSIZE, SWP_NOZORDER, WM_DESTROY, WM_ERASEBKGND, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NCHITTEST, WM_PAINT, WM_TIMER, WNDCLASSEXW,
    WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
// WM_MOUSELEAVE 在 windows-rs 0.62 中位于 Controls 模块（值 0x02A3 = 675），本地定义。
const WM_MOUSELEAVE: u32 = 675;

use crate::state::DaemonState;
use crate::log;

/// 前台看板轮询间隔（毫秒，§6.2）。
const TOOLBAR_POLL_MS: u32 = 250;
/// 私有消息：共享态变化 → 唤醒 reconcile（管道线程 PostMessage）。
const WM_APP_REFRESH: u32 = WM_APP + 41;
/// 定时器 id。
const ID_TIMER: usize = 1;

const CLASS_BAR: PCWSTR = w!("IuvToolbarWindow");
const CLASS_TIP: PCWSTR = w!("IuvTooltipWindow");

/// 工具栏默认边距（主屏右下角，§7 决策点 6）。
const DEFAULT_MARGIN: i32 = 12;

/// 实例表条目。
#[derive(Clone, Copy, Debug, Default)]
struct ToolbarInstance {
    state: ToolbarState,
    active: bool,
}

/// 工具栏共享状态（管道线程写、工具条线程读；Mutex 串行）。
#[derive(Default)]
struct Shared {
    /// 实例表 `{pid:tid → {state, active}}`（§6.1）。
    instances: HashMap<(u32, u32), ToolbarInstance>,
    /// 当前前台 iuv 实例（看板判定结果）。
    focused: Option<(u32, u32)>,
    /// 全局显示偏好（语言栏菜单开关；持久化）。
    visible: bool,
    /// 记忆位置（拖动后写；持久化；None = 首次默认主屏右下角）。
    pos: Option<(i32, i32)>,
}

/// 工具栏宿主（daemon 主线程持有；工具条线程共享共享态 + 唤醒句柄）。
pub struct ToolbarHost {
    shared: Arc<Mutex<Shared>>,
    /// 工具条窗口句柄（工具条线程创建后注册；供 PostMessage 唤醒）。
    hwnd: AtomicUsize,
    /// 工具条线程 id（退出时 PostThreadMessage WM_QUIT）。
    thread_id: AtomicU32,
    /// 工具条线程退出标志。
    quit: Arc<AtomicBool>,
}

impl ToolbarHost {
    /// 启动工具条线程。`state` = daemon 全局状态（读主题）。返回宿主（线程就绪后
    /// 注册窗口句柄，可直接 wake）。启动失败 → 记录日志，宿主仍可用（wake 空操作）。
    pub fn spawn(state: Arc<DaemonState>) -> Arc<ToolbarHost> {
        iuv_win::set_logger(Some(log::log_line));
        let shared = Arc::new(Mutex::new(Shared {
            visible: load_pref().visible,
            ..Default::default()
        }));
        let icons = Arc::new(crate::toolbar_icons::load_icons());
        let host = Arc::new(ToolbarHost {
            shared: shared.clone(),
            hwnd: AtomicUsize::new(0),
            thread_id: AtomicU32::new(0),
            quit: Arc::new(AtomicBool::new(false)),
        });
        let t_shared = shared.clone();
        let t_state = state.clone();
        let t_icons = icons.clone();
        let t_quit = host.quit.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("iuv-toolbar".to_string())
            .spawn(move || toolbar_thread_main(t_shared, t_state, t_icons, t_quit, tx));
        let _spawned = match spawned {
            Ok(_h) => {
                log::log_line("[toolbar] 工具条线程已启动");
            }
            Err(e) => {
                log::log_line(&format!("[toolbar] 工具条线程启动失败：{e}"));
            }
        };
        // 等待工具条线程回传 (hwnd, os_thread_id)（5s 超时；失败 = 窗口未建，wake 空操作）。
        // HWND 为裸指针（!Send），经 usize 回传。
        if let Ok((hwnd, os_tid)) = rx.recv_timeout(Duration::from_secs(5)) {
            host.hwnd.store(hwnd, Ordering::SeqCst);
            host.thread_id.store(os_tid, Ordering::SeqCst);
        }
        host
    }

    /// 处理工具栏相关管道请求（Register/StateSync/Active/Unregister/ToggleToolbar）。
    /// 返回 true = 本请求已消费（调用方不再按用户库写请求处理）。
    pub fn handle_request(&self, req: &Request) -> bool {
        match req {
            Request::Register { pid, tid, state } => {
                let mut sh = self.shared.lock().unwrap_or_else(|p| p.into_inner());
                sh.instances.insert(
                    (*pid, *tid),
                    ToolbarInstance {
                        state: *state,
                        active: false,
                    },
                );
                drop(sh);
                log::log_line(&format!("[toolbar] 实例注册（{pid}:{tid}）"));
                self.wake();
                true
            }
            Request::StateSync { pid, tid, state } => {
                let mut sh = self.shared.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(i) = sh.instances.get_mut(&(*pid, *tid)) {
                    i.state = *state;
                }
                drop(sh);
                self.wake();
                true
            }
            Request::Active { pid, tid, active } => {
                let mut sh = self.shared.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(i) = sh.instances.get_mut(&(*pid, *tid)) {
                    i.active = *active;
                }
                drop(sh);
                self.wake();
                true
            }
            Request::Unregister { pid, tid } => {
                let mut sh = self.shared.lock().unwrap_or_else(|p| p.into_inner());
                sh.instances.remove(&(*pid, *tid));
                drop(sh);
                log::log_line(&format!("[toolbar] 实例注销（{pid}:{tid}）"));
                self.wake();
                true
            }
            Request::ToggleToolbar => {
                let visible = {
                    let mut sh = self.shared.lock().unwrap_or_else(|p| p.into_inner());
                    sh.visible = !sh.visible;
                    sh.visible
                };
                log::log_line(&format!("[toolbar] 全局显隐偏好 → {visible}"));
                save_pref(&ToolbarPref {
                    visible,
                    pos: self.shared.lock().unwrap_or_else(|p| p.into_inner()).pos,
                });
                self.wake();
                true
            }
            _ => false,
        }
    }

    /// 唤醒工具条线程 reconcile（共享态变化后；窗口未就绪 → 静默）。
    fn wake(&self) {
        let h = self.hwnd.load(Ordering::SeqCst) as *mut core::ffi::c_void;
        if h.is_null() {
            return;
        }
        // SAFETY: hwnd 由工具条线程创建、线程存活期间有效；跨线程 PostMessage 合法。
        let _ = unsafe { PostMessageW(Some(HWND(h)), WM_APP_REFRESH, WPARAM(0), LPARAM(0)) };
    }

    /// 停止工具条线程（daemon 退出时；PostThreadMessage WM_QUIT 唤醒消息循环）。
    pub fn shutdown(&self) {
        self.quit.store(true, Ordering::SeqCst);
        let tid = self.thread_id.load(Ordering::SeqCst);
        if tid != 0 {
            // SAFETY: PostThreadMessageW 向工具条线程投递 WM_QUIT（GetMessage 返回 0）。
            let _ = unsafe { PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0)) };
        }
    }
}

// ===== 工具条线程 + 窗口 =====

/// 工具条线程主体：建窗口 → 注册句柄 → 消息循环（定时器轮询前台看板）。退出清理。
fn toolbar_thread_main(
    shared: Arc<Mutex<Shared>>,
    state: Arc<DaemonState>,
    icons: Arc<ToolbarIcons>,
    _quit: Arc<AtomicBool>,
    tx: std::sync::mpsc::Sender<(usize, u32)>,
) {
    register_bar_class();
    register_tip_class();
    let win = Box::new(ToolbarWindow::new(shared, state, icons));
    if win.hwnd.is_invalid() {
        log::log_line("[toolbar] 建窗失败，工具条线程退出");
        return;
    }
    // SAFETY: 定时器回调 id = ID_TIMER（消息循环内 wnd_proc 消费）。
    unsafe { SetTimer(Some(win.hwnd), ID_TIMER, TOOLBAR_POLL_MS, None) };
    let hwnd = win.hwnd;
    // SAFETY: win 为 Box（地址稳定），线程存活期间有效；wnd_proc 经 GWLP_USERDATA 取回。
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, &*win as *const ToolbarWindow as isize) };
    // SAFETY: GetCurrentThreadId 纯查询（PostThreadMessage 退出用）。
    let os_tid = unsafe { GetCurrentThreadId() };
    let _ = tx.send((hwnd.0 as usize, os_tid));
    log::log_line("[toolbar] 工具条窗口就绪，进入消息循环");
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
    }
    drop(win); // 清 GWLP_USERDATA + 销毁窗口
    log::log_line("[toolbar] 工具条线程退出");
}

/// 工具条窗口（仅工具条线程触碰；wnd_proc 经 GWLP_USERDATA 取回）。
struct ToolbarWindow {
    hwnd: HWND,
    shared: Arc<Mutex<Shared>>,
    state: Arc<DaemonState>,
    icons: Arc<ToolbarIcons>,
    theme: Theme,
    text: Option<TextRenderer>,
    ulw: UlwSurface,
    rows: Vec<Rect>,
    visible: bool,
    hover: Option<usize>,
    pressed: Option<usize>,
    /// 拖拽偏移（拖动起点 = 光标 - 窗口位置）；None = 未拖拽。
    drag_offset: Option<(i32, i32)>,
    /// tooltip 窗口（悬停显示按钮说明）。
    tip: TooltipWindow,
}

impl ToolbarWindow {
    fn new(
        shared: Arc<Mutex<Shared>>,
        state: Arc<DaemonState>,
        icons: Arc<ToolbarIcons>,
    ) -> ToolbarWindow {
        let hwnd = create_window(CLASS_BAR);
        let theme = current_theme(&state);
        let tip = TooltipWindow::new(theme.clone());
        let mut w = ToolbarWindow {
            hwnd,
            shared,
            state,
            icons,
            theme,
            text: None,
            ulw: UlwSurface::new(),
            rows: Vec::new(),
            visible: false,
            hover: None,
            pressed: None,
            drag_offset: None,
            tip,
        };
        if !hwnd.is_invalid() {
            w.text = Some(TextRenderer::new());
        }
        w
    }

    fn scale(&self) -> f32 {
        if self.hwnd.is_invalid() {
            return 1.0;
        }
        // SAFETY: hdc 由 GetDC 取得，使用后立即 ReleaseDC。
        unsafe {
            let hdc = GetDC(Some(self.hwnd));
            if hdc.is_invalid() {
                return 1.0;
            }
            let dpi = GetDeviceCaps(Some(hdc), LOGPIXELSY);
            ReleaseDC(Some(self.hwnd), hdc);
            if dpi <= 0 {
                1.0
            } else {
                dpi as f32 / 96.0
            }
        }
    }

    /// reconcile（WM_APP_REFRESH / WM_TIMER 共用）：前台看板判定 + 主题热刷新 + 显隐/重绘。
    fn reconcile(&mut self) {
        // 主题热刷新（设置页保存后经 state.config 更新；读内存快照，零磁盘 I/O）。
        let t = current_theme(&self.state);
        if t != self.theme {
            self.theme = t;
            self.tip.set_theme(t);
            log::log_line(&format!("[toolbar] 主题热刷新：{}", self.theme.name));
        }
        self.poll_foreground();
        // 拖拽中零重绘：只由 drag_move 的 SetWindowPos 移动，避免 250ms 定时器整帧重渲/重传
        // 与拖拽争抢 → 闪烁。状态变化（StateSync）在拖拽结束后下一帧自然刷新。
        if self.visible && self.drag_offset.is_none() {
            self.repaint();
        }
    }

    /// 前台看板判定（§6.2）：前台 pid:tid 命中 active 实例 → focused + 显示；否则隐藏。
    fn poll_foreground(&mut self) {
        // SAFETY: GetForegroundWindow 纯查询；GetWindowThreadProcessId 输出 pid。
        let fg = unsafe { GetForegroundWindow() };
        let mut pid = 0u32;
        let tid = unsafe { GetWindowThreadProcessId(fg, Some(&mut pid)) };
        let (focused, visible) = {
            let sh = self.shared.lock().unwrap_or_else(|p| p.into_inner());
            let inst = sh.instances.get(&(pid, tid)).copied();
            let focused = inst.filter(|i| i.active).map(|_| (pid, tid));
            let visible = sh.visible;
            drop(sh);
            // 前台未命中 active 实例时恒不显示（即使实例表另有 active 也如此）。
            let mut sh2 = self.shared.lock().unwrap_or_else(|p| p.into_inner());
            sh2.focused = focused;
            (focused, visible)
        };
        if focused.is_some() && visible {
            // 已可见：**不重定位**（交由 reconcile 的 repaint 按当前窗口矩形刷新）——
            // 否则每次 250ms 定时器都把窗口拉回 shared.pos（拖拽前旧位置），拖拽中被
            // SetWindowPos 移走后 250ms 又弹回原位 → 闪烁（2026-08-21 实测拖拽闪烁）。
            // 仅在 hidden→shown 转变时 show() 定位一次。
            if !self.visible {
                self.show();
            }
        } else if self.visible {
            self.hide();
        }
    }

    /// 显示（首显定位：记忆位置（clamp 回工作区）或主屏右下角；ULW 上屏 + SW_SHOWNA 不抢焦点）。
    fn show(&mut self) {
        if self.hwnd.is_invalid() {
            return;
        }
        let Some(surf) = self.frame() else { return };
        let (w, h) = (surf.w as i32, surf.h as i32);
        let pos = {
            let sh = self.shared.lock().unwrap_or_else(|p| p.into_inner());
            match sh.pos {
                Some(p) => {
                    // 防御：记忆位置越出显示器工作区（拖拽损坏/多屏变化/旧版本 32767 bug）
                    // → clamp 回工作区，绝不渲染到屏幕外（2026-08-21 实测工具栏隐形）。
                    clamp_to_work(p.0, p.1, w, h)
                }
                None => default_pos(w, h),
            }
        };
        self.present(&surf, pos.0, pos.1, w, h);
        // SAFETY: SW_SHOWNA 显示但不激活——绝不抢焦点（点击不打断活动 composition）。
        let _ = unsafe { ShowWindow(self.hwnd, SW_SHOWNA) };
        self.visible = true;
    }

    /// 原位重绘（reconcile / 悬停 / 按下变化）。
    fn repaint(&mut self) {
        if self.hwnd.is_invalid() || !self.visible {
            return;
        }
        let Some(surf) = self.frame() else { return };
        // SAFETY: GetWindowRect 读当前窗口矩形（ULW 窗口位置即上屏位置）。
        let mut rc = RECT::default();
        if unsafe { GetWindowRect(self.hwnd, &mut rc) }.is_err() {
            return;
        }
        self.present(&surf, rc.left, rc.top, rc.right - rc.left, rc.bottom - rc.top);
    }

    /// 渲染当前帧 → Surface + 刷新按钮命中矩形。
    fn frame(&mut self) -> Option<iuv_ui::Surface> {
        let scale = self.scale();
        let inst = {
            let sh = self.shared.lock().unwrap_or_else(|p| p.into_inner());
            sh.focused
                .and_then(|f| sh.instances.get(&f).copied())
        };
        let state = inst.map(|i| i.state).unwrap_or_default();
        let spec = ToolbarSpec {
            icons: &self.icons,
            mode: state.mode,
            width: state.width,
            punct: state.punct,
            script: state.script,
            hover: self.hover,
            pressed: self.pressed,
        };
        let (surf, rows) = render_toolbar(&spec, &self.theme, scale);
        if surf.w == 0 || surf.h == 0 {
            return None;
        }
        self.rows = rows;
        Some(surf)
    }

    fn present(&mut self, surf: &iuv_ui::Surface, x: i32, y: i32, w: i32, h: i32) {
        self.ulw.upload(self.hwnd, surf, x, y, w, h, "[toolbar]");
    }

    fn hide(&mut self) {
        self.visible = false;
        self.hover = None;
        self.pressed = None;
        self.tip.hide();
        if !self.hwnd.is_invalid() {
            // SAFETY: 隐藏工具条窗口
            let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
        }
    }

    /// 按钮点击（§6.5 严格请求/响应）：四态钮 → 连接 focused 实例控制管道发 SetState →
    /// 按结果更新实例表 + 重绘（成功/失败分别呈现）；齿轮 → OpenSettings；logo → 拖动把手。
    fn on_click(&mut self, index: usize) {
        let focused = self
            .shared
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .focused;
        let Some((pid, tid)) = focused else {
            log::log_line("[toolbar] 点击但无 focused 实例，忽略");
            return;
        };
        match index {
            TB_LOGO => {} // 拖动把手（无动作）
            TB_GEAR => {
                log::log_line("[toolbar] 齿轮 → 打开设置页");
                // 设置页通知（独立管道请求，复用 M6 路径）。
                if let Ok(c) = PipeClient::connect() {
                    let _ = c.request(&Request::OpenSettings);
                }
            }
            _ => {
                let field = match index {
                    TB_MODE => CTL_FIELD_MODE,
                    TB_WIDTH => CTL_FIELD_WIDTH,
                    TB_PUNCT => CTL_FIELD_PUNCT,
                    TB_SCRIPT => CTL_FIELD_SCRIPT,
                    _ => return,
                };
                let cur = self
                    .shared
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .instances
                    .get(&(pid, tid))
                    .map(|i| i.state.field(field))
                    .unwrap_or(0);
                let value = 1u8 - cur; // 双态翻转
                log::log_line(&format!(
                    "[toolbar] 点击按钮#{index} field={field} {cur}→{value}（实例 {pid}:{tid}）"
                ));
                let name = ctl_pipe_name(pid, tid);
                match CtlClient::connect(&name).and_then(|c| c.request(&CtlCmd::SetState { field, value }))
                {
                    Ok(CtlResult::Ok { state }) => {
                        log::log_line(&format!(
                            "[toolbar] 实例应用成功：mode={} width={} script={} punct={}",
                            state.mode, state.width, state.script, state.punct
                        ));
                        let mut sh = self.shared.lock().unwrap_or_else(|p| p.into_inner());
                        if let Some(i) = sh.instances.get_mut(&(pid, tid)) {
                            i.state = state;
                        }
                    }
                    Ok(CtlResult::Err { msg }) => {
                        log::log_line(&format!("[toolbar] 实例应用失败：{msg}"));
                    }
                    Err(e) => {
                        log::log_line(&format!(
                            "[toolbar] 控制管道不可达（实例离线/未注册？）：{e}"
                        ));
                    }
                }
                self.repaint();
            }
        }
    }

    /// 悬停更新（WM_MOUSEMOVE 命中）：改 hover 行 + 刷新 tooltip。
    fn on_hover(&mut self, x: i32, y: i32) {
        let hit = hit_test(&self.rows, x, y);
        let changed_hover = hit != self.hover;
        self.hover = hit;
        // tooltip：悬停按钮（logo/齿轮除外）显示说明；离开/空白隐藏。
        if let Some(i) = hit {
            if let Some(label) = button_tooltip(i) {
                if changed_hover {
                    self.tip.show_near(&self.theme, label, self.hwnd);
                }
            } else {
                self.tip.hide();
            }
        } else {
            self.tip.hide();
        }
        if changed_hover {
            self.repaint();
        }
    }

    /// 拖拽开始（空白区/logo 的 WM_LBUTTONDOWN）：记录光标**屏幕坐标**偏移 + 捕获鼠标。
    /// 用屏幕坐标而非客户区坐标：客户区原点随窗口移动而漂移，固定 offset 会造成
    /// 累积滞后/位置漂移（2026-08-21 实测拖到屏幕外 32767 且保存为记忆位置）。
    fn start_drag(&mut self) {
        let (cx, cy) = cursor_screen();
        // SAFETY: GetWindowRect 读当前窗口矩形。
        let mut rc = RECT::default();
        if unsafe { GetWindowRect(self.hwnd, &mut rc) }.is_err() {
            return;
        }
        self.drag_offset = Some((cx - rc.left, cy - rc.top));
        // SAFETY: SetCapture 捕获鼠标（拖拽期间窗口持续收 WM_MOUSEMOVE）。
        let _ = unsafe { SetCapture(self.hwnd) };
    }

    /// 拖拽移动（WM_MOUSEMOVE）：按光标屏幕坐标差值 SetWindowPos 移动窗口。
    /// **边缘检测**：目标位置先 clamp 到光标所在显示器工作区（x∈[left, right-w]、
    /// y∈[top, bottom-h]）——窗口整体一像素也不越出屏幕（2026-08-21 用户要求）。
    fn drag_move(&mut self) {
        let Some((ox, oy)) = self.drag_offset else { return };
        let (cx, cy) = cursor_screen();
        let (nx, ny) = (cx - ox, cy - oy);
        // SAFETY: GetWindowRect 读当前窗口矩形（窗口尺寸做 clamp 边界）。
        let (nx, ny) = {
            let mut rc = RECT::default();
            if unsafe { GetWindowRect(self.hwnd, &mut rc) }.is_ok() {
                clamp_to_work(nx, ny, rc.right - rc.left, rc.bottom - rc.top)
            } else {
                (nx, ny)
            }
        };
        // SAFETY: SWP_NOACTIVATE|NOSIZE|NOZORDER|NOCOPYBITS 仅移动（NOCOPYBITS 防移动时
        // 复制旧客户区位图产生残影）；layered 窗口内容由 DWM 缓存随动。
        let _ = unsafe {
            SetWindowPos(
                self.hwnd,
                None,
                nx,
                ny,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOCOPYBITS,
            )
        };
    }

    /// 拖拽结束（WM_LBUTTONUP）：释放捕获 + 位置 clamp 回工作区 + 持久化。
    fn end_drag(&mut self) {
        self.drag_offset = None;
        // SAFETY: ReleaseCapture 释放 SetCapture 的捕获。
        let _ = unsafe { ReleaseCapture() };
        // SAFETY: GetWindowRect 读最终位置。
        let mut rc = RECT::default();
        if unsafe { GetWindowRect(self.hwnd, &mut rc) }.is_err() {
            return;
        }
        let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
        let pos = clamp_to_work(rc.left, rc.top, w, h);
        if pos.0 != rc.left || pos.1 != rc.top {
            log::log_line(&format!(
                "[toolbar] 拖拽位置越界，clamp 回工作区：({}, {}) → {pos:?}",
                rc.left, rc.top
            ));
        }
        {
            let mut sh = self.shared.lock().unwrap_or_else(|p| p.into_inner());
            sh.pos = Some(pos);
        }
        save_pref(&ToolbarPref {
            visible: self.shared.lock().unwrap_or_else(|p| p.into_inner()).visible,
            pos: Some(pos),
        });
        log::log_line(&format!("[toolbar] 位置已记忆：{pos:?}"));
    }
}

impl Drop for ToolbarWindow {
    fn drop(&mut self) {
        if !self.hwnd.is_invalid() {
            // SAFETY: 先清零 GWLP_USERDATA，杜绝 wnd_proc 访问到即将释放的 self。
            let _ = unsafe { SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0) };
            // SAFETY: 在创建线程（工具条线程）销毁窗口。
            let _ = unsafe { DestroyWindow(self.hwnd) };
            self.hwnd = HWND::default();
        }
    }
}

// ===== tooltip 窗口 =====

/// 悬停 tooltip 小窗（§6.6：独立 ULW 窗口，不抢焦点；无点击）。
struct TooltipWindow {
    hwnd: HWND,
    theme: Theme,
    text: Option<TextRenderer>,
    ulw: UlwSurface,
}

impl TooltipWindow {
    fn new(theme: Theme) -> TooltipWindow {
        let hwnd = create_window(CLASS_TIP);
        let mut t = TooltipWindow {
            hwnd,
            theme,
            text: None,
            ulw: UlwSurface::new(),
        };
        if !hwnd.is_invalid() {
            t.text = Some(TextRenderer::new());
        }
        t
    }

    fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        self.hide(); // 下轮悬停用新主题重绘（简单起见不缓存标签）
    }

    fn show_near(&mut self, _theme: &Theme, label: &str, _bar: HWND) {
        if self.hwnd.is_invalid() {
            return;
        }
        let scale = self.scale();
        let Some(text) = self.text.as_mut() else {
            return;
        };
        let surf = render_tooltip(label, &self.theme, scale, text);
        if surf.w == 0 || surf.h == 0 {
            return;
        }
        // 定位：鼠标下方（错开工具栏）。
        // SAFETY: GetCursorPos 纯查询。
        let mut pt = POINT::default();
        if unsafe { GetCursorPos(&mut pt) }.is_err() {
            return;
        }
        // 锚定光标处，右/下越界内收。
        let (x, y) = clamp_to_work(pt.x + 8, pt.y + 12, surf.w as i32, surf.h as i32);
        self.ulw.upload(self.hwnd, &surf, x, y, surf.w as i32, surf.h as i32, "[tooltip]");
        // SAFETY: SW_SHOWNA 显示但不激活。
        let _ = unsafe { ShowWindow(self.hwnd, SW_SHOWNA) };
    }

    fn hide(&mut self) {
        if !self.hwnd.is_invalid() {
            // SAFETY: 隐藏 tooltip 窗口。
            let _ = unsafe { ShowWindow(self.hwnd, SW_HIDE) };
        }
    }

    fn scale(&self) -> f32 {
        if self.hwnd.is_invalid() {
            return 1.0;
        }
        // SAFETY: hdc 由 GetDC 取得，使用后立即 ReleaseDC。
        unsafe {
            let hdc = GetDC(Some(self.hwnd));
            if hdc.is_invalid() {
                return 1.0;
            }
            let dpi = GetDeviceCaps(Some(hdc), LOGPIXELSY);
            ReleaseDC(Some(self.hwnd), hdc);
            if dpi <= 0 {
                1.0
            } else {
                dpi as f32 / 96.0
            }
        }
    }
}

impl Drop for TooltipWindow {
    fn drop(&mut self) {
        if !self.hwnd.is_invalid() {
            // SAFETY: 在创建线程销毁窗口。
            let _ = unsafe { DestroyWindow(self.hwnd) };
            self.hwnd = HWND::default();
        }
    }
}

// ===== 窗口类注册 + 创建 + wnd_proc =====

fn register_bar_class() {
    static R: OnceLock<()> = OnceLock::new();
    R.get_or_init(|| register_class(CLASS_BAR, Some(bar_wnd_proc)));
}

fn register_tip_class() {
    static R: OnceLock<()> = OnceLock::new();
    R.get_or_init(|| register_class(CLASS_TIP, Some(tip_wnd_proc)));
}

fn register_class(name: PCWSTR, proc: Option<unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT>) {
    // SAFETY: 类名静态宽字符串；失败仅记日志。
    unsafe {
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: proc,
            hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH::default(),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: name,
            ..Default::default()
        };
        if RegisterClassExW(&class) == 0 {
            let err = windows::Win32::Foundation::GetLastError();
            if err != windows::Win32::Foundation::ERROR_CLASS_ALREADY_EXISTS {
                log::log_line("[toolbar] RegisterClassExW 失败");
            }
        }
    }
}

fn create_window(class: PCWSTR) -> HWND {
    // SAFETY: GetModuleHandleW(None) 取当前进程实例句柄。
    let hinst = unsafe { GetModuleHandleW(None) }.unwrap_or_default();
    if hinst.is_invalid() {
        log::log_line("[toolbar] GetModuleHandleW 失败");
        return HWND::default();
    }
    // SAFETY: TOPMOST|TOOLWINDOW|NOACTIVATE = 置顶、工具窗、不抢焦点；LAYERED = ULW 前置。
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
            class,
            PCWSTR::null(),
            WS_POPUP,
            0,
            0,
            0,
            0,
            None,
            None,
            Some(hinst.into()),
            None,
        )
    };
    match hwnd {
        Ok(h) => h,
        Err(e) => {
            log::log_line(&format!("[toolbar] CreateWindowExW 失败：{e:?}"));
            HWND::default()
        }
    }
}

/// 从 GWLP_USERDATA 取回窗口属主（可变版；调用都在工具条线程）。
fn get_bar_mut(hwnd: HWND) -> Option<&'static mut ToolbarWindow> {
    // SAFETY: 指针在窗口销毁前由 Drop 清零；调用都在创建线程。
    let p = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
    if p == 0 {
        None
    } else {
        Some(unsafe { &mut *(p as *mut ToolbarWindow) })
    }
}

/// lparam 低 32 位客户区坐标 (x, y)。
fn client_pos(lparam: LPARAM) -> (i32, i32) {
    let v = lparam.0 as u32;
    ((v & 0xFFFF) as i32, ((v >> 16) & 0xFFFF) as i32)
}

/// 光标屏幕坐标（拖拽用；失败返回 (0,0)，拖拽窗口回 0 处——失败极罕见）。
fn cursor_screen() -> (i32, i32) {
    // SAFETY: GetCursorPos 纯查询。
    let mut pt = POINT::default();
    if unsafe { GetCursorPos(&mut pt) }.is_err() {
        (0, 0)
    } else {
        (pt.x, pt.y)
    }
}

/// 圆角几何命中（与 render_toolbar 的圆角一致）：圆角外点击穿透（HTTRANSPARENT）。
fn in_rounded_rect(x: i32, y: i32, w: i32, h: i32, r: f32) -> bool {
    if w <= 0 || h <= 0 {
        return false;
    }
    if !r.is_finite() || r <= 0.0 {
        return true;
    }
    let r = (r as i32).min(w / 2).min(h / 2).max(1);
    let in_corner = |cx: i32, cy: i32, px: i32, py: i32| {
        let dx = px - cx;
        let dy = py - cy;
        dx * dx + dy * dy <= r * r
    };
    if x < r && y < r {
        return in_corner(r, r, x, y);
    }
    if x >= w - r && y < r {
        return in_corner(w - r, r, x, y);
    }
    if x < r && y >= h - r {
        return in_corner(r, h - r, x, y);
    }
    if x >= w - r && y >= h - r {
        return in_corner(w - r, h - r, x, y);
    }
    true
}

/// 按钮 tooltip 文案（logo/齿轮无 tooltip；32-toolbar §6.6「全半角」「简体/繁体」等）。
fn button_tooltip(index: usize) -> Option<&'static str> {
    match index {
        TB_MODE => Some("中/英"),
        TB_WIDTH => Some("全半角"),
        TB_PUNCT => Some("中英文标点"),
        TB_SCRIPT => Some("简体/繁体"),
        TB_GEAR => Some("设置"),
        _ => None,
    }
}

unsafe extern "system" fn bar_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TIMER => {
            if let Some(w) = get_bar_mut(hwnd) {
                w.reconcile();
            }
            LRESULT(0)
        }
        WM_APP_REFRESH => {
            if let Some(w) = get_bar_mut(hwnd) {
                w.reconcile();
            }
            LRESULT(0)
        }
        WM_PAINT => {
            // ULW 内容不画窗口 DC：BeginPaint/EndPaint 成对校验更新区（防风暴）。
            let mut ps = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
            let hdc = windows::Win32::Graphics::Gdi::BeginPaint(hwnd, &mut ps);
            if !hdc.is_invalid() {
                let _ = windows::Win32::Graphics::Gdi::EndPaint(hwnd, &ps);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_NCHITTEST => {
            let (sx, sy) = client_pos(lparam); // 屏幕坐标
            let mut rc = RECT::default();
            if unsafe { GetWindowRect(hwnd, &mut rc) }.is_err() {
                return LRESULT(HTCLIENT as isize);
            }
            let (x, y) = (sx - rc.left, sy - rc.top);
            let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
            let radius = match get_bar_mut(hwnd) {
                Some(wnd) => wnd.theme.corner_radius * wnd.scale(),
                None => 0.0,
            };
            if in_rounded_rect(x, y, w, h, radius) {
                LRESULT(HTCLIENT as isize)
            } else {
                LRESULT(HTTRANSPARENT as isize)
            }
        }
        WM_MOUSEMOVE => {
            let (x, y) = client_pos(lparam);
            if let Some(w) = get_bar_mut(hwnd) {
                // 拖拽中：按屏幕坐标移动窗口；否则：悬停更新。
                if w.drag_offset.is_some() {
                    w.drag_move();
                } else {
                    // TrackMouseEvent 重挂 WM_MOUSELEAVE（离开窗口清悬停）。
                    let mut tme = TRACKMOUSEEVENT {
                        cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        ..Default::default()
                    };
                    let _ = unsafe { TrackMouseEvent(&mut tme) };
                    w.on_hover(x, y);
                }
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            if let Some(w) = get_bar_mut(hwnd) {
                w.hover = None;
                w.tip.hide();
                w.repaint();
            }
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let (x, y) = client_pos(lparam);
            if let Some(w) = get_bar_mut(hwnd) {
                match hit_test(&w.rows, x, y) {
                    // 空白区或 logo（拖动把手）：开始拖拽（§6.6 任意非按钮空白区拖动）。
                    None | Some(TB_LOGO) => {
                        w.start_drag();
                    }
                    Some(i) => {
                        // 功能按钮按下（点击反馈）+ 鼠标抬起时执行。
                        w.pressed = Some(i);
                        w.repaint();
                    }
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if let Some(w) = get_bar_mut(hwnd) {
                if let Some(i) = w.pressed.take() {
                    w.repaint();
                    // 释放捕获（若有）
                    if w.drag_offset.is_none() {
                        let _ = unsafe { ReleaseCapture() };
                    }
                    // 点击按钮（抬起执行，贴合 Windows 惯例）
                    w.on_click(i);
                }
                if w.drag_offset.is_some() {
                    w.end_drag();
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            // 工具条线程退出清理触发（Drop 销毁窗口）；消息循环以 WM_QUIT 结束。
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

unsafe extern "system" fn tip_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
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
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize), // tooltip 全程点击穿透
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

// ===== 持久化 toolbar.json + 位置工具 =====

/// toolbar.json 内容（显示偏好 + 位置；全局，daemon 唯一写者，§6.3）。
#[derive(Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct ToolbarPref {
    #[serde(default = "default_visible")]
    pub visible: bool,
    #[serde(default)]
    pub pos: Option<(i32, i32)>,
}

fn default_visible() -> bool {
    true
}

/// %LOCALAPPDATA%\iuv\toolbar.json（独立文件：不触发 config_epoch 热载噪声）。
fn pref_path() -> Option<std::path::PathBuf> {
    let base = std::env::var("LOCALAPPDATA")
        .ok()
        .or_else(|| std::env::var("APPDATA").ok().map(|a| format!("{a}\\Local")))
        .or_else(|| std::env::var("HOME").ok())?;
    Some(std::path::PathBuf::from(base).join("iuv").join("toolbar.json"))
}

/// 加载偏好（缺失/损坏 → 默认 visible=true、pos=None；绝不失败）。
/// 位置清洗（2026-08-21）：越界坐标（旧版本 32767 bug / 拖拽损坏 / 显示器拔除残留）
/// → 置 None（show 时用主屏右下角默认），避免工具栏渲染到屏幕外 = 隐形。
pub fn load_pref() -> ToolbarPref {
    let Some(path) = pref_path() else {
        return ToolbarPref {
            visible: true,
            pos: None,
        };
    };
    match std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str::<ToolbarPref>(&t).ok())
    {
        Some(mut p) => {
            if let Some((x, y)) = p.pos {
                // 越界判据：明显超出 Win32 虚拟桌面合理范围（-10000..40000）。
                if x < -10000 || x > 40000 || y < -10000 || y > 40000 {
                    p.pos = None;
                }
            }
            p
        }
        None => ToolbarPref {
            visible: true,
            pos: None,
        },
    }
}

/// 保存偏好：临时文件 + 先删后 rename 原子替换；失败不阻断（内存态已生效）。
pub fn save_pref(pref: &ToolbarPref) {
    let Some(path) = pref_path() else {
        return;
    };
    let Some(dir) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let tmp = dir.join("toolbar.json.tmp");
    match serde_json::to_string_pretty(pref)
        .map(|t| std::fs::write(&tmp, t))
    {
        Ok(Ok(())) => {
            let _ = std::fs::remove_file(&path);
            let _ = std::fs::rename(&tmp, &path);
        }
        _ => {
            log::log_line("[toolbar] 偏好写盘失败（内存态已生效）");
            let _ = std::fs::remove_file(&tmp);
        }
    }
}

/// 当前主题（从 daemon 配置快照；默认浅色）。
fn current_theme(state: &DaemonState) -> Theme {
    match state.config.lock().unwrap_or_else(|p| p.into_inner()).theme.as_str() {
        "dark" => theme_dark(),
        _ => theme_light(),
    }
}

/// 主屏右下角默认位置（§7 决策点 6）。
fn default_pos(w: i32, h: i32) -> (i32, i32) {
    // SAFETY: MonitorFromPoint 纯查询；GetMonitorInfoW 输出缓冲已初始化。
    let monitor = unsafe {
        MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTONEAREST)
    };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let area = if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        info.rcWork
    } else {
        RECT {
            left: 0,
            top: 0,
            right: 32767,
            bottom: 32767,
        }
    };
    (
        area.right - w - DEFAULT_MARGIN,
        area.bottom - h - DEFAULT_MARGIN,
    )
}

/// tooltip 定位：光标偏移 + 工作区内收（光标所在显示器优先）。
fn clamp_to_work(x: i32, y: i32, w: i32, h: i32) -> (i32, i32) {
    let mut area = RECT {
        left: 0,
        top: 0,
        right: 32767,
        bottom: 32767,
    };
    // SAFETY: MonitorFromPoint 纯查询；GetMonitorInfoW 输出缓冲已初始化。
    let monitor = unsafe { MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        area = info.rcWork;
    }
    let x = if x + w > area.right { area.right - w } else { x };
    let y = if y + h > area.bottom { area.bottom - h } else { y };
    (x.max(area.left), y.max(area.top))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn button_tooltip_mapping() {
        assert_eq!(button_tooltip(TB_MODE), Some("中/英"));
        assert_eq!(button_tooltip(TB_WIDTH), Some("全半角"));
        assert_eq!(button_tooltip(TB_PUNCT), Some("中英文标点"));
        assert_eq!(button_tooltip(TB_SCRIPT), Some("简体/繁体"));
        assert_eq!(button_tooltip(TB_GEAR), Some("设置"));
        assert_eq!(button_tooltip(TB_LOGO), None, "logo 无 tooltip");
    }

    #[test]
    fn in_rounded_rect_center_and_corners() {
        assert!(in_rounded_rect(50, 50, 200, 100, 8.0), "中心命中");
        assert!(!in_rounded_rect(1, 1, 200, 100, 8.0), "角尖圆弧外穿透");
        assert!(in_rounded_rect(100, 99, 200, 100, 8.0), "下边缘中点命中");
        assert!(in_rounded_rect(0, 5, 10, 10, 100.0), "半径钳制后左缘命中");
    }

    #[test]
    fn toggle_value_flips() {
        // 双态翻转：0→1、1→0（纯逻辑验证，与 on_click 的 `1-cur` 一致）。
        for cur in 0..=1u8 {
            assert_eq!(1 - cur, cur ^ 1);
        }
    }
}
