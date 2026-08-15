//! iuv.ime 加载性探针：LoadLibrary + GetProcAddress 验证 15 个 IMM 导出函数。
//! 用法：cargo build --target i686-pc-windows-msvc -p iuv-ime --example probe2 --release

use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

fn main() {
    let path = "C:\\Windows\\SysWOW64\\iuv.ime";
    println!("== LoadLibraryW({path}) ==");
    let hmod = unsafe { LoadLibraryW(windows::core::w!("C:\\Windows\\SysWOW64\\iuv.ime")) };
    match hmod {
        Ok(m) => println!("加载成功: {m:?}"),
        Err(e) => {
            println!("加载失败: {e}");
            return;
        }
    }
    let m = hmod.unwrap();
    let exports = [
        "ImeConversionList",
        "ImeConfigure",
        "ImeDestroy",
        "ImeEscape",
        "ImeInquire",
        "ImeProcessKey",
        "ImeSelect",
        "ImeSetActiveContext",
        "ImeToAsciiEx",
        "NotifyIME",
        "ImeRegisterWord",
        "ImeUnregisterWord",
        "ImeGetRegisterWordStyle",
        "ImeEnumRegisterWord",
        "ImeSetCompositionString",
    ];
    for name in exports {
        let c = std::ffi::CString::new(name).unwrap();
        let p = unsafe { GetProcAddress(m, windows::core::PCSTR(c.as_ptr() as *const u8)) };
        println!("  {name}: {p:?}");
    }
    // 直接调用 ImeInquire 验证 ABI 可用
    unsafe {
        let c = std::ffi::CString::new("ImeInquire").unwrap();
        let inquire: unsafe extern "system" fn(*mut u8, *mut u16, u32) -> i32 = std::mem::transmute(
            GetProcAddress(m, windows::core::PCSTR(c.as_ptr() as *const u8)).unwrap(),
        );
        let mut info = [0u8; 32];
        let mut cls = [0u16; 32];
        let ret = inquire(info.as_mut_ptr(), cls.as_mut_ptr(), 0);
        let cls_name = String::from_utf16_lossy(&cls);
        println!("ImeInquire() => {ret}, UI类名 = {cls_name}");
    }
}
