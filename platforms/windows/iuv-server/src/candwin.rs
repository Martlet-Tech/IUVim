//! 服务端自绘候选窗（49 §2/§4.5.3）：会话级 UI 线程 + ULW 窗口。
//!
//! P4 前候选窗由 TSF 客户端自绘（每键几 KB 候选载荷过线）；本模块把渲染搬进
//! iuv-server——字体只此一份、热路径候选载荷裁撤。结构 = [`CandwinHandle`]
//! （会话持有的命令发送端）+ 每连接一个 UI 线程（命令循环；窗口建在本线程，
//! 满足 Win32 线程亲和）。渲染管线与 iuv-tsf candwin.rs 同源：iuv-ui 软件光栅
//! → `UpdateLayeredWindow` per-pixel alpha；无边框/置顶/不抢焦点。
//!
//! 与客户端版的差异：
//! - **DPI 由服务端自算**（caret 所在显示器 `GetDpiForMonitor`；客户端只报
//!   屏幕物理坐标）；进程需 PMv2（main 启动时置位）。
//! - **点击选词经 Ctl 控制面闭环**（③：ConnSender 主动 REQ `CandidateClick`
//!   → 客户端 TSF 线程以 Digit 键走远端会话 → 正常上屏）；悬停高亮保留（纯视觉）。
//! - 抑制判定（`candidate_owner_apps` 命中 → 客户端游戏桥自绘）在会话层完成，
//!   命中的连接不启动窗口线程。

use std::mem::size_of;
use std::sync::mpsc::{self, Sender, TryRecvError};
use std::sync::Arc;
use std::time::Duration;

use iuv_proto::S2C;
use iuv_win::transport::ConnSender;

use windows::Win32::Foundation::{HANDLE, WAIT_EVENT, WAIT_OBJECT_0};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::System::Threading::CreateEventW;
use windows::Win32::System::Threading::SetEvent;
use windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use windows::Win32::UI::WindowsAndMessaging::MsgWaitForMultipleObjectsEx;
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetWindowRect, PeekMessageW, SetWindowPos, ShowWindow, TranslateMessage,
    HTCLIENT, HTTRANSPARENT, MA_NOACTIVATE, MSG, MWMO_INPUTAVAILABLE, PM_REMOVE, QS_ALLINPUT,
    SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SW_HIDE, SW_SHOWNA, WM_ERASEBKGND, WM_MBUTTONDOWN,
    WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NCHITTEST, WM_PAINT, WM_RBUTTONDOWN,
};
// WM_MOUSELEAVE 在 windows-rs 0.62 中位于 Controls 模块（值 0x02A3），本地定义。
const WM_MOUSELEAVE: u32 = 675;
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};

use iuv_ui::{
    hit_test, render_candidate, update_position, CaretRect, Surface, TextRenderer, Theme,
    UiSnapshot,
};

const CLASS_NAME: PCWSTR = w!("IuvServerCandidateWindow");

/// 跳变阈值（px，欧氏距离）：候选窗可见时，新锚点距**上一次定位用的锚点**位移
/// 超过该值即判为"远跳"而不做平滑移动。
///
/// 为什么需要它：客户端锚点异常时可能一次性位移数百像素（2026-10-09 实测最大
/// 633px；455px = 最大化前后宽度差）。`SetWindowPos` 会把窗口从旧位置**平滑滑**
/// 到新位置——用户看到的就是"候选栏高速漂移"。旧版客户端自绘候选窗有同款
/// `JUMP_THRESHOLD = 150.0` 检测（点击远处/拖拽跨屏就整窗隐藏待下一键重现），
/// 随 2026-10-02 D3 清扫本地候选窗一并退役；服务端版从零实现时漏掉了，本常量
/// 把它补回来。
///
/// 定位：这是**兜底**而非根因——首键错锚点的真凶在客户端会话首拍的陈旧量取
/// （iuv-tsf `Composition::first_stroke`，52 号）。根因修复后本检测只应响应
/// 真实的窗口几何跳变/点击远处；若打字路径频繁出现"远跳"日志即为回归信号。
///
/// 阈值 150 沿用客户端版取值：正常打字锚点恒定（Δ=0），换行/滚动是小步平移
/// （每行 ~20px 行高，远小于 150），远跳只可能来自窗口几何突变或光标真的被
/// 搬到了远处——两种都该"瞬移"而不是"滑过去"。
const JUMP_THRESHOLD: f64 = 150.0;

/// 两点欧氏距离（px；用于跳变判定）。
fn jump_distance(a: CaretRect, b: CaretRect) -> f64 {
    let dx = (a.x - b.x) as f64;
    let dy = (a.y - b.y) as f64;
    (dx * dx + dy * dy).sqrt()
}

/// 候选窗命令（会话线程 → UI 线程；latest-wins 由命令自身语义保证——每条命令
/// 携带完整快照/位置，UI 线程按序应用最后一条即最新状态）。
pub(crate) enum CandwinCmd {
    Show { snap: UiSnapshot, caret: CaretRect },
    Update { snap: UiSnapshot },
    MoveTo { caret: CaretRect },
    Hide,
    SetTheme(Theme),
}

/// 会话持有的候选窗句柄（clone 廉价；全部 sender drop → UI 线程退出并销毁窗口）。
pub(crate) struct CandwinHandle {
    tx: Sender<CandwinCmd>,
    wake: Arc<WakeEvent>,
}

/// 唤醒事件（真机实锤 2026-09-27：UI 线程纯 `recv()` 阻塞 = 无消息泵 →
/// WM_SETCURSOR 等 SendMessage 无响应 → 悬停漏斗；hover/圆角穿透全死）。
/// 现在 sender 发命令后 SetEvent 唤醒，UI 线程醒后排空命令 + 泵窗口消息。
struct WakeEvent(HANDLE);
unsafe impl Send for WakeEvent {}
unsafe impl Sync for WakeEvent {}

impl WakeEvent {
    fn new() -> WakeEvent {
        // SAFETY: 自动重置事件；句柄由 Arc 引用计数管理（最后一个 drop 关闭）。
        let h = unsafe { CreateEventW(None, false, false, None) }.unwrap_or_default();
        WakeEvent(h)
    }
    fn set(&self) {
        // SAFETY: 事件句柄有效（Arc 存活保证）。
        let _ = unsafe { SetEvent(self.0) };
    }
}
impl Drop for WakeEvent {
    fn drop(&mut self) {
        // SAFETY: 最后一个属主关闭；UI 线程此后 MsgWait 返回失败 → 回环见 recv
        // 断开 → 退出（事件句柄值不会在旧线程仍使用时被复用为新事件——复用窗口
        // 内 MsgWait 只会空醒/失败，均回环检查后退出）。
        let _ = unsafe { windows::Win32::Foundation::CloseHandle(self.0) };
    }
}

impl CandwinHandle {
    /// 启动 UI 线程（每连接一个；隐藏态零渲染开销）。
    /// `sender` = 本连接发送器（点击选词经 S2C::Ctl 回客户端，③ 点击闭环）。
    pub(crate) fn spawn(theme: Theme, sender: ConnSender) -> CandwinHandle {
        let (tx, rx) = mpsc::channel::<CandwinCmd>();
        let wake = Arc::new(WakeEvent::new());
        let spawned = std::thread::Builder::new()
            .name("iuv-server-candwin".into())
            .spawn({
                let wake = wake.clone();
                move || run_ui_thread(rx, theme, wake, sender)
            });
        if let Err(e) = spawned {
            iuv_win::logger::log_line(&format!("[candwin] UI 线程创建失败（{e}）→ 该连接无候选窗"));
        }
        CandwinHandle { tx, wake }
    }

    /// 发命令 + 唤醒 UI 线程（UI 线程已退出/未建 → 静默丢弃）。
    pub(crate) fn send(&self, cmd: CandwinCmd) {
        let _ = self.tx.send(cmd);
        self.wake.set();
    }
}

fn run_ui_thread(
    rx: mpsc::Receiver<CandwinCmd>,
    theme: Theme,
    wake: Arc<WakeEvent>,
    sender: ConnSender,
) {
    let mut wnd = ServerCandwin::new(theme, sender);
    // 建窗 + 字体渲染器预热在连接建立时完成（UI 线程启动即建，隐藏态零渲染
    // 开销）：惰性首显会把渲染器装配（字体库）压进打字关键路径，首键候选帧
    // 凭空多出几十~几百 ms。
    wnd.ensure_window();
    loop {
        // 排空命令（按序应用，最后一条即最新）。
        let mut disconnected = false;
        loop {
            match rx.try_recv() {
                Ok(cmd) => match cmd {
                    CandwinCmd::Show { snap, caret } => wnd.show(snap, caret),
                    CandwinCmd::Update { snap } => wnd.update(snap),
                    CandwinCmd::MoveTo { caret } => wnd.move_to(caret),
                    CandwinCmd::Hide => wnd.hide(),
                    CandwinCmd::SetTheme(theme) => wnd.set_theme(theme),
                },
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    disconnected = true;
                    break;
                }
            }
        }
        if disconnected {
            // 全部 sender 已 drop（会话销毁）：隐藏并随线程退出自然析构窗口。
            wnd.hide();
            return;
        }
        // 等唤醒（命令事件 / 窗口消息——跨线程 SendMessage 由 PeekMessage 隐式
        // 分发，WM_SETCURSOR 悬停漏斗根除）。事件被关闭（销毁竞态）→ 回环退出。
        // SAFETY: 事件句柄 Arc 存活；单事件等待。
        let w = unsafe {
            MsgWaitForMultipleObjectsEx(Some(&[wake.0]), u32::MAX, QS_ALLINPUT, MWMO_INPUTAVAILABLE)
        };
        if w == WAIT_EVENT(WAIT_OBJECT_0.0 + 1) {
            // 窗口消息：泵到排空（hover / NCHITTEST / 光标）。
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

/// ULW 自绘候选窗（服务端版；渲染/定位/命中与 iuv-tsf candwin.rs 同源）。
struct ServerCandwin {
    layered: iuv_win::LayeredWindow,
    snap: UiSnapshot,
    visible: bool,
    /// 最近一次定位用的光标锚点。
    last_caret: Option<CaretRect>,
    /// 最近一次布局的候选矩形列表（悬停命中测试用）。
    rows: Vec<iuv_ui::layout::Rect>,
    /// 鼠标悬停行（纯视觉）。
    hover_row: Option<usize>,
    /// 本连接发送器（点击选词 → S2C::Ctl(CandidateClick) 回客户端）。
    sender: ConnSender,
    theme: Theme,
    text: Option<TextRenderer>,
    ulw: iuv_win::UlwSurface,
    /// 本帧 DPI（caret 所在显示器；0 = 未知 → 96）。
    dpi: u32,
}

impl ServerCandwin {
    fn new(theme: Theme, sender: ConnSender) -> Self {
        ServerCandwin {
            layered: iuv_win::LayeredWindow::new(),
            snap: UiSnapshot::default(),
            visible: false,
            last_caret: None,
            rows: Vec::new(),
            hover_row: None,
            sender,
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
        // SAFETY: WS_EX_TOPMOST|TOOLWINDOW|NOACTIVATE 保证置顶且不抢焦点；
        // WS_EX_LAYERED = per-pixel alpha 合成（UpdateLayeredWindow 前置条件）。
        let outer = self as *mut Self;
        self.layered.create(
            CLASS_NAME,
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(
                windows::Win32::UI::WindowsAndMessaging::WS_EX_TOPMOST.0
                    | windows::Win32::UI::WindowsAndMessaging::WS_EX_TOOLWINDOW.0
                    | windows::Win32::UI::WindowsAndMessaging::WS_EX_NOACTIVATE.0
                    | windows::Win32::UI::WindowsAndMessaging::WS_EX_LAYERED.0,
            ),
            wnd_proc,
            outer,
            "server-candwin",
        );
        if self.layered.is_created() {
            self.text = Some(TextRenderer::new());
        }
    }

    fn frame(&mut self) -> Option<Surface> {
        if self.snap.reading.is_empty() && self.snap.candidates.is_empty() {
            return None;
        }
        let scale = self.scale();
        let hover = self.hover_row;
        let (surf, rows) = {
            let text = self.text.as_mut()?;
            render_candidate(&self.snap, &self.theme, scale, text, hover)
        };
        if surf.w == 0 || surf.h == 0 {
            return None;
        }
        self.rows = rows;
        Some(surf)
    }

    fn show(&mut self, snap: UiSnapshot, caret: CaretRect) {
        self.dpi = dpi_for_caret(caret);
        if snap.reading.is_empty() && snap.candidates.is_empty() {
            self.hide();
            return;
        }
        self.snap = snap;
        self.last_caret = Some(caret);
        iuv_win::logger::log_line(&format!(
            "[candwin] show：锚点 ({},{})，候选 {} 项，reading={:?}",
            caret.x,
            caret.y,
            self.snap.candidates.len(),
            self.snap.reading
        ));
        self.show_with_current_snapshot(caret);
    }

    fn update(&mut self, snap: UiSnapshot) {
        self.snap = snap;
        if self.layered.hwnd.is_invalid() || !self.visible {
            return;
        }
        // 打字期只换内容、窗口不动（47 号语义）：传 None 走 `update_position` 原位
        // 保留，仅当新尺寸越出工作区底/右缘才用 last_caret 重定位。传 `Some(caret)`
        // 会每键用 `position_in_area` 从锚点重算上/下侧——光标贴近屏底时，窗口高度
        // 前后键跨过「下方放得下」阈值（候选数 / 页码行变化）→ 上/下来回翻 = 上下弹跳。
        self.apply_layout_and_pos(None);
    }

    fn move_to(&mut self, caret: CaretRect) {
        if self.layered.hwnd.is_invalid() || !self.visible {
            return;
        }
        // 跳变判定（治本 C）：与**上一次定位用的锚点**比，位移过大就不平滑移动，
        // 改为"整窗隐藏 → 按新锚点重定位 → 原样重现"。用户看到的是一次瞬移/重弹，
        // 而不是一行从旧位置高速滑到新位置（那正是被投诉的"候选栏漂移"）。
        let jumped = self
            .last_caret
            .map(|prev| jump_distance(prev, caret) > JUMP_THRESHOLD)
            .unwrap_or(false);
        self.dpi = dpi_for_caret(caret);
        if jumped {
            let prev = self.last_caret.unwrap_or(caret);
            iuv_win::logger::log_line(&format!(
                "[candwin] 锚点远跳：({},{}) → ({},{})，距 {:.0}px > {:.0}px，隐藏后按新锚点重现",
                prev.x,
                prev.y,
                caret.x,
                caret.y,
                jump_distance(prev, caret),
                JUMP_THRESHOLD
            ));
            // 先藏（避免旧位置残留一帧），再按新锚点重新上传并显示。
            // 注意顺序：hide() 会把 last_caret 清 None，所以**先 hide 再回填**，
            // 否则这一拍的定位基准丢失，下一次跳变判定会以 None 起算（漏判一次）。
            self.hide();
            self.last_caret = Some(caret);
            self.show_with_current_snapshot(caret);
            return;
        }
        self.last_caret = Some(caret);
        // SAFETY: GetWindowRect 读当前窗口矩形
        let mut rc = RECT::default();
        if unsafe { GetWindowRect(self.layered.hwnd, &mut rc) }.is_err() {
            return;
        }
        let w = rc.right - rc.left;
        let h = rc.bottom - rc.top;
        let (x, y) = position_for(caret, w, h);
        let prev = (rc.left, rc.top);
        if prev != (x, y) {
            iuv_win::logger::log_line(&format!(
                "[candwin] 平移：({},{}) → ({},{})（锚点 {},{})",
                prev.0, prev.1, x, y, caret.x, caret.y
            ));
        }
        // SAFETY: 仅移动（SWP_NOSIZE），不激活
        let _ = unsafe {
            SetWindowPos(
                self.layered.hwnd,
                None,
                x,
                y,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOSIZE,
            )
        };
    }

    /// 用当前快照 + 给定锚点重新走一次"定位 → 上传 → 显示"（远跳重现用）。
    /// 与 [`Self::show`] 同路径，但不重置快照（快照本就未变，变的只是位置）。
    fn show_with_current_snapshot(&mut self, caret: CaretRect) {
        if self.layered.hwnd.is_invalid() {
            self.ensure_window();
            if self.layered.hwnd.is_invalid() {
                return; // 建窗失败：静默降级
            }
        }
        self.apply_layout_and_pos(Some(caret));
        // SAFETY: SW_SHOWNA 显示但不激活——绝不抢焦点
        let _ = unsafe { ShowWindow(self.layered.hwnd, SW_SHOWNA) };
        self.visible = true;
    }

    fn hide(&mut self) {
        if self.visible {
            iuv_win::logger::log_line("[candwin] hide");
        }
        self.visible = false;
        self.hover_row = None;
        // 作废定位锚点：下次 show 应是"重新弹出"，不该拿隐藏前的旧锚点做跳变比较
        // （否则重现路径里 last_caret 会在 show() 里被立刻覆盖 —— 这里先清保证语义单纯）。
        self.last_caret = None;
        if !self.layered.hwnd.is_invalid() {
            // SAFETY: 隐藏候选窗
            let _ = unsafe { ShowWindow(self.layered.hwnd, SW_HIDE) };
        }
    }

    fn set_theme(&mut self, theme: Theme) {
        self.theme = theme;
        self.repaint();
    }

    fn apply_layout_and_pos(&mut self, caret: Option<CaretRect>) {
        let Some(surf) = self.frame() else {
            return;
        };
        let w = surf.w as i32;
        let h = surf.h as i32;
        let (x, y) = match caret {
            Some(c) => position_for(c, w, h),
            None => {
                // SAFETY: GetWindowRect 读当前窗口矩形
                let mut rc = RECT::default();
                let _ = unsafe { GetWindowRect(self.layered.hwnd, &mut rc) };
                update_position(
                    (rc.left, rc.top),
                    w,
                    h,
                    work_area_for(self.layered.hwnd),
                    self.last_caret,
                )
            }
        };
        self.ulw
            .upload(self.layered.hwnd, &surf, x, y, w, h, "[server-candwin]");
    }

    /// 原位重绘（悬停高亮 / 主题热载）。
    fn repaint(&mut self) {
        if self.layered.hwnd.is_invalid() || !self.visible {
            return;
        }
        let Some(surf) = self.frame() else {
            return;
        };
        // SAFETY: GetWindowRect 读当前窗口矩形
        let mut rc = RECT::default();
        if unsafe { GetWindowRect(self.layered.hwnd, &mut rc) }.is_err() {
            return;
        }
        self.ulw.upload(
            self.layered.hwnd,
            &surf,
            rc.left,
            rc.top,
            rc.right - rc.left,
            rc.bottom - rc.top,
            "[server-candwin]",
        );
    }
}

/// caret 所在显示器的有效 DPI（进程需 PMv2；查询失败回退 96）。
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
    // SAFETY: 输出指针有效。
    // SAFETY: 输出指针有效（windows 0.62 签名带第四个 *mut u32 reserved 位）。
    if unsafe { GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut dpi, &mut 0u32 as *mut u32) }
        .is_err()
        || dpi == 0
    {
        return 96;
    }
    dpi
}

/// caret 所在显示器工作区（与客户端版同源；失败兜底近乎全屏）。
fn work_area_for(hwnd: HWND) -> iuv_ui::layout::Area {
    // SAFETY: MonitorFromWindow 纯查询；GetMonitorInfoW 输出缓冲已初始化。
    let monitor =
        unsafe { windows::Win32::Graphics::Gdi::MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        rect_to_area(info.rcWork)
    } else {
        iuv_ui::layout::Area {
            left: 0,
            top: 0,
            right: 32767,
            bottom: 32767,
        }
    }
}

/// 按 caret 定位：取 caret 所在显示器物理工作区（副屏不 clamp 回主屏）。
fn position_for(caret: CaretRect, w: i32, h: i32) -> (i32, i32) {
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: MonitorFromPoint/GetMonitorInfoW 纯查询；输出缓冲已初始化。
    let area = {
        let monitor = unsafe {
            MonitorFromPoint(
                POINT {
                    x: caret.x,
                    y: caret.y,
                },
                MONITOR_DEFAULTTONEAREST,
            )
        };
        if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
            rect_to_area(info.rcWork)
        } else {
            iuv_ui::layout::Area {
                left: 0,
                top: 0,
                right: 32767,
                bottom: 32767,
            }
        }
    };
    iuv_ui::position_in_area(caret, w, h, area)
}

fn rect_to_area(rc: RECT) -> iuv_ui::layout::Area {
    iuv_ui::layout::Area {
        left: rc.left,
        top: rc.top,
        right: rc.right,
        bottom: rc.bottom,
    }
}

/// 圆角几何命中（与客户端版同源：圆角外 HTTRANSPARENT 点击穿透）。
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

/// 类窗口过程（与客户端版同源；无 WM_LBUTTONDOWN 选词——见模块注释）。
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    use windows::Win32::UI::WindowsAndMessaging::WM_LBUTTONDOWN;
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
        WM_NCHITTEST => {
            let (sx, sy) = iuv_win::LayeredWindow::client_pos(lparam);
            let mut rc = RECT::default();
            if GetWindowRect(hwnd, &mut rc).is_err() {
                return LRESULT(HTCLIENT as isize);
            }
            let (x, y) = (sx - rc.left, sy - rc.top);
            let (w, h) = (rc.right - rc.left, rc.bottom - rc.top);
            let radius = match unsafe { iuv_win::LayeredWindow::get_self::<ServerCandwin>(hwnd) } {
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
            let (x, y) = iuv_win::LayeredWindow::client_pos(lparam);
            if let Some(wnd) =
                unsafe { iuv_win::LayeredWindow::get_self_mut::<ServerCandwin>(hwnd) }
            {
                // SAFETY: hwnd 由消息循环保证有效；单次调用无副作用。
                let mut tme = TRACKMOUSEEVENT {
                    cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    ..Default::default()
                };
                let _ = unsafe { TrackMouseEvent(&mut tme) };
                let row = hit_test(&wnd.rows, x, y).filter(|r| *r < wnd.snap.candidates.len());
                if row != wnd.hover_row {
                    wnd.hover_row = row;
                    wnd.repaint();
                }
            }
            LRESULT(0)
        }
        WM_MOUSELEAVE => {
            if let Some(wnd) =
                unsafe { iuv_win::LayeredWindow::get_self_mut::<ServerCandwin>(hwnd) }
            {
                if wnd.hover_row.take().is_some() {
                    wnd.repaint();
                }
            }
            LRESULT(0)
        }
        // 点击选词（③ 闭环）：命中行 → 后台线程经 ConnSender 发 S2C::Ctl
        // （CandidateClick），客户端 TSF 线程以 Digit(row+1) 走远端会话。短命线程
        // 承担阻塞等待，UI 线程保持响应（悬停/重绘不被 3s 截止拖住）。
        WM_LBUTTONDOWN => {
            if let Some(wnd) = unsafe { iuv_win::LayeredWindow::get_self::<ServerCandwin>(hwnd) } {
                let (x, y) = iuv_win::LayeredWindow::client_pos(lparam);
                if let Some(row) =
                    hit_test(&wnd.rows, x, y).filter(|r| *r < wnd.snap.candidates.len())
                {
                    let sender = wnd.sender.clone();
                    let _ = std::thread::Builder::new()
                        .name("iuv-candwin-click".into())
                        .spawn(move || {
                            let _ = sender.request(
                                S2C::Ctl {
                                    cmd: iuv_proto::CtlCmd::CandidateClick(row as u8),
                                },
                                Duration::from_secs(3),
                            );
                        });
                }
            }
            LRESULT(0)
        }
        WM_RBUTTONDOWN | WM_MBUTTONDOWN => LRESULT(0),
        _ => iuv_win::LayeredWindow::default_wnd_proc(hwnd, msg, wparam, lparam),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn in_rounded_rect_matches_client_version() {
        assert!(in_rounded_rect(50, 50, 200, 100, 8.0), "中心必然命中");
        assert!(!in_rounded_rect(1, 1, 200, 100, 8.0), "左上角圆弧外穿透");
        assert!(in_rounded_rect(8, 8, 200, 100, 8.0), "圆弧边界命中");
    }

    fn caret(x: i32, y: i32) -> CaretRect {
        CaretRect { x, y, w: 0, h: 21 }
    }

    #[test]
    fn jump_distance_is_euclidean() {
        assert_eq!(jump_distance(caret(0, 0), caret(3, 4)), 5.0);
        assert_eq!(jump_distance(caret(100, 200), caret(100, 200)), 0.0);
    }

    #[test]
    fn jump_threshold_separates_typing_from_geometry_change() {
        // 正常打字：锚点恒定（Δ=0）→ 远小于阈值，走平滑移动（其实不动）。
        assert!(jump_distance(caret(800, 681), caret(800, 681)) <= JUMP_THRESHOLD);
        // 换行/滚动：一个小步（行高 ~20px）→ 仍是平滑移动。
        assert!(jump_distance(caret(800, 681), caret(800, 701)) <= JUMP_THRESHOLD);
        // 窗口几何切换：实测最大 455px（= 最大化前后宽度差 1920-1465）→ 判为远跳。
        assert!(jump_distance(caret(1013, 481), caret(558, 481)) > JUMP_THRESHOLD);
        // 斜跳：实测 (−213, +200) ≈ 292px → 判为远跳。
        assert!(jump_distance(caret(1013, 481), caret(800, 681)) > JUMP_THRESHOLD);
    }
}
