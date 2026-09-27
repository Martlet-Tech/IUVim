//! M10 transport（49 §4.1/§4.2 P2）：`iuv.service.v1` 长连接传输层。
//!
//! ## 实现决策（对 49 §4.1「async select!」的实现层修订）
//!
//! 用 **std 线程 + overlapped IO**，不引 async 运行时：
//! - transport 位于 `iuv-win`，会被 TSF DLL 链接进**每个宿主应用进程**——tokio 级
//!   依赖树与「TSF DLL 轻到可忽略」（49 §1.3）冲突；
//! - 输入法场景连接数 = TSF 实例数（个位数~几十），长连接线程成本可忽略；
//! - 49 §4.9 真正否决的是「单线程 accept→serve→断开」串行化与全局互斥（weasel 病灶），
//!   不是长连接线程本身；三平面语义（§4.1）由连接内帧循环提供，与线程模型正交；
//! - overlapped IO 提供**型别化截止时间**（49 §4.7#10）：读写都可限时、可取消。
//!
//! ## 分帧
//!
//! 管道为 **BYTE 模式**（49 §4.2「显式分帧，不依赖管道消息模式」）：iuv-proto 帧头自带
//! payload_len，读侧 `read_exact` 拼帧，字节流的任意切分都不破坏帧边界。
//!
//! ## 线程模型
//!
//! - 服务端：accept 线程（overlapped `ConnectNamedPipe`，stop 可取消）+ **每连接一线程**
//!   （长连接，连接内串行处理请求；连接间天然并行，无全局互斥）；
//! - 客户端：读线程（分发 RESP/PUSH）+ 写互斥（overlapped 同句柄并发读写安全，
//!   写侧加锁仅防帧交错）。

mod auth_file;
mod client;
mod server;

pub use auth_file::load_or_create_token;
pub use client::{connect, ClientConfig, HelloAck, PushStream, TransportClient};
pub use server::{ConnHandler, ConnSender, Reply, ServerConfig, Session, TransportServer};

use std::io;

use windows::Win32::Foundation::{CloseHandle, ERROR_IO_PENDING, HANDLE};
use windows::Win32::Foundation::{WAIT_EVENT, WAIT_OBJECT_0};
use windows::Win32::Storage::FileSystem::{ReadFile, WriteFile};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows_core::PCWSTR;

use iuv_proto::{decode_payload, encode_frame, FrameHeader, Payload, ProtoError};

/// 服务端连接写者计数 RAII（conn 线程退出前等归零，防句柄值复用错写）。
pub(crate) struct WriteGuard<'a> {
    counter: &'a std::sync::atomic::AtomicUsize,
}
impl<'a> WriteGuard<'a> {
    pub(crate) fn new(counter: &'a std::sync::atomic::AtomicUsize) -> Self {
        counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        WriteGuard { counter }
    }
}
impl Drop for WriteGuard<'_> {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
}

/// 服务管道名（49 §2）。
pub const SERVICE_PIPE_NAME: &str = r"\\.\pipe\iuv.service.v1";
/// 握手截止（毫秒）：覆盖连接建立后 Hello 往返 + 服务端会话装配。
pub const HANDSHAKE_TIMEOUT_MS: u32 = 5000;
/// 服务端写帧截止（毫秒）：客户端卡死不拖住会话线程。
pub const WRITE_TIMEOUT_MS: u32 = 5000;
/// 客户端连接重试上界（毫秒）：服务端两次 accept 间隙的 NOT_FOUND 不算失败。
pub const CONNECT_RETRY_MS: u64 = 2000;

/// 句柄属主：`own` = drop 时关闭（连接唯一属主）；`reference` = clone 出来的
/// 共享视图（连接生命周期由原属主管理）。
pub(crate) struct HandleGuard {
    h: HANDLE,
    owns: bool,
}

impl HandleGuard {
    pub(crate) fn own(h: HANDLE) -> HandleGuard {
        HandleGuard { h, owns: true }
    }
}

impl Drop for HandleGuard {
    fn drop(&mut self) {
        if self.owns {
            // SAFETY: 唯一属主关闭句柄；失败无处理路径，忽略。
            unsafe {
                let _ = CloseHandle(self.h);
            }
        }
    }
}

/// 跨线程句柄：HANDLE 本体 `!Send/!Sync`，但其值就是整数；所有权/生命周期由
/// [`HandleGuard`] 管理，本类型只负责把值搬进线程。
#[derive(Clone, Copy)]
pub(crate) struct SendHandle(isize);

impl SendHandle {
    pub(crate) fn new(h: HANDLE) -> SendHandle {
        SendHandle(h.0 as isize)
    }

    pub(crate) fn get(&self) -> HANDLE {
        HANDLE(self.0 as *mut _)
    }
}

// SAFETY: 句柄值整数搬运；生命周期由 HandleGuard 保证，内核对象本身线程无关。
unsafe impl Send for SendHandle {}
unsafe impl Sync for SendHandle {}

/// 传输层错误。握手拒绝（版本/认证）以 [`TransportError::Proto`] 透出——
/// 调用方按 49 §4.4 降级为完全透明。
#[derive(Debug)]
pub enum TransportError {
    Io(io::Error),
    /// 协议级错误（含握手拒绝）。
    Proto(ProtoError),
    /// 对端断开/连接已失效。
    Closed,
    /// 截止时间已过（49 §4.7#10 型别化截止时间）。
    Deadline,
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportError::Io(e) => write!(f, "IO 错误: {e}"),
            TransportError::Proto(e) => write!(f, "协议错误: {e:?}"),
            TransportError::Closed => write!(f, "连接已断开"),
            TransportError::Deadline => write!(f, "截止时间已过"),
        }
    }
}

impl std::error::Error for TransportError {}

impl From<io::Error> for TransportError {
    fn from(e: io::Error) -> Self {
        TransportError::Io(e)
    }
}

impl From<ProtoError> for TransportError {
    fn from(e: ProtoError) -> Self {
        TransportError::Proto(e)
    }
}

/// 读端读到 0 字节 = 对端正常关闭（BYTE 模式管道语义）。
fn is_pipe_broken(e: &windows_core::Error) -> bool {
    let code = e.code().0 as u32;
    // ERROR_BROKEN_PIPE(0x6D) / ERROR_PIPE_NOT_CONNECTED(0xE9) / ERROR_NO_DATA(0xE8)
    matches!(code & 0xFFFF, 0x6D | 0xE8 | 0xE9)
}

/// 一次 overlapped IO 的宿主：OVERLAPPED 与其事件同生共死
/// （IO 期间 OVERLAPPED 必须存活，49 §4.7#9 RAII 纪律）。
pub(crate) struct OverlapHost {
    ev: HANDLE,
    pub(crate) ov: OVERLAPPED,
}

impl OverlapHost {
    fn new() -> io::Result<OverlapHost> {
        // SAFETY: 无安全属性/匿名事件；返回 HANDLE。
        let ev = unsafe { CreateEventW(None, false, false, PCWSTR::null()) }
            .map_err(|e| io::Error::other(format!("CreateEventW 失败: {e}")))?;
        let ov = OVERLAPPED {
            hEvent: ev,
            ..Default::default()
        };
        Ok(OverlapHost { ev, ov })
    }
}

impl Drop for OverlapHost {
    fn drop(&mut self) {
        // SAFETY: 事件句柄由本对象独占。
        unsafe {
            let _ = CloseHandle(self.ev);
        }
    }
}

/// 等待一次挂起的 overlapped IO 完成/超时。超时即取消 IO（不留悬挂写）。
/// 返回传输的字节数；`already` = IO 已同步完成（跳过等待）。
fn wait_io(
    h: HANDLE,
    host: &OverlapHost,
    already: bool,
    timeout_ms: u32,
) -> Result<u32, TransportError> {
    if !already {
        let w: WAIT_EVENT = unsafe { WaitForSingleObject(host.ev, timeout_ms) };
        if w != WAIT_OBJECT_0 {
            // 超时：取消在途 IO。但 IO 可能恰在超时瞬间完成——CancelIoEx 返回
            // ERROR_NOT_FOUND（无在途可取消）时必须收取已完成字节数，否则丢数据
            //（读线程的有界超时读会周期性踩此窗口）。
            let cancelled = unsafe { CancelIoEx(h, Some(&host.ov)) };
            let mut n = 0u32;
            // SAFETY: OVERLAPPED 属本调用持有；bWait=true 等取消/完成落定。
            let r = unsafe { GetOverlappedResult(h, &host.ov, &mut n, true) };
            if cancelled.is_err() && r.is_ok() && n > 0 {
                return Ok(n);
            }
            return Err(TransportError::Deadline);
        }
    }
    let mut n = 0u32;
    // SAFETY: OVERLAPPED 属本调用持有；bWait=false（事件已置位或 IO 已同步完成）。
    let r = unsafe { GetOverlappedResult(h, &host.ov, &mut n, false) };
    if let Err(e) = r {
        if is_pipe_broken(&e) {
            return Err(TransportError::Closed);
        }
        return Err(TransportError::Io(io::Error::other(format!(
            "GetOverlappedResult 失败: {e}"
        ))));
    }
    Ok(n)
}

/// 读满 `buf`（BYTE 模式单次读可能不满，循环拼齐）。
pub(crate) fn read_exact_ov(
    h: HANDLE,
    buf: &mut [u8],
    timeout_ms: u32,
) -> Result<(), TransportError> {
    let mut filled = 0;
    while filled < buf.len() {
        let mut host = OverlapHost::new()?;
        // SAFETY: buf 可写；overlapped 读（lpNumberOfBytesRead 必须为 None）。
        let r = unsafe { ReadFile(h, Some(&mut buf[filled..]), None, Some(&mut host.ov)) };
        let already = match r {
            Ok(()) => true,
            Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => false,
            Err(e) => return Err(TransportError::Io(io::Error::other(e.to_string()))),
        };
        let n = wait_io(h, &host, already, timeout_ms)?;
        if n == 0 {
            return Err(TransportError::Closed);
        }
        filled += n as usize;
    }
    Ok(())
}

/// 写满一帧（BYTE 模式单次写可能部分完成，循环写齐）。
pub(crate) fn write_all_ov(
    h: HANDLE,
    mut data: &[u8],
    timeout_ms: u32,
) -> Result<(), TransportError> {
    while !data.is_empty() {
        let mut host = OverlapHost::new()?;
        // SAFETY: data 只读；overlapped 写。
        let r = unsafe { WriteFile(h, Some(data), None, Some(&mut host.ov)) };
        let already = match r {
            Ok(()) => true,
            Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => false,
            Err(e) => return Err(TransportError::Io(io::Error::other(e.to_string()))),
        };
        let n = wait_io(h, &host, already, timeout_ms)?;
        if n == 0 {
            return Err(TransportError::Closed);
        }
        data = &data[n as usize..];
    }
    Ok(())
}

/// 读一帧（头 + 载荷）。`timeout_ms` 作用于每次底层读。
pub(crate) fn read_frame_ov(
    h: HANDLE,
    timeout_ms: u32,
) -> Result<(FrameHeader, Payload), TransportError> {
    let mut head = [0u8; iuv_proto::HEADER_LEN];
    read_exact_ov(h, &mut head, timeout_ms)?;
    let header = FrameHeader::decode(&head)?;
    let mut body = vec![0u8; header.payload_len as usize];
    read_exact_ov(h, &mut body, timeout_ms)?;
    let payload = decode_payload(header.kind, &body)?;
    Ok((header, payload))
}

/// 写一帧。`stream_id`/`urgent` 写进帧头；kind 由载荷变体推导。
pub(crate) fn write_frame_ov(
    h: HANDLE,
    stream_id: u16,
    urgent: bool,
    payload: &Payload,
    timeout_ms: u32,
) -> Result<(), TransportError> {
    let frame = encode_frame(stream_id, urgent, payload)?;
    write_all_ov(h, &frame, timeout_ms)
}
