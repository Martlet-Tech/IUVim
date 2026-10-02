//! 传输客户端（会话进程/TSF 侧）：一条长连接，读线程分发应答与推送。
//!
//! 热路径用法（49 §4.5）：`request(Key{..}, urgent=true, deadline)` 同步等待应答；
//! 超时 → [`TransportError::Deadline`]，调用方按 §4.5.2 放行按键并标记 degraded。
//! PUSH（四态/配置/会话令牌）经 [`PushStream`] 异步消费。

use std::collections::HashMap;
use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{ERROR_PIPE_BUSY, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
};
use windows::Win32::System::Pipes::WaitNamedPipeW;
use windows_core::PCWSTR;

use iuv_proto::{
    Auth, BuildId, Caps, ClientInfo, ProtoError, Push, ResumeToken, StreamIdAlloc, C2S, S2C,
};

use std::sync::atomic::AtomicUsize;

use super::{
    read_frame_ov, write_frame_ov, SendHandle, TransportError, CONNECT_RETRY_MS, WRITE_TIMEOUT_MS,
};

/// 服务端请求处理器：把 `S2C::Ctl` 等服务端主动请求转换为客户端应答
/// （`C2S::CtlResult` 等）。**阻塞执行**（内部自行跨线程编排 + 限时），由
/// 独立线程调用，不阻塞读线程。None = 服务端请求被丢弃（服务端等待超时）。
pub type ServerReqHandler = Arc<dyn Fn(S2C) -> C2S + Send + Sync>;

/// 客户端连接配置。
pub struct ClientConfig {
    pub pipe_name: String,
    /// 协议版本区间（生产侧填 `PROTO_MIN..=PROTO_MAX`；测试可传越界值验证协商报错）。
    pub proto_min: u16,
    pub proto_max: u16,
    pub auth: Auth,
    /// 请求的能力位（服务端取交集后回 `HelloAck.caps`，未协商的 PUSH 不发）。
    pub caps: Caps,
    /// 宿主进程名（握手报备，服务端日志/看板用）。
    pub app: String,
    /// 会话重绑令牌（§4.5.4 方案 C；P2 阶段服务端尚不处理重绑）。
    pub resume: Option<ResumeToken>,
    pub handshake_timeout: Duration,
    /// 服务端主动请求处理（49 §4.1 控制面：工具栏 Ctl → 客户端应用 → 回结果）。
    pub on_server_req: Option<ServerReqHandler>,
}

/// 握手成功后的服务端应答摘要（`S2C::HelloAck` 的字段提升，调用方免解枚举）。
pub struct HelloAck {
    pub proto: u16,
    pub caps: Caps,
    pub build: BuildId,
}

/// 共享连接状态（客户端句柄 + 读线程与请求方之间的事件通道）。
struct Shared {
    h: SendHandle,
    /// 在途请求：stream_id → 应答通道（客户端偶数号，服务端奇数号，49 §4.2）。
    inflight: Mutex<HashMap<u16, mpsc::Sender<S2C>>>,
    ids: Mutex<StreamIdAlloc>,
    push_tx: mpsc::Sender<Push>,
    closed: AtomicBool,
    /// 读线程句柄（最后一个 client drop 时先 join 再关句柄——防句柄值复用后
    /// 读线程读到新连接的数据，2026-09-27 P5 重连实测的帧错乱根因）。
    reader: Mutex<Option<std::thread::JoinHandle<()>>>,
    /// 存活的 TransportClient 数（最后一个 drop 关连接）。
    clients: AtomicUsize,
    /// 写互斥：overlapped 同句柄并发读写安全，但两次写不得交错（防帧字节交错）。
    write_lock: Mutex<()>,
    /// 服务端请求处理器（None = 丢弃服务端请求）。
    server_req_handler: Option<ServerReqHandler>,
}

/// 传输客户端。`Clone` 共享同一条连接；**最后一个 clone drop 时关闭连接**
/// （句柄收尾在此，读线程随后因管道断开自然退出）。
pub struct TransportClient {
    shared: Arc<Shared>,
}

impl Clone for TransportClient {
    fn clone(&self) -> Self {
        self.shared.clients.fetch_add(1, Ordering::SeqCst);
        TransportClient {
            shared: self.shared.clone(),
        }
    }
}

impl Drop for TransportClient {
    fn drop(&mut self) {
        if self.shared.clients.fetch_sub(1, Ordering::SeqCst) == 1 {
            // 收尾协议（2026-09-27 修订）：**句柄由读线程负责关闭**——它退出后
            // 不再有 ReadFile，句柄值被新连接 CreateFileW 复用也绝不会命中旧读
            // 线程（否则两读线程瓜分新连接字节流 → 帧错乱；P5 断开即重连实测）。
            // Drop 只置关闭标志 + 尽力唤醒挂起读 + 有界 join（读线程以
            // READER_TICK_MS 周期醒来检查 closed，最坏一个 tick 内退出并关句柄）。
            self.shared.closed.store(true, Ordering::SeqCst);
            let h = self.shared.h.get();
            // SAFETY: 尽力取消挂起读以加速收尾；若读线程恰在两次读之间，
            // 下一轮读会在关闭标志检查/管道断开处退出，句柄由读线程关闭。
            unsafe {
                let _ = windows::Win32::System::IO::CancelIoEx(h, None);
            }
            let reader = self
                .shared
                .reader
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            if let Some(t) = reader {
                let _ = t.join();
            }
        }
    }
}

/// 推送接收端（状态面，49 §4.6 latest-wins 由消费方按 epoch/version 判新旧）。
pub struct PushStream {
    rx: mpsc::Receiver<Push>,
}

impl PushStream {
    /// 阻塞收一条推送（限时）。
    pub fn recv_timeout(&self, d: Duration) -> Result<Push, TransportError> {
        match self.rx.recv_timeout(d) {
            Ok(p) => Ok(p),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(TransportError::Deadline),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(TransportError::Closed),
        }
    }
}

/// 连接（含握手）。任何握手失败（版本无交集/认证失败/超时）→ `Err`，
/// 调用方按 49 §4.4 降级为完全透明。
pub fn connect(
    cfg: &ClientConfig,
) -> Result<(TransportClient, HelloAck, PushStream), TransportError> {
    let h = open_with_retry(&cfg.pipe_name)?;
    // 握手期属主：任何失败路径自动关句柄；成功后 forget（属主移交「最后一个 client drop」）。
    let guard = super::HandleGuard::own(h);

    // —— 握手（49 §4.4）：Hello → HelloAck / Err ——
    let hello = C2S::Hello {
        proto_min: cfg.proto_min,
        proto_max: cfg.proto_max,
        auth: cfg.auth.clone(),
        caps: cfg.caps,
        resume: cfg.resume,
        client: ClientInfo {
            pid: crate::logger::process_id(),
            tid: crate::logger::thread_id(),
            app: cfg.app.clone(),
        },
    };
    write_frame_ov(
        h,
        0,
        false,
        &iuv_proto::Payload::ClientReq(hello),
        WRITE_TIMEOUT_MS,
    )?;
    let (_, payload) = read_frame_ov(h, ms_of(cfg.handshake_timeout))?;
    let ack = match payload {
        iuv_proto::Payload::ServerResp(S2C::HelloAck {
            proto,
            caps,
            server_build,
        }) => HelloAck {
            proto,
            caps,
            build: server_build,
        },
        iuv_proto::Payload::ServerResp(S2C::Err(e)) => return Err(TransportError::Proto(e)),
        _ => {
            return Err(TransportError::Proto(ProtoError::Malformed {
                offset: 0,
                reason: "握手应答类型错误".into(),
            }))
        }
    };

    // —— 读线程：分发 ServerResp（按 stream_id）/ Push ——
    let (push_tx, push_rx) = mpsc::channel();
    let shared = Arc::new(Shared {
        h: SendHandle::new(h),
        inflight: Mutex::new(HashMap::new()),
        ids: Mutex::new(StreamIdAlloc::client()),
        push_tx,
        closed: AtomicBool::new(false),
        reader: Mutex::new(None),
        clients: AtomicUsize::new(1),
        write_lock: Mutex::new(()),
        server_req_handler: cfg.on_server_req.clone(),
    });
    let reader = std::thread::Builder::new()
        .name("iuv-transport-rx".into())
        .spawn({
            let shared = shared.clone();
            move || reader_loop(shared)
        })
        .map_err(|e| TransportError::Io(io::Error::other(e.to_string())))?;
    *shared.reader.lock().unwrap_or_else(|e| e.into_inner()) = Some(reader);

    std::mem::forget(guard);
    Ok((TransportClient { shared }, ack, PushStream { rx: push_rx }))
}

impl TransportClient {
    /// 发请求等应答（同步，有截止时间）。热路径传 `urgent=true`。
    ///
    /// 超时的请求**烧掉**其 stream_id（不复用）——迟到应答绝不串到后续请求
    /// （49 §4.5.2：超时后服务端可能已推进，基线失效由会话层处理）。
    pub fn request(
        &self,
        req: C2S,
        urgent: bool,
        timeout: Duration,
    ) -> Result<S2C, TransportError> {
        if self.shared.closed.load(Ordering::Relaxed) {
            return Err(TransportError::Closed);
        }
        let id = self
            .shared
            .ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .alloc()
            .ok_or_else(|| TransportError::Io(io::Error::other("stream_id 耗尽")))?;
        let (tx, rx) = mpsc::channel();
        self.shared
            .inflight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, tx);
        let wr = {
            let _w = self
                .shared
                .write_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            write_frame_ov(
                self.shared.h.get(),
                id,
                urgent,
                &iuv_proto::Payload::ClientReq(req),
                WRITE_TIMEOUT_MS,
            )
        };
        if let Err(e) = wr {
            self.shared
                .inflight
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            return Err(e);
        }
        match rx.recv_timeout(timeout) {
            Ok(s2c) => {
                self.shared
                    .ids
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .release(id);
                Ok(s2c)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => Err(TransportError::Deadline),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(TransportError::Closed),
        }
    }

    /// 连接是否已失效（读线程退出 = 管道断开）。
    pub fn is_closed(&self) -> bool {
        self.shared.closed.load(Ordering::Relaxed)
    }
}

/// 读线程 tick：有界超时读的周期。数据到达即返回，tick 只影响空闲空转与
/// 关闭收尾延迟（最坏一个 tick）。
const READER_TICK_MS: u32 = 500;

/// 读线程独占连接的读取与**句柄关闭**（见 `TransportClient::drop` 收尾协议）。
fn reader_loop(shared: Arc<Shared>) {
    let exit = |shared: &Arc<Shared>| {
        shared.closed.store(true, Ordering::Relaxed);
        shared
            .inflight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        // SAFETY: 读线程是唯一在关闭时机做 ReadFile 的一方，退出即关——
        // 此后句柄值可被复用且无人再读旧值。
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(shared.h.get());
        }
    };
    loop {
        if shared.closed.load(Ordering::Relaxed) {
            exit(&shared);
            return;
        }
        match read_frame_ov(shared.h.get(), READER_TICK_MS) {
            Ok((hdr, payload)) => match payload {
                iuv_proto::Payload::ServerResp(s2c) => {
                    let tx = shared
                        .inflight
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&hdr.stream_id);
                    if let Some(tx) = tx {
                        let _ = tx.send(s2c);
                    }
                    // 迟到应答（超时烧号后到达）→ 通道不在 → 丢弃
                }
                iuv_proto::Payload::Push(p) => {
                    let _ = shared.push_tx.send(p);
                }
                iuv_proto::Payload::ServerReq(s2c) => {
                    // 49 §4.1 控制面：服务端主动请求 → 独立线程执行处理器
                    //（可能阻塞至 3s 等 TSF 线程应用），读线程继续分发不受阻。
                    if let Some(handler) = shared.server_req_handler.clone() {
                        let shared2 = shared.clone();
                        let stream_id = hdr.stream_id;
                        let spawned = std::thread::Builder::new()
                            .name("iuv-transport-ctl".into())
                            .spawn(move || {
                                let resp = handler(s2c);
                                let _w =
                                    shared2.write_lock.lock().unwrap_or_else(|e| e.into_inner());
                                let _ = write_frame_ov(
                                    shared2.h.get(),
                                    stream_id,
                                    false,
                                    &iuv_proto::Payload::ClientResp(resp),
                                    WRITE_TIMEOUT_MS,
                                );
                            });
                        if spawned.is_err() {
                            // 线程创建失败：无法应答，服务端按超时处理。
                        }
                    }
                    // 无处理器：静默丢弃（服务端等待超时自行降级）。
                }
                _ => {
                    exit(&shared);
                    return;
                }
            },
            Err(TransportError::Deadline) => {
                // 空 tick：回环查关闭标志即可
            }
            Err(_) => {
                // 管道断开：置关闭 + 清在途（Sender 丢弃 → 请求方得到 Closed）。
                exit(&shared);
                return;
            }
        }
    }
}

/// 打开管道（overlapped 模式）。NOT_FOUND 短重试（服务端 accept 间隙），BUSY 走
/// `WaitNamedPipeW`。上界 [`CONNECT_RETRY_MS`]。
fn open_with_retry(name: &str) -> Result<HANDLE, TransportError> {
    let wide: Vec<u16> = OsStr::new(name).encode_wide().chain(Some(0)).collect();
    let deadline = Instant::now() + Duration::from_millis(CONNECT_RETRY_MS);
    loop {
        // SAFETY: name NUL 结尾；overlapped 双工；独占打开（与在役 daemon_client 一致）。
        let r = unsafe {
            CreateFileW(
                PCWSTR(wide.as_ptr()),
                (PIPE_ACCESS_DUPLEX).0,
                Default::default(),
                None,
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                None,
            )
        };
        match r {
            Ok(h) => return Ok(h),
            Err(e) => {
                let code = e.code().0 as u32 & 0xFFFF;
                if code == ERROR_PIPE_BUSY.0 {
                    // SAFETY: name NUL 结尾。
                    let ok = unsafe { WaitNamedPipeW(PCWSTR(wide.as_ptr()), 1500) };
                    if !ok.as_bool() {
                        return Err(TransportError::Io(io::Error::other(
                            "管道忙且超时（服务端不在线）",
                        )));
                    }
                    continue;
                }
                if code == 2 && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(1));
                    continue;
                }
                return Err(TransportError::Io(io::Error::other(format!(
                    "命名管道不可达: {code}"
                ))));
            }
        }
    }
}

fn ms_of(d: Duration) -> u32 {
    d.as_millis().min(u32::MAX as u128) as u32
}
