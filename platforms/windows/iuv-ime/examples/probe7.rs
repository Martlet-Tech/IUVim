//! 完整 IMM 模拟探针：激活布局 + 可见窗口 + ImmGetContext + 发送按键 → 验证 ImeProcessKey 被调。
//! 用法：cargo build --target i686-pc-windows-msvc -p iuv-ime --example probe7 --release

use std::ffi::c_void;
use std::thread;
use std::time::Duration;

use windows::Win32::UI::Input::KeyboardAndMouse::{LoadKeyboardLayoutW, KLF_ACTIVATE};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DestroyWindow, PostMessageW, WM_KEYDOWN,
};

#[link(name = "imm32")]
extern "system" {
    fn ImmGetContext(hwnd: *mut c_void) -> *mut c_void;
    fn ImmReleaseContext(hwnd: *mut c_void, himc: *mut c_void) -> i32;
}

fn main() {
    println!("== 1. LoadKeyboardLayoutW(E0210804, KLF_ACTIVATE) ==");
    unsafe { LoadKeyboardLayoutW(windows::core::w!("E0210804"), KLF_ACTIVATE) }
        .map(|h| println!("HKL = {h:?}"))
        .unwrap_or_else(|e| println!("失败: {e}"));

    println!("== 2. 创建普通窗口 ==");
    let hwnd = unsafe {
        CreateWindowExW(
            Default::default(),
            windows::core::w!("STATIC"),
            windows::core::w!("iuv-probe7"),
            Default::default(),
            100,
            100,
            200,
            100,
            None,
            None,
            None,
            None,
        )
        .unwrap()
    };

    println!("== 3. ImmGetContext ==");
    let himc = unsafe { ImmGetContext(hwnd.0 as *mut c_void) };
    println!("HIMC = {himc:p}");

    println!("== 4. 发送按键 'a' (0x41) 和 'w' (0x57) ==");
    unsafe {
        PostMessageW(Some(hwnd), WM_KEYDOWN, windows::Win32::Foundation::WPARAM(0x41), windows::Win32::Foundation::LPARAM(0x1E0001));
        PostMessageW(Some(hwnd), WM_KEYDOWN, windows::Win32::Foundation::WPARAM(0x57), windows::Win32::Foundation::LPARAM(0x110001));
    }

    println!("== 5. 消息循环 2 秒（触发 TranslateMessage → imm32 钩子） ==");
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    loop {
        if std::time::Instant::now() >= deadline {
            break;
        }
        let mut msg = std::mem::MaybeUninit::<windows::Win32::UI::WindowsAndMessaging::MSG>::uninit();
        let ret = unsafe { windows::Win32::UI::WindowsAndMessaging::PeekMessageW(msg.as_mut_ptr(), None, 0, 0, windows::Win32::UI::WindowsAndMessaging::PM_REMOVE) };
        if ret.as_bool() {
            let msg = unsafe { msg.assume_init() };
            unsafe {
                windows::Win32::UI::WindowsAndMessaging::TranslateMessage(&msg);
                windows::Win32::UI::WindowsAndMessaging::DispatchMessageW(&msg);
            }
        } else {
            thread::sleep(Duration::from_millis(10));
        }
    }

    if !himc.is_null() {
        unsafe {
            ImmReleaseContext(hwnd.0 as *mut c_void, himc);
        }
    }
    unsafe { DestroyWindow(hwnd) }.ok();
    println!("== 完成，检查 %TEMP%\\iuv-ime.log ==");
}
