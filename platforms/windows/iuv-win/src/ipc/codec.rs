//! 载荷编解码残余（49 号 ②/③ 迁移后）：帧前缀工具。
//!
//! 旧手写 Request/Response/ToolbarSignal/CtlCmd 编码表已随旧管道/信号管道退役
//! （数据面与控制面统一走 transport + iuv-proto serde codec）。此处仅存
//! `to_frame`/`parse_frame`——4 字节 LE 长度前缀帧，供 `pipe::imp` 的
//! `read_frame`/`write_frame`（rtt 基准使用）。

use std::io;

/// 编码失败（解码非法字节 / 越界）。
pub(crate) fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// 载荷 → 帧（前缀 u32 长度 + 载荷）。
pub(crate) fn to_frame(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// 校验并剥帧前缀：返回 (载荷, 载荷起点)；帧头不完整/长度越界 → `Err`。
/// 供读取端在整帧缓冲（已含前缀）上调用。
pub(crate) fn parse_frame(buf: &[u8]) -> io::Result<&[u8]> {
    if buf.len() < 4 {
        return Err(bad("帧头不完整"));
    }
    let len = u32::from_le_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
    if buf.len() != 4 + len {
        return Err(bad(&format!(
            "帧长度不符：头声明 {len}，实际 {}",
            buf.len() - 4
        )));
    }
    Ok(&buf[4..])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip() {
        let payload = b"hello echo";
        let frame = to_frame(payload);
        assert_eq!(parse_frame(&frame).unwrap(), payload);
    }

    #[test]
    fn frame_rejects_malformed() {
        assert!(parse_frame(&[0, 0, 0, 5, 1]).is_err(), "帧长度不符");
        assert!(parse_frame(&[0, 0]).is_err(), "帧头不完整");
    }
}
