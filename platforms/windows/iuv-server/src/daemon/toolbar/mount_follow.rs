//! 桌宠挂靠跟随（54-2 Step 6b）：WinEventHook 追目标位置 → 工具栏跟随。
//!
//! ## 为什么挂在工具栏线程
//! `WINEVENT_OUTOFCONTEXT` 的回调由**安装线程的消息循环**派发——本模块只在
//! 工具栏线程（`mod.rs` 的 GetMessage 循环）上 install / refresh / uninstall。
//! 于是**不需要定时器**，守住 40 号那份"无定时器、无前台查询"的显隐治理。
//!
//! ## 事件合并
//! 拖宠物时 `EVENT_OBJECT_LOCATIONCHANGE` 高频触发；若每个事件都重定位，就是每帧
//! 一次 `SetWindowPos`。故回调只投一发 `WM_APP_MOUNT_REFRESH`，并用 `PENDING`
//! 把一串事件压成一次（处理时先清标志，允许后续事件再排一发）。
//!
//! ## 生命周期
//! - 目标**隐藏**（Shimeji 会自己藏）→ 跳过本次定位，**保留 offset**，再现时自动恢复
//! - 目标**销毁**（`EVENT_OBJECT_DESTROY` 或 `IsWindow` 失效）→ 卸载 hook，
//!   工具栏**留在原地**当普通工具栏（§5 决策）
//!
//! 失败一律静默降级（记日志，不 panic）。

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Mutex;

use iuv_win::mount::{self, MountProfile};
use iuv_win::window_probe::{virtual_desktop_rect, ProbeRect};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows::Win32::UI::WindowsAndMessaging::{
    GetWindowRect, GetWindowThreadProcessId, IsWindow, IsWindowVisible, PostMessageW, SetWindowPos,
    EVENT_OBJECT_DESTROY, EVENT_OBJECT_LOCATIONCHANGE, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOSIZE,
    SWP_NOZORDER, WINEVENT_OUTOFCONTEXT,
};

use crate::daemon::log;

/// 工具栏窗口（回调据此投递刷新消息）。
static BAR_HWND: AtomicUsize = AtomicUsize::new(0);
/// 已有一发刷新在队列里（事件合并）。
static PENDING: AtomicBool = AtomicBool::new(false);

/// 当前跟随会话。`hook` 存 `isize` 而非 `HWINEVENTHOOK`（后者含裸指针、`!Send`，
/// 静态量要求 `Sync`）。
struct Follow {
    profile: MountProfile,
    /// 目标顶层窗口句柄（isize 同上）。
    target: isize,
    hook: isize,
}

static FOLLOW: Mutex<Option<Follow>> = Mutex::new(None);

/// 挂上跟随（幂等：先卸旧的）。返回是否成功装上 hook（失败则只做一次性定位）。
pub fn install(bar: HWND, target: isize, profile: MountProfile) -> bool {
    uninstall();
    let target_hwnd = HWND(target as *mut _);
    let mut pid = 0u32;
    // SAFETY: 纯查询。
    unsafe { GetWindowThreadProcessId(target_hwnd, Some(&mut pid)) };
    // SAFETY: 事件范围 DESTROY(0x8001)..LOCATIONCHANGE(0x800B) 一次覆盖四类；
    // idProcess 限定目标进程（比全局 hook 少几个数量级的事件）；OUTOFCONTEXT 回调
    // 由本线程消息循环派发，故不需要 DLL 注入。
    let hook = unsafe {
        SetWinEventHook(
            EVENT_OBJECT_DESTROY,
            EVENT_OBJECT_LOCATIONCHANGE,
            None,
            Some(win_event_proc),
            pid,
            0,
            WINEVENT_OUTOFCONTEXT,
        )
    };
    let hook_raw = hook.0 as isize;
    BAR_HWND.store(bar.0 as usize, Ordering::SeqCst);
    if let Ok(mut g) = FOLLOW.lock() {
        *g = Some(Follow {
            profile,
            target,
            hook: hook_raw,
        });
    }
    if hook_raw == 0 {
        log::log_line("[mount] SetWinEventHook 失败 → 仅做一次定位，不持续跟随");
        false
    } else {
        log::log_line(&format!("[mount] 已挂上跟随（目标 pid={pid}）"));
        true
    }
}

/// 卸载跟随（hook + 会话状态）。未跟随时无副作用。
pub fn uninstall() {
    let old = FOLLOW.lock().ok().and_then(|mut g| g.take());
    if let Some(f) = old {
        if f.hook != 0 {
            // SAFETY: 与 SetWinEventHook 配对；句柄来自 install。
            let _ = unsafe { UnhookWinEvent(HWINEVENTHOOK(f.hook as *mut _)) };
        }
    }
    BAR_HWND.store(0, Ordering::SeqCst);
    PENDING.store(false, Ordering::Release);
}

/// 投一发刷新（事件合并：已有一发在队列里就直接返回）。
fn post_refresh() {
    if PENDING.swap(true, Ordering::AcqRel) {
        return;
    }
    let h = BAR_HWND.load(Ordering::SeqCst);
    if h == 0 {
        PENDING.store(false, Ordering::Release);
        return;
    }
    // SAFETY: 跨线程投递到工具栏窗口（其线程的消息循环消费）。
    let _ = unsafe {
        PostMessageW(
            Some(HWND(h as *mut _)),
            super::WM_APP_MOUNT_REFRESH,
            WPARAM(0),
            LPARAM(0),
        )
    };
}

/// 窗口事件回调（工具栏线程派发）。只认目标顶层窗口。
unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    _id_child: i32,
    _thread: u32,
    _time: u32,
) {
    if id_object != 0 {
        return; // OBJID_WINDOW = 0：只关心顶层窗口对象
    }
    let target = FOLLOW
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|f| f.target));
    if target != Some(hwnd.0 as isize) {
        return;
    }
    if event == EVENT_OBJECT_DESTROY {
        log::log_line("[mount] 目标窗口销毁 → 被动解挂（工具栏留在原地）");
        uninstall();
        return;
    }
    post_refresh();
}

/// 一次跟随：读目标当前矩形 → 算落点（分轴 clamp）→ 移动工具栏。
/// 由 `WM_APP_MOUNT_REFRESH` 驱动，也用于安装后立即定位一次。
pub fn refresh(bar: HWND) {
    PENDING.store(false, Ordering::Release); // 先清，允许后续事件再排一发
    let Some((target, profile)) = FOLLOW
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|f| (f.target, f.profile.clone())))
    else {
        return;
    };
    let target_hwnd = HWND(target as *mut _);
    // SAFETY: 以下均为纯查询。
    if !unsafe { IsWindow(Some(target_hwnd)) }.as_bool() {
        log::log_line("[mount] 目标窗口已失效 → 被动解挂（工具栏留在原地）");
        uninstall();
        return;
    }
    if !unsafe { IsWindowVisible(target_hwnd) }.as_bool() {
        // 隐藏态：跳过本次定位（**保留 offset**，宠物再现时自动恢复精确跟踪）
        return;
    }
    let mut rc = RECT::default();
    if unsafe { GetWindowRect(target_hwnd, &mut rc) }.is_err() {
        return;
    }
    let pet = ProbeRect {
        x: rc.left,
        y: rc.top,
        w: rc.right - rc.left,
        h: rc.bottom - rc.top,
    };
    if pet.w <= 0 || pet.h <= 0 {
        return;
    }
    let mut bar_rc = RECT::default();
    if unsafe { GetWindowRect(bar, &mut bar_rc) }.is_err() {
        return;
    }
    let (bw, bh) = (bar_rc.right - bar_rc.left, bar_rc.bottom - bar_rc.top);
    if bw <= 0 || bh <= 0 {
        return;
    }
    let (x, y) = mount::follow_pos(pet, bw, bh, &profile, virtual_desktop_rect());
    if x == bar_rc.left && y == bar_rc.top {
        return; // 没动就不调 SetWindowPos
    }
    // SAFETY: 只移动（NOSIZE|NOZORDER|NOACTIVATE），不抢焦点、不改 z 序。
    let _ = unsafe {
        SetWindowPos(
            bar,
            Some(HWND_TOPMOST),
            x,
            y,
            0,
            0,
            SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
        )
    };
}

/// 拖拽松手：用**视觉位置**反推 offset，并按方位重判锚点对，存回档案。
/// 拖拽是 offset 的唯一写入口（§2 冻结）。
pub fn recompute_pose(bar_pos: (i32, i32), bar_w: i32, bar_h: i32) {
    let Some((target, mut profile)) = FOLLOW
        .lock()
        .ok()
        .and_then(|g| g.as_ref().map(|f| (f.target, f.profile.clone())))
    else {
        return; // 未挂载：拖拽只是普通移动
    };
    let target_hwnd = HWND(target as *mut _);
    let mut rc = RECT::default();
    if unsafe { GetWindowRect(target_hwnd, &mut rc) }.is_err() {
        return;
    }
    let pet = ProbeRect {
        x: rc.left,
        y: rc.top,
        w: rc.right - rc.left,
        h: rc.bottom - rc.top,
    };
    if pet.w <= 0 || pet.h <= 0 {
        return;
    }
    profile.anchor = mount::judge_anchor(pet, bar_pos, bar_w, bar_h);
    profile.offset = mount::offset_from_visual(bar_pos, bar_w, bar_h, pet, profile.anchor);
    if let Ok(mut g) = FOLLOW.lock() {
        if let Some(f) = g.as_mut() {
            f.profile = profile.clone();
        }
    }
    if mount::save(&profile) {
        log::log_line(&format!(
            "[mount] 姿态已更新：anchor={:?} offset={:?}",
            profile.anchor, profile.offset
        ));
    }
}
