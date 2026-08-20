//! 用户库命名管道 IPC（M6：会话进程写请求 → 守护进程）。
//!
//! 设计见 `docs/plan/22-m6-daemon.md` §3 与文末「会话进程客户端对接规格」。
//! **M6 仅 Windows 生效**；非 Windows 编译降级 stub（全部方法返回 `Err(Unsupported)`）。
//!
//! ## 管道名
//!
//! `\\.\pipe\iuv-userdict`（单用户桌面足够，不做用户 SID 隔离——多用户各自有会话，
//! `Local\` 域天然隔离；文档注明）。
//!
//! ## 帧格式（前缀长度 + 二进制载荷，零依赖手写编解码）
//!
//! ```text
//! [0..4]  u32 msg_len（LE）= 载荷字节数（不含本前缀）
//! [4..]   载荷 = 序列化 Request / Response（见下方编码表）
//! ```
//!
//! 管道为**消息模式**（PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE）：一次 WriteFile 写
//! 一帧、一次 ReadFile 读一帧（缓冲足够时整帧返回）——配合前缀长度做校验。
//!
//! ## 载荷二进制编码（LE，UTF-8 字符串 = u32 长度 + 字节）
//!
//! ```text
//! Request:
//!   u8 tag
//!     0x01 Swap   : u32 a_code_len|a_code  u32 a_word_len|a_word  u32 a_adj
//!                   u32 b_code_len|b_code  u32 b_word_len|b_word  u32 b_adj
//!     0x02 Set    : u32 code_len|code  u32 word_len|word  u32 adj
//!     0x03 Remove : u32 code_len|code  u32 word_len|word
//!     0x04 Block  : u32 code_len|code  u32 word_len|word
//!     0x05 Ping   : （无载荷）
//!
//! Response:
//!   u8 tag
//!     0x01 Ok  : u32 version   （应用后的用户库段 version；Ping 时为当前 version）
//!     0x02 Err : u32 msg_len|msg（UTF-8 错误消息）
//! ```
//!
//! ## 守护进程不在线判定（会话进程客户端）
//!
//! - 连接：`CreateFileW` 失败 ERROR_FILE_NOT_FOUND / `WaitNamedPipeW` 超时 → daemon 不在线 →
//!   降级自读文件；
//! - 连接成功但 `Ping` 超时/读失败 → 同样降级；
//! - 请求后 Response::Err → 本会话内降级（调权/隐藏立即失效但保留内存态），绝不 panic。

use std::io;

// windows API 导入放模块顶层：公共结构体字段（PipeClient/PipeServer 的 HANDLE）与
// imp 子模块共用（imp 内 `use super::*` 继承）。cfg(windows) 保证非 Windows 不引用。
#[cfg(windows)]
use windows::Win32::Foundation::{
    CloseHandle, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE,
};
#[cfg(windows)]
use windows::Win32::Storage::FileSystem::{
    CreateFileW, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile, OPEN_EXISTING,
};
#[cfg(windows)]
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, WaitNamedPipeW,
    PIPE_READMODE_MESSAGE, PIPE_TYPE_MESSAGE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
#[cfg(windows)]
use windows_core::PCWSTR;

/// 管道名（单用户桌面足够；多用户 SID 隔离在 M7 安装器范畴）。
pub const PIPE_NAME: &str = r"\\.\pipe\iuv-userdict";
/// 单帧最大字节数（消息模式 ReadFile 缓冲；用户库条目小，64KB 充裕）。
pub const PIPE_FRAME_MAX: usize = 64 * 1024;
/// 连接超时（WaitNamedPipeW，毫秒；超时视为 daemon 不在线）。
pub const PIPE_CONNECT_TIMEOUT_MS: u32 = 1500;

/// 会话进程 → 守护进程的写请求（见模块头编码表）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    /// Shift+←/→ 主动调权：a/b 两词互写对方合成权重（双 code 签名，见 UserDict::apply_swap）。
    Swap {
        a_code: String,
        a_word: String,
        a_adj: u32,
        b_code: String,
        b_word: String,
        b_adj: u32,
    },
    /// 自造词/覆盖写入（upsert）。
    Set { code: String, word: String, adj: u32 },
    /// 移除用户库条目（隐藏自造词/覆盖 = 撤销自造）。
    Remove { code: String, word: String },
    /// 屏蔽基础库词条（Shift+Delete 隐藏）。
    Block { code: String, word: String },
    /// 健康检查：探测 daemon 在线 + 拿当前 version。
    Ping,
    /// M6 语言栏菜单「设置」：通知 daemon 打开设置页（不触碰用户库）。
    OpenSettings,
    /// M6 语言栏菜单/卸载脚本：通知 daemon 干净退出（写盘后退出）。
    Quit,
    /// 32-status-toolbar.md §4.1：TSF 实例 Activate 时注册 + 上报初始四态。
    /// daemon 记入实例表（pid:tid 唯一），供看板判定/点击寻址。
    Register {
        pid: u32,
        tid: u32,
        state: ToolbarState,
    },
    /// 32-status-toolbar.md §4.1：实例运行时四态变化上报（OPENCLOSE OnChange /
    /// Cmd::SetState 应用成功后）。
    StateSync {
        pid: u32,
        tid: u32,
        state: ToolbarState,
    },
    /// 32-status-toolbar.md §4.1：Activate/Deactivate 通知（daemon 判「iuv 被选中」）。
    Active { pid: u32, tid: u32, active: bool },
    /// 32-status-toolbar.md §4.1：语言栏右键菜单「显示/隐藏工具栏」（全局偏好切换）。
    ToggleToolbar,
    /// 32-status-toolbar.md §4.1：实例 Drop 注销（从实例表移除）。
    Unregister { pid: u32, tid: u32 },
}

/// 守护进程 → 会话进程的响应。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Response {
    Ok { version: u32 },
    Err { msg: String },
}

// ===== 32-status-toolbar.md 工具栏四态 + 反向控制通道 =====

/// 工具栏四态传输值（每 TSF 实例，32-status-toolbar.md §2.4/§4）。
/// u8 编码（与 iuv-core `RuntimeState` 一致，见其 to_toolbar/from_toolbar）：
/// `mode` 0=中文 1=英文；`width` 0=半角 1=全角；`script` 0=简体 1=繁体；`punct` 0=中文标点 1=英文标点。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ToolbarState {
    pub mode: u8,
    pub width: u8,
    pub script: u8,
    pub punct: u8,
}

impl ToolbarState {
    pub const fn new(mode: u8, width: u8, script: u8, punct: u8) -> Self {
        ToolbarState {
            mode,
            width,
            script,
            punct,
        }
    }

    /// 读单字段（field 0=mode 1=width 2=script 3=punct；非法 → 0）。
    pub fn field(&self, field: u8) -> u8 {
        match field {
            0 => self.mode,
            1 => self.width,
            2 => self.script,
            3 => self.punct,
            _ => 0,
        }
    }
}

/// 反向控制通道字段 id（daemon → TSF 的 Cmd::SetState 用）。
pub const CTL_FIELD_MODE: u8 = 0;
pub const CTL_FIELD_WIDTH: u8 = 1;
pub const CTL_FIELD_SCRIPT: u8 = 2;
pub const CTL_FIELD_PUNCT: u8 = 3;

/// 反向控制通道管道名前缀：`\\.\pipe\iuv-ctl-<pid>-<tid>`。
pub const CTL_PIPE_PREFIX: &str = r"\\.\pipe\iuv-ctl";

/// 实例控制管道完整名（pid:tid 唯一，32-status-toolbar.md §4.2）。
pub fn ctl_pipe_name(pid: u32, tid: u32) -> String {
    format!("{CTL_PIPE_PREFIX}-{pid}-{tid}")
}

/// daemon → TSF 的控制命令（按需连接 per-实例管道，32-status-toolbar.md §4.2）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CtlCmd {
    /// 设置某字段为指定值（field 0=mode 1=width 2=script 3=punct，value 0/1）。
    SetState { field: u8, value: u8 },
}

/// TSF 应用命令后的响应（§6.5 点击协议：daemon 按结果更新实例表 + 按钮图标）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CtlResult {
    /// 应用成功：返回**新**四态（成功后 TSF 还会 StateSync 上报，双路径一致）。
    Ok { state: ToolbarState },
    /// 应用失败（写 OPENCLOSE 失败/非法字段等）。
    Err { msg: String },
}

/// 编码失败（解码非法字节 / 越界）。
fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

#[cfg(not(windows))]
fn err_unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "命名管道 IPC M6 仅 Windows 生效（iuv-data/src/ipc.rs 非 Windows 为桩）",
    )
}

/// 载荷 → 帧（前缀 u32 长度 + 载荷）。
pub fn to_frame(payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    out.extend_from_slice(payload);
    out
}

/// 校验并剥帧前缀：返回 (载荷, 载荷起点)；帧头不完整/长度越界 → `Err`。
/// 供读取端在整帧缓冲（已含前缀）上调用。
pub fn parse_frame(buf: &[u8]) -> io::Result<&[u8]> {
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
pub fn encode_request(req: &Request) -> Vec<u8> {
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

fn put_toolbar_state(out: &mut Vec<u8>, s: &ToolbarState) {
    out.push(s.mode);
    out.push(s.width);
    out.push(s.script);
    out.push(s.punct);
}

/// Response → 载荷字节（不含帧前缀）。
pub fn encode_response(resp: &Response) -> Vec<u8> {
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
pub fn decode_request(payload: &[u8]) -> io::Result<Request> {
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
pub fn decode_response(payload: &[u8]) -> io::Result<Response> {
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
///
/// ```text
/// u8 tag
///   0x01 SetState : u8 field, u8 value
/// ```
pub fn encode_ctl_cmd(cmd: &CtlCmd) -> Vec<u8> {
    let mut out = Vec::with_capacity(8);
    match cmd {
        CtlCmd::SetState { field, value } => {
            out.push(0x01);
            out.push(*field);
            out.push(*value);
        }
    }
    out
}

/// 载荷 → CtlCmd。非法 → `Err`。
pub fn decode_ctl_cmd(payload: &[u8]) -> io::Result<CtlCmd> {
    let mut r = Reader::new(payload);
    let tag = r.u8()?;
    match tag {
        0x01 => {
            let field = r.u8()?;
            let value = r.u8()?;
            r.finish()?;
            Ok(CtlCmd::SetState { field, value })
        }
        t => Err(bad(&format!("未知 CtlCmd tag 0x{t:02X}"))),
    }
}

/// CtlResult → 载荷字节。
///
/// ```text
/// u8 tag
///   0x01 Ok  : 新四态（4 × u8）
///   0x02 Err : u32 msg_len|msg
/// ```
pub fn encode_ctl_result(res: &CtlResult) -> Vec<u8> {
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
pub fn decode_ctl_result(payload: &[u8]) -> io::Result<CtlResult> {
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
    fn toolbar_state(&mut self) -> io::Result<ToolbarState> {
        let mode = self.u8()?;
        let width = self.u8()?;
        let script = self.u8()?;
        let punct = self.u8()?;
        Ok(ToolbarState {
            mode,
            width,
            script,
            punct,
        })
    }
    fn finish(&self) -> io::Result<()> {
        if self.pos != self.data.len() {
            return Err(bad("载荷尾部有残留字节"));
        }
        Ok(())
    }
}

#[cfg(windows)]
mod imp {
    use super::*;
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    pub fn pipe_name_wide() -> Vec<u16> {
        OsStr::new(PIPE_NAME).encode_wide().chain(Some(0)).collect()
    }

    /// 任意管道名 → 宽字符（NUL 结尾）。
    pub fn name_wide(name: &str) -> Vec<u16> {
        OsStr::new(name).encode_wide().chain(Some(0)).collect()
    }

    /// 创建命名管道服务端句柄（不连接；调用方持有，可跨线程 Close 中断 ConnectNamedPipe）。
    pub fn create_server(name_wide: &[u16]) -> io::Result<HANDLE> {
        // SAFETY: 消息模式 + 阻塞；缓冲 PIPE_FRAME_MAX 内单帧。返回 HANDLE（非 Result）。
        let handle = unsafe {
            CreateNamedPipeW(
                PCWSTR(name_wide.as_ptr()),
                PIPE_ACCESS_DUPLEX,
                PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                PIPE_FRAME_MAX as u32,
                PIPE_FRAME_MAX as u32,
                0,
                None,
            )
        };
        if handle.is_invalid() {
            let e = unsafe { windows::Win32::Foundation::GetLastError() };
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("创建命名管道失败: {}", e.0),
            ));
        }
        Ok(handle)
    }

    /// 阻塞等待客户端连接（非重叠）。失败 → `Err`（含被跨线程 Close 中断的取消路径）。
    pub fn connect_server(handle: HANDLE) -> io::Result<()> {
        // SAFETY: 阻塞等待客户端 ConnectNamedPipe（非重叠）。
        let r = unsafe { ConnectNamedPipe(handle, None) };
        if let Err(_e) = r {
            let code = unsafe { windows::Win32::Foundation::GetLastError() };
            if code != ERROR_PIPE_CONNECTED {
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("等待客户端连接失败: {}", code.0),
                ));
            }
        }
        Ok(())
    }

    /// 客户端连接任意命名管道（daemon 侧发起，按需连接 per-实例，§4.2）。
    /// daemon 不在线（文件未找到 / 忙超时）→ `Err`，调用方降级。
    pub fn connect_client(name_wide: &[u16]) -> io::Result<HANDLE> {
        loop {
            // SAFETY: name 以 NUL 结尾；管道句柄读写复用。
            let result = unsafe {
                CreateFileW(
                    PCWSTR(name_wide.as_ptr()),
                    (GENERIC_READ | GENERIC_WRITE).0,
                    windows::Win32::Storage::FileSystem::FILE_SHARE_MODE(0),
                    None,
                    OPEN_EXISTING,
                    Default::default(),
                    None,
                )
            };
            if let Ok(handle) = result {
                return Ok(handle);
            }
            let e = unsafe { windows::Win32::Foundation::GetLastError() };
            if e == ERROR_PIPE_BUSY {
                // SAFETY: name 以 NUL 结尾；等待超时视为 daemon 不在线。
                let ok = unsafe { WaitNamedPipeW(PCWSTR(name_wide.as_ptr()), PIPE_CONNECT_TIMEOUT_MS) };
                if !ok.as_bool() {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "命名管道忙且超时（服务端不在线）",
                    ));
                }
                continue; // 管道可用了，重试 CreateFileW
            }
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("命名管道不可达: {}", e.0),
            ));
        }
    }

    pub fn read_frame(handle: HANDLE) -> io::Result<Vec<u8>> {
        // 消息模式 + 大缓冲：一次 ReadFile 取整帧（含 4 字节长度前缀）。
        let mut buf = vec![0u8; PIPE_FRAME_MAX];
        let mut read: u32 = 0;
        // SAFETY: buf 可写，read 输出实际字节数；同步（无 OVERLAPPED）。
        unsafe { ReadFile(handle, Some(&mut buf), Some(&mut read), None) }
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("读管道失败: {}", e.code())))?;
        buf.truncate(read as usize);
        let payload = parse_frame(&buf)?;
        Ok(payload.to_vec())
    }

    pub fn write_frame(handle: HANDLE, payload: &[u8]) -> io::Result<()> {
        let frame = to_frame(payload);
        let mut written: u32 = 0;
        // SAFETY: frame 只读；written 输出实际字节数；同步。
        unsafe { WriteFile(handle, Some(&frame), Some(&mut written), None) }
            .map_err(|e| io::Error::new(io::ErrorKind::Other, format!("写管道失败: {}", e.code())))?;
        if written as usize != frame.len() {
            return Err(bad(&format!("写管道字节数不符 {written} != {}", frame.len())));
        }
        Ok(())
    }
}

/// 管道客户端（会话进程侧）：连接 + 发一帧收一帧。
/// 非 Windows = 桩（不可构造成功）。
pub struct PipeClient {
    #[cfg(windows)]
    handle: HANDLE,
    #[cfg(not(windows))]
    _stub: (),
}

impl PipeClient {
    /// 连接守护进程管道。daemon 不在线（文件未找到 / 忙超时）→ `Err`，调用方降级。
    pub fn connect() -> io::Result<PipeClient> {
        #[cfg(windows)]
        {
            let name = imp::pipe_name_wide();
            loop {
                // SAFETY: name 以 NUL 结尾；管道句柄读写复用。
                let result = unsafe {
                    CreateFileW(
                        PCWSTR(name.as_ptr()),
                        (GENERIC_READ | GENERIC_WRITE).0,
                        windows::Win32::Storage::FileSystem::FILE_SHARE_MODE(0),
                        None,
                        OPEN_EXISTING,
                        Default::default(),
                        None,
                    )
                };
                if let Ok(handle) = result {
                    return Ok(PipeClient { handle });
                }
                let e = unsafe { windows::Win32::Foundation::GetLastError() };
                if e == ERROR_PIPE_BUSY {
                    // SAFETY: name 以 NUL 结尾；等待超时视为 daemon 不在线。
                    let ok = unsafe { WaitNamedPipeW(PCWSTR(name.as_ptr()), PIPE_CONNECT_TIMEOUT_MS) };
                    if !ok.as_bool() {
                        return Err(io::Error::new(
                            io::ErrorKind::TimedOut,
                            "守护进程管道忙且超时（daemon 不在线）",
                        ));
                    }
                    continue; // 管道可用了，重试 CreateFileW
                }
                return Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    format!("守护进程管道不可达: {}", e.0),
                ));
            }
        }
        #[cfg(not(windows))]
        {
            Err(err_unsupported())
        }
    }

    /// 发请求 → 收响应（单次会话）。
    pub fn request(&self, req: &Request) -> io::Result<Response> {
        #[cfg(windows)]
        {
            let payload = imp::write_frame(self.handle, &encode_request(req))?;
            let _ = payload;
            let resp_payload = imp::read_frame(self.handle)?;
            decode_response(&resp_payload)
        }
        #[cfg(not(windows))]
        {
            let _ = req;
            Err(err_unsupported())
        }
    }
}

impl Drop for PipeClient {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            // SAFETY: 句柄由本对象独占持有；关闭失败无处理路径，忽略。
            let _ = unsafe { CloseHandle(self.handle) };
        }
        #[cfg(not(windows))]
        {
            let _ = ();
        }
    }
}

/// 管道服务端（守护进程侧）：阻塞接受一条连接 → 处理一请求 → 断开。
/// 非 Windows = 桩。
pub struct PipeServer {
    #[cfg(windows)]
    handle: HANDLE,
    #[cfg(not(windows))]
    _stub: (),
}

impl PipeServer {
    /// 创建管道并阻塞等待会话进程连接。返回连接就绪的实例。
    /// 无客户端时永久阻塞（守护进程专用线程；进程退出时随线程终止）。
    pub fn accept() -> io::Result<PipeServer> {
        #[cfg(windows)]
        {
            let name = imp::pipe_name_wide();
            // SAFETY: 消息模式 + 阻塞；缓冲 PIPE_FRAME_MAX 内单帧。返回 HANDLE（非 Result）。
            let handle = unsafe {
                CreateNamedPipeW(
                    PCWSTR(name.as_ptr()),
                    PIPE_ACCESS_DUPLEX,
                    PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT,
                    PIPE_UNLIMITED_INSTANCES,
                    PIPE_FRAME_MAX as u32,
                    PIPE_FRAME_MAX as u32,
                    0,
                    None,
                )
            };
            if handle.is_invalid() {
                let e = unsafe { windows::Win32::Foundation::GetLastError() };
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("创建命名管道失败: {}", e.0),
                ));
            }
            // SAFETY: 阻塞等待客户端 ConnectNamedPipe（非重叠）。
            let r = unsafe { ConnectNamedPipe(handle, None) };
            if let Err(_e) = r {
                let code = unsafe { windows::Win32::Foundation::GetLastError() };
                if code != ERROR_PIPE_CONNECTED {
                    // SAFETY: 等待失败，关闭管道句柄。
                    let _ = unsafe { CloseHandle(handle) };
                    return Err(io::Error::new(
                        io::ErrorKind::Other,
                        format!("等待客户端连接失败: {}", code.0),
                    ));
                }
            }
            Ok(PipeServer { handle })
        }
        #[cfg(not(windows))]
        {
            Err(err_unsupported())
        }
    }

    /// 读一请求 → `handler` 求响应 → 写回。任何一步失败返回 `Err`（调用方记日志继续）。
    pub fn serve(&self, handler: impl FnOnce(&Request) -> Response) -> io::Result<()> {
        #[cfg(windows)]
        {
            let payload = imp::read_frame(self.handle)?;
            let req = decode_request(&payload)?;
            let resp = handler(&req);
            imp::write_frame(self.handle, &encode_response(&resp))
        }
        #[cfg(not(windows))]
        {
            let _ = handler;
            Err(err_unsupported())
        }
    }
}

impl Drop for PipeServer {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            // SAFETY: 断开 + 关闭句柄；失败无处理路径，忽略。
            let _ = unsafe { DisconnectNamedPipe(self.handle) };
            let _ = unsafe { CloseHandle(self.handle) };
        }
        #[cfg(not(windows))]
        {
            let _ = ();
        }
    }
}

/// 反向控制管道服务端（TSF 每实例一个 accept 线程，§4.2）：阻塞等待 daemon 连接 →
/// 处理一条 Cmd → 回 CtlResult → 断开。句柄经 `handle()` 暴露供调用方跨线程 `Close` 中断等待。
/// 非 Windows = 桩。
pub struct CtlServer {
    #[cfg(windows)]
    pub(crate) handle: HANDLE,
    #[cfg(not(windows))]
    _stub: (),
}

impl CtlServer {
    /// 创建管道实例（**不连接**）：`connect` 才阻塞等待。句柄可由其他线程
    /// `CloseHandle` 中断 `connect`（TSF Deactivate 停 accept 线程用）。
    pub fn create(name: &str) -> io::Result<CtlServer> {
        #[cfg(windows)]
        {
            let wide = imp::name_wide(name);
            let handle = imp::create_server(&wide)?;
            Ok(CtlServer { handle })
        }
        #[cfg(not(windows))]
        {
            let _ = name;
            Err(err_unsupported())
        }
    }

    /// 阻塞等待 daemon 连接。跨线程关闭句柄（`interrupt`）→ `Err`（取消路径）。
    pub fn connect(&self) -> io::Result<()> {
        #[cfg(windows)]
        {
            imp::connect_server(self.handle)
        }
        #[cfg(not(windows))]
        {
            Err(err_unsupported())
        }
    }

    /// 读一 Cmd → `handler` 求结果 → 写回。任何一步失败返回 `Err`。
    pub fn serve(&self, handler: impl FnOnce(&CtlCmd) -> CtlResult) -> io::Result<()> {
        #[cfg(windows)]
        {
            let payload = imp::read_frame(self.handle)?;
            let cmd = decode_ctl_cmd(&payload)?;
            let result = handler(&cmd);
            imp::write_frame(self.handle, &encode_ctl_result(&result))
        }
        #[cfg(not(windows))]
        {
            let _ = handler;
            Err(err_unsupported())
        }
    }

    /// 服务端句柄（跨进程/线程共享：调用方可持有后从其他线程 `CloseHandle` 中断阻塞等待）。
    pub fn handle(&self) -> HANDLE {
        #[cfg(windows)]
        {
            self.handle
        }
        #[cfg(not(windows))]
        {
            HANDLE::default()
        }
    }

    /// 中断阻塞中的 `connect`（从其他线程调用；幂等，关闭后句柄无效）。
    pub fn interrupt(&self) {
        #[cfg(windows)]
        {
            // SAFETY: 关闭句柄中断 ConnectNamedPipe 等待；句柄值的重复关闭由 Drop 忽略错误。
            let _ = unsafe { CloseHandle(self.handle) };
        }
        #[cfg(not(windows))]
        {
            let _ = ();
        }
    }
}

impl Drop for CtlServer {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            // SAFETY: 断开 + 关闭句柄；重复关闭（interrupt 已关）返回错误，忽略。
            let _ = unsafe { DisconnectNamedPipe(self.handle) };
            let _ = unsafe { CloseHandle(self.handle) };
        }
        #[cfg(not(windows))]
        {
            let _ = ();
        }
    }
}

/// 反向控制管道客户端（daemon 侧，§4.2 按需连接）：连入 `\\.\pipe\iuv-ctl-<pid>-<tid>` →
/// 发一帧 Cmd → 收一帧 CtlResult → 断开。非 Windows = 桩。
pub struct CtlClient {
    #[cfg(windows)]
    handle: HANDLE,
    #[cfg(not(windows))]
    _stub: (),
}

impl CtlClient {
    /// 连接指定实例控制管道。实例不在线（未启动 accept / 已死）→ `Err`（daemon 干净退出）。
    pub fn connect(name: &str) -> io::Result<CtlClient> {
        #[cfg(windows)]
        {
            let wide = imp::name_wide(name);
            let handle = imp::connect_client(&wide)?;
            Ok(CtlClient { handle })
        }
        #[cfg(not(windows))]
        {
            let _ = name;
            Err(err_unsupported())
        }
    }

    /// 发命令 → 收结果（单次会话；成功后调用方断开连接，贴合按需连接风格）。
    pub fn request(&self, cmd: &CtlCmd) -> io::Result<CtlResult> {
        #[cfg(windows)]
        {
            let _ = imp::write_frame(self.handle, &encode_ctl_cmd(cmd))?;
            let resp_payload = imp::read_frame(self.handle)?;
            decode_ctl_result(&resp_payload)
        }
        #[cfg(not(windows))]
        {
            let _ = cmd;
            Err(err_unsupported())
        }
    }
}

impl Drop for CtlClient {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            // SAFETY: 句柄由本对象独占持有；关闭失败无处理路径，忽略。
            let _ = unsafe { CloseHandle(self.handle) };
        }
        #[cfg(not(windows))]
        {
            let _ = ();
        }
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
        let state = ToolbarState::new(1, 0, 1, 1);
        for req in [
            Request::Register {
                pid: 1234,
                tid: 56,
                state,
            },
            Request::StateSync {
                pid: 1234,
                tid: 56,
                state: ToolbarState::new(0, 1, 0, 0),
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
    fn ctl_cmd_roundtrip() {
        for cmd in [
            CtlCmd::SetState {
                field: CTL_FIELD_MODE,
                value: 1,
            },
            CtlCmd::SetState {
                field: CTL_FIELD_SCRIPT,
                value: 0,
            },
        ] {
            let bytes = encode_ctl_cmd(&cmd);
            assert_eq!(decode_ctl_cmd(&bytes).unwrap(), cmd);
            // 帧前缀往返
            let frame = to_frame(&bytes);
            assert_eq!(decode_ctl_cmd(parse_frame(&frame).unwrap()).unwrap(), cmd);
        }
        assert!(decode_ctl_cmd(&[0x01]).is_err(), "SetState 截断");
        assert!(decode_ctl_cmd(&[0xFF]).is_err(), "未知 Cmd tag");
        let mut bytes = encode_ctl_cmd(&CtlCmd::SetState {
            field: 1,
            value: 0,
        });
        bytes.push(0xAA);
        assert!(decode_ctl_cmd(&bytes).is_err(), "残留字节");
    }

    #[test]
    fn ctl_result_roundtrip() {
        let ok = CtlResult::Ok {
            state: ToolbarState::new(0, 1, 1, 0),
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

    #[test]
    fn toolbar_state_field_accessor() {
        let s = ToolbarState::new(1, 0, 1, 0);
        assert_eq!(s.field(CTL_FIELD_MODE), 1);
        assert_eq!(s.field(CTL_FIELD_WIDTH), 0);
        assert_eq!(s.field(CTL_FIELD_SCRIPT), 1);
        assert_eq!(s.field(CTL_FIELD_PUNCT), 0);
        assert_eq!(s.field(0xFF), 0, "非法字段 → 0");
        assert_eq!(
            ctl_pipe_name(1234, 56),
            r"\\.\pipe\iuv-ctl-1234-56"
        );
    }
}
