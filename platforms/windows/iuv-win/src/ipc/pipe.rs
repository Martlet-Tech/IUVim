//! 命名管道底层原语 `imp`（49 号 ②/③ 迁移后唯一消费者 = `rtt.rs` 基准模块）。
//!
//! 旧数据面 PipeClient/PipeServer（`\\.\pipe\iuv-userdict`，用户库写请求）与
//! `ctl.rs` 反向控制通道已随 ②/③ 迁移退役——数据面走 transport `C2S::UserMutation`，
//! 反向控制走 transport `S2C::Ctl`。此处保留同步消息模式管道的读写原语，
//! 供 `rtt_bench` 做 per-request-connect vs persistent 双形态延迟对比（49 号 P0）。
//!
//! ## 帧格式（前缀长度 + 二进制载荷）
//!
//! ```text
//! [0..4]  u32 msg_len（LE）= 载荷字节数（不含本前缀）
//! [4..]   载荷（echo 场景为任意字节；见 `codec.rs`）
//! ```
//!
//! 管道为**消息模式**（PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE）：一次 WriteFile 写
//! 一帧、一次 ReadFile 读一帧（缓冲足够时整帧返回）——配合前缀长度做校验。

use std::io;

use windows::Win32::Foundation::{
    ERROR_PIPE_BUSY, ERROR_PIPE_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, ReadFile, WriteFile, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, WaitNamedPipeW, PIPE_READMODE_MESSAGE, PIPE_TYPE_MESSAGE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_core::PCWSTR;

use super::codec::{bad, parse_frame, to_frame};

/// 单帧最大字节数（消息模式 ReadFile 缓冲；rtt echo 帧很小，64KB 充裕）。
pub(super) const PIPE_FRAME_MAX: usize = 64 * 1024;
/// 连接超时（WaitNamedPipeW，毫秒；超时视为服务端不在线）。
pub(super) const PIPE_CONNECT_TIMEOUT_MS: u32 = 1500;

/// 命名管道底层原语（同步消息模式；rtt 基准双形态共用）。
pub(super) mod imp {
    use super::*;
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

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
            return Err(io::Error::other(format!("创建命名管道失败: {}", e.0)));
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
                return Err(io::Error::other(format!("等待客户端连接失败: {}", code.0)));
            }
        }
        Ok(())
    }

    /// 客户端连接任意命名管道。服务端不在线（文件未找到 / 忙超时）→ `Err`。
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
                // SAFETY: name 以 NUL 结尾；等待超时视为服务端不在线。
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
            .map_err(|e| io::Error::other(format!("读管道失败: {}", e.code())))?;
        buf.truncate(read as usize);
        let payload = parse_frame(&buf)?;
        Ok(payload.to_vec())
    }

    pub fn write_frame(handle: HANDLE, payload: &[u8]) -> io::Result<()> {
        let frame = to_frame(payload);
        let mut written: u32 = 0;
        // SAFETY: frame 只读；written 输出实际字节数；同步。
        unsafe { WriteFile(handle, Some(&frame), Some(&mut written), None) }
            .map_err(|e| io::Error::other(format!("写管道失败: {}", e.code())))?;
        if written as usize != frame.len() {
            return Err(bad(&format!(
                "写管道字节数不符 {written} != {}",
                frame.len()
            )));
        }
        Ok(())
    }
}
