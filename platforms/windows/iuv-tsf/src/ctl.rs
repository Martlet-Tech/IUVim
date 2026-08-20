//! 反向控制端点（32-status-toolbar.md §4.2/§4.3）：per-实例 accept 线程 + 隐藏消息窗。
//!
//! daemon 点工具栏按钮 → 按需连入 `\\.\pipe\iuv-ctl-<pid>-<tid>`（§4.2 按需连接，
//! 一请求一连接）→ 本模块 accept 线程收到 `CtlCmd` → **跨线程分发**到 TSF 线程
//! （§4.3：隐藏消息窗 + `PostMessage(WM_APP_TOOLBAR_CMD)`，随应用消息泵执行）→
//! TSF 线程 wndproc 应用（写 OPENCLOSE / 改运行时 / 会话刷新）→ 信号控制线程 →
//! 控制线程写响应帧 → 断开。
//!
//! 端点生命周期 = TextService 实例（Activate 起，Deactivate/Drop 停）。accept 线程
//! 阻塞在 `ConnectNamedPipe`，Deactivate 停线程 = `CloseHandle` 中断 + join。
//! 全部失败静默降级（记日志，不 panic；iuv-tsf 硬性约定）。

use std::mem::size_of;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::Duration;

use iuv_data::{ctl_pipe_name, CtlCmd, CtlResult, CtlServer};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_CLASS_ALREADY_EXISTS, HANDLE, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowLongPtrW, PostMessageW,
    RegisterClassExW, SetWindowLongPtrW, GWLP_USERDATA, HWND_MESSAGE, WM_APP, WNDCLASS_STYLES,
    WNDCLASSEXW, WS_POPUP,
};

use crate::log::log_line;

/// TSF 线程应用命令的私有消息（accept 线程 PostMessage 唤醒）。
pub(crate) const WM_APP_TOOLBAR_CMD: u32 = WM_APP + 40;

/// 隐藏消息窗类名（进程内唯一；每实例一窗）。
const CLASS_NAME: PCWSTR = w!("IuvCtlWindow");

/// TSF 线程应用命令的回调目标（`TextService_Impl` 实现；经原始指针跨线程间接调用，
/// 只在 TSF 线程 wndproc 内解引用——地址在端点存活期间稳定）。
pub(crate) trait CtlApplier {
    fn apply_cmd(&self, cmd: &CtlCmd) -> CtlResult;
}

/// 待应用命令槽：accept 线程写入 + PostMessage；TSF 线程 wndproc 取出应用 + 回送结果。
pub(crate) struct CtlJob {
    pub cmd: CtlCmd,
    pub resp: mpsc::SyncSender<CtlResult>,
}

/// 反向控制端点（每 TextService 实例一个）。
pub(crate) struct CtlEndpoint {
    /// 隐藏消息窗（TSF 线程创建；wndproc 经 GWLP_USERDATA 取回本端点）。
    hwnd: HWND,
    /// 待应用命令（跨线程共享；accept 线程写、TSF 线程取）。
    pending: Arc<Mutex<Option<CtlJob>>>,
    /// 当前 accept 服务端句柄（跨线程共享；Deactivate 关闭以中断 ConnectNamedPipe）。
    handle_slot: Arc<Mutex<Option<usize>>>,
    /// 停止标志（accept 线程退出判定）。
    stop: Arc<AtomicBool>,
    /// accept 线程句柄（Drop 时 join）。
    thread: Option<std::thread::JoinHandle<()>>,
    /// 应用目标：TSF 线程上的 `TextService_Impl`（`&dyn CtlApplier`）。
    svc: *const dyn CtlApplier,
}

// SAFETY: 端点只在创建线程（TSF 线程）触碰 wndproc 路径；accept 线程经 Arc 字段访问
// pending/handle_slot/stop（互斥保护）。svc 指针只在 TSF 线程解引用，不跨线程移动。
// 端点整体不 Send（JoinHandle 之外字段含裸指针，但未声明 Send——默认非 Send，安全）。
impl CtlEndpoint {
    /// 建隐藏窗（TSF 线程调用；懒注册类）。失败 → None（记录日志）。
    pub(crate) fn create_window() -> Option<HWND> {
        register_class();
        // SAFETY: GetModuleHandleW(None) 取当前进程实例句柄。
        let hinst = unsafe { GetModuleHandleW(None) }.unwrap_or_default();
        if hinst.is_invalid() {
            log_line("[ctl] GetModuleHandleW 失败");
            return None;
        }
        // SAFETY: HWND_MESSAGE 父 = 消息窗（不可见，仅收消息）；TSF 线程消息泵会分发。
        let hwnd = unsafe {
            CreateWindowExW(
                Default::default(),
                CLASS_NAME,
                PCWSTR::null(),
                WS_POPUP,
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(hinst.into()),
                None,
            )
        };
        match hwnd {
            Ok(h) => Some(h),
            Err(e) => {
                log_line(&format!("[ctl] 创建隐藏消息窗失败：{e:?}"));
                None
            }
        }
    }

    /// 构造端点（不挂窗口、不起线程；调用方把对象放进稳定槽位后调 `attach`）。
    pub(crate) fn new(
        hwnd: HWND,
        svc: *const dyn CtlApplier,
        _pid: u32,
        _tid: u32,
    ) -> CtlEndpoint {
        CtlEndpoint {
            hwnd,
            pending: Arc::new(Mutex::new(None)),
            handle_slot: Arc::new(Mutex::new(None)),
            stop: Arc::new(AtomicBool::new(false)),
            thread: None,
            svc,
        }
    }

    /// 把本端点挂到窗口（GWLP_USERDATA）+ 启动 accept 线程。
    /// **必须在端点地址固定后调用**（存于 TextService 的 `RefCell<Option<CtlEndpoint>>`
    /// 内）；返回 false = 线程启动失败（窗口保持，Drop 清理）。
    pub(crate) fn attach(&mut self, pid: u32, tid: u32) -> bool {
        // SAFETY: self 地址在窗口存活期间稳定（调用方保证，同 MenuWindow/Candwin 模式）。
        unsafe { SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, self as *const Self as usize as _) };
        let name = ctl_pipe_name(pid, tid);
        // HWND 为裸指针（!Send），线程闭包只做 PostMessage（跨线程投递合法）——以 usize 传递。
        let hwnd_val = self.hwnd.0 as usize;
        let pending = self.pending.clone();
        let handle_slot = self.handle_slot.clone();
        let stop = self.stop.clone();
        let spawned = std::thread::Builder::new()
            .name("iuv-ctl-accept".to_string())
            .spawn(move || accept_thread(name, hwnd_val, pending, handle_slot, stop));
        match spawned {
            Ok(h) => {
                self.thread = Some(h);
                log_line(&format!(
                    "[ctl] 控制端点就绪（{pid}:{tid}，accept 线程已启动）"
                ));
                true
            }
            Err(e) => {
                log_line(&format!("[ctl] accept 线程启动失败：{e}"));
                false
            }
        }
    }
}

impl Drop for CtlEndpoint {
    fn drop(&mut self) {
        // 1. 停止 accept 线程：置标志 + 关闭当前服务端句柄（中断 ConnectNamedPipe）。
        self.stop.store(true, Ordering::SeqCst);
        let slot = self.handle_slot.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(h) = *slot {
            // SAFETY: 关闭服务端句柄中断阻塞中的 ConnectNamedPipe；accept 线程随后
            // 的重复关闭返回错误被忽略（良性竞态，见 accept_thread 注释）。
            let _ = unsafe { CloseHandle(HANDLE(h as *mut core::ffi::c_void)) };
        }
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        // 2. 清 GWLP_USERDATA（wndproc 不再能取回本端点）→ 销毁窗口（残留消息丢弃）。
        if !self.hwnd.is_invalid() {
            // SAFETY: 同 Drop 惯例：先清零再销毁。
            let _ = unsafe { SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0) };
            // SAFETY: 在创建线程（TSF 线程）销毁窗口。
            let _ = unsafe { DestroyWindow(self.hwnd) };
            self.hwnd = HWND::default();
        }
    }
}

/// accept 线程主体：循环建服务端管道 → 阻塞等待 daemon 连接 → 服务一条 Cmd → 断开。
///
/// 跨线程中断（Deactivate）：`stop` 置位 + `CloseHandle` 中断 `ConnectNamedPipe`/读写；
/// 中断后 `connect`/`serve` 返回 Err → 循环顶部判 stop 退出。服务端句柄值被跨线程关闭
/// 后本线程的 CtlServer::drop 再关闭一次（CloseHandle 返回错误，忽略）——理论上的句柄
/// 值复用窗口极小（微秒级同进程，可接受，注释留痕）。
fn accept_thread(
    name: String,
    hwnd_val: usize,
    pending: Arc<Mutex<Option<CtlJob>>>,
    handle_slot: Arc<Mutex<Option<usize>>>,
    stop: Arc<AtomicBool>,
) {
    let hwnd = HWND(hwnd_val as *mut core::ffi::c_void);
    while !stop.load(Ordering::SeqCst) {
        let server = match CtlServer::create(&name) {
            Ok(s) => s,
            Err(e) => {
                log_line(&format!("[ctl] 创建控制管道失败（200ms 后重试）：{e}"));
                std::thread::sleep(Duration::from_millis(200));
                continue;
            }
        };
        // 全程暴露服务端句柄（connect + serve 期间），Deactivate 可关闭中断。
        {
            let mut slot = handle_slot.lock().unwrap_or_else(|p| p.into_inner());
            *slot = Some(server.handle().0 as usize);
        }
        let connected = server.connect().is_ok();
        if !connected {
            let mut slot = handle_slot.lock().unwrap_or_else(|p| p.into_inner());
            *slot = None;
            continue; // 被中断（stop）或瞬时失败 → 下一轮判 stop
        }
        let _ = server.serve(|cmd| dispatch_ctl_cmd(hwnd, &pending, *cmd));
        let mut slot = handle_slot.lock().unwrap_or_else(|p| p.into_inner());
        *slot = None;
    }
    log_line("[ctl] accept 线程退出");
}

/// 跨线程分发：写待应用命令 → PostMessage 唤醒 TSF 线程 → 等 TSF 应用结果（超时兜底）。
fn dispatch_ctl_cmd(
    hwnd: HWND,
    pending: &Arc<Mutex<Option<CtlJob>>>,
    cmd: CtlCmd,
) -> CtlResult {
    let (tx, rx) = mpsc::sync_channel(1);
    let job = CtlJob { cmd, resp: tx };
    *pending.lock().unwrap_or_else(|p| p.into_inner()) = Some(job);
    // SAFETY: hwnd 为 TSF 线程的隐藏消息窗（端点存活期间有效）；PostMessage 跨线程投递
    // 到窗口所属线程队列，非阻塞。
    let _ = unsafe { PostMessageW(Some(hwnd), WM_APP_TOOLBAR_CMD, WPARAM(0), LPARAM(0)) };
    match rx.recv_timeout(Duration::from_millis(3000)) {
        Ok(r) => r,
        Err(_) => CtlResult::Err {
            msg: "TSF 线程应用命令超时".into(),
        },
    }
}

/// 进程内注册一次窗口类。
fn register_class() {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| {
        // SAFETY: 类名静态宽字符串，进程生命周期有效；失败仅记日志。
        unsafe {
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                style: WNDCLASS_STYLES(0),
                lpfnWndProc: Some(wnd_proc),
                hbrBackground: HBRUSH::default(),
                lpszMenuName: PCWSTR::null(),
                lpszClassName: CLASS_NAME,
                ..Default::default()
            };
            if RegisterClassExW(&class) == 0 {
                let err = GetLastError();
                if err != ERROR_CLASS_ALREADY_EXISTS {
                    log_line("[ctl] RegisterClassExW 失败");
                }
            }
        }
    });
}

/// 隐藏消息窗 wndproc：WM_APP_TOOLBAR_CMD → 取待应用命令 → TSF 线程应用 → 回送结果。
unsafe extern "system" fn wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == WM_APP_TOOLBAR_CMD {
        // SAFETY: GWLP_USERDATA 由 attach 写入端点指针，Drop 先清零再销毁窗口——取到的
        // 指针在窗口存活期间有效（调用都在 TSF 线程）。
        let p = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
        if p != 0 {
            let ep = &*(p as *const CtlEndpoint);
            let job = ep.pending.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(job) = job {
                // SAFETY: ep.svc 指向 TextService（端点存活期间有效），TSF 线程解引用。
                let result = (&*ep.svc).apply_cmd(&job.cmd);
                let _ = job.resp.send(result);
            }
        }
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
}
