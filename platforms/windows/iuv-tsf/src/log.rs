//! 文件日志：%TEMP%\iuv-tsf.log。契约 30-conventions.md §3。
//! 【Agent D】W1 实现。

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};

/// 禁用日志模块集（denylist，见 26-log-modules.md）。空 = 全记录（默认）。
/// 由配置加载/热载调 `set_log_modules_disabled` 替换。
static DISABLED: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();

fn disabled() -> &'static Mutex<std::collections::HashSet<String>> {
    DISABLED.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// 替换禁用日志模块集（配置加载/热载调用；空 = 全记录）。
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

/// 追加一行日志，带时间戳与进程名。全 crate 错误路径必记。
///
/// 日志写失败静默忽略（日志本身不允许影响输入法行为）。
/// 模块被禁用（config `disabled_log_modules`）时整行丢弃（不构建、不写文件）。
pub fn log_line(msg: &str) {
    if module_disabled(msg) {
        return;
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 三个测试共用进程级禁用集，并行执行会互相清/写竞态（偶发失败）；
    /// 加全局互斥串行化。
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

/// %TEMP%\iuv-tsf.log（TEMP 缺失时回退 TMP）。
fn log_path() -> Option<PathBuf> {
    std::env::var("TEMP")
        .or_else(|_| std::env::var("TMP"))
        .ok()
        .map(|dir| PathBuf::from(dir).join("iuv-tsf.log"))
}

pub(crate) fn process_id() -> u32 {
    // SAFETY: 纯查询系统 API，无指针参数，无副作用。
    unsafe { GetCurrentProcessId() }
}

/// 当前线程 id（32-toolbar 实例标识 pid:tid 的 tid = **OS 线程 id**，非 TSF client id——
/// 前台看板判定 `GetWindowThreadProcessId` 返回的就是 OS 线程 id，直接匹配）。
pub(crate) fn thread_id() -> u32 {
    // SAFETY: 纯查询系统 API，无指针参数，无副作用。
    unsafe { GetCurrentThreadId() }
}

/// 当前模块文件名（如 "notepad.exe"），失败返回空串。白名单判定复用（进程 exe 名）。
pub(crate) fn module_name() -> String {
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
