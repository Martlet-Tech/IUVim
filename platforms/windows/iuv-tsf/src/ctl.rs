//! 反向控制端点（32-status-toolbar.md §4.3 跨线程分发机制保留）。
//!
//! 49 号 ② 控制面迁移后命令入口 = transport `S2C::Ctl`（进程级提交端点注册表）：
//! remote_host 的 ServerReq 处理器线程 → [`submit_cmd`] → 写待应用命令槽 +
//! `PostMessage(WM_APP_TOOLBAR_CMD)` 唤醒 TSF 线程（隐藏消息窗，随应用消息泵执行）→
//! wndproc 应用（写 OPENCLOSE / 改运行时 / 会话刷新）→ 回送结果。
//!
//! 注册表而非单槽（2026-10-07 Trae 双实例案例）：TSF 每次激活 CoCreate 一份
//! TextService（同线程可并存多实例，Chromium 系宿主激活/弃置旧实例不保证先
//! Deactivate），单槽「后 attach 覆盖 + 任一实例停止清全场」会让幽灵实例的
//! Deactivate 陪葬存活实例的控制通道（工具栏点击报「无控制端点」）。现语义 =
//! attach 追加 / 停止只移除自己 / 提交取最近端点 + 死条目回退更早端点。
//!
//! ③-3 清理：旧 per-实例 ctl 管道（`\\.\pipe\iuv-ctl-<pid>-<tid>` + accept 线程）
//! 已删——② 迁移后 `CtlClient` 全仓零消费者，管道纯空转。
//!
//! 端点生命周期 = TextService 实例（Activate 起，Deactivate/Drop 停）。
//! 全部失败静默降级（记日志，不 panic；iuv-tsf 硬性约定）。

use std::mem::size_of;
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::Duration;

use iuv_win::{CtlCmd, CtlResult};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{
    GetLastError, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::Graphics::Gdi::HBRUSH;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowLongPtrW, PostMessageW,
    RegisterClassExW, SetWindowLongPtrW, GWLP_USERDATA, HWND_MESSAGE, WM_APP, WNDCLASSEXW,
    WNDCLASS_STYLES, WS_POPUP,
};

use crate::log::log_line;

/// TSF 线程应用命令的私有消息（ServerReq 处理器线程 PostMessage 唤醒）。
pub(crate) const WM_APP_TOOLBAR_CMD: u32 = WM_APP + 40;

/// 隐藏消息窗类名（进程内唯一；每实例一窗）。
const CLASS_NAME: PCWSTR = w!("IuvCtlWindow");

/// TSF 线程应用命令的回调目标（`TextService_Impl` 实现；经原始指针跨线程间接调用，
/// 只在 TSF 线程 wndproc 内解引用——地址在端点存活期间稳定）。
pub(crate) trait CtlApplier {
    fn apply_cmd(&self, cmd: &CtlCmd) -> CtlResult;
}

/// 待应用命令槽：提交线程写入 + PostMessage；TSF 线程 wndproc 取出应用 + 回送结果。
pub(crate) struct CtlJob {
    pub cmd: CtlCmd,
    pub resp: mpsc::SyncSender<CtlResult>,
}

/// 反向控制端点（每 TextService 实例一个）。
pub(crate) struct CtlEndpoint {
    /// 隐藏消息窗（TSF 线程创建；wndproc 经 GWLP_USERDATA 取回本端点）。
    hwnd: HWND,
    /// 待应用命令（跨线程共享；提交线程写、TSF 线程取）。
    pending: Arc<Mutex<Option<CtlJob>>>,
    /// 应用目标：TSF 线程上的 `TextService`（`&dyn CtlApplier`）。
    svc: *const dyn CtlApplier,
}

// SAFETY: 端点只在创建线程（TSF 线程）触碰 wndproc 路径；提交线程经 Arc 访问 pending
// （互斥保护）。svc 指针只在 TSF 线程解引用，不跨线程移动。
// 端点整体不 Send（字段含裸指针，未声明 Send——默认非 Send，安全）。
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

    /// 构造端点（不挂窗口；调用方把对象放进稳定槽位后调 `attach`）。
    pub(crate) fn new(hwnd: HWND, svc: *const dyn CtlApplier) -> CtlEndpoint {
        CtlEndpoint {
            hwnd,
            pending: Arc::new(Mutex::new(None)),
            svc,
        }
    }

    /// 把本端点挂到窗口（GWLP_USERDATA）+ 登记进进程级提交端点注册表。
    /// **必须在端点地址固定后调用**（存于 TextService 的 `RefCell<Option<CtlEndpoint>>`
    /// 内）。
    pub(crate) fn attach(&mut self) {
        // SAFETY: self 地址在窗口存活期间稳定（调用方保证，同 MenuWindow/Candwin 模式）。
        unsafe { SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, self as *const Self as usize as _) };
        // HWND 为裸指针（!Send），提交线程只做 PostMessage（跨线程投递合法）——以 usize 传递。
        let hwnd_val = self.hwnd.0 as usize;
        // ② 控制面迁移：登记提交端点（transport ServerReq → 本端点分发）。
        let count = set_submit_hook(hwnd_val, self.pending.clone());
        log_line(&format!("[ctl] 控制端点就绪（提交注册表 {count} 个端点）"));
    }

    /// 从提交端点注册表移除本端点（stop_ctl_endpoint 调用；Drop 前显式停）。
    pub(crate) fn remove_submit_hook(&self) {
        remove_submit_hook(self.hwnd.0 as usize);
    }
}

impl Drop for CtlEndpoint {
    fn drop(&mut self) {
        // 清 GWLP_USERDATA（wndproc 不再能取回本端点）→ 销毁窗口（残留消息丢弃）。
        if !self.hwnd.is_invalid() {
            // SAFETY: 同 Drop 惯例：先清零再销毁。
            let _ = unsafe { SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0) };
            // SAFETY: 在创建线程（TSF 线程）销毁窗口。
            let _ = unsafe { DestroyWindow(self.hwnd) };
            self.hwnd = HWND::default();
        }
    }
}

/// ② 控制面迁移：进程级提交端点注册表（transport ServerReq 处理器经 [`submit_cmd`]
/// 路由到此）。注册表语义见模块注释——端点身份 = hwnd_val（每实例一窗，存活期唯一）。
/// 提交钩子条目（hwnd_val + 待应用命令槽）。
type SubmitHook = (usize, Arc<Mutex<Option<CtlJob>>>);
static SUBMIT_HOOKS: OnceLock<Mutex<Vec<SubmitHook>>> = OnceLock::new();

/// 登记端点（attach 时调用；hwnd_val = 隐藏消息窗）。返回登记后端点数。
pub(crate) fn set_submit_hook(hwnd_val: usize, pending: Arc<Mutex<Option<CtlJob>>>) -> usize {
    let reg = SUBMIT_HOOKS.get_or_init(|| Mutex::new(Vec::new()));
    let mut r = reg.lock().unwrap_or_else(|e| e.into_inner());
    registry_attach(&mut r, (hwnd_val, pending));
    r.len()
}

/// 移除端点（端点停止时；**只移除自己**——幽灵实例 Deactivate 不陪葬存活实例）。
pub(crate) fn remove_submit_hook(hwnd_val: usize) {
    if let Some(reg) = SUBMIT_HOOKS.get() {
        let mut r = reg.lock().unwrap_or_else(|e| e.into_inner());
        registry_remove(&mut r, hwnd_val);
    }
}

/// 追加端点；同 hwnd_val 重挂先去重（attach 幂等）。
fn registry_attach(reg: &mut Vec<SubmitHook>, hook: SubmitHook) {
    reg.retain(|(h, _)| *h != hook.0);
    reg.push(hook);
}

/// 只移除匹配自己的条目。返回是否移除了东西（供日志）。
fn registry_remove(reg: &mut Vec<SubmitHook>, hwnd_val: usize) -> bool {
    let before = reg.len();
    reg.retain(|(h, _)| *h != hwnd_val);
    before != reg.len()
}

/// 进程内提交一条命令（最近端点优先；无端点 → None，调用方回失败）。
pub(crate) fn submit_cmd(cmd: CtlCmd) -> Option<CtlResult> {
    let reg = SUBMIT_HOOKS.get()?;
    // 快照后即放锁：dispatch 最长阻塞 3s 等 TSF 线程，不能持注册表锁干等。
    let snapshot: Vec<SubmitHook> = reg.lock().unwrap_or_else(|e| e.into_inner()).clone();
    // 尾优先（最近 attach 的实例赢，即原单槽语义）；死条目（PostMessage 失败）
    // 移除后回退更早端点——Trae 双实例序列下幽灵停掉后仍路由到存活实例。
    for (hwnd_val, pending) in snapshot.iter().rev() {
        let hwnd = HWND(*hwnd_val as *mut core::ffi::c_void);
        match dispatch_ctl_cmd(hwnd, pending, cmd) {
            Some(r) => return Some(r),
            None => {
                remove_submit_hook(*hwnd_val);
                log_line(&format!(
                    "[ctl] 端点提交失败（窗口失效 {hwnd_val:#x}），回退更早端点"
                ));
            }
        }
    }
    None
}

/// 跨线程分发：写待应用命令 → PostMessage 唤醒 TSF 线程 → 等 TSF 应用结果（超时兜底）。
/// PostMessage 失败（窗口已销毁）→ 收回 job 返回 None（调用方回退下一端点）。
fn dispatch_ctl_cmd(
    hwnd: HWND,
    pending: &Arc<Mutex<Option<CtlJob>>>,
    cmd: CtlCmd,
) -> Option<CtlResult> {
    let (tx, rx) = mpsc::sync_channel(1);
    let job = CtlJob { cmd, resp: tx };
    *pending.lock().unwrap_or_else(|p| p.into_inner()) = Some(job);
    // SAFETY: hwnd 为 TSF 线程的隐藏消息窗（端点存活期间有效）；PostMessage 跨线程投递
    // 到窗口所属线程队列，非阻塞。失败 = 窗口已销毁（注册表条目滞后于窗口生命周期）。
    let posted =
        unsafe { PostMessageW(Some(hwnd), WM_APP_TOOLBAR_CMD, WPARAM(0), LPARAM(0)) }.is_ok();
    if !posted {
        // 收回 job（窗口已死没人应用；留着会被下次提交覆盖或永占槽位）。
        *pending.lock().unwrap_or_else(|p| p.into_inner()) = None;
        return None;
    }
    match rx.recv_timeout(Duration::from_millis(3000)) {
        Ok(r) => Some(r),
        Err(_) => Some(CtlResult::Err {
            msg: "TSF 线程应用命令超时".into(),
        }),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn hook(hwnd_val: usize) -> SubmitHook {
        (hwnd_val, Arc::new(Mutex::new(None)))
    }

    /// Trae 双实例回归（2026-10-07 案例）：同线程两实例先后 attach，幽灵（后
    /// attach 的 #2）停止只移除自己 → 提交目标仍轮到存活的 #1（旧单槽下这里是空表）。
    #[test]
    fn ghost_deactivate_keeps_survivor() {
        let mut reg = Vec::new();
        registry_attach(&mut reg, hook(0x100));
        registry_attach(&mut reg, hook(0x200));
        // 存活期提交目标 = 尾优先（最近 attach 赢，即原单槽语义）
        assert_eq!(reg.last().unwrap().0, 0x200);
        // #2 Deactivate：只移除自己，#1 不陪葬
        assert!(registry_remove(&mut reg, 0x200));
        assert_eq!(reg.last().unwrap().0, 0x100);
        assert_eq!(reg.len(), 1);
    }

    /// 正常 Deactivate→Activate 循环：旧端点移除后新端点顶上。
    #[test]
    fn normal_deactivate_activate_cycle() {
        let mut reg = Vec::new();
        registry_attach(&mut reg, hook(0x100));
        assert!(registry_remove(&mut reg, 0x100));
        registry_attach(&mut reg, hook(0x200));
        assert_eq!(reg.len(), 1);
        assert_eq!(reg.last().unwrap().0, 0x200);
    }

    /// 移除不存在的端点 = 无事发生（别人的条目绝不陪葬）；空表同理。
    #[test]
    fn remove_unknown_is_noop() {
        let mut reg = Vec::new();
        assert!(!registry_remove(&mut reg, 0x1));
        registry_attach(&mut reg, hook(0x100));
        assert!(!registry_remove(&mut reg, 0x999));
        assert_eq!(reg.len(), 1);
        assert!(registry_remove(&mut reg, 0x100));
        assert!(reg.is_empty());
    }

    /// 同端点重挂去重（attach 幂等）。
    #[test]
    fn reattach_same_endpoint_dedup() {
        let mut reg = Vec::new();
        registry_attach(&mut reg, hook(0x100));
        registry_attach(&mut reg, hook(0x100));
        assert_eq!(reg.len(), 1);
    }

    /// 注册表从未登记（无 Activate）→ submit_cmd 返回 None（调用方回「无控制端点」）。
    #[test]
    fn submit_without_registry_is_none() {
        assert!(submit_cmd(CtlCmd::SetMode(true)).is_none());
    }
}
