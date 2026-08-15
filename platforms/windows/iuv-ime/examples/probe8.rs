//! A/B 探针：E0200804 布局 + Ime File=WEASEL.IME，完整 IMM 流程后检查 weasel.ime 是否被系统加载。
//! 用法：cargo build --target i686-pc-windows-msvc -p iuv-ime --example probe8 --release

use std::ffi::c_void;
use std::thread;
use std::time::Duration;

use windows::Win32::System::LibraryLoader::GetModuleHandleW;
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
    println!("== LoadKeyboardLayoutW(E0200804, KLF_ACTIVATE) ==");
    unsafe { LoadKeyboardLayoutW(windows::core::w!("E0200804"), KLF_ACTIVATE) }
        .map(|h| println!("HKL = {h:?}"))
        .unwrap_or_else(|e| println!("失败: {e}"));

    let hwnd = unsafe {
        CreateWindowExW(
            Default::default(),
            windows::core::w!("STATIC"),
            windows::core::w!("iuv-probe8"),
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

    let himc = unsafe { ImmGetContext(hwnd.0 as *mut c_void) };
    println!("HIMC = {himc:p}");

    unsafe {
        PostMessageW(Some(hwnd), WM_KEYDOWN, windows::Win32::Foundation::WPARAM(0x41), windows::Win32::Foundation::LPARAM(0x1E0001));
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
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

    println!("== GetModuleHandleW 检查 ==");
    for name in ["weasel.ime", "iuv.ime"] {
        let w: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let h = unsafe { GetModuleHandleW(windows::core::PCWSTR(w.as_ptr())) };
        println!("{name}: {h:?}");
    }

    if !himc.is_null() {
        unsafe {
            ImmReleaseContext(hwnd.0 as *mut c_void, himc);
        }
    }
    unsafe { DestroyWindow(hwnd) }.ok();
}
