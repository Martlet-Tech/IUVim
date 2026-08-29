//! iuv-win：Windows 共享 UI 呈现层（32-status-toolbar.md §6.4 定稿 = iuv-ui + ULW 自绘）。
//!
//! daemon（工具栏）与 TSF（候选窗/菜单）复用同一份 `UpdateLayeredWindow` 呈现代码
//! （原 iuv-tsf `ui/ulw.rs` 抽取）。日志 = `logger.rs` 共享文件日志（两进程各自
//! `logger::init` 装配文件名后直接写，本 crate 不再走外部注入钩子）。
//!
//! P2.5 追加：`popup.rs`（LayeredWindow：类注册/创建/DPI/GWLP_USERDATA/wndproc 默认臂/Drop，
//! candwin/menu_window/工具栏三处窗口样板收敛）。
//!
//! P3.2 追加：`ipc/`（M6 用户库管道 + 反向控制通道，自 iuv-data 移入）+ `shm.rs`（共享内存段，
//! 自 iuv-data 移入）——iuv-data 恢复跨平台，纯 Windows 代码集中在本 crate。
//!
//! 全部公开函数不 panic：呈现失败记日志并静默降级（iuv 各进程硬性约定）。

pub mod ipc;
pub mod keys;
pub mod logger;
pub mod popup;
pub mod shm;
pub mod ulw;

pub use ipc::{
    ctl_pipe_name, CtlClient, CtlCmd, CtlResult, CtlServer, PipeClient, PipeServer, Request,
    Response, SignalClient, SignalServer, ToolbarSignal,
};
pub use keys::{base_key_to_vk, combo_from_vk, combo_mods, vk_to_base_key};
pub use logger::{
    init_logger, log_line, log_path, module_name, process_id, set_log_modules_disabled, temp_dir,
    thread_id,
};
pub use popup::LayeredWindow;
pub use shm::{ShmReader, ShmWriter};
pub use ulw::UlwSurface;
