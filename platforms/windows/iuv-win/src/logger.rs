//! 共享文件日志（iuv-tsf / iuv-daemon 两进程共用的唯一实现，2026-08-29 自两份
//! 近乎复制的 `log.rs` 收敛）。denylist 的 `[tag]` 解析只有这一份——设置页
//! 「日志模块」开关在两个进程的行为由实现保证一致，不再靠人肉同步。
//!
//! 进程启动时 `init(file_name, with_module_name)` 装配一次（TSF 带宿主 exe 名前缀，
//! daemon 不带）；未装配 → 全部丢弃（与旧"钩子未注入即丢弃"语义一致）。
//! 日志写失败静默忽略（日志不允许影响输入法行为——硬性约定）。

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};

/// 禁用日志模块集（denylist，见 26-log-modules.md）。空 = 全记录（默认）。
static DISABLED: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
static FILE_NAME: OnceLock<String> = OnceLock::new();
static WITH_MODULE: AtomicBool = AtomicBool::new(false);

/// 进程启动装配：`%TEMP%` 下的日志文件名；`with_module_name` = 行前缀是否含宿主
/// 模块文件名（TSF 在宿主进程内，需要区分 notepad.exe / wow.exe …；daemon 不需要）。
pub fn init_logger(file_name: &str, with_module_name: bool) {
    let _ = FILE_NAME.set(file_name.to_owned());
    WITH_MODULE.store(with_module_name, Ordering::Relaxed);
}

fn disabled() -> &'static Mutex<std::collections::HashSet<String>> {
    DISABLED.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// 替换禁用日志模块集（配置加载/热载/设置页调用；空 = 全记录）。
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

/// 追加一行日志（时间戳 + pid [+ 宿主模块名]）。模块被禁用时整行丢弃（不构建、不写文件）。
pub fn log_line(msg: &str) {
    if module_disabled(msg) {
        return;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let module = if WITH_MODULE.load(Ordering::Relaxed) {
        format!("{} ", module_name())
    } else {
        String::new()
    };
    let line = format!(
        "[{}.{:03}] pid={} {module}{msg}\n",
        now.as_secs(),
        now.subsec_millis(),
        process_id()
    );
    if let Some(path) = log_path() {
        let _ = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| f.write_all(line.as_bytes()));
    }
}

/// %TEMP%（TEMP 缺失时回退 TMP）。
pub fn temp_dir() -> Option<PathBuf> {
    std::env::var("TEMP")
        .or_else(|_| std::env::var("TMP"))
        .ok()
        .map(PathBuf::from)
}

/// 当前进程日志文件路径（init 未装配 → None）。
pub fn log_path() -> Option<PathBuf> {
    let name = FILE_NAME.get()?;
    temp_dir().map(|dir| dir.join(name))
}

// SAFETY（下两函数）：纯查询系统 API，无指针参数，无副作用。
pub fn process_id() -> u32 {
    unsafe { GetCurrentProcessId() }
}

/// 当前线程 id（OS 线程 id——前台看板 `GetWindowThreadProcessId` 返回的就是它）。
pub fn thread_id() -> u32 {
    unsafe { GetCurrentThreadId() }
}

/// 当前模块文件名（如 "notepad.exe"），失败返回空串。白名单判定复用（进程 exe 名）。
pub fn module_name() -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 测试共用进程级禁用集，并行执行会互相清/写竞态（偶发失败）；加互斥串行化。
    static TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn clear() {
        disabled().lock().unwrap().clear();
    }

    #[test]
    fn empty_set_logs_everything() {
        let _g = TEST_LOCK.lock().unwrap();
        clear();
        assert!(!module_disabled("[uielem] GetString(0) 被调"));
        assert!(!module_disabled("无 tag 的消息"));
    }

    #[test]
    fn disabled_module_suppressed() {
        let _g = TEST_LOCK.lock().unwrap();
        clear();
        set_log_modules_disabled(&["uielem".to_owned()]);
        assert!(module_disabled("[uielem] GetString(0) 被调"));
        assert!(!module_disabled("[caret] GetTextExt"));
        assert!(!module_disabled("无 tag 的消息"));
    }

    #[test]
    fn tag_parse_edge_cases() {
        let _g = TEST_LOCK.lock().unwrap();
        clear();
        set_log_modules_disabled(&["key".to_owned()]);
        assert!(!module_disabled("["), "孤立左括号不是有效 tag");
        assert!(!module_disabled("[]"), "空 tag");
        assert!(!module_disabled("[] x"), "空 tag");
        assert!(module_disabled("[key] 按键：g（会话内）"));
    }
}
