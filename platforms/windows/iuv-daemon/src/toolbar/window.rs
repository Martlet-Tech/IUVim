//! 工具条主窗口（§6；P2.6 自 toolbar.rs 拆出）：渲染/交互/拖拽/看板判定。
//! 仅工具条线程触碰；wnd_proc 经 GWLP_USERDATA 取回本结构。

use std::mem::size_of;
use std::sync::{Arc, Mutex};

use iuv_ui::{
    hit_test, render_toolbar, TextRenderer, Theme, ToolbarIcons, ToolbarSpec, TB_GEAR, TB_LOGO,
    TB_MODE, TB_PUNCT, TB_SCRIPT, TB_WIDTH,
};
use iuv_ui::layout::Rect;
use iuv_win::{
    ctl_pipe_name, CtlClient, CtlCmd, CtlResult, PipeClient, Request, CTL_FIELD_MODE,
    CTL_FIELD_PUNCT, CTL_FIELD_SCRIPT, CTL_FIELD_WIDTH,
};
use iuv_win::UlwSurface;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetDC, GetDeviceCaps, ReleaseDC, LOGPIXELSY};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, TrackMouseEvent, TRACKMOUSEEVENT, TME_LEAVE,
};
use windows::Win32::UI::WindowsAndMessaging::{
    DefWindowProcW, DestroyWindow, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect,
    GetWindowThreadProcessId, SetWindowLongPtrW, SetWindowPos, ShowWindow, SWP_NOACTIVATE,
    SWP_NOCOPYBITS, SWP_NOSIZE, SWP_NOZORDER, SW_HIDE, SW_SHOWNA, GWLP_USERDATA, HTCLIENT,
    HTTRANSPARENT, MA_NOACTIVATE, WM_DESTROY, WM_ERASEBKGND, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NCHITTEST, WM_PAINT, WM_TIMER,
};

use super::prefs::{save_pref, ToolbarPref};
use super::tooltip::TooltipWindow;
use super::{
    button_tooltip, clamp_to_work, client_pos, create_window, cursor_screen, current_theme,
    default_pos, in_rounded_rect, Shared, CLASS_BAR, WM_APP_REFRESH, WM_MOUSELEAVE,
};
use crate::log;
use crate::state::DaemonState;

/// 工具条窗口（仅工具条线程触碰；wnd_proc 经 GWLP_USERDATA 取回）。
pub(super) struct ToolbarWindow {
    pub(super) hwnd: HWND,
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
    pub(super) fn new(
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
            sh.focused.and_then(|f| sh.instances.get(&f).copied())
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
                match CtlClient::connect(&name)
                    .and_then(|c| c.request(&CtlCmd::SetState { field, value }))
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

/// 工具条窗口过程：定时器/刷新 → reconcile；ULW 内容不画窗口 DC（WM_PAINT 只校验）；
/// 圆角外点击穿透（WM_NCHITTEST）；悬停高亮 + tooltip；空白区/logo 拖拽；按下反馈 + 抬起执行。
pub(super) unsafe extern "system" fn bar_wnd_proc(
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