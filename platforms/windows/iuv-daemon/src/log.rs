//! 守护进程文件日志：`%TEMP%\input-iuv-daemon.log`（契约 30-conventions.md §3）。
//! 全错误路径必记；日志写失败静默忽略（日志不允许影响守护进程行为）。

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use windows::Win32::System::Threading::GetCurrentProcessId;

/// 禁用日志模块集（denylist，见 26-log-modules.md）。空 = 全记录（默认）。
/// 由启动配置加载/设置页 apply 调 `set_log_modules_disabled` 替换。
static DISABLED: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();

fn disabled() -> &'static Mutex<std::collections::HashSet<String>> {
    DISABLED.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// 替换禁用日志模块集（空 = 全记录）。
pub fn set_log_modules_disabled(modules: &[String]) {
    let mut set = disabled().lock().unwrap_or_else(|p| p.into_inner());
    set.clear();
    set.extend(modules.iter().cloned());
}

/// 按消息前缀 `[tag]` 判断是否被禁用；无 tag 恒放行。禁用集为空走快路径。
fn module_disabled(msg: &str) -> bool {
    let set = disabled().lock().unwrap_or_else(|p| p.into_inner());
    if set.is_empty() {
        return false;
    }
    if let Some(rest) = msg.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            return set.contains(&rest[..end]);
        }
    }
    false
}

/// 追加一行日志（时间戳 + pid）。模块被禁用时整行丢弃。
pub fn log_line(msg: &str) {
    if module_disabled(msg) {
        return;
    }
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

/// %TEMP%\input-iuv-daemon.log（TEMP 缺失时回退 TMP）。
fn log_path() -> Option<PathBuf> {
    temp_dir().map(|dir| dir.join("input-iuv-daemon.log"))
}

fn temp_dir() -> Option<PathBuf> {
    std::env::var("TEMP")
        .or_else(|_| std::env::var("TMP"))
        .ok()
        .map(PathBuf::from)
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