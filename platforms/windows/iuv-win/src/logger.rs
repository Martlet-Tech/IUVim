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
use windows::Win32::System::SystemInformation::{GetLocalTime, GetTickCount64};
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

// ============================================================
// 日志按「开机会话」分文件（2026-10-12，55 号体检 ④「日志轮转」）
// ------------------------------------------------------------
// 诉求：iuv-tsf.log 只增不减（实测 55MB）。方案 = **每次开机一个文件**，旧文件由
// server 后台按保留天数清理。设计要点：
// - **写入侧不改**：同一开机内所有 TSF 宿主进程仍写同一个共享文件（保留单文件
//   全局时间线 + 零滚动竞态）。
// - **只有 iuv-server 决定本次开机的日志名**：启动时算开机标识并发布指针文件，
//   TSF **直接信指针**取名（不做本地比对——`GetTickCount64` 与墙钟是两个时钟源、
//   有毫秒级抖动，各自截断到秒后可能差 1 秒，比对会误判回退）。
// - TSF 指针缺失（server 本开机尚未发布）→ 回退旧固定名 `iuv-tsf.log`。指针可能
//   来自上一开机（server 本次未运行）：此时 TSF 写入上一开机文件——在保留期内该
//   文件仍在，属可接受的降级（server 正常登录自启时不会发生）。
// - server 启动时顺带起一个后台线程，删「非本次开机」且「早于 keep_days 天」的日志。
// ============================================================

/// 本次开机标识 = 开机时刻的 Unix 秒。`now - GetTickCount64()` 恒等于开机瞬间，
/// 故同一开机内各进程、各时刻算出的值一致（秒级；无需日期/时区换算）。
pub fn compute_boot_id() -> String {
    let tick_ms = unsafe { GetTickCount64() };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    now.saturating_sub(tick_ms / 1000).to_string()
}

/// 按开机分名的日志文件名：`{stem}-{boot}.log`（如 `iuv-tsf-1760289000.log`）。
pub fn boot_scoped_name(stem: &str, boot: &str) -> String {
    format!("{stem}-{boot}.log")
}

/// server 发布指针文件路径：`%TEMP%\iuv-boot.id`（内容 = 本次开机标识）。
pub fn boot_id_path() -> Option<PathBuf> {
    temp_dir().map(|d| d.join("iuv-boot.id"))
}

/// 开机标识容差（秒）：同一次开机内 server 重启时 `now - GetTickCount64` 会有 ±1s
/// 抖动；与指针现值差在此容差内即视为同一次开机、**沿用旧值**（不换日志文件名），
/// 差得远（真重启）才发布新值。
const BOOT_ID_TOLERANCE_SECS: u64 = 120;

/// 选择生效的开机标识（纯函数，便于单测）：`existing` 与 `fresh` 差在容差内 → 沿用
/// `existing`；否则用 `fresh`（解析失败/缺失一律用 `fresh`）。
fn choose_boot_id(fresh: &str, existing: Option<&str>) -> String {
    if let (Ok(f), Some(e)) = (fresh.parse::<u64>(), existing) {
        if let Ok(x) = e.parse::<u64>() {
            if f.abs_diff(x) <= BOOT_ID_TOLERANCE_SECS {
                return e.to_owned();
            }
        }
    }
    fresh.to_owned()
}

/// server 发布本次开机标识并返回**生效值**（best-effort；失败静默——日志机制不得影响
/// 主流程）。同一次开机内重启沿用旧值，避免 ±1s 抖动把一次开机裂成多个日志文件；
/// 返回的生效值须由调用方用于自身日志名与清理基准，保证与指针一致。
pub fn publish_boot_id(fresh: &str) -> String {
    let effective = choose_boot_id(fresh, read_boot_id().as_deref());
    if let Some(p) = boot_id_path() {
        let _ = std::fs::write(p, &effective);
    }
    effective
}

/// 读取 server 发布的开机标识（裁剪空白；缺失/空 → None）。
pub fn read_boot_id() -> Option<String> {
    let s = std::fs::read_to_string(boot_id_path()?).ok()?;
    let s = s.trim().to_owned();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

/// 解析「按开机分名」文件名里的开机标识（`iuv-tsf-<digits>.log` /
/// `iuv-server-<digits>.log`）；非本机制命名（旧固定名、指针文件等）→ None。
pub fn parse_boot_from_name(name: &str) -> Option<&str> {
    let stem = name.strip_suffix(".log")?;
    let rest = stem
        .strip_prefix("iuv-tsf-")
        .or_else(|| stem.strip_prefix("iuv-server-"))?;
    if rest.is_empty() || !rest.bytes().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(rest)
}

/// 解析本次应写的文件名：**直接采用 server 指针**的开机标识（server 是唯一命名者）；
/// 指针缺失 → 回退 `fallback`（旧固定名）。TSF 侧取名用它。
///
/// 不做"与本地推算比对"：`compute_boot_id` 用 `GetTickCount64` 反推，而墙钟是另一
/// 时钟源，二者有毫秒级抖动，各进程在不同时刻截断到秒可能差 1 秒 → 比对会误判回退。
pub fn resolve_boot_scoped_name(stem: &str, fallback: &str) -> String {
    match read_boot_id() {
        Some(id) => boot_scoped_name(stem, &id),
        None => fallback.to_owned(),
    }
}

/// 本进程当前日志文件名（init 未装配 → None）。设置页「清日志」用。
pub fn current_log_name() -> Option<String> {
    FILE_NAME.get().cloned()
}

/// 起后台线程清理旧日志（**仅 server 调用**）：删 `%TEMP%` 下按开机分名、且
/// 「非本次开机」「开机时刻早于 keep_days 天」的日志文件。best-effort：占用/权限
/// 等失败一律静默；本线程不参与输入热路径。
pub fn spawn_log_cleanup(current_boot: &str, keep_days: u64) {
    let cur = current_boot.to_owned();
    let _ = std::thread::Builder::new()
        .name("iuv-log-cleanup".into())
        .spawn(move || cleanup_logs(&cur, keep_days));
}

fn cleanup_logs(current_boot: &str, keep_days: u64) {
    let Some(dir) = temp_dir() else {
        return;
    };
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return;
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let keep_secs = keep_days.saturating_mul(86_400);
    for ent in rd.flatten() {
        let name = ent.file_name().to_string_lossy().into_owned();
        let Some(boot) = parse_boot_from_name(&name) else {
            continue;
        };
        if boot == current_boot {
            continue; // 本次开机正在写，绝不删
        }
        if let Ok(b) = boot.parse::<u64>() {
            if now.saturating_sub(b) > keep_secs {
                let _ = std::fs::remove_file(ent.path());
            }
        }
    }
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
    fn parse_boot_from_name_only_matches_boot_scoped() {
        assert_eq!(
            parse_boot_from_name("iuv-tsf-1760289000.log"),
            Some("1760289000")
        );
        assert_eq!(
            parse_boot_from_name("iuv-server-1760289000.log"),
            Some("1760289000")
        );
        // 旧固定名 / 其它文件不入本机制
        assert_eq!(parse_boot_from_name("iuv-tsf.log"), None);
        assert_eq!(parse_boot_from_name("iuv-server.log"), None);
        assert_eq!(parse_boot_from_name("iuv-boot.id"), None);
        assert_eq!(parse_boot_from_name("iuv-tsf-.log"), None);
        assert_eq!(parse_boot_from_name("iuv-tsf-abc.log"), None);
        assert_eq!(parse_boot_from_name("iuv-script.log"), None);
    }

    #[test]
    fn choose_boot_id_reuses_within_tolerance() {
        // 同一次开机内重启：±1s 抖动 → 沿用旧值
        assert_eq!(choose_boot_id("1000", Some("999")), "999");
        assert_eq!(choose_boot_id("999", Some("1000")), "1000");
        assert_eq!(choose_boot_id("1000", Some("1000")), "1000");
        // 超过容差（真重启）→ 用新值
        assert_eq!(choose_boot_id("100000", Some("1000")), "100000");
        // 无旧值 / 旧值非法 → 用新值
        assert_eq!(choose_boot_id("1000", None), "1000");
        assert_eq!(choose_boot_id("1000", Some("abc")), "1000");
    }

    #[test]
    fn boot_scoped_name_shape() {
        assert_eq!(
            boot_scoped_name("iuv-tsf", "1760289000"),
            "iuv-tsf-1760289000.log"
        );
        assert_eq!(boot_scoped_name("iuv-server", "42"), "iuv-server-42.log");
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
