//! 用户库命名管道 IPC + 反向控制通道（M6：会话进程写请求 → 守护进程）。
//!
//! 设计见 `docs/plan/22-m6-daemon.md` §3 与文末「会话进程客户端对接规格」。
//! 纯 Windows（本 crate 平台限定）。P3.2 自 iuv-data/ipc.rs 移入 iuv-win：
//!
//! - `msg.rs`：消息类型（Request/Response/工具栏四态/CtlCmd/CtlResult）
//! - `codec.rs`：零依赖手写 LE 二进制编解码
//! - `pipe.rs`：用户库管道 PipeClient/PipeServer + 底层 `imp`（与 ctl.rs 共用）
//! - `ctl.rs`：反向控制通道 CtlServer/CtlClient

mod codec;
mod ctl;
mod msg;
mod pipe;

pub use ctl::{CtlClient, CtlServer};
pub use msg::{ctl_pipe_name, CtlCmd, CtlResult, Request, Response};
pub use pipe::{PipeClient, PipeServer};