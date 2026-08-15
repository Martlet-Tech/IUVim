//! 裸文件名 LoadLibrary 测试（32 位进程搜索路径）。
//! 用法：cargo build --target i686-pc-windows-msvc -p iuv-ime --example probe4 --release

use windows::Win32::System::LibraryLoader::LoadLibraryW;

fn main() {
    for name in ["iuv.ime", "WEASEL.IME"] {
        let w: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
        let h = unsafe { LoadLibraryW(windows::core::PCWSTR(w.as_ptr())) };
        match h {
            Ok(m) => println!("{name}: 加载成功 {m:?}"),
            Err(e) => println!("{name}: 失败 {e}"),
        }
    }
}
