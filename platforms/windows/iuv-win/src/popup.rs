//! 自绘弹窗窗口公共骨架（P2.5）：类注册 / 创建 / DPI / GWLP_USERDATA 挂接 /
//! wndproc 默认臂 / Drop。候选窗、自绘菜单、工具栏等 ULW 呈现窗口复用。
//!
//! 约束（与 ULW 呈现一致）：窗口必须建在使用线程；`attach` 存的是**外层窗口结构**
//! 指针，`Drop` 先清零 GWLP_USERDATA 再销毁——wnd_proc 经 `get_self(_mut)` 取回
//! 的指针不悬垂。全部方法不 panic：失败记日志并静默降级（窗口保持无效）。

use std::mem::size_of;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    GetLastError, ERROR_CLASS_ALREADY_EXISTS, HWND, LPARAM, LRESULT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{GetDC, GetDeviceCaps, ReleaseDC, LOGPIXELSY};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, GetWindowLongPtrW, LoadCursorW, RegisterClassExW,
    SetWindowLongPtrW, CS_HREDRAW, CS_VREDRAW, GWLP_USERDATA, IDC_ARROW, WS_POPUP, WNDCLASSEXW,
    WINDOW_EX_STYLE,
};

/// 类窗口过程签名（与 WNDCLASSEXW.lpfnWndProc 一致）。
type WndProc = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;

/// ULW 自绘弹窗窗口骨架：持有 HWND，负责类注册 / 创建 / 指针挂接 / DPI 查询 / 销毁。
/// 具体窗口（候选窗/菜单/工具栏）持有本结构，wnd_proc 中经 `get_self(_mut)` 取回
/// 自身处理业务消息；业务不命中的默认臂走 `default_wnd_proc`。
pub struct LayeredWindow {
    pub hwnd: HWND,
}

impl LayeredWindow {
    /// 空窗口（未建；`create` 后有效）。
    pub fn new() -> Self {
        LayeredWindow {
            hwnd: HWND::default(),
        }
    }

    /// 是否已建窗（hwnd 有效）。
    pub fn is_created(&self) -> bool {
        !self.hwnd.is_invalid()
    }

    /// 进程内注册窗口类（幂等：ERROR_CLASS_ALREADY_EXISTS 视为成功）。失败仅记日志。
    pub fn register_class(class_name: PCWSTR, wnd_proc: WndProc, log_tag: &str) {
        // SAFETY: 所有字段显式/Default 填充；类名为静态宽字符串，进程生命周期有效。
        unsafe {
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(wnd_proc),
                // 类默认光标 = 箭头：hCursor 为 NULL 时 DefWindowProc 不设光标，悬停
                // 残留上一窗口的形状（实测忙等漏斗）。具体窗口可再经 WM_SETCURSOR 覆盖。
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                hbrBackground: windows::Win32::Graphics::Gdi::HBRUSH::default(),
                lpszMenuName: PCWSTR::null(),
                lpszClassName: class_name,
                ..Default::default()
            };
            if RegisterClassExW(&class) == 0 {
                let err = GetLastError();
                if err != ERROR_CLASS_ALREADY_EXISTS {
                    crate::log_line(&format!("[{log_tag}] RegisterClassExW 失败"));
                }
            }
        }
    }

    /// 创建窗口（WS_POPUP + 指定扩展样式，0,0,0,0 起步）并挂接外层结构指针。
    /// 类注册幂等（ALREADY_EXISTS 视为成功）。返回是否成功；失败保持 hwnd 无效
    /// （记日志，不 panic）。`wnd_proc` 为具体窗口自己的类窗口过程（业务消息 +
    /// 默认臂转发 `default_wnd_proc`）。
    pub fn create<T>(
        &mut self,
        class_name: PCWSTR,
        ex_style: WINDOW_EX_STYLE,
        wnd_proc: WndProc,
        outer: *mut T,
        log_tag: &str,
    ) -> bool {
        if self.is_created() {
            return true;
        }
        Self::register_class(class_name, wnd_proc, log_tag);
        // SAFETY: GetModuleHandleW(None) 取当前进程实例句柄
        let hinst = unsafe { GetModuleHandleW(None) }.unwrap_or_default();
        if hinst.is_invalid() {
            crate::log_line(&format!("[{log_tag}] GetModuleHandleW 失败"));
            return false;
        }
        // SAFETY: 扩展样式由调用方给定（ULW 弹窗一般 TOPMOST|TOOLWINDOW[±NOACTIVATE]）；
        // WS_POPUP 无边框。
        let hwnd = unsafe {
            CreateWindowExW(
                ex_style,
                class_name,
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
            crate::log_line(&format!("[{log_tag}] CreateWindowExW 失败"));
            return false;
        };
        // SAFETY: outer 在创建线程存活；Drop 先清零 GWLP_USERDATA 再销毁窗口。
        // `as usize as _` 按平台推断：x64 = isize（指针同宽），x86 = i32（32 位指针无损）。
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, outer as usize as _) };
        self.hwnd = hwnd;
        true
    }

    /// 当前 DPI：窗口 HDC 的 `LOGPIXELSY`，失败兜底 96。
    pub fn dpi(&self) -> u32 {
        // SAFETY: hdc 由 GetDC 取得，使用后立即 ReleaseDC
        unsafe {
            let hdc = GetDC(Some(self.hwnd));
            if hdc.is_invalid() {
                return 96;
            }
            let dpi = GetDeviceCaps(Some(hdc), LOGPIXELSY);
            ReleaseDC(Some(self.hwnd), hdc);
            if dpi <= 0 {
                96
            } else {
                dpi as u32
            }
        }
    }

    /// lparam 低 32 位的坐标 (x, y)（WM_MOUSEMOVE 等 = 客户区；WM_NCHITTEST = 屏幕坐标）。
    pub fn client_pos(lparam: LPARAM) -> (i32, i32) {
        let v = lparam.0 as u32;
        ((v & 0xFFFF) as i32, ((v >> 16) & 0xFFFF) as i32)
    }

    /// 从 GWLP_USERDATA 取回窗口属主；0（未挂接/已销毁）返回 None。
    ///
    /// # Safety
    /// 指针在窗口销毁前由 Drop 先清零，不悬垂；调用都在创建线程。
    pub unsafe fn get_self<T>(hwnd: HWND) -> Option<&'static T> {
        let p = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
        if p == 0 {
            None
        } else {
            // SAFETY: p 非 0 即此前 SetWindowLongPtrW 写入的有效指针
            Some(unsafe { &*(p as *const T) })
        }
    }

    /// 可变版（wnd_proc 更新窗口本地状态用）。线程约束同 `get_self`。
    ///
    /// # Safety
    /// 同 get_self：指针生命周期由 Drop 清零保证；调用都在创建线程。
    pub unsafe fn get_self_mut<T>(hwnd: HWND) -> Option<&'static mut T> {
        let p = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
        if p == 0 {
            None
        } else {
            // SAFETY: p 非 0 即此前 SetWindowLongPtrW 写入的有效指针
            Some(unsafe { &mut *(p as *mut T) })
        }
    }

    /// 业务不命中的默认窗口过程（DefWindowProcW 转发）。
    ///
    /// # Safety
    /// wnd_proc 签名与系统回调约定一致。
    pub unsafe extern "system" fn default_wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
    }
}

impl Default for LayeredWindow {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for LayeredWindow {
    fn drop(&mut self) {
        if self.is_created() {
            // SAFETY: 先清零 GWLP_USERDATA，杜绝 wnd_proc 访问到即将释放的外层结构
            let _ = unsafe { SetWindowLongPtrW(self.hwnd, GWLP_USERDATA, 0) };
            // SAFETY: 在创建线程上销毁窗口
            unsafe {
                let _ = DestroyWindow(self.hwnd);
            }
            self.hwnd = HWND::default();
        }
    }
}