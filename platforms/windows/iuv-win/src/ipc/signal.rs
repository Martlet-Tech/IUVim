//! 工具条信号通道（40-toolbar-show-hide-governance.md 纯信号模型）：专用命名管道，
//! 与数据面（`pipe.rs` 用户库管道）物理隔离——控制面零争用、消息不丢。
//!
//! 协议：客户端连接一次终身复用（每 TSF 实例一条），循环写 `ToolbarSignal` 帧；
//! 服务端 thread-per-connection（daemon 侧每连接一线程），循环读帧直至对端断开。
//! 无应答帧（fire-and-forget over 可靠长连接；断线由下次写入的 Err 自然暴露）。
//!
//! 帧格式复用 `codec::to_frame/parse_frame`（u32 LE 长度前缀 + 载荷）。

use std::io;

use windows::core::PCWSTR;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE,
};
use windows::Win32::Storage::FileSystem::{CreateFileW, PIPE_ACCESS_DUPLEX, OPEN_EXISTING};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, WaitNamedPipeW, PIPE_READMODE_MESSAGE, PIPE_TYPE_MESSAGE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};

use super::codec::{decode_signal, encode_signal};
use super::msg::ToolbarSignal;
use super::pipe::{imp, PIPE_CONNECT_TIMEOUT_MS, PIPE_FRAME_MAX};

/// 信号管道名（与数据面 `iuv-userdict` 并列；单用户桌面足够）。
const SIGNAL_PIPE_NAME: &str = r"\\.\pipe\iuv-toolbar-signal";

/// 信号管道服务端连接（守护进程侧；一条客户端连接 = 一条长会话）。
pub struct SignalServer {
    handle: HANDLE,
}

impl SignalServer {
    /// 创建信号管道实例并阻塞等待一个客户端连接。
    /// 调用方 accept 循环每轮新建实例 → 连接后交独立线程 `recv_loop`，实现并发多连接。
    pub fn accept() -> io::Result<SignalServer> {
        let name = imp::name_wide(SIGNAL_PIPE_NAME);
        // SAFETY: 消息模式 + 阻塞；缓冲 64KB 内单帧。返回 HANDLE（非 Result）。
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
                format!("创建信号管道失败: {}", e.0),
            ));
        }
        // SAFETY: 阻塞等待客户端 ConnectNamedPipe（非重叠）。
        let r = unsafe { ConnectNamedPipe(handle, None) };
        if let Err(_e) = r {
            let code = unsafe { windows::Win32::Foundation::GetLastError() };
            if code != ERROR_PIPE_CONNECTED {
                // SAFETY: 等待失败，关闭本实例句柄后返回错误（accept 循环重试）。
                let _ = unsafe { CloseHandle(handle) };
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    format!("等待信号客户端连接失败: {}", code.0),
                ));
            }
        }
        Ok(SignalServer { handle })
    }

    /// 阻塞读下一条信号。对端断开 / 解码失败 → `Err`（调用方结束本连接线程）。
    pub fn recv(&self) -> io::Result<ToolbarSignal> {
        let payload = imp::read_frame(self.handle)?;
        decode_signal(&payload)
    }
}

impl Drop for SignalServer {
    fn drop(&mut self) {
        // SAFETY: 句柄由本对象独占持有；关闭失败无处理路径，忽略。
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

// SAFETY: HANDLE 为内核句柄值（线程无关）。SignalServer 在 accept 线程创建、
// 整体所有权移入连接线程独占使用至 Drop——访问严格串行，CloseHandle 跨线程安全。
unsafe impl Send for SignalServer {}

/// 信号管道客户端（会话进程侧）：连接一次、复用发多条。
pub struct SignalClient {
    handle: HANDLE,
}

impl SignalClient {
    /// 连接信号管道（daemon 不在线 / 忙超时 → `Err`，调用方记日志后放弃本次发送；
    /// 下次发送前重新 connect——持久连接的自然生命周期，非兜底机制）。
    pub fn connect() -> io::Result<SignalClient> {
        let name = imp::name_wide(SIGNAL_PIPE_NAME);
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
                return Ok(SignalClient { handle });
            }
            let e = unsafe { windows::Win32::Foundation::GetLastError() };
            if e == ERROR_PIPE_BUSY {
                // SAFETY: name 以 NUL 结尾；等待超时视为 daemon 不在线。
                let ok =
                    unsafe { WaitNamedPipeW(PCWSTR(name.as_ptr()), PIPE_CONNECT_TIMEOUT_MS) };
                if !ok.as_bool() {
                    return Err(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "信号管道忙且超时（daemon 不在线）",
                    ));
                }
                continue; // 管道可用了，重试 CreateFileW
            }
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("信号管道不可达: {}", e.0),
            ));
        }
    }

    /// 发送一条信号（无应答帧）。失败 → `Err`（调用方弃缓存连接，下次发送重连）。
    pub fn send(&self, sig: &ToolbarSignal) -> io::Result<()> {
        imp::write_frame(self.handle, &encode_signal(sig))
    }
}

impl Drop for SignalClient {
    fn drop(&mut self) {
        // SAFETY: 句柄由本对象独占持有；关闭失败无处理路径，忽略。
        let _ = unsafe { CloseHandle(self.handle) };
    }
}

// SAFETY: 同 SignalServer——内核句柄值线程无关；缓存于 DaemonClient::signal
// （Mutex 内串行访问），所有权整体移动无并发使用。
unsafe impl Send for SignalClient {}
