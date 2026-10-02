//! 服务进程日志门面：实际日志文件 = `%TEMP%\iuv-server.log`（main.rs 装配，
//! 2026-10-02 品质审查 S1 归一——旧 `input-iuv-daemon.log` 是 daemon 时代遗留，
//! daemon→server 迁移后 main 经 `iuv_win::logger::init_logger` 先行装配，本模块
//! 的惰性 init 因 OnceLock 先到先得而静默失效）。
//! 本文件只保留服务端特有的清日志与 panic 钩子。

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
        "iuv-server.log", // 本服务进程（main.rs 装配的真相源；旧
        // input-iuv-daemon.log 已无人写，daemon 时代遗留）
        "iuv-tsf.log",     // TSF 会话进程
        "iuv-script.log",  // install/dev-deploy 脚本
        "iuv-cleanup.log", // 延迟清理计划任务
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
