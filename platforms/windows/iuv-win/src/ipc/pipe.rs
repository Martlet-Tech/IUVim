//! 命名管道传输（P3.2 自 iuv-data/ipc.rs 移入 iuv-win）：PipeClient（会话进程侧）/
//! PipeServer（守护进程侧）+ 底层 `imp`（PipeClient/PipeServer 与 `ctl.rs` 共用）。
//!
//! ## 管道名
//!
//! `\\.\pipe\iuv-userdict`（单用户桌面足够，不做用户 SID 隔离——多用户各自有会话，
//! `Local\` 域天然隔离；文档注明）。
//!
//! ## 帧格式（前缀长度 + 二进制载荷）
//!
//! ```text
//! [0..4]  u32 msg_len（LE）= 载荷字节数（不含本前缀）
//! [4..]   载荷 = 序列化 Request / Response（见 `codec.rs` 编码表）
//! ```
//!
//! 管道为**消息模式**（PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE）：一次 WriteFile 写
//! 一帧、一次 ReadFile 读一帧（缓冲足够时整帧返回）——配合前缀长度做校验。
//!
//! ## 守护进程不在线判定（会话进程客户端）
//!
//! - 连接：`CreateFileW` 失败 ERROR_FILE_NOT_FOUND / `WaitNamedPipeW` 超时 → daemon 不在线 →
//!   降级自读文件；
//! - 连接成功但 `Ping` 超时/读失败 → 同样降级；
//! - 请求后 Response::Err → 本会话内降级（调权/隐藏立即失效但保留内存态），绝不 panic。

use std::io;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile, OPEN_EXISTING,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, WaitNamedPipeW,
    PIPE_READMODE_MESSAGE, PIPE_TYPE_MESSAGE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_core::PCWSTR;

use super::codec::{
    bad, decode_request, decode_response, encode_request, encode_response, parse_frame, to_frame,
};
use super::msg::{Request, Response};

/// 管道名（单用户桌面足够；多用户 SID 隔离在 M7 安装器范畴）。
const PIPE_NAME: &str = r"\\.\pipe\iuv-userdict";
/// 单帧最大字节数（消息模式 ReadFile 缓冲；用户库条目小，64KB 充裕）。
const PIPE_FRAME_MAX: usize = 64 * 1024;
/// 连接超时（WaitNamedPipeW，毫秒；超时视为 daemon 不在线）。
const PIPE_CONNECT_TIMEOUT_MS: u32 = 1500;

/// 命名管道底层原语（PipeClient/PipeServer 与 `ctl.rs` 的 CtlServer/CtlClient 共用）。
pub(super) mod imp {
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
                let ok =
                    unsafe { WaitNamedPipeW(PCWSTR(name_wide.as_ptr()), PIPE_CONNECT_TIMEOUT_MS) };
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
pub struct PipeClient {
    handle: HANDLE,
}

impl PipeClient {
    /// 连接守护进程管道。daemon 不在线（文件未找到 / 忙超时）→ `Err`，调用方降级。
    pub fn connect() -> io::Result<PipeClient> {
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
                let ok =
                    unsafe { WaitNamedPipeW(PCWSTR(name.as_ptr()), PIPE_CONNECT_TIMEOUT_MS) };
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

    /// 发请求 → 收响应（单次会话）。
    pub fn request(&self, req: &Request) -> io::Result<Response> {
        let _ = imp::write_frame(self.handle, &encode_request(req))?;
        let resp_payload = imp::read_frame(self.handle)?;
        decode_response(&resp_payload)
    }
}

impl Drop for PipeClient {
    fn drop(&mut self) {
        // SAFETY: 句柄由本对象独占持有；关闭失败无处理路径，忽略。
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

/// 管道服务端（守护进程侧）：阻塞接受一条连接 → 处理一请求 → 断开。
pub struct PipeServer {
    handle: HANDLE,
}

impl PipeServer {
    /// 创建管道并阻塞等待会话进程连接。返回连接就绪的实例。
    /// 无客户端时永久阻塞（守护进程专用线程；进程退出时随线程终止）。
    pub fn accept() -> io::Result<PipeServer> {
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

    /// 读一请求 → `handler` 求响应 → 写回。任何一步失败返回 `Err`（调用方记日志继续）。
    pub fn serve(&self, handler: impl FnOnce(&Request) -> Response) -> io::Result<()> {
        let payload = imp::read_frame(self.handle)?;
        let req = decode_request(&payload)?;
        let resp = handler(&req);
        imp::write_frame(self.handle, &encode_response(&resp))
    }
}

impl Drop for PipeServer {
    fn drop(&mut self) {
        // SAFETY: 断开 + 关闭句柄；失败无处理路径，忽略。
        let _ = unsafe { DisconnectNamedPipe(self.handle) };
        let _ = unsafe { CloseHandle(self.handle) };
    }
}