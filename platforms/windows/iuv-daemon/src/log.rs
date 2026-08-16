//! 守护进程文件日志：`%TEMP%\input-iuv-daemon.log`（契约 30-conventions.md §3）。
//! 全错误路径必记；日志写失败静默忽略（日志不允许影响守护进程行为）。

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use windows::Win32::System::Threading::GetCurrentProcessId;

/// 追加一行日志（时间戳 + pid）。
pub fn log_line(msg: &str) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let line = format!("[{}.{:03}] pid={} {msg}\n", now.as_secs(), now.subsec_millis(), process_id());
    if let Some(path) = log_path() {
        let _ = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| f.write_all(line.as_bytes()));
    }
}

/// %TEMP%\input-iuv-daemon.log（TEMP 缺失时回退 TMP）。
fn log_path() -> Option<PathBuf> {
    std::env::var("TEMP")
        .or_else(|_| std::env::var("TMP"))
        .ok()
        .map(|dir| PathBuf::from(dir).join("input-iuv-daemon.log"))
}

fn process_id() -> u32 {
    // SAFETY: 纯查询系统 API，无指针参数，无副作用。
    unsafe { GetCurrentProcessId() }
}

/// 安装 panic 钩子：panic 信息落日志（守护进程"绝不 panic"纪律——即使发生也留痕）。
/// 另设默认钩子兜底（std 行为不变）。
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "未知 panic".into());
        let loc = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_else(|| "未知位置".into());
        log_line(&format!("[panic] {msg} @ {loc}"));
    }));
}