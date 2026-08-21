//! 载荷编解码（P3.2 自 iuv-data/ipc.rs 移入 iuv-win）：零依赖手写 LE 二进制编解码。
//! 帧格式见 `super::pipe` 模块头；载荷编码表：
//!
//! ```text
//! Request:
//!   u8 tag
//!     0x01 Swap   : u32 a_code_len|a_code  u32 a_word_len|a_word  u32 a_adj
//!                   u32 b_code_len|b_code  u32 b_word_len|b_word  u32 b_adj
//!     0x02 Set    : u32 code_len|code  u32 word_len|word  u32 adj
//!     0x03 Remove : u32 code_len|code  u32 word_len|word
//!     0x04 Block  : u32 code_len|code  u32 word_len|word
//!     0x05 Ping / 0x06 OpenSettings / 0x07 Quit / 0x0B ToggleToolbar : （无载荷）
//!     0x08 Register / 0x09 StateSync : u32 pid u32 tid 4×u8（ImeState，序 mode/width/script/punct）
//!     0x0A Active : u32 pid u32 tid u8(active)
//!     0x0C Unregister : u32 pid u32 tid
//!
//! Response:
//!   u8 tag
//!     0x01 Ok  : u32 version   （应用后的用户库段 version；Ping 时为当前 version）
//!     0x02 Err : u32 msg_len|msg（UTF-8 错误消息）
//!
//! CtlCmd:
//!   u8 tag
//!     0x01 SetMode   : u8 value（0/1；false=中文 true=英文）
//!     0x02 SetWidth  : u8 value（false=半角 true=全角）
//!     0x03 SetScript : u8 value（false=简体 true=繁体）
//!     0x04 SetPunct  : u8 value（false=中文标点 true=英文标点）
//!
//! CtlResult:
//!   u8 tag
//!     0x01 Ok  : 4×u8（新四态，ImeState 线编码）
//!     0x02 Err : u32 msg_len|msg
//! ```
//!
//! 四态字节 ↔ `iuv_core::ImeState` 的唯一转换点在 iuv-core runtime.rs
//! （`From<ImeState> for [u8;4]` / `TryFrom<[u8;4]>`）；本模块只套用——非法字节解码即 Err。

use std::io;

use iuv_core::ImeState;

use super::msg::{CtlCmd, CtlResult, Request, Response};

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

// ===== 编码 =====

/// Request → 载荷字节（不含帧前缀）。
pub(crate) fn encode_request(req: &Request) -> Vec<u8> {
    let mut out = Vec::with_capacity(64);
    match req {
        Request::Swap {
            a_code,
            a_word,
            a_adj,
            b_code,
            b_word,
            b_adj,
        } => {
            out.push(0x01);
            put_str(&mut out, a_code);
            put_str(&mut out, a_word);
            out.extend_from_slice(&a_adj.to_le_bytes());
            put_str(&mut out, b_code);
            put_str(&mut out, b_word);
            out.extend_from_slice(&b_adj.to_le_bytes());
        }
        Request::Set { code, word, adj } => {
            out.push(0x02);
            put_str(&mut out, code);
            put_str(&mut out, word);
            out.extend_from_slice(&adj.to_le_bytes());
        }
        Request::Remove { code, word } => {
            out.push(0x03);
            put_str(&mut out, code);
            put_str(&mut out, word);
        }
        Request::Block { code, word } => {
            out.push(0x04);
            put_str(&mut out, code);
            put_str(&mut out, word);
        }
        Request::Ping => {
            out.push(0x05);
        }
        Request::OpenSettings => {
            out.push(0x06);
        }
        Request::Quit => {
            out.push(0x07);
        }
        Request::Register { pid, tid, state } => {
            out.push(0x08);
            out.extend_from_slice(&pid.to_le_bytes());
            out.extend_from_slice(&tid.to_le_bytes());
            put_toolbar_state(&mut out, state);
        }
        Request::StateSync { pid, tid, state } => {
            out.push(0x09);
            out.extend_from_slice(&pid.to_le_bytes());
            out.extend_from_slice(&tid.to_le_bytes());
            put_toolbar_state(&mut out, state);
        }
        Request::Active {
            pid,
            tid,
            active,
        } => {
            out.push(0x0A);
            out.extend_from_slice(&pid.to_le_bytes());
            out.extend_from_slice(&tid.to_le_bytes());
            out.push(u8::from(*active));
        }
        Request::ToggleToolbar => {
            out.push(0x0B);
        }
        Request::Unregister { pid, tid } => {
            out.push(0x0C);
            out.extend_from_slice(&pid.to_le_bytes());
            out.extend_from_slice(&tid.to_le_bytes());
        }
    }
    out
}

fn put_toolbar_state(out: &mut Vec<u8>, s: &ImeState) {
    out.extend_from_slice(&<[u8; 4]>::from(*s));
}

/// Response → 载荷字节（不含帧前缀）。
pub(crate) fn encode_response(resp: &Response) -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    match resp {
        Response::Ok { version } => {
            out.push(0x01);
            out.extend_from_slice(&version.to_le_bytes());
        }
        Response::Err { msg } => {
            out.push(0x02);
            put_str(&mut out, msg);
        }
    }
    out
}

fn put_str(out: &mut Vec<u8>, s: &str) {
    let b = s.as_bytes();
    out.extend_from_slice(&(b.len() as u32).to_le_bytes());
    out.extend_from_slice(b);
}

// ===== 解码 =====

/// 载荷 → Request。非法 → `Err`。
pub(crate) fn decode_request(payload: &[u8]) -> io::Result<Request> {
    let mut r = Reader::new(payload);
    let tag = r.u8()?;
    match tag {
        0x01 => {
            let a_code = r.str_()?;
            let a_word = r.str_()?;
            let a_adj = r.u32()?;
            let b_code = r.str_()?;
            let b_word = r.str_()?;
            let b_adj = r.u32()?;
            r.finish()?;
            Ok(Request::Swap {
                a_code,
                a_word,
                a_adj,
                b_code,
                b_word,
                b_adj,
            })
        }
        0x02 => {
            let code = r.str_()?;
            let word = r.str_()?;
            let adj = r.u32()?;
            r.finish()?;
            Ok(Request::Set { code, word, adj })
        }
        0x03 => {
            let code = r.str_()?;
            let word = r.str_()?;
            r.finish()?;
            Ok(Request::Remove { code, word })
        }
        0x04 => {
            let code = r.str_()?;
            let word = r.str_()?;
            r.finish()?;
            Ok(Request::Block { code, word })
        }
        0x05 => {
            r.finish()?;
            Ok(Request::Ping)
        }
        0x06 => {
            r.finish()?;
            Ok(Request::OpenSettings)
        }
        0x07 => {
            r.finish()?;
            Ok(Request::Quit)
        }
        0x08 => {
            let pid = r.u32()?;
            let tid = r.u32()?;
            let state = r.toolbar_state()?;
            r.finish()?;
            Ok(Request::Register { pid, tid, state })
        }
        0x09 => {
            let pid = r.u32()?;
            let tid = r.u32()?;
            let state = r.toolbar_state()?;
            r.finish()?;
            Ok(Request::StateSync { pid, tid, state })
        }
        0x0A => {
            let pid = r.u32()?;
            let tid = r.u32()?;
            let active = r.u8()? != 0;
            r.finish()?;
            Ok(Request::Active { pid, tid, active })
        }
        0x0B => {
            r.finish()?;
            Ok(Request::ToggleToolbar)
        }
        0x0C => {
            let pid = r.u32()?;
            let tid = r.u32()?;
            r.finish()?;
            Ok(Request::Unregister { pid, tid })
        }
        t => Err(bad(&format!("未知 Request tag 0x{t:02X}"))),
    }
}

/// 载荷 → Response。非法 → `Err`。
pub(crate) fn decode_response(payload: &[u8]) -> io::Result<Response> {
    let mut r = Reader::new(payload);
    let tag = r.u8()?;
    match tag {
        0x01 => {
            let version = r.u32()?;
            r.finish()?;
            Ok(Response::Ok { version })
        }
        0x02 => {
            let msg = r.str_()?;
            r.finish()?;
            Ok(Response::Err { msg })
        }
        t => Err(bad(&format!("未知 Response tag 0x{t:02X}"))),
    }
}

/// CtlCmd → 载荷字节（反向控制通道，§4.2）。
pub(crate) fn encode_ctl_cmd(cmd: &CtlCmd) -> Vec<u8> {
    let mut out = Vec::with_capacity(8);
    match cmd {
        CtlCmd::SetMode(v) => {
            out.push(0x01);
            out.push(u8::from(*v));
        }
        CtlCmd::SetWidth(v) => {
            out.push(0x02);
            out.push(u8::from(*v));
        }
        CtlCmd::SetScript(v) => {
            out.push(0x03);
            out.push(u8::from(*v));
        }
        CtlCmd::SetPunct(v) => {
            out.push(0x04);
            out.push(u8::from(*v));
        }
    }
    out
}

/// 载荷 → CtlCmd。非法 → `Err`。
pub(crate) fn decode_ctl_cmd(payload: &[u8]) -> io::Result<CtlCmd> {
    let mut r = Reader::new(payload);
    let tag = r.u8()?;
    let cmd = match tag {
        0x01 => CtlCmd::SetMode(r.bool()?),
        0x02 => CtlCmd::SetWidth(r.bool()?),
        0x03 => CtlCmd::SetScript(r.bool()?),
        0x04 => CtlCmd::SetPunct(r.bool()?),
        t => return Err(bad(&format!("未知 CtlCmd tag 0x{t:02X}"))),
    };
    r.finish()?;
    Ok(cmd)
}

/// CtlResult → 载荷字节。
pub(crate) fn encode_ctl_result(res: &CtlResult) -> Vec<u8> {
    let mut out = Vec::with_capacity(16);
    match res {
        CtlResult::Ok { state } => {
            out.push(0x01);
            put_toolbar_state(&mut out, state);
        }
        CtlResult::Err { msg } => {
            out.push(0x02);
            put_str(&mut out, msg);
        }
    }
    out
}

/// 载荷 → CtlResult。非法 → `Err`。
pub(crate) fn decode_ctl_result(payload: &[u8]) -> io::Result<CtlResult> {
    let mut r = Reader::new(payload);
    let tag = r.u8()?;
    match tag {
        0x01 => {
            let state = r.toolbar_state()?;
            r.finish()?;
            Ok(CtlResult::Ok { state })
        }
        0x02 => {
            let msg = r.str_()?;
            r.finish()?;
            Ok(CtlResult::Err { msg })
        }
        t => Err(bad(&format!("未知 CtlResult tag 0x{t:02X}"))),
    }
}

/// 前缀长度 + 二进制读取器（边界严格，越界即 Err）。
struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }
    fn take(&mut self, n: usize) -> io::Result<&'a [u8]> {
        if self.pos + n > self.data.len() {
            return Err(bad("载荷截断"));
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    fn u8(&mut self) -> io::Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> io::Result<u32> {
        let b = self.take(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }
    fn str_(&mut self) -> io::Result<String> {
        let len = self.u32()? as usize;
        let b = self.take(len)?;
        std::str::from_utf8(b)
            .map(str::to_owned)
            .map_err(|_| bad("字符串非 UTF-8"))
    }
    fn toolbar_state(&mut self) -> io::Result<ImeState> {
        let b = self.take(4)?;
        let state = ImeState::try_from([b[0], b[1], b[2], b[3]])
            .map_err(|_| bad("四态字节非法（须 0/1）"))?;
        Ok(state)
    }

    /// u8 → bool（仅 0/1 合法）。
    fn bool(&mut self) -> io::Result<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(bad("布尔字节非法（须 0/1）")),
        }
    }
    fn finish(&self) -> io::Result<()> {
        if self.pos != self.data.len() {
            return Err(bad("载荷尾部有残留字节"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_swap_roundtrip() {
        let req = Request::Swap {
            a_code: "haoshi".into(),
            a_word: "好使".into(),
            a_adj: 5800,
            b_code: "haoshi".into(),
            b_word: "耗时".into(),
            b_adj: 3800,
        };
        let bytes = encode_request(&req);
        let back = decode_request(&bytes).unwrap();
        assert_eq!(back, req);
        // 帧前缀 + 载荷往返
        let frame = to_frame(&bytes);
        let payload = parse_frame(&frame).unwrap();
        assert_eq!(decode_request(payload).unwrap(), req);
    }

    #[test]
    fn request_set_remove_block_ping_roundtrip() {
        for req in [
            Request::Set {
                code: "zhang'wei'wei".into(),
                word: "张葳葳".into(),
                adj: 8000,
            },
            Request::Remove {
                code: "de".into(),
                word: "的".into(),
            },
            Request::Block {
                code: "shou'xuan".into(),
                word: "手癣".into(),
            },
            Request::Ping,
        ] {
            let bytes = encode_request(&req);
            assert_eq!(decode_request(&bytes).unwrap(), req);
        }
    }

    #[test]
    fn response_roundtrip() {
        let ok = Response::Ok { version: 42 };
        let bytes = encode_response(&ok);
        assert_eq!(decode_response(&bytes).unwrap(), ok);
        let err = Response::Err {
            msg: "写盘失败".into(),
        };
        let bytes = encode_response(&err);
        assert_eq!(decode_response(&bytes).unwrap(), err);
    }

    #[test]
    fn decode_rejects_malformed() {
        assert!(decode_request(&[0x01]).is_err(), "Swap 截断");
        assert!(decode_request(&[0xFF]).is_err(), "未知 tag");
        assert!(decode_response(&[0x01]).is_err(), "Ok 截断");
        assert!(parse_frame(&[0, 0, 0, 5, 1]).is_err(), "帧长度不符");
        assert!(parse_frame(&[0, 0]).is_err(), "帧头不完整");
        // 尾部残留
        let mut bytes = encode_request(&Request::Ping);
        bytes.push(0xAA);
        assert!(decode_request(&bytes).is_err(), "Ping 后残留字节");
    }

    #[test]
    fn toolbar_requests_roundtrip() {
        let state = ImeState {
            mode: iuv_core::InitialMode::English,
            width: iuv_core::WidthMode::Half,
            script: iuv_core::ScriptMode::Traditional,
            punct: iuv_core::PunctMode::English,
        };
        for req in [
            Request::Register {
                pid: 1234,
                tid: 56,
                state,
            },
            Request::StateSync {
                pid: 1234,
                tid: 56,
                state: ImeState::default(),
            },
            Request::Active {
                pid: 1234,
                tid: 56,
                active: true,
            },
            Request::Active {
                pid: 1234,
                tid: 56,
                active: false,
            },
            Request::ToggleToolbar,
            Request::Unregister {
                pid: 1234,
                tid: 56,
            },
        ] {
            let bytes = encode_request(&req);
            assert_eq!(decode_request(&bytes).unwrap(), req);
        }
        // 帧前缀 + 载荷往返
        let frame = to_frame(&encode_request(&Request::Register {
            pid: 7,
            tid: 8,
            state,
        }));
        let payload = parse_frame(&frame).unwrap();
        assert_eq!(
            decode_request(payload).unwrap(),
            Request::Register {
                pid: 7,
                tid: 8,
                state,
            }
        );
        // 截断拒绝
        assert!(decode_request(&[0x08, 0x01]).is_err(), "Register 截断");
    }

    #[test]
    fn toolbar_state_rejects_invalid_byte() {
        // 四态字节非 0/1 → 整条消息拒绝（不静默收垃圾）
        let mut bytes = encode_request(&Request::StateSync {
            pid: 1,
            tid: 2,
            state: ImeState::default(),
        });
        let n = bytes.len();
        bytes[n - 1] = 7;
        assert!(decode_request(&bytes).is_err(), "punct 字节非法");
    }

    #[test]
    fn ctl_cmd_roundtrip() {
        for cmd in [
            CtlCmd::SetMode(true),
            CtlCmd::SetWidth(false),
            CtlCmd::SetScript(true),
            CtlCmd::SetPunct(false),
        ] {
            let bytes = encode_ctl_cmd(&cmd);
            assert_eq!(decode_ctl_cmd(&bytes).unwrap(), cmd);
            // 帧前缀往返
            let frame = to_frame(&bytes);
            assert_eq!(decode_ctl_cmd(parse_frame(&frame).unwrap()).unwrap(), cmd);
        }
        assert!(decode_ctl_cmd(&[0x01]).is_err(), "SetMode 截断");
        assert!(decode_ctl_cmd(&[0xFF]).is_err(), "未知 Cmd tag");
        assert!(
            decode_ctl_cmd(&[0x02, 5]).is_err(),
            "布尔字节非法（须 0/1）"
        );
        let mut bytes = encode_ctl_cmd(&CtlCmd::SetPunct(true));
        bytes.push(0xAA);
        assert!(decode_ctl_cmd(&bytes).is_err(), "残留字节");
    }

    #[test]
    fn ctl_result_roundtrip() {
        let ok = CtlResult::Ok {
            state: ImeState {
                mode: iuv_core::InitialMode::Chinese,
                width: iuv_core::WidthMode::Full,
                script: iuv_core::ScriptMode::Traditional,
                punct: iuv_core::PunctMode::Chinese,
            },
        };
        let bytes = encode_ctl_result(&ok);
        assert_eq!(decode_ctl_result(&bytes).unwrap(), ok);
        let err = CtlResult::Err {
            msg: "写 OPENCLOSE 失败".into(),
        };
        let bytes = encode_ctl_result(&err);
        assert_eq!(decode_ctl_result(&bytes).unwrap(), err);
        assert!(decode_ctl_result(&[0x01]).is_err(), "Ok 截断");
    }
}