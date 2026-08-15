//! weasel.ime 代理探针：导出同款 15 个 IMM 函数，每个打日志（含 PID）后转发给真实实现
//! （部署为 SysWOW64\weasel.ime，真实实现备份为 weasel_real.ime 同目录）。
//! 用途：观测老 Rime 在 WoW 里被调用的完整函数序列，对比我们 iuv-ime 的调用差异。

use std::ffi::c_void;
use std::io::Write;
use std::sync::OnceLock;

use windows::Win32::Foundation::HMODULE;
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Threading::GetCurrentProcessId;

fn log_line(msg: &str) {
    let path = std::env::var("TEMP")
        .or_else(|_| std::env::var("TMP"))
        .ok()
        .map(|d| std::path::PathBuf::from(d).join("iuv-ime-probe.log"));
    if let Some(p) = path {
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
            let _ = writeln!(f, "{msg}");
        }
    }
}

fn pid() -> u32 {
    unsafe { GetCurrentProcessId() }
}

fn real_module() -> HMODULE {
    static REAL: OnceLock<usize> = OnceLock::new();
    let raw = *REAL.get_or_init(|| unsafe {
        let h = LoadLibraryW(windows::core::w!("C:\\Windows\\SysWOW64\\weasel_real.ime"));
        match h {
            Ok(m) => {
                log_line(&format!("[pid={}] 加载 weasel_real.ime OK", pid()));
                m.0 as usize
            }
            Err(e) => {
                log_line(&format!("[pid={}] 加载 weasel_real.ime 失败: {e}", pid()));
                0
            }
        }
    });
    HMODULE(raw as *mut c_void)
}

fn real_fn(name: &str) -> *mut c_void {
    unsafe {
        let c = std::ffi::CString::new(name).unwrap();
        GetProcAddress(real_module(), windows::core::PCSTR(c.as_ptr() as *const u8))
            .map(|p| p as *mut c_void)
            .unwrap_or(std::ptr::null_mut())
    }
}

macro_rules! forward {
    ($name:ident, $($arg:ident : $ty:ty),*) => {
        #[no_mangle]
        pub unsafe extern "system" fn $name($($arg: $ty),*) -> usize {
            log_line(&format!("[pid={}] CALL {}", pid(), stringify!($name)));
            let f: unsafe extern "system" fn($($ty),*) -> usize =
                std::mem::transmute(real_fn(stringify!($name)));
            f($($arg),*)
        }
    };
}

// 15 个 IMM 导出（stdcall 转发，参数按 usize 通配——32 位下指针/整数同为 4 字节）
forward!(ImeInquire, a: usize, b: usize, c: usize);
forward!(ImeConversionList, a: usize, b: usize, c: usize, d: usize, e: usize);
forward!(ImeConfigure, a: usize, b: usize, c: usize, d: usize);
forward!(ImeDestroy, a: usize);
forward!(ImeEscape, a: usize, b: usize, c: usize);
forward!(ImeProcessKey, a: usize, b: usize, c: usize, d: usize);
forward!(ImeSelect, a: usize, b: usize);
forward!(ImeSetActiveContext, a: usize, b: usize);
forward!(ImeToAsciiEx, a: usize, b: usize, c: usize, d: usize, e: usize, f: usize);
forward!(NotifyIME, a: usize, b: usize, c: usize, d: usize);
forward!(ImeRegisterWord, a: usize, b: usize, c: usize);
forward!(ImeUnregisterWord, a: usize, b: usize, c: usize);
forward!(ImeGetRegisterWordStyle, a: usize, b: usize);
forward!(ImeEnumRegisterWord, a: usize, b: usize, c: usize, d: usize, e: usize);
forward!(ImeSetCompositionString, a: usize, b: usize, c: usize, d: usize, e: usize, f: usize);
