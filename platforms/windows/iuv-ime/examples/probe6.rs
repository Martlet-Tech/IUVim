//! 绑定探针：GetKeyboardLayout(当前线程) + ImmGetIMEFileName(HIMC 绑定的 IME 文件)。
//! 用法：cargo build --target i686-pc-windows-msvc -p iuv-ime --example probe6 --release

use std::ffi::c_void;

use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyboardLayout, LoadKeyboardLayoutW, KLF_ACTIVATE};
use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow};

#[link(name = "imm32")]
extern "system" {
    fn ImmGetContext(hwnd: *mut c_void) -> *mut c_void;
    fn ImmReleaseContext(hwnd: *mut c_void, himc: *mut c_void) -> i32;
    fn ImmGetIMEFileNameA(himc: *mut c_void, buf: *mut u8, len: u32) -> u32;
}

fn main() {
    println!("== LoadKeyboardLayoutW(E0200804, KLF_ACTIVATE) ==");
    unsafe { LoadKeyboardLayoutW(windows::core::w!("E0200804"), KLF_ACTIVATE) }
        .map(|h| println!("HKL = {h:?}"))
        .unwrap_or_else(|e| println!("失败: {e}"));

    let cur = unsafe { GetKeyboardLayout(0) };
    println!("当前线程布局: 0x{:08X}", cur.0 as u32);

    let hwnd = unsafe {
        CreateWindowExW(
            Default::default(),
            windows::core::w!("STATIC"),
            windows::core::w!("iuv-probe"),
            Default::default(),
            0,
            0,
            0,
            0,
            Some(windows::Win32::UI::WindowsAndMessaging::HWND_MESSAGE),
            None,
            None,
            None,
        )
        .unwrap()
    };

    let himc = unsafe { ImmGetContext(hwnd.0 as *mut c_void) };
    println!("HIMC = {himc:p}");
    if !himc.is_null() {
        let mut buf = [0u8; 512];
        let n = unsafe { ImmGetIMEFileNameA(himc, buf.as_mut_ptr(), 512) };
        let name = String::from_utf8_lossy(&buf[..n as usize]);
        println!("ImmGetIMEFileName => [{name}] (len={n})");
        unsafe {
            ImmReleaseContext(hwnd.0 as *mut c_void, himc);
        }
    }
    unsafe { DestroyWindow(hwnd) }.ok();
}
