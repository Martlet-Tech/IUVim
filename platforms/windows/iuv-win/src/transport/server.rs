//! 传输服务端（daemon/server 侧）：accept 线程 + 每连接一线程的长连接服务。
//!
//! 握手三关（49 §4.4）：首帧必须是 `Hello` → 版本协商（无交集 = 类型化报错后断连）→
//! 认证（密钥不符 = `Unauthenticated` 后断连）。会话建立后发 `Push::SessionAttached`
//! 下发重绑令牌（P2 阶段仅下发，重绑 P5 落地）。
//!
//! 过载防线（49 §4.7）：连接数上限（超限即关，`Session` 不装配）。

use std::collections::HashMap;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use windows::Win32::Foundation::{
    CloseHandle, ERROR_IO_PENDING, ERROR_PIPE_CONNECTED, HANDLE, WAIT_OBJECT_0,
};
use windows::Win32::Storage::FileSystem::{FILE_FLAG_OVERLAPPED, PIPE_ACCESS_DUPLEX};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::Win32::System::IO::CancelIoEx;
use windows_core::PCWSTR;

use iuv_proto::{
    negotiate, Auth, BuildId, Caps, ClientInfo, Payload, ProtoError, Push, ResumeToken,
    StreamIdAlloc, C2S, PROTO_MAX, PROTO_MIN, S2C,
};

use super::{
    read_frame_ov, write_frame_ov, HandleGuard, OverlapHost, SendHandle, TransportError,
    HANDSHAKE_TIMEOUT_MS, WRITE_TIMEOUT_MS,
};

/// 单帧读缓冲（64 KiB，与 `iuv_proto::MAX_PAYLOAD` 一致）。
const FRAME_BUF: u32 = 64 * 1024;

/// 服务端配置。
pub struct ServerConfig {
    pub pipe_name: String,
    /// 期望的共享密钥（不符 = `Unauthenticated` 断连）。
    pub auth: Auth,
    /// 服务端支持的能力集（与客户端请求取交集）。
    pub caps: Caps,
    pub build: BuildId,
    /// 连接上限（49 §4.7：服务端是打字单点，防拖死）。
    pub max_connections: usize,
}

/// 每连接会话：连接线程独占调用 `on_c2s`，可回应答 + 推送。
pub trait Session: Send + 'static {
    fn on_c2s(&mut self, req: C2S, reply: &mut Reply);
}

/// 会话工厂：新连接接入时装配。认证/版本由 transport 先行校验，到此处必然合法；
/// `caps` = 服务端 ∩ 客户端的能力交集（会话据此决定候选数据等推送）。
/// `resume` = 客户端重绑请求（49 §4.4：断线重连回绑旧会话，由实现方查注册表）；
/// `token` = 本连接的重绑令牌（服务端生成，随 `Push::SessionAttached` 下发，
/// 断连时会话实现可按它保存现场）；
/// `sender` = 服务端主动请求通道（49 §4.1 控制面：工具栏 Ctl → 客户端）。
pub trait ConnHandler: Send + Sync + 'static {
    fn on_connect(
        &self,
        client: &ClientInfo,
        caps: Caps,
        resume: Option<ResumeToken>,
        token: ResumeToken,
        sender: ConnSender,
    ) -> Box<dyn Session>;
}

/// 连接级共享写状态（conn 线程与 [`ConnSender`] 共用）。
pub(crate) struct ConnShared {
    pub(crate) h: SendHandle,
    pub(crate) write_lock: Mutex<()>,
    /// 在途服务端请求：stream_id（奇数）→ 应答通道。
    pub(crate) inflight: Mutex<HashMap<u16, mpsc::Sender<C2S>>>,
    /// 进行中的写计数：conn 线程退出前等待归零（防句柄值复用后写错连接——
    /// 与客户端读线程收尾协议同类问题，2026-09-27）。
    pub(crate) writers: AtomicUsize,
    pub(crate) closed: AtomicBool,
}

/// 服务端主动请求通道（49 §4.1 控制面）：连接线程之外（如工具栏线程）可经它
/// 向客户端发 `S2C::Ctl` 并等 `C2S::CtlResult`。连接关闭后 request 返回 Closed。
pub struct ConnSender {
    shared: Arc<ConnShared>,
    /// 共享分配器（`Arc`）：Clone 多副本**共用同一号段游标**——旧实现 clone 快照
    /// 复制 ids，两副本各自演化会分到相同奇数号，inflight 路由表（共享）同号
    /// insert 覆盖前一个 tx，先发方永远等不到应答（2026-10-02 品质审查 T4）。
    ids: Arc<Mutex<StreamIdAlloc>>,
}

impl Clone for ConnSender {
    fn clone(&self) -> Self {
        ConnSender {
            shared: self.shared.clone(),
            ids: self.ids.clone(),
        }
    }
}

impl ConnSender {
    /// 服务端发起请求（`S2C::Ctl`/`Ping`），同步等客户端应答（`C2S::CtlResult` 等）。
    /// 超时烧号不复用（与客户端 `request` 同纪律）。
    pub fn request(&self, req: S2C, timeout: Duration) -> Result<C2S, TransportError> {
        use std::sync::atomic::Ordering;
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(TransportError::Closed);
        }
        let id = self
            .ids
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .alloc()
            .ok_or_else(|| TransportError::Io(io::Error::other("服务端 stream_id 耗尽")))?;
        let (tx, rx) = mpsc::channel();
        self.shared
            .inflight
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, tx);
        let _guard = crate::transport::WriteGuard::new(&self.shared.writers);
        let wr = {
            let _w = self
                .shared
                .write_lock
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if self.shared.closed.load(Ordering::Acquire) {
                Err(TransportError::Closed)
            } else {
                write_frame_ov(
                    self.shared.h.get(),
                    id,
                    false,
                    &Payload::ServerReq(req),
                    WRITE_TIMEOUT_MS,
                )
            }
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
            Ok(c2s) => {
                self.ids
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .release(id);
                Ok(c2s)
            }
            Err(mpsc::RecvTimeoutError::Timeout) => Err(TransportError::Deadline),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(TransportError::Closed),
        }
    }

    /// 单向推送（fire-and-forget，无应答无流号）：优雅停机广播 `Push::Shutdown`
    /// 等服务端主动下行。连接已关/写失败返回 Err，调用方按连接死亡处理。
    pub fn push(&self, p: Push) -> Result<(), TransportError> {
        use std::sync::atomic::Ordering;
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(TransportError::Closed);
        }
        let _guard = crate::transport::WriteGuard::new(&self.shared.writers);
        let _w = self
            .shared
            .write_lock
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if self.shared.closed.load(Ordering::Acquire) {
            return Err(TransportError::Closed);
        }
        write_frame_ov(
            self.shared.h.get(),
            0,
            false,
            &Payload::Push(p),
            WRITE_TIMEOUT_MS,
        )
    }
}

/// 一次 `on_c2s` 的产出：0/1 条应答 + 任意条推送。
#[derive(Default)]
pub struct Reply {
    resp: Option<S2C>,
    pushes: Vec<Push>,
}

impl Reply {
    /// 回应答（以最后一条为准）。
    pub fn respond(&mut self, s2c: S2C) {
        self.resp = Some(s2c);
    }
    /// 追加推送（`SessionAttached` / 四态等；latest-wins 合并由消费方按 epoch 判）。
    pub fn push(&mut self, p: Push) {
        self.pushes.push(p);
    }
}

/// 传输服务端。`stop()`/`Drop` 关闭 accept 并回收线程；在途连接随客户端断开自然结束。
pub struct TransportServer {
    stop: Arc<AtomicBool>,
    /// 当前挂起的 accept 实例句柄（stop 时取消其 ConnectNamedPipe）。
    pending: Arc<Mutex<Option<SendHandle>>>,
    accept_thread: Option<JoinHandle<io::Result<()>>>,
}

impl TransportServer {
    /// 启动服务。返回即表示 accept 线程已就绪（管道实例已创建，等待首个连接）。
    pub fn start(cfg: ServerConfig, handler: Arc<dyn ConnHandler>) -> io::Result<TransportServer> {
        let stop = Arc::new(AtomicBool::new(false));
        let pending: Arc<Mutex<Option<SendHandle>>> = Arc::new(Mutex::new(None));
        let conns = Arc::new(AtomicUsize::new(0));
        let cfg = Arc::new(cfg);
        let accept_thread = std::thread::Builder::new()
            .name("iuv-transport-accept".into())
            .spawn({
                let stop = stop.clone();
                let pending = pending.clone();
                move || accept_loop(cfg, handler, stop, pending, conns)
            })
            .map_err(|e| io::Error::other(e.to_string()))?;
        Ok(TransportServer {
            stop,
            pending,
            accept_thread: Some(accept_thread),
        })
    }

    /// 停止接受新连接并回收 accept 线程（在途连接线程随客户端断开自然结束）。
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(sh) = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            // SAFETY: 取消挂起的 ConnectNamedPipe 以唤醒 accept 线程。**只取消
            // 不关闭**——句柄由 accept 线程在其退出路径统一关闭（2026-10-02
            // 品质审查 T5：此处 CloseHandle 后 accept 线程的取消完成仍会让
            // WaitForSingleObject 返回 WAIT_OBJECT_0 被误判 connected，随后
            // stop 分支对同一句柄二次 CloseHandle，句柄值复用窗口内可能关到
            // 无关句柄）。
            unsafe {
                let _ = CancelIoEx(sh.get(), None);
            }
        }
        if let Some(t) = self.accept_thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for TransportServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn accept_loop(
    cfg: Arc<ServerConfig>,
    handler: Arc<dyn ConnHandler>,
    stop: Arc<AtomicBool>,
    pending: Arc<Mutex<Option<SendHandle>>>,
    conns: Arc<AtomicUsize>,
) -> io::Result<()> {
    let wide: Vec<u16> = std::ffi::OsStr::new(&cfg.pipe_name)
        .encode_wide()
        .chain(Some(0))
        .collect();
    loop {
        if stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        let h = create_server_stream(&wide)?;
        pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .replace(SendHandle::new(h));
        // SAFETY: overlapped 句柄的 ConnectNamedPipe 必须带 OVERLAPPED。
        let connected = {
            let mut host = OverlapHost::new()?;
            let r = unsafe { ConnectNamedPipe(h, Some(&mut host.ov)) };
            match r {
                Ok(()) => true,
                Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => loop {
                    // 分片等待（200ms tick）：stop 置位即放弃等待——CancelIoEx 完成会
                    // 置信号导致 WAIT_OBJECT_0，不能据此判定连接成功；分片检查保证
                    // 任何时序下 accept 线程都能自行退出并关闭句柄（T5 收尾对齐）。
                    let w = unsafe { WaitForSingleObject(host.ev, 200) };
                    if w == WAIT_OBJECT_0 {
                        // 事件已信号：可能是连接完成也可能是取消完成——由调用方
                        // 后续 stop 检查与 !connected 关闭路径统一兜底。
                        break true;
                    }
                    if stop.load(Ordering::SeqCst) {
                        break false;
                    }
                },
                Err(e) => win32_code(&e) == ERROR_PIPE_CONNECTED.0, // 客户端抢先连入
            }
        };
        pending.lock().unwrap_or_else(|e| e.into_inner()).take();
        if !connected {
            // SAFETY: 未连成的实例句柄由本线程关闭。
            unsafe {
                let _ = CloseHandle(h);
            }
            if stop.load(Ordering::SeqCst) {
                return Ok(());
            }
            continue; // 瞬时错误（含 stop 取消）→ 重建实例
        }
        if stop.load(Ordering::SeqCst) {
            // SAFETY: stop 后不再服务。
            unsafe {
                let _ = CloseHandle(h);
            }
            return Ok(());
        }
        if conns.load(Ordering::SeqCst) >= cfg.max_connections {
            // SAFETY: 超限连接即关（49 §4.7）。
            unsafe {
                let _ = CloseHandle(h);
            }
            continue;
        }
        conns.fetch_add(1, Ordering::SeqCst);
        let ctx = ConnCtx {
            auth: cfg.auth.clone(),
            caps: cfg.caps,
            build: cfg.build.clone(),
            handler: handler.clone(),
            stop: stop.clone(),
        };
        let sh = SendHandle::new(h);
        let conns2 = conns.clone();
        let spawned = std::thread::Builder::new()
            .name("iuv-transport-conn".into())
            .spawn(move || {
                let _ = conn_thread(sh.get(), ctx);
                conns2.fetch_sub(1, Ordering::SeqCst);
            });
        if spawned.is_err() {
            // SAFETY: 线程创建失败，实例无人服务，关闭。
            unsafe {
                let _ = CloseHandle(h);
            }
            conns.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

use windows::Win32::System::Threading::WaitForSingleObject;
struct ConnCtx {
    auth: Auth,
    caps: Caps,
    build: BuildId,
    handler: Arc<dyn ConnHandler>,
    stop: Arc<AtomicBool>,
}

/// 错误码低 16 位（HRESULT → WIN32_ERROR）。
fn win32_code(e: &windows_core::Error) -> u32 {
    (e.code().0 as u32) & 0xFFFF
}

/// 重绑令牌发生器：时间戳 ^ 进程号（P2 仅保证唯一性，重绑语义 P5 落地）。
fn next_token() -> ResumeToken {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    ResumeToken(nanos ^ (std::process::id() as u64) << 32 ^ SEQ.fetch_add(1, Ordering::Relaxed))
}

fn conn_thread(h: HANDLE, ctx: ConnCtx) -> io::Result<()> {
    let _guard = HandleGuard::own(h);

    // —— 握手三关（49 §4.4），任一不过 = 类型化报错后断连 ——
    let (_, payload) = read_frame_ov(h, HANDSHAKE_TIMEOUT_MS)
        .map_err(|e| io::Error::other(format!("握手读失败: {e}")))?;
    let hello = match payload {
        Payload::ClientReq(C2S::Hello {
            proto_min,
            proto_max,
            auth,
            caps,
            resume,
            client,
        }) => (proto_min, proto_max, auth, caps, resume, client),
        _ => {
            return Err(io::Error::other("协议违规: 首帧非 Hello"));
        }
    };
    let (proto_min, proto_max, auth, want_caps, resume, client) = hello;
    let proto = match negotiate(proto_min, proto_max, PROTO_MIN, PROTO_MAX) {
        Ok(p) => p,
        Err(e) => {
            // 明确文本报错（49 §4.4：非静默错乱），客户端据此降级/提示。
            let _ = write_frame_ov(
                h,
                0,
                false,
                &Payload::ServerResp(S2C::Err(e)),
                WRITE_TIMEOUT_MS,
            );
            return Err(io::Error::other("握手失败: 版本无交集"));
        }
    };
    if auth != ctx.auth {
        let _ = write_frame_ov(
            h,
            0,
            false,
            &Payload::ServerResp(S2C::Err(ProtoError::Unauthenticated)),
            WRITE_TIMEOUT_MS,
        );
        return Err(io::Error::other("握手失败: 认证不符"));
    }
    let caps = Caps(ctx.caps.0 & want_caps.0);
    let token = next_token();
    let conn_shared = Arc::new(ConnShared {
        h: SendHandle::new(h),
        write_lock: Mutex::new(()),
        inflight: Mutex::new(HashMap::new()),
        writers: AtomicUsize::new(0),
        closed: AtomicBool::new(false),
    });
    let mut session = ctx.handler.on_connect(
        &client,
        caps,
        resume,
        token,
        ConnSender {
            shared: conn_shared.clone(),
            ids: Arc::new(Mutex::new(StreamIdAlloc::server())),
        },
    );
    write_frame_ov(
        h,
        0,
        false,
        &Payload::ServerResp(S2C::HelloAck {
            proto,
            caps,
            server_build: ctx.build.clone(),
        }),
        WRITE_TIMEOUT_MS,
    )
    .map_err(|e| io::Error::other(format!("HelloAck 写失败: {e}")))?;
    write_frame_ov(
        h,
        0,
        false,
        &Payload::Push(Push::SessionAttached { token }),
        WRITE_TIMEOUT_MS,
    )
    .map_err(|e| io::Error::other(format!("SessionAttached 写失败: {e}")))?;

    // —— 帧循环：客户端只准发 ClientReq / ClientResp（49 §4.3 方向即类型）——
    loop {
        if ctx.stop.load(Ordering::SeqCst) {
            break;
        }
        let (hdr, payload) =
            read_frame_ov(h, u32::MAX).map_err(|e| io::Error::other(format!("请求读失败: {e}")))?;
        match payload {
            Payload::ClientReq(c2s) => {
                let mut reply = Reply::default();
                session.on_c2s(c2s, &mut reply);
                // 应答与推送统一走共享写锁（ConnSender 可能并发写服务端主动请求帧）。
                let _guard = crate::transport::WriteGuard::new(&conn_shared.writers);
                let w = conn_shared
                    .write_lock
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if let Some(s2c) = reply.resp.take() {
                    write_frame_ov(
                        h,
                        hdr.stream_id,
                        hdr.urgent,
                        &Payload::ServerResp(s2c),
                        WRITE_TIMEOUT_MS,
                    )
                    .map_err(|e| io::Error::other(format!("应答写失败: {e}")))?;
                }
                for p in reply.pushes.drain(..) {
                    write_frame_ov(h, 0, false, &Payload::Push(p), WRITE_TIMEOUT_MS)
                        .map_err(|e| io::Error::other(format!("推送写失败: {e}")))?;
                }
                drop(w);
            }
            // 服务端主动请求的应答（C2S::CtlResult 等）→ 按号路由给等待方。
            Payload::ClientResp(c2s) => {
                let tx = conn_shared
                    .inflight
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&hdr.stream_id);
                if let Some(tx) = tx {
                    let _ = tx.send(c2s);
                }
            }
            _ => {
                return Err(io::Error::other(
                    "协议违规: 客户端只能发 ClientReq/ClientResp",
                ))
            }
        }
    }
    // —— 收尾协议（与客户端读线程同类）：停新写 → 等在途写归零 → 清在途 → 关句柄。
    conn_shared.closed.store(true, Ordering::SeqCst);
    for _ in 0..100 {
        if conn_shared.writers.load(Ordering::SeqCst) == 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    conn_shared
        .inflight
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
    Ok(())
}

/// BYTE 模式 + overlapped 双工服务实例（49 §4.2：显式分帧，不依赖消息模式）。
fn create_server_stream(wide: &[u16]) -> io::Result<HANDLE> {
    // SAFETY: 消息缓冲 FRAME_BUF；返回 HANDLE。
    let h = unsafe {
        CreateNamedPipeW(
            PCWSTR(wide.as_ptr()),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
            PIPE_TYPE_BYTE | PIPE_WAIT,
            PIPE_UNLIMITED_INSTANCES,
            FRAME_BUF,
            FRAME_BUF,
            0,
            None,
        )
    };
    if h.is_invalid() {
        return Err(io::Error::other("创建命名管道失败"));
    }
    Ok(h)
}
