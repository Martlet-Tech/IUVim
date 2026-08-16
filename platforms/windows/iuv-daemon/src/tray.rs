//! 托盘图标 + 自绘右键菜单（M6 守护进程持有；M5 单实例机制退役）。
//!
//! - 托盘：`Shell_NotifyIconW`（NIF_MESSAGE|ICON|TIP），消息回调 WM_TRAY → 主线程消息循环。
//! - 菜单：iuv-ui `render_menu` 自绘（tiny-skia）→ GDI DIB + `UpdateLayeredWindow` 呈现
//!   （premultiplied BGRA 直供，圆角/阴影真透明）；非激活窗口（WS_EX_NOACTIVATE）+ 捕获
//!   鼠标（SetCapture）处理悬停高亮 / 点击分发 / 点击外部关闭。
//! - 与 M5 约定：托盘宿主互斥 `Local\iuv-tray-host` 由 main 获取（本模块只托管图标）。
//!
//! 全部函数不 panic：任何失败记日志并静默降级（托盘不可用 = 管道服务仍工作）。

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use iuv_ui::layout::Rect;
use iuv_ui::{render_menu, theme_dark, theme_light, MenuEntry, Surface, TextRenderer, Theme};
use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, GetDeviceCaps, ReleaseDC,
    AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, HGDIOBJ,
    LOGPIXELSY,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Shell::{Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetCursorPos, GetWindowLongPtrW, GetWindowRect,
    LoadIconW, MessageBoxW, RegisterClassExW, SetWindowLongPtrW, ShowWindow, SystemParametersInfoW,
    GWLP_USERDATA, IDI_APPLICATION, MB_ICONINFORMATION, MB_OK, SPI_GETWORKAREA, SW_SHOWNOACTIVATE,
    WM_APP, WM_CONTEXTMENU, WM_DESTROY, WM_ERASEBKGND, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MBUTTONUP, WM_MOUSEMOVE, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_TIMER, WS_EX_LAYERED,
    WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WNDCLASSEXW,
};

use crate::log;
use crate::settings;
use crate::state::DaemonState;

/// 托盘回调消息。
pub const WM_TRAY: u32 = WM_APP + 1;
/// 主线程 flush 定时器 id（main 的 SetTimer 使用）。
pub const TIMER_FLUSH: usize = 1;

const MENU_OPEN_SETTINGS: u16 = 1;
const MENU_ABOUT: u16 = 2;
const MENU_EXIT: u16 = 3;

const TRAY_CLASS: PCWSTR = w!("IuvDaemonTray");
const MENU_CLASS: PCWSTR = w!("IuvDaemonMenu");
const TRAY_UID: u32 = 1;

/// 全局状态（托盘/菜单 wnd_proc 在主线程，无并发）。
static STATE: OnceLock<Arc<DaemonState>> = OnceLock::new();
/// 当前打开的菜单窗口句柄（防重复弹出；WM_DESTROY 清零）。
static CURRENT_MENU: AtomicUsize = AtomicUsize::new(0);

/// 安装托盘图标（守护进程主线程调用一次）。失败 → None（记日志，管道服务不受影响）。
pub fn install(state: Arc<DaemonState>) -> Option<HWND> {
    let _ = STATE.set(state);
    register_tray_class();
    let hinst = unsafe { GetModuleHandleW(None) }.unwrap_or_default();
    if hinst.is_invalid() {
        log::log_line("[tray] GetModuleHandleW 失败");
        return None;
    }
    // SAFETY: 隐藏窗口（无边框/无消息外的行为），仅接收托盘回调消息。
    let hwnd = unsafe {
        CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            TRAY_CLASS,
            PCWSTR::null(),
            windows::Win32::UI::WindowsAndMessaging::WINDOW_STYLE(0),
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
        log::log_line("[tray] 创建托盘窗口失败");
        return None;
    };
    if !add_icon(hwnd) {
        log::log_line("[tray] 托盘图标注册失败（守护进程继续运行，仅管道服务可用）");
        return Some(hwnd); // 窗口仍在（定时器/flush 依赖它）
    }
    log::log_line("[tray] 托盘图标已注册");
    Some(hwnd)
}

/// 移除托盘图标（退出清理）。
pub fn remove_icon(hwnd: HWND) {
    let nid = make_nid(hwnd);
    // SAFETY: nid 结构有效；NIM_DELETE 移除。
    let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &nid) };
    log::log_line("[tray] 托盘图标已移除");
}

fn add_icon(hwnd: HWND) -> bool {
    // SAFETY: 标准系统图标（IDI_APPLICATION）；LoadIconW(None) 从系统图标取。
    let Ok(icon) = (unsafe { LoadIconW(None, IDI_APPLICATION) }) else {
        log::log_line("[tray] LoadIconW 失败");
        return false;
    };
    let mut nid = make_nid(hwnd);
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = WM_TRAY;
    nid.hIcon = icon;
    // szTip：UTF-16 拷贝（≤127 字符 + NUL）。
    let tip: Vec<u16> = "iuv 输入法".encode_utf16().collect();
    for (i, c) in tip.iter().take(nid.szTip.len() - 1).enumerate() {
        nid.szTip[i] = *c;
    }
    // SAFETY: nid 字段齐全（cbSize/hWnd/uID/…）；NIM_ADD 注册托盘。
    unsafe { Shell_NotifyIconW(NIM_ADD, &nid) }.as_bool()
}

/// 构造空 NOTIFYICONDATAW（cbSize/hWnd/uID 填好）。
fn make_nid(hwnd: HWND) -> NOTIFYICONDATAW {
    // SAFETY: NOTIFYICONDATAW 为 POD；zeroed 后填关键字段（其余默认 0）。
    let mut nid: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = TRAY_UID;
    nid
}

fn register_tray_class() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        // SAFETY: 类字段齐全；注册失败多为"已注册"，忽略。
        unsafe {
            let class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(tray_wnd_proc),
                hInstance: GetModuleHandleW(None).unwrap_or_default().into(),
                lpszClassName: TRAY_CLASS,
                ..Default::default()
            };
            RegisterClassExW(&class);
        }
    });
}

/// 托盘窗口过程：WM_TRAY（左键=设置，右键=菜单）、WM_TIMER（flush）。
unsafe extern "system" fn tray_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_TRAY => match lparam.0 as u32 {
            WM_LBUTTONUP => {
                if let Some(s) = STATE.get() {
                    settings::open(s);
                }
                LRESULT(0)
            }
            WM_RBUTTONUP | WM_CONTEXTMENU => {
                show_menu(hwnd);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        },
        WM_TIMER => {
            if wparam.0 as usize == TIMER_FLUSH {
                if let Some(s) = STATE.get() {
                    s.flush_if_dirty();
                }
                LRESULT(0)
            } else {
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
        }
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

/// 菜单窗口状态（GWLP_USERDATA 挂指针；WM_DESTROY 时释放）。
struct MenuWindow {
    hwnd: HWND,
    owner: HWND,
    items: Vec<MenuEntry>,
    rows: Vec<Rect>,
    selected: Option<usize>,
    surface: Surface,
    theme: Theme,
    scale: f32,
    text: TextRenderer,
}

/// 显示自绘右键菜单（光标处；主线程调用）。
fn show_menu(owner: HWND) {
    let Some(state) = STATE.get() else { return };
    // 关闭既有菜单（防叠加）。
    let existing = CURRENT_MENU.swap(0, Ordering::AcqRel);
    if existing != 0 {
        let _ = unsafe { DestroyWindow(HWND(existing as *mut std::ffi::c_void)) };
    }
    let theme = {
        let cfg = state.config.lock().unwrap_or_else(|p| p.into_inner());
        match cfg.theme.as_str() {
            "dark" => theme_dark(),
            _ => theme_light(),
        }
    };
    let items = vec![
        MenuEntry::new("打开设置", MENU_OPEN_SETTINGS),
        MenuEntry::separator(),
        MenuEntry::new("关于 iuv", MENU_ABOUT),
        MenuEntry::separator(),
        MenuEntry::new("退出 iuv", MENU_EXIT),
    ];
    let mut pt = POINT::default();
    // SAFETY: pt 输出光标屏幕坐标。
    if unsafe { GetCursorPos(&mut pt) }.is_err() {
        log::log_line("[tray] GetCursorPos 失败");
        return;
    }
    let scale = dpi_scale();
    let mut text = TextRenderer::new();
    let (surface, rows) = render_menu(&items, None, &theme, scale, &mut text);
    if surface.w == 0 || surface.h == 0 {
        log::log_line("[tray] render_menu 空 surface");
        return;
    }
    let w = surface.w as i32;
    let h = surface.h as i32;
    // 工作区约束（防菜单越出屏幕）。
    let (x, y) = clamp_to_work_area(pt.x, pt.y, w, h);
    register_menu_class();
    let hinst = unsafe { GetModuleHandleW(None) }.unwrap_or_default();
    // SAFETY: 分层置顶非激活弹出窗（圆角/阴影真透明）；无边框。
    let hwnd = unsafe {
        CreateWindowExW(
            WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_LAYERED,
            MENU_CLASS,
            PCWSTR::null(),
            WS_POPUP,
            x,
            y,
            w,
            h,
            None,
            None,
            Some(hinst.into()),
            None,
        )
    };
    let Ok(hwnd) = hwnd else {
        log::log_line("[tray] 创建菜单窗口失败");
        return;
    };
    let menu = Box::new(MenuWindow {
        hwnd,
        owner,
        items,
        rows,
        selected: None,
        surface,
        theme,
        scale,
        text,
    });
    // SAFETY: MenuWindow 泄漏至 WM_DESTROY 释放（Drop 前指针有效）；与 GetWindowLongPtrW 配对。
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(menu) as isize) };
    CURRENT_MENU.store(hwnd.0 as usize, Ordering::Release);
    // 首帧呈现 + 显示。
    if let Some(m) = menu_mut(hwnd) {
        present_layered(m);
    }
    // SAFETY: 捕获鼠标（菜单外点击也能收到，用于关闭）；SW_SHOWNOACTIVATE 不抢焦点。
    unsafe {
        SetCapture(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
    }
    log::log_line("[tray] 菜单已显示");
}

/// 取菜单窗口可变引用（GWLP_USERDATA；指针必有效——窗口存活期内持有）。
fn menu_mut(hwnd: HWND) -> Option<&'static mut MenuWindow> {
    // SAFETY: 由 show_menu 置入且 WM_DESTROY 才释放；主线程单线程访问。
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut MenuWindow;
    if ptr.is_null() {
        None
    } else {
        // SAFETY: 同上；非空指针有效。
        Some(unsafe { &mut *ptr })
    }
}

/// 把菜单 surface 呈现到分层窗口（premultiplied BGRA → DIB → UpdateLayeredWindow）。
fn present_layered(m: &mut MenuWindow) {
    let surf = &m.surface;
    let w = surf.w as i32;
    let h = surf.h as i32;
    if w <= 0 || h <= 0 {
        return;
    }
    // SAFETY: 屏 DC（GetDC(None)）+ 兼容内存 DC 成对释放。
    let screen_dc = unsafe { GetDC(None) };
    let mem_dc = unsafe { CreateCompatibleDC(Some(screen_dc)) };
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h, // 顶层向下（top-down）
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        bmiColors: [windows::Win32::Graphics::Gdi::RGBQUAD::default()],
    };
    let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
    // SAFETY: 32bpp top-down DIB；bits 输出像素缓冲指针。
    let dib = unsafe { CreateDIBSection(Some(mem_dc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0) };
    let Ok(dib) = dib else {
        // SAFETY: 释放 DC；GetDC(None) 用 ReleaseDC(None)。
        unsafe {
            let _ = DeleteDC(mem_dc);
            let _ = ReleaseDC(None, screen_dc);
        }
        log::log_line("[tray] CreateDIBSection 失败（菜单呈现降级）");
        return;
    };
    // SAFETY: bits 非空（CreateDIBSection 成功）；surface 行宽 w*4 恒 4 对齐（top-down 直拷）。
    unsafe {
        std::ptr::copy_nonoverlapping(surf.pixels.as_ptr(), bits as *mut u8, surf.pixels.len());
    }
    let mut wr = RECT::default();
    // SAFETY: wr 输出窗口矩形。
    let _ = unsafe { GetWindowRect(m.hwnd, &mut wr) };
    let size = SIZE { cx: w, cy: h };
    let pos = POINT {
        x: wr.left,
        y: wr.top,
    };
    let src = POINT { x: 0, y: 0 };
    // SAFETY: 分层窗口呈现：premultiplied BGRA + AC_SRC_ALPHA（真透明）。
    let _ = unsafe {
        windows::Win32::UI::WindowsAndMessaging::UpdateLayeredWindow(
            m.hwnd,
            Some(screen_dc),
            Some(&pos),
            Some(&size),
            Some(mem_dc),
            Some(&src),
            windows::Win32::Foundation::COLORREF(0),
            Some(&windows::Win32::Graphics::Gdi::BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            }),
            windows::Win32::UI::WindowsAndMessaging::ULW_ALPHA,
        )
    };
    // SAFETY: 释放 GDI 对象；顺序无关紧要。
    unsafe {
        let _ = DeleteObject(HGDIOBJ(dib.0));
        let _ = DeleteDC(mem_dc);
        let _ = ReleaseDC(None, screen_dc);
    }
}

/// 光标位置钳制到工作区（菜单完整可见）。
fn clamp_to_work_area(mx: i32, my: i32, w: i32, h: i32) -> (i32, i32) {
    let mut work = RECT::default();
    // SAFETY: work 输出工作区矩形。
    let _ = unsafe {
        SystemParametersInfoW(
            SPI_GETWORKAREA,
            0,
            Some(&mut work as *mut RECT as *mut std::ffi::c_void),
            windows::Win32::UI::WindowsAndMessaging::SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    let clamp = |v: i32, lo: i32, hi: i32| {
        if hi <= lo {
            v
        } else {
            v.max(lo).min(hi)
        }
    };
    (
        clamp(mx, work.left, work.right - w),
        clamp(my, work.top, work.bottom - h),
    )
}

/// 当前 DPI 缩放（LOGPIXELSY / 96；失败兜底 1.0）。
fn dpi_scale() -> f32 {
    // SAFETY: 屏 DC 生命周期内使用 GetDeviceCaps。
    unsafe {
        let dc = GetDC(None);
        let dpi = GetDeviceCaps(Some(dc), LOGPIXELSY);
        ReleaseDC(None, dc);
        if dpi <= 0 {
            1.0
        } else {
            dpi as f32 / 96.0
        }
    }
}

fn register_menu_class() {
    static ONCE: OnceLock<()> = OnceLock::new();
    ONCE.get_or_init(|| {
        // SAFETY: 类字段齐全；注册失败多为"已注册"，忽略。
        unsafe {
            let class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(menu_wnd_proc),
                hInstance: GetModuleHandleW(None).unwrap_or_default().into(),
                lpszClassName: MENU_CLASS,
                ..Default::default()
            };
            RegisterClassExW(&class);
        }
    });
}

/// LPARAM 低位 = 客户端坐标 X（带符号）。
fn lparam_x(l: isize) -> i32 {
    (l & 0xFFFF) as i16 as i32
}

/// LPARAM 高位 = 客户端坐标 Y（带符号）。
fn lparam_y(l: isize) -> i32 {
    ((l >> 16) & 0xFFFF) as i16 as i32
}

/// 菜单窗口过程：悬停高亮 / 点击分发 / 外部点击关闭 / 销毁释放。
unsafe extern "system" fn menu_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_MOUSEMOVE => {
            if let Some(m) = menu_mut(hwnd) {
                let pt = POINT {
                    x: lparam_x(lparam.0),
                    y: lparam_y(lparam.0),
                };
                let idx = iuv_ui::menu_hit_test(&m.rows, pt.x, pt.y);
                if idx != m.selected {
                    m.selected = idx;
                    // 重渲染 + 呈现（分隔线不高亮）。
                    let (surface, rows) =
                        render_menu(&m.items, m.selected, &m.theme, m.scale, &mut m.text);
                    if surface.w > 0 && surface.h > 0 {
                        m.surface = surface;
                        m.rows = rows;
                        present_layered(m);
                    }
                }
            }
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if let Some(m) = menu_mut(hwnd) {
                let pt = POINT {
                    x: lparam_x(lparam.0),
                    y: lparam_y(lparam.0),
                };
                let idx = iuv_ui::menu_hit_test(&m.rows, pt.x, pt.y);
                if let Some(i) = idx {
                    let id = m.items[i].id;
                    if id != 0 {
                        dispatch_menu(m.owner, id);
                    }
                }
            }
            close_menu(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONDOWN | WM_RBUTTONDOWN | WM_RBUTTONUP | WM_MBUTTONUP => {
            // 捕获鼠标下点击空白（负坐标/行外）→ 关闭；行内按下等抬起分发。
            if let Some(m) = menu_mut(hwnd) {
                let pt = POINT {
                    x: lparam_x(lparam.0),
                    y: lparam_y(lparam.0),
                };
                if iuv_ui::menu_hit_test(&m.rows, pt.x, pt.y).is_none() {
                    close_menu(hwnd);
                }
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            if CURRENT_MENU.load(Ordering::Acquire) == hwnd.0 as usize {
                CURRENT_MENU.store(0, Ordering::Release);
            }
            // SAFETY: 释放 Box（show_menu 置入的指针）；先清 USERDATA 防残留。
            let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA);
            if ptr != 0 {
                let _ = ReleaseCapture();
                drop(Box::from_raw(ptr as *mut MenuWindow));
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            }
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1), // 分层窗口无背景擦除需求
        _ => DefWindowProcW(hwnd, msg, wparam, lparam),
    }
}

fn close_menu(hwnd: HWND) {
    // SAFETY: DestroyWindow 同步发 WM_DESTROY（释放 MenuWindow + ReleaseCapture）。
    let _ = unsafe { DestroyWindow(hwnd) };
}

/// 分发菜单动作。
fn dispatch_menu(owner: HWND, id: u16) {
    let Some(state) = STATE.get() else { return };
    match id {
        MENU_OPEN_SETTINGS => settings::open(state),
        MENU_ABOUT => {
            // SAFETY: 模态提示框（父窗 = 托盘隐藏窗）；静态 UTF-16 文本。
            unsafe {
                MessageBoxW(
                    Some(owner),
                    w!("iuv 输入法（iuvim）\n\n版本 0.1.0（M6 守护进程）\n中文输入法 · 用户掌控排序"),
                    w!("关于 iuv"),
                    MB_OK | MB_ICONINFORMATION,
                );
            }
        }
        MENU_EXIT => {
            log::log_line("[tray] 用户选择「退出 iuv」");
            state.close_settings.store(true, Ordering::Release);
            // SAFETY: 主线程消息循环收到 WM_QUIT 后退出（本过程运行在主线程）。
            unsafe { windows::Win32::UI::WindowsAndMessaging::PostQuitMessage(0) };
        }
        _ => {}
    }
}