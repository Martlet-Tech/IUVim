//! 文件日志：%TEMP%\input-ime-tsf.log。契约 30-conventions.md §3。
//! 【Agent D】W1 实现。

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::Threading::GetCurrentProcessId;

/// 追加一行日志，带时间戳与进程名。全 crate 错误路径必记。
///
/// 日志写失败静默忽略（日志本身不允许影响输入法行为）。
pub fn log_line(msg: &str) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let millis = now.subsec_millis();
    let line = format!("[{secs}.{millis:03}] pid={} {} {msg}\n", process_id(), module_name());
    if let Some(path) = log_path() {
        let _ = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| f.write_all(line.as_bytes()));
    }
}

/// %TEMP%\input-ime-tsf.log（TEMP 缺失时回退 TMP）。
fn log_path() -> Option<PathBuf> {
    std::env::var("TEMP")
        .or_else(|_| std::env::var("TMP"))
        .ok()
        .map(|dir| PathBuf::from(dir).join("input-ime-tsf.log"))
}

fn process_id() -> u32 {
    // SAFETY: 纯查询系统 API，无指针参数，无副作用。
    unsafe { GetCurrentProcessId() }
}

/// 当前模块文件名（如 "notepad.exe"），失败返回空串。
fn module_name() -> String {
    let mut buf = [0u16; 512];
    // SAFETY: GetModuleFileNameW 写入我们提供的 512 宽的缓冲，返回实际写入长度。
    let len = unsafe { GetModuleFileNameW(None, &mut buf) };
    if len == 0 {
        return String::new();
    }
    let name = String::from_utf16_lossy(&buf[..len as usize]);
    match name.rsplit('\\').next() {
        Some(base) => base.to_owned(),
        None => name,
    }
}
