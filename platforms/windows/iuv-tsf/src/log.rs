//! TSF 文件日志门面：%TEMP%\iuv-tsf.log。契约 02-conventions.md §3。
//! 实现 = [`iuv_win::logger`] 共享文件日志（2026-08-29 与 iuv-daemon 的复制实现收敛）；
//! 本文件只保留 TSF 特有的性能埋点装配。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

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

/// 性能埋点开关的缓存（true = 关闭）：埋点位于按键热路径（每键 5 次），
/// 不能每次都查 denylist 的 Mutex 集合，故缓存为原子量。
/// 由 [`set_perf_probe`] 刷新（配置 `perf_probe`），**不**随 denylist 变化。
/// 初值取 true（关闭）：配置装配完成前绝不写埋点。
static PERF_DISABLED: AtomicBool = AtomicBool::new(true);

/// 性能埋点模块名（日志形如 `[perf] render 1748us update`）。
/// 配置 `disabled_log_modules` 含此项即静音；新配置的默认值已含此项。
pub(crate) use iuv_core::config::PERF_LOG_TAG as PERF_TAG;

/// 开始计时。`perf` 被禁用时返回 `None`，配套的 [`perf_record_with`] 随即成为空操作
/// ——热路径每键 5 处埋点，关闭时只多一次原子读，不做格式化、不算耗时。
///
/// 用法：
/// ```ignore
/// let t = perf_tick();
/// // ... 被测代码 ...
/// perf_record_with("render", t, || "update".into());
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
