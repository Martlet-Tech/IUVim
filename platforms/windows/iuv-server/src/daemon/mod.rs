//! daemon 功能迁入（M10 ②：iuv-daemon → iuv-server 演进，49 §2）。
//!
//! 工具栏/桌宠、egui 设置页、全局热键、用户库状态原样迁入，IPC 触点重接线：
//! - 入（TSF → server）：toolbar signal → `C2S::{FocusChanged, ImeState,
//!   TypingActivity}`；数据面管道写 → `C2S::UserMutation`；langbar 查询 →
//!   `C2S::{OpenSettings, ToggleToolbar, ToolbarVisibleQuery}`；
//! - 出（server → TSF）：ctl 反向通道 → transport `ConnSender`（`S2C::Ctl`），
//!   由 [`crate::EngineService`] 按 pid/tid 路由。
//!
//! daemon 的 SHM 写者随迁移退役（server 端 [`crate::EngineService`] 的
//! ShmWriter 是唯一写者），`DaemonState` 以 `shm: None` 构造。

pub mod capture;
pub mod config;
pub mod hotkey;
pub mod log;
pub mod pet_assets;
pub mod settings;
pub mod state;
pub mod toolbar;
pub mod toolbar_icons;
