//! 全屏探测（工具栏显隐的第三维度：**全屏抑制**）。
//!
//! 判定语义（对齐 QQ 输入法）：前台窗口矩形覆盖其所在显示器整屏 = 全屏 → 抑制工具栏显示。
//! 覆盖视频播放器全屏、浏览器 F11 全屏、无边框游戏全屏、D3D 独占全屏（后者前台窗口同样是
//! 覆盖整屏的顶层窗口）。
//!
//! ## 为什么是轮询（而非事件驱动）
//! 浏览器 F11 全屏、最大化切全屏时，**前台窗口句柄本身没有变化**，只有窗口矩形变了 ——
//! `EVENT_SYSTEM_FOREGROUND` 不会触发；`EVENT_OBJECT_LOCATIONCHANGE` 则在每次拖动窗口时
//! 高频触发。故由调用方（工具条线程）以 1 秒低频轮询驱动，每次仅 3 次系统调用，无分配。
//!
//! ## 与 40 号裁决的边界
//! `docs/closed/40-toolbar-show-hide-governance.md` 明确禁止前台查询参与**焦点显隐判定**
//! （失败记录 #1：TSF 焦点通知常跑在系统更新前台窗口之前，一次性查询会导致显示被吞）。
//! 本模块的结果**只驱动全屏抑制这一个慢速开关**，绝不参与焦点归属判定——两者正交，
//! 全屏抑制容忍 1 秒延迟，不与 TSF 焦点信号竞争真相源。

use std::mem::size_of;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetShellWindow, GetWindowRect,
};

/// 覆盖判定容差（px）：DWM 阴影、部分播放器窗口矩形与显示器矩形有数 px 出入，
/// 严格相等会漏判。取 8 已能覆盖常见偏差，又不至于把最大化窗口误判为全屏
/// （最大化窗口高度会因任务栏少 40~48px，远超容差）。
const TOLERANCE: i32 = 8;

/// 轮询间隔（ms）。调用方据此 `SetTimer`。
pub(super) const PROBE_INTERVAL_MS: u32 = 1000;

/// 矩形是否覆盖显示器（**纯函数**，可单测）。
///
/// `tol` = 容差（px）：窗口各边允许内缩/外扩的量。显示器矩形无效（退化）时恒 false。
fn covers_monitor(win: &RECT, mon: &RECT, tol: i32) -> bool {
    if mon.right <= mon.left || mon.bottom <= mon.top {
        return false; // 无效显示器矩形：不猜
    }
    win.left <= mon.left + tol
        && win.top <= mon.top + tol
        && win.right >= mon.right - tol
        && win.bottom >= mon.bottom - tol
}

/// 窗口类名（失败返回空串；失败不做猜测，交由调用方按非桌面处理）。
fn class_name(hwnd: HWND) -> String {
    let mut buf = [0u16; 256];
    // SAFETY: buf 为栈上可写缓冲，长度由数组保证。
    let n = unsafe { GetClassNameW(hwnd, &mut buf) } as usize;
    if n == 0 || n > buf.len() {
        return String::new();
    }
    String::from_utf16_lossy(&buf[..n])
}

/// 是否桌面窗口。**必须排除**：桌面自身矩形恒等于整屏，不排除会导致「回到桌面就永远隐藏」。
/// `GetShellWindow()` 覆盖常规情形；`Progman`/`WorkerW` 覆盖壁纸层抢到前台的情形。
fn is_desktop(hwnd: HWND) -> bool {
    // SAFETY: GetShellWindow 纯查询。
    let shell = unsafe { GetShellWindow() };
    if !shell.is_invalid() && shell == hwnd {
        return true;
    }
    matches!(class_name(hwnd).as_str(), "Progman" | "WorkerW")
}

/// 探测当前是否处于全屏态。
///
/// 返回 `None` = 查询失败或无前台窗口（锁屏界面等）→ 调用方应**保持上一次状态不变**，
/// 这是最安全的中立行为（不做猜测、不 panic）。
pub(super) fn probe() -> Option<bool> {
    // SAFETY: GetForegroundWindow 纯查询。
    let fg = unsafe { GetForegroundWindow() };
    if fg.is_invalid() {
        return None;
    }
    if is_desktop(fg) {
        return Some(false);
    }
    let mut wr = RECT::default();
    // SAFETY: wr 为栈上可写 RECT。
    if unsafe { GetWindowRect(fg, &mut wr) }.is_err() {
        return None;
    }
    // 退化矩形（最小化窗口等）→ 明确非全屏，而非查询失败
    if wr.right <= wr.left || wr.bottom <= wr.top {
        return Some(false);
    }
    // SAFETY: MonitorFromWindow 纯查询；MONITOR_DEFAULTTONEAREST 保证返回有效句柄。
    let monitor = unsafe { MonitorFromWindow(fg, MONITOR_DEFAULTTONEAREST) };
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: info 已初始化且存活；比 rcMonitor（全屏窗口会盖住任务栏）。
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return None;
    }
    Some(covers_monitor(&wr, &info.rcMonitor, TOLERANCE))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(l: i32, t: i32, rt: i32, b: i32) -> RECT {
        RECT {
            left: l,
            top: t,
            right: rt,
            bottom: b,
        }
    }

    /// 1920x1080 显示器
    fn mon() -> RECT {
        r(0, 0, 1920, 1080)
    }

    #[test]
    fn exact_cover_is_fullscreen() {
        assert!(covers_monitor(&r(0, 0, 1920, 1080), &mon(), TOLERANCE));
    }

    #[test]
    fn within_tolerance_is_fullscreen() {
        // DWM 阴影导致窗口矩形比屏幕小几 px（内缩 4）→ 仍判全屏
        assert!(covers_monitor(&r(4, 4, 1916, 1076), &mon(), TOLERANCE));
        // 外扩（部分游戏矩形略大于屏幕）→ 仍判全屏
        assert!(covers_monitor(&r(-4, -4, 1924, 1084), &mon(), TOLERANCE));
    }

    #[test]
    fn beyond_tolerance_not_fullscreen() {
        // 矩形小了 16px（超出容差 8）
        assert!(!covers_monitor(&r(16, 16, 1904, 1064), &mon(), TOLERANCE));
    }

    #[test]
    fn maximized_window_not_fullscreen() {
        // 最大化窗口：高度因任务栏少 40px → 不判全屏（这是最大化不被误伤的关键）
        assert!(!covers_monitor(&r(0, 0, 1920, 1040), &mon(), TOLERANCE));
    }

    #[test]
    fn normal_window_not_fullscreen() {
        assert!(!covers_monitor(&r(100, 100, 1200, 800), &mon(), TOLERANCE));
    }

    #[test]
    fn secondary_monitor_origin_respected() {
        // 副屏在右（起点 1920,0）：窗口覆盖副屏 → 全屏
        let m = r(1920, 0, 3840, 1080);
        assert!(covers_monitor(&r(1920, 0, 3840, 1080), &m, TOLERANCE));
        // 主屏大小的窗口落在副屏坐标系 → 不该误判（left 1920+0 vs mon.left+8）
        assert!(!covers_monitor(&r(0, 0, 1920, 1080), &m, TOLERANCE));
    }

    #[test]
    fn degenerate_rects_rejected() {
        // 显示器矩形退化 → 不猜
        assert!(!covers_monitor(&r(0, 0, 1920, 1080), &r(100, 100, 100, 100), TOLERANCE));
        // 窗口矩形退化（最小化）→ 不覆盖
        assert!(!covers_monitor(&r(0, 0, 0, 0), &mon(), TOLERANCE));
    }
}
