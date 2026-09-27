//! 帧格式（49 §4.2）：8 字节头 + serde 载荷，显式分帧。
//!
//! 线格式（文字契约，改动 = 破坏性变更 → `PROTO_MIN` +1）：
//!
//! ```text
//! 偏移  长度  字段          说明
//! 0     4     payload_len   u32 LE，不含 8B 头
//! 4     1     kind          0=客户端REQ 1=服务端REQ 2=客户端RESP 3=服务端RESP 4=PUSH
//! 5     1     flags         bit0=URGENT（热路径帧优先出队），其余保留，必须为 0
//! 6     2     stream_id     u16 LE：客户端偶数 / 服务端奇数（防双向 REQ 撞号）
//! 8     N     payload       postcard（serde）编码的 Payload
//! ```
//!
//! 纪律（49 §4.8）：解码错误 = 拒整帧 + 类型化上报，**绝不猜测、绝不截断、绝不静默收垃圾**。

use crate::msg::{Payload, ProtoError};

/// 帧头长度。
pub const HEADER_LEN: usize = 8;
/// 单帧载荷上限（沿用现值 `PIPE_FRAME_MAX`；超限 = 协议级错误，绝不静默截断）。
pub const MAX_PAYLOAD: usize = 64 * 1024;
/// flags 保留位掩码（bit0 之外都必须为 0——前向兼容的把关）。
const FLAGS_RESERVED: u8 = !0x01;

/// kind 字节：双向 REQ/RESP 分开编号，帧**自描述**（不依赖连接方向即可解码）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum FrameKind {
    ClientReq = 0,
    ServerReq = 1,
    ClientResp = 2,
    ServerResp = 3,
    Push = 4,
}

impl FrameKind {
    fn from_u8(v: u8) -> Option<FrameKind> {
        match v {
            0 => Some(FrameKind::ClientReq),
            1 => Some(FrameKind::ServerReq),
            2 => Some(FrameKind::ClientResp),
            3 => Some(FrameKind::ServerResp),
            4 => Some(FrameKind::Push),
            _ => None,
        }
    }
}

impl Payload {
    /// 本载荷对应的 kind 字节。
    pub fn kind(&self) -> FrameKind {
        match self {
            Payload::ClientReq(_) => FrameKind::ClientReq,
            Payload::ServerReq(_) => FrameKind::ServerReq,
            Payload::ClientResp(_) => FrameKind::ClientResp,
            Payload::ServerResp(_) => FrameKind::ServerResp,
            Payload::Push(_) => FrameKind::Push,
        }
    }
}

/// 帧头。`payload_len` 由 [`encode_frame`] 填 / [`decode_header`] 校验，
/// 不提供直接构造——避免头与载荷自相矛盾。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameHeader {
    pub kind: FrameKind,
    /// bit0：热路径帧，传输层优先出队（排队优先级；传输插队由 49 §4.7 发送纪律解决）。
    pub urgent: bool,
    pub stream_id: u16,
    pub payload_len: u32,
}

impl FrameHeader {
    /// 编码 8 字节头到 `buf`。
    pub fn encode_into(&self, buf: &mut [u8; HEADER_LEN]) {
        buf[0..4].copy_from_slice(&self.payload_len.to_le_bytes());
        buf[4] = self.kind as u8;
        buf[5] = u8::from(self.urgent);
        buf[6..8].copy_from_slice(&self.stream_id.to_le_bytes());
    }

    /// 解码 8 字节头。头字段本身全部校验（kind 未知 / 保留 flags 位非 0 / 超限即拒）。
    pub fn decode(buf: &[u8; HEADER_LEN]) -> Result<FrameHeader, ProtoError> {
        let payload_len = u32::from_le_bytes(buf[0..4].try_into().expect("定长切片"));
        let kind = FrameKind::from_u8(buf[4]).ok_or(ProtoError::UnknownTag {
            kind: buf[4],
            tag: 0,
        })?;
        let flags = buf[5];
        if flags & FLAGS_RESERVED != 0 {
            return Err(ProtoError::Malformed {
                offset: 5,
                reason: format!("帧头保留 flags 位非 0: {flags:#04x}"),
            });
        }
        if payload_len as usize > MAX_PAYLOAD {
            return Err(ProtoError::FrameTooLarge {
                len: payload_len,
                max: MAX_PAYLOAD as u32,
            });
        }
        Ok(FrameHeader {
            kind,
            urgent: flags & 0x01 != 0,
            stream_id: u16::from_le_bytes(buf[6..8].try_into().expect("定长切片")),
            payload_len,
        })
    }
}

/// 编码一帧：头（kind 由载荷变体推导，不存在 kind/载荷错配）+ postcard 载荷。
pub fn encode_frame(
    stream_id: u16,
    urgent: bool,
    payload: &Payload,
) -> Result<Vec<u8>, ProtoError> {
    let body = postcard::to_allocvec(payload).map_err(|e| ProtoError::Malformed {
        offset: 0,
        reason: format!("编码失败: {e}"),
    })?;
    if body.len() > MAX_PAYLOAD {
        return Err(ProtoError::FrameTooLarge {
            len: body.len() as u32,
            max: MAX_PAYLOAD as u32,
        });
    }
    let mut head = [0u8; HEADER_LEN];
    FrameHeader {
        kind: payload.kind(),
        urgent,
        stream_id,
        payload_len: body.len() as u32,
    }
    .encode_into(&mut head);
    let mut out = Vec::with_capacity(HEADER_LEN + body.len());
    out.extend_from_slice(&head);
    out.extend_from_slice(&body);
    Ok(out)
}

/// 只解头（transport 据此切片读载荷）。`buf` 不足 8 字节即拒。
pub fn decode_header(buf: &[u8]) -> Result<FrameHeader, ProtoError> {
    let head: &[u8; HEADER_LEN] = buf
        .get(0..HEADER_LEN)
        .and_then(|s| s.try_into().ok())
        .ok_or(ProtoError::Malformed {
            offset: 0,
            reason: format!("帧头不足 {HEADER_LEN} 字节: got {}", buf.len()),
        })?;
    FrameHeader::decode(head)
}

/// 解码一帧。`buf` 必须**恰好**一帧（头 + 载荷）：不足 = 截断、有余 = 残留，
/// 一律 `Malformed` 拒整帧（49 §4.8 纪律）。
pub fn decode_frame(buf: &[u8]) -> Result<(FrameHeader, Payload), ProtoError> {
    let header = decode_header(buf)?;
    let expect = HEADER_LEN + header.payload_len as usize;
    if buf.len() != expect {
        return Err(ProtoError::Malformed {
            offset: HEADER_LEN as u32,
            reason: format!("帧长不匹配: 头声明 {expect}B, 实得 {}B", buf.len()),
        });
    }
    let body = &buf[HEADER_LEN..expect];
    let (payload, rest) =
        postcard::take_from_bytes::<Payload>(body).map_err(|e| ProtoError::Malformed {
            offset: HEADER_LEN as u32,
            reason: format!("载荷解码失败: {e}"),
        })?;
    if !rest.is_empty() {
        return Err(ProtoError::Malformed {
            offset: (HEADER_LEN + body.len() - rest.len()) as u32,
            reason: format!("载荷残留 {} 字节", rest.len()),
        });
    }
    if payload.kind() != header.kind {
        return Err(ProtoError::UnknownTag {
            kind: header.kind as u8,
            tag: 0,
        });
    }
    Ok((header, payload))
}
