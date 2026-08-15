//! IMM 布局加载探针：LoadKeyboardLayoutW("E0220804", KLF_ACTIVATE) 立即验证布局可加载性。
//! 用法：cargo build --target i686-pc-windows-msvc -p iuv-ime --example probe --release
//! 产物：target/i686-pc-windows-msvc/release/examples/probe.exe（32 位）

use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyboardLayoutList, GetKeyboardLayoutNameW, LoadKeyboardLayoutW, KLF_ACTIVATE,
};

fn main() {
    println!("== LoadKeyboardLayoutW(E0220804, KLF_ACTIVATE) ==");
    let hkl = unsafe { LoadKeyboardLayoutW(windows::core::w!("E0220804"), KLF_ACTIVATE) };
    match hkl {
        Ok(h) => println!("HKL = {h:?} 加载成功"),
        Err(e) => {
            println!("加载失败: {e}");
            return;
        }
    }

    let mut name = [0u16; 9];
    let ok = unsafe { GetKeyboardLayoutNameW(&mut name) };
    println!("GetKeyboardLayoutNameW ok={ok:?} name={}", String::from_utf16_lossy(&name));

    let count = unsafe { GetKeyboardLayoutList(None) };
    let mut list = vec![Default::default(); count as usize];
    let n = unsafe { GetKeyboardLayoutList(Some(&mut list)) };
    println!("GetKeyboardLayoutList count={n}");
    for h in &list {
        println!("  0x{:08X}", h.0 as u32);
    }
}
