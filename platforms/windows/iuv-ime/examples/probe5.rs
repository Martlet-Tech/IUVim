//! IMM 触发探针：LoadKeyboardLayout + ImmGetContext + ImeSelect 触发，验证 iuv.ime 被调用。
//! 用法：cargo build --target i686-pc-windows-msvc -p iuv-ime --example probe5 --release

use std::ffi::c_void;

use windows::Win32::UI::Input::KeyboardAndMouse::{LoadKeyboardLayoutW, KLF_ACTIVATE};
use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DestroyWindow, HWND_MESSAGE};

#[link(name = "imm32")]
extern "system" {
    fn ImmGetContext(hwnd: *mut c_void) -> *mut c_void;
    fn ImmReleaseContext(hwnd: *mut c_void, himc: *mut c_void) -> i32;
    fn ImmGetOpenStatus(himc: *mut c_void) -> i32;
    fn ImmSetOpenStatus(himc: *mut c_void, open: i32) -> i32;
}

fn main() {
    println!("== 1. LoadKeyboardLayoutW(E0210804, KLF_ACTIVATE) ==");
    match unsafe { LoadKeyboardLayoutW(windows::core::w!("E0210804"), KLF_ACTIVATE) } {
        Ok(h) => println!("HKL = {h:?}"),
        Err(e) => {
            println!("失败: {e}");
            return;
        }
    }

    println!("== 2. 创建窗口 + ImmGetContext ==");
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
    };
    match hwnd {
        Ok(h) => println!("窗口: {h:?}"),
        Err(e) => {
            println!("窗口创建失败: {e}");
            return;
        }
    }
    let hwnd = hwnd.unwrap();

    let himc = unsafe { ImmGetContext(hwnd.0 as *mut c_void) };
    println!("HIMC = {himc:p}");
    if !himc.is_null() {
        let open = unsafe { ImmGetOpenStatus(himc) };
        println!("ImmGetOpenStatus = {open}");
        unsafe {
            ImmSetOpenStatus(himc, 1);
        }
        let open2 = unsafe { ImmGetOpenStatus(himc) };
        println!("ImmSetOpenStatus(1) 后 = {open2}");
        unsafe {
            ImmReleaseContext(hwnd.0 as *mut c_void, himc);
        }
    }

    unsafe { DestroyWindow(hwnd) }.ok();
    println!("== 完成，检查 %TEMP%\\iuv-ime.log ==");
}
