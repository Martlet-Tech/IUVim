//! 守护进程文件日志门面：`%TEMP%\input-iuv-daemon.log`（契约 02-conventions.md §3）。
//! 实现 = [`iuv_win::logger`] 共享文件日志（2026-08-29 与 iuv-tsf 的复制实现收敛）；
//! 本文件只保留 daemon 特有的清日志与 panic 钩子。

use std::fs::OpenOptions;
use std::sync::OnceLock;

/// 共享日志装配（file name 一次定死，无宿主模块名前缀；`log_line` 内惰性调用）。
pub fn init() {
    iuv_win::logger::init_logger("input-iuv-daemon.log", false);
}

pub use iuv_win::logger::{set_log_modules_disabled, temp_dir};

/// 共享日志转发（首次调用惰性装配）。
pub fn log_line(msg: &str) {
    static INIT: OnceLock<()> = OnceLock::new();
    INIT.get_or_init(init);
    iuv_win::logger::log_line(msg);
}

/// 清空 `%TEMP%` 下 4 个 iuv 相关日志文件（truncate 而非删除：文件保留，持有方继续追加）。
/// 返回 (成功数, 失败数)。失败多为日志文件此刻被活跃进程占用（TSF/脚本瞬时持有），
/// 只计数不报错——设置页开发者标签据此显示"被占用"反馈。
#[cfg(any(debug_assertions, feature = "dev"))]
pub fn clear_logs() -> (usize, usize) {
    const FILES: &[&str] = &[
        "input-iuv-daemon.log", // 本守护进程
        "iuv-tsf.log",          // TSF 会话进程
        "iuv-script.log",       // install/dev-deploy 脚本
        "iuv-cleanup.log",      // 延迟清理计划任务
    ];
    let Some(dir) = temp_dir() else {
        return (0, FILES.len());
    };
    let mut ok = 0usize;
    let mut fail = 0usize;
    for name in FILES {
        let path = dir.join(name);
        match OpenOptions::new().write(true).truncate(true).open(&path) {
            Ok(_) => ok += 1,
            Err(e) => {
                fail += 1;
                log_line(&format!("[log] 清除 {name} 失败（占用？）: {e}"));
            }
        }
    }
    log_line(&format!("[log] 清除日志完成：成功 {ok}、失败 {fail}"));
    (ok, fail)
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
