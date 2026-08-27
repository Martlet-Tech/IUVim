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
//! 显隐治理（40-toolbar-show-hide-governance.md 纯信号模型定稿）：管道线程只把
//! Request **入 FIFO**（不丢不弃）+ PostMessage 唤醒；工具条线程 drain 串行消费，
//! 每条消息同步执行完显/隐动作再取下一条。判定零前台查询——TSF 线程焦点信号
//! 即真相源：`Active(true)`（SetThreadFocus/Register 链）= 绑定该实例并立即显示，
//! `Active(false)`（KillThreadFocus/OPENCLOSE 关）= 绑定者本人则立即隐藏。
//! 消息时序颠倒自收敛（[B真,A假] 与 [A假,B真] 终态一致）。无定时器、无前台查询、
//! 无兜底（2026-08-22 用户裁决：好使就是好使，不好使就是没改对地方）。
//!
//! 持久化（§6.3）：独立 `toolbar.json`（`%LOCALAPPDATA%\iuv\`，显示偏好 + 位置），
//! 写盘复用「临时文件 + rename 原子替换」，失败不阻断（内存态已生效）。首次无位置 →
//! 主屏右下角（§7 决策点 6）。
//!
//! 交互（§6.6）：不抢焦点（NOACTIVATE + 点击穿透圆角外空白）、拖拽空白区移动 + 持久化、
//! 悬停 tooltip、**无自身右键菜单**（显隐唯一入口 = 语言栏「中/英」右键菜单）。
//! 全部失败静默降级（记日志，不 panic；守护进程硬性约定）。
//!
//! P2.6 拆分：宿主/线程骨架（本文件）+ 窗口 `window.rs` + tooltip `tooltip.rs` +
//! 持久化 `prefs.rs`。

mod prefs;
mod tooltip;
mod window;

use self::prefs::load_pref;
use self::tooltip::tip_wnd_proc;
use self::window::{bar_wnd_proc, ToolbarWindow};

use std::collections::{HashMap, VecDeque};use std::mem::size_of;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use iuv_core::ImeState;
use iuv_win::{Request, ToolbarSignal};
use iuv_ui::{theme_dark, theme_light, Theme, ToolbarIcons};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{GetLastError, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTONEAREST};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DispatchMessageW, GetCursorPos, GetMessageW, LoadCursorW, PostMessageW,
    PostThreadMessageW, RegisterClassExW, SetWindowLongPtrW, TranslateMessage, WM_APP,
    WM_QUIT, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, IDC_ARROW, MSG, WNDCLASSEXW, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
// WM_MOUSELEAVE 在 windows-rs 0.62 中位于 Controls 模块（值 0x02A3 = 675），本地定义。
const WM_MOUSELEAVE: u32 = 675;

use crate::state::DaemonState;
use crate::log;

/// 私有消息：FIFO 有新请求 → 唤醒工具条线程 drain（管道线程 PostMessage）。
const WM_APP_REFRESH: u32 = WM_APP + 41;

const CLASS_BAR: PCWSTR = w!("IuvToolbarWindow");
const CLASS_TIP: PCWSTR = w!("IuvTooltipWindow");

/// 工具栏默认边距（主屏右下角，§7 决策点 6）。
const DEFAULT_MARGIN: i32 = 12;

/// 实例表条目。
#[derive(Clone, Copy, Debug, Default)]
struct ToolbarInstance {
    state: ImeState,
    active: bool,
}

/// 工具栏共享状态（工具条线程独占写——管道线程只入队；Mutex 仅为跨线程读兜底）。
#[derive(Default)]
struct Shared {
    /// 实例表 `{pid:tid → {state, active}}`（§6.1）。
    instances: HashMap<(u32, u32), ToolbarInstance>,
    /// 当前绑定实例（最近一条 `Active{true}` 的 pid:tid；看板渲染其四态）。
    focused: Option<(u32, u32)>,
    /// 全局显示偏好（语言栏菜单开关；持久化）。
    visible: bool,
    /// 记忆位置（拖动后写；持久化；None = 首次默认主屏右下角）。
    pos: Option<(i32, i32)>,
}

/// 工具条事件（FIFO 载荷）：信号通道三消息 + 语言栏菜单开关 + 全局热键变更。
/// FocusGained/FocusLost/StateChanged 来自信号管道；ToggleVisible 来自数据面
/// 语言栏右键菜单（Request::ToggleToolbar）；HotkeysChanged 来自 daemon 主循环
/// （设置页保存 keymap 后入队，见 main.rs）。单队列保证全局顺序。
pub(super) enum BarEvent {
    /// 激活：绑定该实例并显示（渲染其四态）。
    FocusGained { pid: u32, tid: u32, state: ImeState },
    /// 失焦：绑定者本人 → 解绑并隐藏；他人 → 仅改表。
    FocusLost { pid: u32, tid: u32 },
    /// 态变更：更新四态（可见且绑定 → 重绘）。
    StateChanged { pid: u32, tid: u32, state: ImeState },
    /// 全局显隐偏好切换（语言栏菜单）。
    ToggleVisible,
    /// 全局热键变更（keymap 保存后）：工具条线程全量注销 + 重注册。
    HotkeysChanged,
}

/// 工具栏宿主（daemon 主线程持有；信号线程/管道线程经它入队，工具条线程 drain 消费）。
pub struct ToolbarHost {
    shared: Arc<Mutex<Shared>>,
    /// FIFO 事件队列（信号线程/管道线程 push、工具条线程 pop；串行消费不丢不弃）。
    pending: Arc<Mutex<VecDeque<BarEvent>>>,
    /// 工具条窗口句柄（工具条线程创建后注册；供 PostMessage 唤醒）。
    hwnd: AtomicUsize,
    /// 工具条线程 id（退出时 PostThreadMessage WM_QUIT）。
    thread_id: AtomicU32,
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
            pending: Arc::new(Mutex::new(VecDeque::new())),
            hwnd: AtomicUsize::new(0),
            thread_id: AtomicU32::new(0),
        });
        let t_shared = shared.clone();
        let t_state = state.clone();
        let t_icons = icons.clone();
        let t_pending = host.pending.clone();
        let (tx, rx) = std::sync::mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("iuv-toolbar".to_string())
            .spawn(move || toolbar_thread_main(t_shared, t_state, t_icons, t_pending, tx));
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

    /// 当前全局显隐偏好（`Request::GetToolbarVisible` 应答用；语言栏菜单项文案）。
    pub fn visible(&self) -> bool {
        self.shared
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .visible
    }

    /// 处理数据面管道请求：仅语言栏菜单开关（Request::ToggleToolbar）入队。
    /// 其余工具条类 Request（Register/Active/…）已由信号通道取代——一律不消费
    /// （返回 false 交调用方按未知请求处理；TSF 侧同版本起不再发送）。
    pub fn handle_request(&self, req: &Request) -> bool {
        if !matches!(req, Request::ToggleToolbar) {
            return false;
        }
        self.enqueue(BarEvent::ToggleVisible);
        true
    }

    /// 处理信号通道消息（激活/失焦/态变更）→ 入 FIFO。
    pub fn handle_signal(&self, sig: &ToolbarSignal) {
        let ev = match sig {
            ToolbarSignal::FocusGained { pid, tid, state } => BarEvent::FocusGained {
                pid: *pid,
                tid: *tid,
                state: *state,
            },
            ToolbarSignal::FocusLost { pid, tid } => BarEvent::FocusLost {
                pid: *pid,
                tid: *tid,
            },
            ToolbarSignal::StateChanged { pid, tid, state } => BarEvent::StateChanged {
                pid: *pid,
                tid: *tid,
                state: *state,
            },
        };
        self.enqueue(ev);
    }

    /// 全局热键变更通知（41-keymap-settings.md §4）：设置页保存 keymap 后由 daemon
    /// 主循环调用 → 工具条线程全量注销 + 按新配置重注册。
    pub fn hotkeys_changed(&self) {
        self.enqueue(BarEvent::HotkeysChanged);
    }

    /// 入队 + 唤醒工具条线程 drain。
    fn enqueue(&self, ev: BarEvent) {
        {
            let mut q = self.pending.lock().unwrap_or_else(|p| p.into_inner());
            q.push_back(ev);
        }
        self.wake();
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
        let tid = self.thread_id.load(Ordering::SeqCst);
        if tid != 0 {
            // SAFETY: PostThreadMessageW 向工具条线程投递 WM_QUIT（GetMessage 返回 0）。
            let _ = unsafe { PostThreadMessageW(tid, WM_QUIT, WPARAM(0), LPARAM(0)) };
        }
    }
}

// ===== 工具条线程 =====

/// 工具条线程主体：建窗口 → 注册句柄 → 消息循环（定时器轮询前台看板）。退出清理。
fn toolbar_thread_main(
    shared: Arc<Mutex<Shared>>,
    state: Arc<DaemonState>,
    icons: Arc<ToolbarIcons>,
    pending: Arc<Mutex<VecDeque<BarEvent>>>,
    tx: std::sync::mpsc::Sender<(usize, u32)>,
) {
    register_bar_class();
    register_tip_class();
    // 全局热键首注册（41-keymap-settings.md §4）：keymap 全局六动作主/备两槽。
    // 后续变更走 BarEvent::HotkeysChanged 全量重注册。先读配置再移交 state。
    let keymap = state
        .config
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .keymap
        .clone();
    let win = Box::new(ToolbarWindow::new(shared, state, icons, pending));
    if win.hwnd.is_invalid() {
        log::log_line("[toolbar] 建窗失败，工具条线程退出");
        return;
    }
    let hwnd = win.hwnd;
    // SAFETY: win 为 Box（地址稳定），线程存活期间有效；wnd_proc 经 GWLP_USERDATA 取回。
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, &*win as *const ToolbarWindow as isize) };
    {
        let (ok, fail) = crate::hotkey::register_all(hwnd, &keymap);
        log::log_line(&format!("[toolbar] 全局热键首注册：成功 {ok}，失败 {fail}"));
    }
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

// ===== 窗口类注册 + 创建 + 共享工具 =====

fn register_bar_class() {
    static R: OnceLock<()> = OnceLock::new();
    R.get_or_init(|| register_class(CLASS_BAR, Some(bar_wnd_proc)));
}

fn register_tip_class() {
    static R: OnceLock<()> = OnceLock::new();
    R.get_or_init(|| register_class(CLASS_TIP, Some(tip_wnd_proc)));
}

fn register_class(
    name: PCWSTR,
    proc: Option<unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT>,
) {
    // SAFETY: 类名静态宽字符串；失败仅记日志。
    unsafe {
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: proc,
            // 类默认光标 = 箭头：hCursor 为 NULL 时 DefWindowProc 不设光标，悬停工具条
            // 残留上一窗口的形状（实测忙等漏斗）。功能钮手指头在 bar_wnd_proc 覆盖。
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH::default(),
            lpszMenuName: PCWSTR::null(),
            lpszClassName: name,
            ..Default::default()
        };
        if RegisterClassExW(&class) == 0 {
            let err = GetLastError();
            if err != ERROR_CLASS_ALREADY_EXISTS {
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
    use iuv_ui::{TB_GEAR, TB_MODE, TB_PUNCT, TB_SCRIPT, TB_WIDTH};
    match index {
        TB_MODE => Some("中/英"),
        TB_WIDTH => Some("全半角"),
        TB_PUNCT => Some("中英文标点"),
        TB_SCRIPT => Some("简体/繁体"),
        TB_GEAR => Some("设置"),
        _ => None,
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
    let monitor = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTONEAREST) };
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
    use iuv_ui::{TB_GEAR, TB_LOGO, TB_MODE, TB_PUNCT, TB_SCRIPT, TB_WIDTH};

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