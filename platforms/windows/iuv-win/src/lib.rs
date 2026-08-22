//! iuv-win：Windows 共享 UI 呈现层（32-status-toolbar.md §6.4 定稿 = iuv-ui + ULW 自绘）。
//!
//! daemon（工具栏）与 TSF（候选窗/菜单）复用同一份 `UpdateLayeredWindow` 呈现代码
//! （原 iuv-tsf `ui/ulw.rs` 抽取）。日志经**外部注入**的 `fn(&str)` 钩子转发到各进程
//! 自己的 `log.rs`（本 crate 不自建日志设施）。
//!
//! P2.5 追加：`popup.rs`（LayeredWindow：类注册/创建/DPI/GWLP_USERDATA/wndproc 默认臂/Drop，
//! candwin/menu_window/工具栏三处窗口样板收敛）。
//!
//! P3.2 追加：`ipc/`（M6 用户库管道 + 反向控制通道，自 iuv-data 移入）+ `shm.rs`（共享内存段，
//! 自 iuv-data 移入）——iuv-data 恢复跨平台，纯 Windows 代码集中在本 crate。
//!
//! 全部公开函数不 panic：呈现失败记日志并静默降级（iuv 各进程硬性约定）。

use std::sync::atomic::{AtomicUsize, Ordering};

pub mod ipc;
pub mod popup;
pub mod shm;
pub mod ulw;

pub use ipc::{
    ctl_pipe_name, CtlClient, CtlCmd, CtlResult, CtlServer, PipeClient, PipeServer, Request,
    Response, SignalClient, SignalServer, ToolbarSignal,
};
pub use popup::LayeredWindow;
pub use shm::{ShmReader, ShmWriter};
pub use ulw::UlwSurface;

/// 日志钩子槽位：`set_logger` 注入（进程启动时一次）；未注入 → 丢弃。
static LOGGER: AtomicUsize = AtomicUsize::new(0);

/// 注入日志函数（iuv-tsf/daemon 启动时调用一次；`None` = 恢复丢弃）。
pub fn set_logger(f: Option<fn(&str)>) {
    LOGGER.store(f.map(|f| f as usize).unwrap_or(0), Ordering::SeqCst);
}

/// 转发日志（内部模块用；未注入静默丢弃）。
pub(crate) fn log_line(msg: &str) {
    let p = LOGGER.load(Ordering::SeqCst);
    if p == 0 {
        return;
    }
    // SAFETY: p 由 set_logger 注入的 fn 指针转换而来（函数指针与 usize 同宽），
    // 注入方保证函数签名 `fn(&str)` 与槽位一致且进程存活期内有效。
    let f: fn(&str) = unsafe { std::mem::transmute(p) };
    f(msg);
}
