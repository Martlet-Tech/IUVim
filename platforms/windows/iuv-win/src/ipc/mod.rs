//! 用户库命名管道 IPC + 反向控制通道（M6：会话进程写请求 → 守护进程）。
//!
//! 设计见 `docs/plan/22-m6-daemon.md` §3 与文末「会话进程客户端对接规格」。
//! 纯 Windows（本 crate 平台限定）。P3.2 自 iuv-data/ipc.rs 移入 iuv-win：
//!
//! - `msg.rs`：消息类型（Request/Response/ToolbarSignal/工具栏四态/CtlCmd/CtlResult）
//! - `codec.rs`：零依赖手写 LE 二进制编解码
//! - `pipe.rs`：用户库管道 PipeClient/PipeServer + 底层 `imp`（与 ctl.rs 共用）
//! - `signal.rs`：工具条信号通道（专用管道，控制面/数据面物理隔离）
//! - `ctl.rs`：反向控制通道 CtlServer/CtlClient
//! - `rtt.rs`：IPC 往返延迟基准（49 号 P0 前置：热路径往返实测）

mod codec;
mod ctl;
mod msg;
mod pipe;
mod rtt;
mod signal;

pub use ctl::{CtlClient, CtlServer};
pub use msg::{ctl_pipe_name, CtlCmd, CtlResult, Request, Response, ToolbarSignal};
pub use pipe::{PipeClient, PipeServer};
pub use rtt::{bench as rtt_bench, test_pipe_name as rtt_test_pipe_name, RttReport, RttStats};
pub use signal::{SignalClient, SignalServer};
