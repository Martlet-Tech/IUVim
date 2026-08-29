//! 文件日志：%TEMP%\iuv-tsf.log。契约 30-conventions.md §3。
//! 【Agent D】W1 实现。

use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use windows::Win32::System::LibraryLoader::GetModuleFileNameW;
use windows::Win32::System::Threading::{GetCurrentProcessId, GetCurrentThreadId};

/// 禁用日志模块集（denylist，见 26-log-modules.md）。空 = 全记录（默认）。
/// 由配置加载/热载调 `set_log_modules_disabled` 替换。
static DISABLED: OnceLock<Mutex<std::collections::HashSet<String>>> = OnceLock::new();

/// 性能埋点开关的缓存（true = 关闭）：埋点位于按键热路径（每键 5 次），
/// 不能每次都进 `disabled()` 的 Mutex 查集合，故缓存为原子量。
/// 由 [`set_perf_probe`] 刷新（配置 `perf_probe`），**不**随 denylist 变化。
/// 初值取 true（关闭）：配置装配完成前绝不写埋点。
static PERF_DISABLED: AtomicBool = AtomicBool::new(true);

fn disabled() -> &'static Mutex<std::collections::HashSet<String>> {
    DISABLED.get_or_init(|| Mutex::new(std::collections::HashSet::new()))
}

/// 替换禁用日志模块集（配置加载/热载调用；空 = 全记录）。
pub fn set_log_modules_disabled(modules: &[String]) {
    let mut set = disabled().lock().unwrap_or_else(|p| p.into_inner());
    set.clear();
    set.extend(modules.iter().cloned());
}

/// iuv-core 引擎内部埋点的转发目标：引擎层是跨平台纯 Rust、不碰 IO，
/// 计时结果经此落到 `[perf]` 日志，与平台层埋点同一开关、同一格式。
pub(crate) fn perf_probe_emit(phase: &'static str, micros: u64) {
    log_line(&format!("[{PERF_TAG}] {phase} {micros}us"));
}

/// 一次性装配性能埋点（平台层开关 + 引擎层转发目标），供配置加载/热载两处调用。
pub fn configure_perf_probe(enabled: bool) {
    set_perf_probe(enabled);
    iuv_core::perf::set_enabled(enabled);
    iuv_core::perf::set_sink(perf_probe_emit);
}

/// 性能埋点开关（配置 `perf_probe`）：**显式开启才记录**，默认关闭。
///
/// 刻意不并入 `disabled_log_modules`——后者是 denylist（未列出即记录），会让埋点
/// 在新配置/未列出 perf 时默认打开，等于每键多 5 次文件写入，恰好抵消关闭日志
/// 换来的手感（2026-08-29 实测：浏览器打字明显变卡）。排查时置 true 即可，
/// 热载路径与 denylist 同一处装配。
pub fn set_perf_probe(enabled: bool) {
    PERF_DISABLED.store(!enabled, Ordering::Relaxed);
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

/// 性能埋点模块名（日志形如 `[perf] render 1748us update`）。
/// 配置 `disabled_log_modules` 含此项即静音；新配置的默认值已含此项。
pub(crate) use iuv_core::config::PERF_LOG_TAG as PERF_TAG;

/// 开始计时。`perf` 被禁用时返回 `None`，配套的 [`perf_record`] 随即成为空操作
/// ——热路径每键 5 处埋点，关闭时只多一次原子读，不做格式化、不算耗时。
///
/// 用法：
/// ```ignore
/// let t = perf_tick();
/// // ... 被测代码 ...
/// perf_record("render", t, "update");
/// ```
pub(crate) fn perf_tick() -> Option<Instant> {
    if PERF_DISABLED.load(Ordering::Relaxed) {
        None
    } else {
        Some(Instant::now())
    }
}

/// 记录一段耗时（微秒），附加信息由 `detail` 惰性提供——perf 关闭时闭包根本不执行，
/// 热路径不会为一段看不到的日志去做字符串格式化与堆分配。
pub(crate) fn perf_record_with(
    phase: &str,
    start: Option<Instant>,
    detail: impl FnOnce() -> String,
) {
    let Some(t) = start else { return };
    let us = t.elapsed().as_micros();
    let d = detail();
    if d.is_empty() {
        log_line(&format!("[{PERF_TAG}] {phase} {us}us"));
    } else {
        log_line(&format!("[{PERF_TAG}] {phase} {us}us {d}"));
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
