//! 传输服务端（daemon/server 侧）：accept 线程 + 每连接一线程的长连接服务。
//!
//! 握手三关（49 §4.4）：首帧必须是 `Hello` → 版本协商（无交集 = 类型化报错后断连）→
//! 认证（密钥不符 = `Unauthenticated` 后断连）。会话建立后发 `Push::SessionAttached`
//! 下发重绑令牌（P2 阶段仅下发，重绑 P5 落地）。
//!
//! 过载防线（49 §4.7）：连接数上限（超限即关，`Session` 不装配）。

use std::io;
use std::os::windows::ffi::OsStrExt;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{SystemTime, UNIX_EPOCH};

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
    negotiate, Auth, BuildId, Caps, ClientInfo, Payload, ProtoError, Push, ResumeToken, C2S,
    PROTO_MAX, PROTO_MIN, S2C,
};

use super::{
    read_frame_ov, write_frame_ov, HandleGuard, OverlapHost, SendHandle, HANDSHAKE_TIMEOUT_MS,
    WRITE_TIMEOUT_MS,
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

/// 会话工厂：新连接接入时装配。认证/版本由 transport 先行校验，到此处必然合法。
pub trait ConnHandler: Send + Sync + 'static {
    fn on_connect(&self, client: &ClientInfo) -> Box<dyn Session>;
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
            // SAFETY: 取消挂起的 ConnectNamedPipe；句柄随后关闭。
            unsafe {
                let _ = CancelIoEx(sh.get(), None);
                let _ = CloseHandle(sh.get());
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
                Err(e) if e.code() == ERROR_IO_PENDING.to_hresult() => {
                    // INFINITE 等待；stop 经 CancelIoEx 使其以错误返回。
                    let w = unsafe { WaitForSingleObject(host.ev, u32::MAX) };
                    w == WAIT_OBJECT_0
                }
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
            resume: _,
            client,
        }) => (proto_min, proto_max, auth, caps, client),
        _ => {
            return Err(io::Error::other("协议违规: 首帧非 Hello"));
        }
    };
    let (proto_min, proto_max, auth, want_caps, client) = hello;
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
    let mut session = ctx.handler.on_connect(&client);
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
        &Payload::Push(Push::SessionAttached {
            token: next_token(),
        }),
        WRITE_TIMEOUT_MS,
    )
    .map_err(|e| io::Error::other(format!("SessionAttached 写失败: {e}")))?;

    // —— 帧循环：客户端只准发 ClientReq（49 §4.3 方向即类型）——
    loop {
        if ctx.stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        let (hdr, payload) =
            read_frame_ov(h, u32::MAX).map_err(|e| io::Error::other(format!("请求读失败: {e}")))?;
        match payload {
            Payload::ClientReq(c2s) => {
                let mut reply = Reply::default();
                session.on_c2s(c2s, &mut reply);
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
            }
            _ => return Err(io::Error::other("协议违规: 客户端只能发 ClientReq")),
        }
    }
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
