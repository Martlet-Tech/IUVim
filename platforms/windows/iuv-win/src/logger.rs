//! 共享文件日志（iuv-tsf / iuv-server 两进程共用的唯一实现，2026-08-29 自两份
//! 近乎复制的 `log.rs` 收敛；2026-10-02 人读时间戳 + 持久句柄）。denylist 的
//! `[tag]` 解析只有这一份——设置页「日志模块」开关在两个进程的行为由实现
//! 保证一致，不再靠人肉同步。
//!
//! 进程启动时 `init(file_name, with_module_name)` 装配一次（TSF 带宿主 exe 名前缀，
//! server 不带）；未装配 → 全部丢弃（与旧"钩子未注入即丢弃"语义一致）。
//! 日志写失败静默忽略（日志不允许影响输入法行为——硬性约定）。
//!
//! 写入实现：**持久句柄**（进程内一次 open，Mutex 串行写）替代旧"每行 open"。
//! std 打开默认共享读/写/删除，设置页 truncate 清日志照常可用；append 语义
//! 保证写入总落文件尾（外部 truncate 后写入回到 0，不产生空洞）；句柄失效
//! （文件被删等）即丢弃，下次调用惰性重开。
//!
//! 时间戳：`GetLocalTime` 人读格式 `YYYY-MM-DD HH:MM:SS.mmm`（日期时间部分按秒
//! 缓存，同一秒内不重算；毫秒每行现取）。

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::SYSTEMTIME;
use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};

/// 禁用日志模块集（denylist，见 26-log-modules.md）。空 = 全记录（默认）。
static DISABLED: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();
static FILE_NAME: OnceLock<String> = OnceLock::new();
static WITH_MODULE: AtomicBool = AtomicBool::new(false);

/// 持久日志句柄（None = 未开/失效，写时惰性重开）。
static FILE_HANDLE: Mutex<Option<File>> = Mutex::new(None);

/// 按秒缓存的时间文本：key = SYSTEMTIME 秒级字段，value = "YYYY-MM-DD HH:MM:SS"。
static TIME_CACHE: Mutex<([u16; 7], String)> = Mutex::new(([0; 7], String::new()));

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

/// 追加一行日志（人读时间戳 + pid [+ 宿主模块名]）。模块被禁用时整行丢弃（不构建、不写文件）。
pub fn log_line(msg: &str) {
    if module_disabled(msg) {
        return;
    }
    let st = unsafe { GetLocalTime() };
    let secs_text = cached_secs_text(&st);
    let module = if WITH_MODULE.load(Ordering::Relaxed) {
        format!("{} ", module_name())
    } else {
        String::new()
    };
    let line = format!(
        "[{}.{:03}] pid={} {module}{msg}\n",
        secs_text,
        st.wMilliseconds,
        process_id()
    );
    if let Some(path) = log_path() {
        write_line(&line, &path);
    }
}

/// "YYYY-MM-DD HH:MM:SS"（按秒缓存：key 未变直接复用，避免每行重复格式化）。
fn cached_secs_text(st: &SYSTEMTIME) -> String {
    let key = [
        st.wYear,
        st.wMonth,
        st.wDayOfWeek,
        st.wDay,
        st.wHour,
        st.wMinute,
        st.wSecond,
    ];
    let mut cache = TIME_CACHE.lock().unwrap_or_else(|p| p.into_inner());
    if cache.0 != key {
        cache.0 = key;
        cache.1 = format!(
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
        );
    }
    cache.1.clone()
}

/// 经持久句柄写入（进程内 Mutex 串行）。open/写失败静默忽略——日志不允许影响输入法行为。
fn write_line(line: &str, path: &Path) {
    let mut guard = FILE_HANDLE.lock().unwrap_or_else(|p| p.into_inner());
    if guard.is_none() {
        match OpenOptions::new().create(true).append(true).open(path) {
            Ok(f) => *guard = Some(f),
            Err(_) => return,
        }
    }
    let file = guard.as_mut().unwrap();
    if file.write_all(line.as_bytes()).is_err() {
        // 句柄失效（文件被删/盘满等）：丢弃，下次调用惰性重开。
        *guard = None;
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
    fn secs_text_format() {
        let st = SYSTEMTIME {
            wYear: 2026,
            wMonth: 10,
            wDayOfWeek: 5,
            wDay: 2,
            wHour: 14,
            wMinute: 33,
            wSecond: 5,
            wMilliseconds: 456,
        };
        assert_eq!(cached_secs_text(&st), "2026-10-02 14:33:05");
        let st2 = SYSTEMTIME {
            wYear: 1999,
            wMonth: 1,
            wDayOfWeek: 0,
            wDay: 9,
            wHour: 7,
            wMinute: 8,
            wSecond: 9,
            wMilliseconds: 1,
        };
        assert_eq!(cached_secs_text(&st2), "1999-01-09 07:08:09");
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
