//! 引擎内部性能埋点（2026-08-29 打字延迟排查引入）。
//!
//! ## 为什么单独一层
//!
//! 按键延迟里最大的一块（候选生成 `onkey`）只表现为一个数字，无法判断落在
//! 切分 / 重排 / 候选生成哪一步。要细分就得在引擎内部计时——但 iuv-core 是
//! **跨平台纯 Rust**，不能碰任何平台 IO（不写文件、不打日志、不依赖 iuv-tsf）。
//!
//! 故此处只提供「计时 + 转发」：输出去向由平台层注入的 sink 决定
//! （TSF 侧转发到 `[perf]` 日志，REPL 可转发到 stdout，测试可转发到 Vec）。
//!
//! ## 开销约定
//!
//! 关闭时 `tick()` 只做一次原子读并返回 `None`，`record()` 立即返回——
//! 热路径**零格式化、零分配**。埋点常关，仅在排查时由配置 `perf_probe` 打开。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

/// 总开关（由平台层按配置 `perf_probe` 装配）。默认关闭。
static ENABLED: AtomicBool = AtomicBool::new(false);
/// 输出目标。重复注入以首次为准（进程内不变）。
static SINK: OnceLock<fn(&'static str, u64)> = OnceLock::new();

/// 开关：true = 记录。
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

/// 注入输出目标：`sink(阶段名, 耗时微秒)`。
pub fn set_sink(sink: fn(&'static str, u64)) {
    let _ = SINK.set(sink);
}

/// 开始计时。未开启时返回 `None`，配套的 [`record`] 随即成为空操作。
pub fn tick() -> Option<Instant> {
    if ENABLED.load(Ordering::Relaxed) {
        Some(Instant::now())
    } else {
        None
    }
}

/// 记录一段耗时。`start` 为 `None`（未开启）或 sink 未注入时直接返回。
pub fn record(phase: &'static str, start: Option<Instant>) {
    if let (Some(t), Some(sink)) = (start, SINK.get()) {
        sink(phase, t.elapsed().as_micros() as u64);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// 进程级静态状态（ENABLED/SINK），并行跑会互相干扰 → 串行化。
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn disabled_tick_is_none_and_record_is_noop() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        set_enabled(false);
        assert!(tick().is_none(), "关闭时不取时间戳");
        // sink 未注入 / 未开启都不应 panic
        record("unit.never", tick());
    }

    #[test]
    fn enabled_tick_records_elapsed() {
        let _g = TEST_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        // 捕获到一个静态槽里，断言确实被调用过且耗时合法（≥0）。
        static SEEN: Mutex<Option<(&'static str, u64)>> = Mutex::new(None);
        fn sink(phase: &'static str, micros: u64) {
            *SEEN.lock().unwrap_or_else(|p| p.into_inner()) = Some((phase, micros));
        }
        set_sink(sink);
        set_enabled(true);
        let t = tick();
        record("unit.ok", t);
        set_enabled(false);

        let (phase, _micros) = SEEN
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .expect("开启后 record 应回调 sink 一次");
        assert_eq!(phase, "unit.ok");
    }
}
