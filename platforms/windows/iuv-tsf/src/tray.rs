//! 通知区托盘图标 + 自绘右键菜单装配（M5；任务书 21-m5-tray-menu.md §3）。
//!
//! 单实例方案 A（首个会话进程托管）：各会话进程创建命名互斥 `Local\iuv-tray-host`，
//! 持有者才 `Shell_NotifyIcon` 建托盘图标；其他进程 `ERROR_ALREADY_EXISTS` → 放弃托管
//! （静默返回 None）。持有者退出/崩溃 → 互斥释放，后续会话激活接替。
//!
//! 托盘消息窗口（隐藏小窗，类 `IuvTrayWindow`）挂现有 TSF 线程消息循环：`WM_APP+1`
//! 回调（Shell_NotifyIcon 的 uCallbackMessage）→ 右键（WM_CONTEXTMENU）/ 左键（NIN_SELECT）
//! → `GetCursorPos` → 弹自绘菜单（`ui::menu_window::MenuWindow`，iuv-ui 渲染）。
//! 菜单项点击 → 本模块分发：设置…（M6 占位日志）/ 帮助/关于（MessageBoxW）/ 退出 iuv
//! （移除图标 + 释放互斥，进程继续但不再托管）。
//!
//! 全部路径不 panic（DLL 内硬性约定）：失败记日志 + 静默降级（不建图标/不弹菜单）。

use std::cell::RefCell;
use std::mem::size_of;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use iuv_ui::MenuEntry;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, GetLastError, SetLastError, ERROR_ALREADY_EXISTS, ERROR_CLASS_ALREADY_EXISTS,
    HANDLE, HWND, LPARAM, LRESULT, POINT, WPARAM,
};
use windows::Win32::System::LibraryLoader::{
    GetModuleHandleExW, GetModuleHandleW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
};
use windows::Win32::System::Threading::{CreateMutexW, ReleaseMutex};
use windows::Win32::UI::Shell::{
    NOTIFYICONDATAW, NOTIFYICONDATAW_0, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    NIM_SETVERSION, NIN_SELECT, NOTIFYICON_VERSION_4,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, CS_HREDRAW, CS_VREDRAW, DefWindowProcW, DestroyWindow, GetCursorPos,
    GetSystemMetrics, LoadImageW, MessageBoxW, RegisterClassExW, ShowWindow, SM_CXSMICON,
    SM_CYSMICON, SW_HIDE, WM_APP, WM_CONTEXTMENU, WNDCLASSEXW, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
    WS_POPUP, HICON, IMAGE_ICON, LR_SHARED, MB_ICONINFORMATION, MB_OK,
};

use crate::ui::MenuWindow;

/// 命名互斥名（会话级；`Local\` 前缀 = 当前终端会话内单实例，无需跨会话权限）。
static MUTEX_NAME: [u16; 32] = wide("Local\\iuv-tray-host");

/// 托盘窗口类名。
static TRAY_CLASS_NAME: [u16; 32] = wide("IuvTrayWindow");

/// 托盘图标回调消息（Shell_NotifyIcon 的 uCallbackMessage）。
const TRAY_CALLBACK_MSG: u32 = WM_APP + 1;

/// 托盘图标资源 ID（DLL 内嵌 .ico，langbar 已在用 ID 101；本模块复用同款图标）。
const TRAY_ICON_ID: u32 = 101;

/// 托盘图标标识（uID，单实例唯一）。
const TRAY_ICON_UID: u32 = 1;

/// 菜单项 id（tray.rs 自定义语义；0 保留给分隔线）。
const MENU_SETTINGS: u16 = 1;
const MENU_ABOUT: u16 = 2;
const MENU_QUIT: u16 = 3;

/// 静态单例：托盘状态（TSF 线程专用；Mutex 仅为 `Sync` 满足 static，无跨线程竞争）。
/// 句柄以原始指针 `usize` 存储——HANDLE/HWND 包装 `*mut c_void`（`!Send`），无法进
/// 静态；同线程内 usize↔句柄往返无损（见 teardown/try_host 的转换）。
/// 菜单窗口不在此处——`MenuWindow` 含 COM 接口/闭包（`!Sync`），放线程局部存储
/// （见 `MENU`），由创建它的 TSF 线程独占访问。
pub struct TrayState {
    /// 本进程是否正在托管托盘（teardown 幂等闸）。
    hosting: AtomicBool,
    /// 已执行"退出 iuv"：本进程不再重新托管（即使后续 Activate）。
    quit: AtomicBool,
    /// 命名互斥句柄原始指针（托管期持有；他进程 CreateMutexW 才能探测到 ERROR_ALREADY_EXISTS）。
    mutex_handle: Mutex<Option<usize>>,
    /// 托盘消息窗口句柄原始指针（NIM_DELETE 需要）。
    tray_hwnd: Mutex<Option<usize>>,
}

impl TrayState {
    fn new() -> Self {
        TrayState {
            hosting: AtomicBool::new(false),
            quit: AtomicBool::new(false),
            mutex_handle: Mutex::new(None),
            tray_hwnd: Mutex::new(None),
        }
    }
}

/// 静态单例（OnceLock 首次 try_host 初始化；此后全生命周期复用）。
static TRAY: OnceLock<TrayState> = OnceLock::new();

// 自绘菜单窗口（线程局部存储）：创建与所有访问都发生在 TSF 线程
// （try_host / 托盘回调 / 菜单窗口自身回调全在同一线程），线程退出时自动销毁。
// 每个线程持有独立副本——其他线程拿到的是空 None，无跨线程竞争。
thread_local! {
    static MENU: RefCell<Option<MenuWindow>> = RefCell::new(None);
}

/// HANDLE ↔ usize 无损往返（同一指针值，不改变位型；仅跨过 Send/Sync 约束）。
fn handle_to_usize(h: HANDLE) -> usize {
    h.0 as usize
}

/// 见 `handle_to_usize`；usize → HANDLE。
fn handle_from_usize(v: usize) -> HANDLE {
    HANDLE(v as *mut core::ffi::c_void)
}

/// HWND ↔ usize 无损往返（同上）。
fn hwnd_to_usize(h: HWND) -> usize {
    h.0 as usize
}

/// 见 `hwnd_to_usize`；usize → HWND。
fn hwnd_from_usize(v: usize) -> HWND {
    HWND(v as *mut core::ffi::c_void)
}

/// 托盘宿主守卫：唯一宿主进程持有（首个 try_host 成功者）。
/// Drop 时清理（NIM_DELETE + 释放互斥）——TextService Drop 随进程/实例生命周期触发。
pub struct TrayHost;

/// 尝试接管托盘宿主（单实例协调）：
/// - 命名互斥已存在（他进程托管）→ 静默返回 None；
/// - 本进程已托管 / 已退出 → 返回 None；
/// - 建托盘窗口 + 图标失败 → 清理后返回 None（记日志）；
/// - 成功 → 返回 Some(TrayHost)（唯一宿主）。
/// 绝不 panic；失败均记日志后静默降级。
pub fn try_host() -> Option<TrayHost> {
    let tray = TRAY.get_or_init(TrayState::new);
    // 本进程已托管（其他 TextService 实例）或已"退出 iuv" → 不再托管。
    if tray.hosting.load(Ordering::SeqCst) || tray.quit.load(Ordering::SeqCst) {
        return None;
    }
    // 命名互斥：会话内单实例。
    // SAFETY: 先清 last-error（防上一个无关 Win32 调用残留的 ERROR_ALREADY_EXISTS 误判）。
    unsafe { SetLastError(windows::Win32::Foundation::WIN32_ERROR(0)) };
    // SAFETY: SECURITY_ATTRIBUTES = None（默认）；bInitialOwner = false；
    // 名字为静态宽字符串数组，进程生命周期有效。
    let mutex = match unsafe { CreateMutexW(None, false, PCWSTR(MUTEX_NAME.as_ptr())) } {
        Ok(m) => m,
        Err(e) => {
            crate::log::log_line(&format!("[tray] CreateMutexW 失败：{e:?}"));
            return None;
        }
    };
    // 句柄非空 + ERROR_ALREADY_EXISTS = 已存在同名互斥（他进程托管）→ 本进程放弃。
    // SAFETY: CreateMutexW 成功返回句柄；GetLastError 紧跟调用读取创建结果。
    let already = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if already {
        // SAFETY: 关闭本次取得的句柄（未持有，仅探测用）。
        unsafe { let _ = CloseHandle(mutex); };
        crate::log::log_line("[tray] 命名互斥已存在（他进程托管托盘），本进程放弃");
        return None;
    }

    // 建托盘消息窗口（隐藏）+ 注册图标；失败 → 释放互斥返回 None。
    if let Some(hwnd) = create_tray_window() {
        if add_icon(hwnd) {
            *tray.tray_hwnd.lock().unwrap_or_else(|e| e.into_inner()) = Some(hwnd_to_usize(hwnd));
            *tray.mutex_handle.lock().unwrap_or_else(|e| e.into_inner()) = Some(handle_to_usize(mutex));
            tray.hosting.store(true, Ordering::SeqCst);
            crate::log::log_line("[tray] 托盘托管成功（首个会话进程）");
            return Some(TrayHost);
        }
        // SAFETY: 图标添加失败 → 销毁托盘窗口
        unsafe { let _ = DestroyWindow(hwnd); };
    }
    // SAFETY: 失败路径释放互斥句柄（未托管）。
    unsafe { let _ = CloseHandle(mutex); };
    None
}

/// 进程内注册一次托盘窗口类；失败（非"已注册"）记日志，不 panic。
fn register_tray_class() {
    static REGISTERED: OnceLock<()> = OnceLock::new();
    REGISTERED.get_or_init(|| {
        // SAFETY: 所有字段显式/Default 填充；类名为静态宽字符串，进程生命周期有效。
        unsafe {
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(tray_wnd_proc),
                hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH::default(),
                lpszMenuName: PCWSTR::null(),
                lpszClassName: PCWSTR(TRAY_CLASS_NAME.as_ptr()),
                ..Default::default()
            };
            if RegisterClassExW(&class) == 0 {
                let err = GetLastError();
                if err != ERROR_CLASS_ALREADY_EXISTS {
                    crate::log::log_line("[tray] 托盘窗口 RegisterClassExW 失败");
                }
            }
        }
    });
}

/// 建托盘消息窗口（独立隐藏小窗，类 `IuvTrayWindow`）：Shell_NotifyIcon 的
/// `WM_APP+1` 回调经此窗口送达 TSF 线程消息循环。失败返回 None（记日志）。
fn create_tray_window() -> Option<HWND> {
    register_tray_class();
    // SAFETY: GetModuleHandleW(None) 取当前进程实例句柄（窗口属于进程，非 DLL 资源）。
    let hinst = unsafe { GetModuleHandleW(None) }.unwrap_or_default();
    if hinst.is_invalid() {
        crate::log::log_line("[tray] GetModuleHandleW 失败");
        return None;
    }
    // SAFETY: WS_EX_TOOLWINDOW|NOACTIVATE = 工具窗不抢焦点、不出任务栏；隐藏用。
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            PCWSTR(TRAY_CLASS_NAME.as_ptr()),
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
    let Ok(hwnd) = hwnd else {
        crate::log::log_line("[tray] CreateWindowExW 失败");
        return None;
    };
    // SAFETY: 隐藏窗口（托盘消息窗口不需要可见）
    let _ = unsafe { ShowWindow(hwnd, SW_HIDE) };
    Some(hwnd)
}

/// 添加托盘图标（NIM_ADD + NIM_SETVERSION v4）。失败返回 false（记日志）。
fn add_icon(hwnd: HWND) -> bool {
    // hIcon 从本 DLL 资源加载（LR_SHARED：系统缓存共享句柄，调用方不得 DestroyIcon）——
    // 必须取 DLL 模块句柄（同 langbar 的已验证模式），不能 GetModuleHandleW(None)
    // （那会取到宿主进程 EXE，资源 ID 101 可能撞车/缺失）。
    let hicon = load_icon(TRAY_ICON_ID);
    if hicon.is_invalid() {
        crate::log::log_line("[tray] 托盘图标资源加载失败（ID 101）");
        return false;
    }
    // tip "iuv 输入法"（UTF-16，null 结尾，szTip 128 上限）。
    let tip: Vec<u16> = "iuv 输入法".encode_utf16().collect();
    let mut nid = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ICON_UID,
        uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
        uCallbackMessage: TRAY_CALLBACK_MSG,
        hIcon: hicon,
        ..Default::default()
    };
    let n = tip.len().min(nid.szTip.len() - 1);
    nid.szTip[..n].copy_from_slice(&tip[..n]);
    nid.szTip[n] = 0;
    // SAFETY: nid 在本调用期间存活；NIM_ADD 复制所需数据（图标句柄、窗口句柄等）。
    if !unsafe { windows::Win32::UI::Shell::Shell_NotifyIconW(NIM_ADD, &nid) }.as_bool() {
        crate::log::log_line("[tray] Shell_NotifyIconW(NIM_ADD) 失败");
        return false;
    }
    // NOTIFYICON_VERSION_4：右键弹出菜单（WM_CONTEXTMENU）+ 左键 NIN_SELECT。
    let ver = NOTIFYICONDATAW {
        cbSize: size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: TRAY_ICON_UID,
        uFlags: NIF_MESSAGE,
        Anonymous: NOTIFYICONDATAW_0 { uVersion: NOTIFYICON_VERSION_4 },
        ..Default::default()
    };
    // SAFETY: 同上；NIM_SETVERSION 读取 Anonymous.uVersion。
    if !unsafe { windows::Win32::UI::Shell::Shell_NotifyIconW(NIM_SETVERSION, &ver) }.as_bool() {
        crate::log::log_line("[tray] Shell_NotifyIconW(NIM_SETVERSION) 失败（图标仍可见，仅无 v4 事件）");
    }
    true
}

/// 从本 DLL 资源加载托盘图标（LR_SHARED，系统管理生命周期，无需销毁）。
fn load_icon(id: u32) -> HICON {
    use std::os::raw::c_void;
    let mut module = windows::Win32::Foundation::HMODULE::default();
    // SAFETY: FROM_ADDRESS 把 lpModuleName 解释为函数地址，指向本 DLL 内的代码
    // （load_icon 在本 DLL 中，进程存活期间有效）。
    let ok = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
            PCWSTR(load_icon as *const c_void as *const u16),
            &mut module,
        )
    };
    if ok.is_err() || module.0.is_null() {
        crate::log::log_line("[tray] GetModuleHandleExW(FROM_ADDRESS) 失败");
        return HICON::default();
    }
    // SAFETY: MAKEINTRESOURCEW 语义 = 数字资源 ID（低 16 位有效，PCWSTR 整数转指针）。
    let name = PCWSTR::from_raw(id as usize as *const u16);
    let size = unsafe { GetSystemMetrics(SM_CXSMICON) };
    let cy = unsafe { GetSystemMetrics(SM_CYSMICON) };
    // SAFETY: 标准资源加载；hinst/name 在本调用期间有效；LR_SHARED 不销毁。
    let handle = unsafe {
        LoadImageW(
            Some(module.into()),
            name,
            IMAGE_ICON,
            size,
            cy,
            LR_SHARED,
        )
    }
    .unwrap_or_default();
    if handle.0.is_null() {
        crate::log::log_line(&format!("[tray] 托盘图标加载失败：id={id}"));
    }
    HICON(handle.0)
}

/// 托盘清理（幂等，`hosting` 闸）：NIM_DELETE + 释放互斥 + 隐藏菜单。
/// Drop（TrayHost）/ "退出 iuv" 共用；任一后本进程不再托管。
fn teardown() {
    let Some(tray) = TRAY.get() else {
        return;
    };
    if !tray.hosting.swap(false, Ordering::SeqCst) {
        return; // 已清理（幂等）
    }
    let hwnd = *tray.tray_hwnd.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(v) = hwnd {
        let hwnd = hwnd_from_usize(v);
        let nid = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: TRAY_ICON_UID,
            ..Default::default()
        };
        // SAFETY: NIM_DELETE 按 hWnd+uID 移除图标；nid 存活于调用期间。
        unsafe { let _ = windows::Win32::UI::Shell::Shell_NotifyIconW(NIM_DELETE, &nid); };
        *tray.tray_hwnd.lock().unwrap_or_else(|e| e.into_inner()) = None;
        // SAFETY: 在创建线程销毁托盘窗口
        unsafe { let _ = DestroyWindow(hwnd); };
    }
    if let Some(v) = tray.mutex_handle.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let mutex = handle_from_usize(v);
        // SAFETY: 释放命名互斥（让他进程可接替）+ 关闭句柄。bInitialOwner=false，
        // ReleaseMutex 失败（未持有）可忽略——CloseHandle 已释放名称。
        let _ = unsafe { ReleaseMutex(mutex) };
        // SAFETY: 关闭句柄（命名互斥释放即他进程可重新托管）。
        unsafe { let _ = CloseHandle(mutex); };
    }
    // 隐藏打开的菜单（线程局部存储；不销毁窗口——线程退出时自动清理）。
    MENU.with(|menu| {
        if let Some(menu) = menu.borrow_mut().as_mut() {
            menu.hide();
        }
    });
}

impl Drop for TrayHost {
    fn drop(&mut self) {
        teardown();
    }
}

/// 托盘回调：Shell_NotifyIcon 经 uCallbackMessage（WM_APP+1）送达。
/// lParam 低 16 位 = 鼠标消息：WM_CONTEXTMENU（右键）/ 高 16 位 = NIN_SELECT（左键）
/// → GetCursorPos → 弹自绘菜单。其余忽略（NIN_BALLOON* 等暂无处理）。
unsafe extern "system" fn tray_wnd_proc(
    _hwnd: HWND,
    msg: u32,
    _wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if msg == TRAY_CALLBACK_MSG {
        let lo = (lparam.0 as u32) & 0xFFFF;
        let hi = ((lparam.0 as u32) >> 16) & 0xFFFF;
        if lo == WM_CONTEXTMENU || hi == NIN_SELECT {
            let mut pt = POINT::default();
            // SAFETY: GetCursorPos 纯查询；pt 在调用期间存活。
            if unsafe { GetCursorPos(&mut pt) }.is_ok() {
                tray_popup(pt.x, pt.y);
            }
        }
        LRESULT(0)
    } else {
        DefWindowProcW(_hwnd, msg, _wparam, lparam)
    }
}

/// 弹自绘右键菜单（菜单项由本模块定义；点击 → on_menu_select 分发）。
fn tray_popup(x: i32, y: i32) {
    let Some(tray) = TRAY.get() else {
        return;
    };
    if !tray.hosting.load(Ordering::SeqCst) {
        return; // 已退出/未托管：不弹菜单
    }
    let items = vec![
        MenuEntry::new("设置…", MENU_SETTINGS),
        MenuEntry::new("帮助 / 关于", MENU_ABOUT),
        MenuEntry::separator(),
        MenuEntry::new("退出 iuv", MENU_QUIT),
    ];
    // 菜单窗口 = 线程局部存储（TSF 线程）：首次弹菜单时懒建，此后复用。
    MENU.with(|menu| {
        let mut guard = menu.borrow_mut();
        let m = guard.get_or_insert_with(|| MenuWindow::new(theme_for_config()));
        m.set_on_select(Some(Box::new(on_menu_select)));
        m.popup_at(items, x, y);
    });
}

/// 菜单主题按 config（light/dark，与候选窗一致；深色切换需重载输入法）。
fn theme_for_config() -> iuv_ui::Theme {
    match iuv_core::Config::load().theme {
        iuv_core::ThemeChoice::Light => iuv_ui::theme_light(),
        iuv_core::ThemeChoice::Dark => iuv_ui::theme_dark(),
    }
}

/// 菜单点击分发：设置…（M6 占位）/ 帮助/关于（MessageBoxW 版本）/ 退出 iuv。
fn on_menu_select(id: u16) {
    match id {
        MENU_SETTINGS => {
            crate::log::log_line("[tray] 菜单：设置…（M6 打开设置页，本轮占位）");
        }
        MENU_ABOUT => show_about(),
        MENU_QUIT => {
            crate::log::log_line("[tray] 菜单：退出 iuv（移除图标 + 释放互斥，本进程不再托管）");
            quit_host();
        }
        _ => crate::log::log_line(&format!("[tray] 菜单：未知 id={id}")),
    }
}

/// "退出 iuv"：本进程不再托管托盘（图标移除 + 互斥释放 → 他进程可接替）。
/// 输入法本身不卸载，按键照常（托盘与按键管线无关）。
fn quit_host() {
    let Some(tray) = TRAY.get() else {
        return;
    };
    tray.quit.store(true, Ordering::SeqCst);
    teardown();
}

/// 帮助/关于：MessageBoxW 显示版本信息（不 panic；失败记日志）。
fn show_about() {
    let text: Vec<u16> =
        "iuv 输入法（IUV 输入法）\nv0.1.0（M5：托盘 + 自绘右键菜单预览）\n\nWindows TSF 中文输入法，用户掌控排序。"
            .encode_utf16()
            .collect();
    let title: Vec<u16> = "关于 iuv 输入法".encode_utf16().collect();
    // SAFETY: 两个宽字符串缓冲在本调用期间存活；无 owner 窗口（MB_OK + 系统图标）。
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

/// 编译期宽字符串字面量（生成以 NUL 结尾的静态 UTF-16 数组；MUTEX_NAME/TRAY_CLASS_NAME 用）。
const fn wide(s: &str) -> [u16; 32] {
    let mut out = [0u16; 32];
    let mut i = 0;
    let bytes = s.as_bytes();
    while i < bytes.len() && i < 31 {
        out[i] = bytes[i] as u16; // 仅 ASCII：宽字符与字节同值
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_ascii_nul_terminated() {
        let exp: Vec<u16> = b"Local\\iuv-tray-host".iter().map(|&b| b as u16).collect();
        let a = wide("Local\\iuv-tray-host");
        assert_eq!(&a[..exp.len()], exp.as_slice());
        assert_eq!(a[exp.len()], 0, "NUL 结尾");
        let exp2: Vec<u16> = b"IuvTrayWindow".iter().map(|&b| b as u16).collect();
        let b = wide("IuvTrayWindow");
        assert_eq!(&b[..exp2.len()], exp2.as_slice());
        assert_eq!(b[exp2.len()], 0, "NUL 结尾");
    }

    #[test]
    fn mutex_name_ends_with_expected() {
        // 断言互斥名内容（会话级单实例标识）。
        let name: Vec<u16> = "Local\\iuv-tray-host".encode_utf16().collect();
        assert_eq!(&MUTEX_NAME[..name.len()], name.as_slice());
        assert_eq!(MUTEX_NAME[name.len()], 0, "null 结尾");
    }
}
