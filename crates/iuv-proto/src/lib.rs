//! iuv-proto：M10 薄客户端 IPC 协议的**线上契约唯一权威**（49-thin-client-arch.md §4）。
//!
//! 本 crate 只做三件事：
//! 1. **帧格式**（[`frame`]）：8 字节头 + 载荷，显式分帧，不依赖管道消息模式；
//! 2. **消息定义**（[`msg`]）：C2S / S2C / Push 三个方向枚举 + 线上载荷类型——
//!    方向即类型，穷尽 match；载荷一律 serde 派生（拍板：全量 serde，见 49 §6.5）；
//! 3. **stream_id 分配**（[`stream`]）：客户端偶数 / 服务端奇数，回绕跳过在途号。
//!
//! 线上类型（Key/ImeState/PageInfo…）在这里拥有 wire 副本（与 iuv-core 语义镜像），
//! P3 接线时由 engine/client 侧做 core ↔ proto 映射；最终态按 49 §3.1 收敛为唯一定义。
//!
//! 传输无关：本 crate 不接触任何 Win32/管道 API，mac/linux 换传输不改这里。

pub mod frame;
pub mod msg;
pub mod stream;

pub use frame::{
    decode_frame, decode_payload, encode_frame, FrameHeader, FrameKind, HEADER_LEN, MAX_PAYLOAD,
};
pub use msg::{
    Auth, BuildId, Candidate, CandidateKind, Caps, CaretRect, ClientConfig, ClientInfo, CtlCmd,
    CtlResult, Effect, ImeMode, ImePunct, ImeScript, ImeState, ImeWidth, Key, KeyOutcome, KeyPhase,
    KeyToken, KeyVerdict, Mods, PageInfo, Payload, ProtoError, Push, ResumeToken, SessionEnd,
    UserMutation, C2S, S2C,
};
pub use stream::StreamIdAlloc;

/// 协议版本下限（握手协商用，49 §4.4）。破坏性线格式变更 +1。
pub const PROTO_MIN: u16 = 1;
/// 协议版本上限。区间 [PROTO_MIN, PROTO_MAX] 允许前后兼容窗口。
pub const PROTO_MAX: u16 = 1;

/// 版本协商：取双方支持区间的**最大共同值**（= 双方上限的较小者）；无交集 →
/// [`msg::ProtoError::VersionMismatch`]。
///
/// 报错时 `client`/`server` 字段分别回填双方的**最低要求**（排障时一眼看出谁太新谁太旧）。
pub fn negotiate(
    client_min: u16,
    client_max: u16,
    server_min: u16,
    server_max: u16,
) -> Result<u16, msg::ProtoError> {
    let proto = client_max.min(server_max);
    if proto >= client_min.max(server_min) {
        Ok(proto)
    } else {
        Err(msg::ProtoError::VersionMismatch {
            client: client_min,
            server: server_min,
        })
    }
}
