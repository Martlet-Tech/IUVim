//! ImmInstallIMEW 探针：让系统自己安装 iuv.ime 并分配 KLID（GPT 建议）。
//! 用法：cargo build --target i686-pc-windows-msvc -p iuv-ime --example probe3 --release

use std::ffi::c_void;

#[link(name = "imm32")]
extern "system" {
    fn ImmInstallIMEW(lpsz_ime_file: *const u16, lpsz_layout_text: *const u16) -> *mut c_void;
}

fn main() {
    let file = "C:\\Windows\\SysWOW64\\iuv.ime";
    let text = "iuv";
    let file_w: Vec<u16> = file.encode_utf16().chain(std::iter::once(0)).collect();
    let text_w: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    println!("== ImmInstallIMEW({file}, {text}) ==");
    let hkl = unsafe { ImmInstallIMEW(file_w.as_ptr(), text_w.as_ptr()) };
    println!("返回 HKL = {hkl:p} (null = 失败)");
    if hkl.is_null() {
        println!("GetLastError = {}", unsafe { windows::Win32::Foundation::GetLastError().0 });
    }
}
