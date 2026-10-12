//! 服务进程日志门面：实际日志文件 = `%TEMP%\iuv-server-<开机>.log`（main.rs 装配；
//! 按开机会话分名，55 号 ④）。
//! 2026-10-02 清理：删除 daemon 时代遗留的惰性 `init()`（旧 `input-iuv-daemon.log`
//! 文件名）——main.rs 第一行即经 `iuv_win::logger::init_logger` 装配真相源，
//! 本模块的日志转发直通共享实现。只保留服务端特有的清日志与 panic 钩子。

use std::fs::OpenOptions;

pub use iuv_win::logger::{set_log_modules_disabled, temp_dir};

/// 共享日志转发（直通 iuv-win 共享实现）。
pub fn log_line(msg: &str) {
    iuv_win::logger::log_line(msg);
}

/// 清空 `%TEMP%` 下 iuv 相关日志文件（truncate 而非删除：文件保留，持有方继续追加）。
/// 返回 (成功数, 失败数)。含**按开机分名的当前两端日志**（server 本进程 + TSF 当前）
/// 与旧固定名（历史遗留一并清）。失败多为日志文件此刻被活跃进程占用（TSF/脚本瞬时
/// 持有），只计数不报错——设置页高级标签据此显示"被占用"反馈。
pub fn clear_logs() -> (usize, usize) {
    let mut names: Vec<String> = Vec::new();
    // 本服务进程当前日志（按开机分名）
    if let Some(n) = iuv_win::logger::current_log_name() {
        names.push(n);
    }
    // TSF 当前日志（读 server 发布的开机指针取名；缺失则回退旧名）
    names.push(iuv_win::logger::resolve_boot_scoped_name(
        "iuv-tsf",
        "iuv-tsf.log",
    ));
    // 旧固定名（历史遗留）与脚本/清理日志
    for n in [
        "iuv-tsf.log",
        "iuv-server.log",
        "iuv-script.log",
        "iuv-cleanup.log",
    ] {
        names.push(n.to_owned());
    }
    names.sort();
    names.dedup();

    let Some(dir) = temp_dir() else {
        return (0, names.len());
    };
    let mut ok = 0usize;
    let mut fail = 0usize;
    for name in &names {
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

/// 安装 panic 钩子：panic 信息落日志（服务进程"绝不 panic"纪律——即使发生也留痕）。
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
