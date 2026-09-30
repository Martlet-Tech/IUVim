//! 旧管道 IPC 残余 + 反向控制 + 基准（M6 起家，49 号 ②/③ 迁移后大幅收敛）。
//!
//! 数据面与控制面已统一走 `transport`（iuv-proto serde codec），旧的手写
//! Request/Response 编码表、用户库管道 PipeClient/PipeServer、反向控制通道
//! CtlServer/CtlClient、工具条信号专用管道 signal.rs 均已删除。此处仅存：
//!
//! - `msg.rs`：Request::ToggleToolbar（语言栏菜单，进程内直调）+ ToolbarSignal
//!   （工具栏信号类型）+ CtlCmd/CtlResult re-export
//! - `codec.rs`：4 字节长度前缀帧工具（to_frame/parse_frame）
//! - `pipe.rs`：同步消息模式管道原语 `imp`（唯一消费者 = rtt 基准）
//! - `rtt.rs`：IPC 往返延迟基准（49 号 P0 前置：热路径往返实测）

mod codec;
mod msg;
mod pipe;
mod rtt;

pub use msg::{CtlCmd, CtlResult, Request, ToolbarSignal};
pub use rtt::{bench as rtt_bench, test_pipe_name as rtt_test_pipe_name, RttReport, RttStats};
