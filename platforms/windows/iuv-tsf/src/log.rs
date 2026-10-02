//! TSF 文件日志门面：%TEMP%\iuv-tsf.log。契约 02-conventions.md §3。
//! 实现 = [`iuv_win::logger`] 共享文件日志（2026-08-29 与 iuv-daemon 的复制实现收敛；
//! 2026-10-02 perf 埋点机制整体退役，本文件只保留 TSF 特有的装配）。

use std::sync::OnceLock;

/// 共享日志装配（file name 与模块名前缀一次定死；`log_line` 内惰性调用，无需显式初始化）。
pub fn init() {
    iuv_win::logger::init_logger("iuv-tsf.log", true);
}

pub use iuv_win::logger::{
    log_path, module_name, process_id, set_log_modules_disabled, temp_dir, thread_id,
};

/// 共享日志转发（首次调用惰性装配——regsvr32/DllMain 等早于 TextService 的路径同样有日志）。
pub fn log_line(msg: &str) {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(init);
    iuv_win::logger::log_line(msg);
}
