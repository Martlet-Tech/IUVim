//! M10 薄客户端远端引擎宿主（49 §5 P3b）：进程级 `RemoteHandle`，连接 iuv-server。
//!
//! 模式切换：config `use_engine_server`（默认 false = 现状本地引擎，零行为变化）。
//! 进程级一次性判定（`init_mode`，Activate 调用），`use_server()` 供各处分支。
//!
//! 热路径契约（49 §4.5）：
//! - **Test/KeyDown 单槽去重**（§4.5.1）：`key_test` 发请求并缓存裁定；`key_down`
//!   命中缓存零 IPC 复用、未命中（只发 OnKeyDown 的应用）现场处理；
//! - **截止时间**（§4.5.2）：每请求 20ms；超时 → 放行按键（返回 `None`）+ degraded
//!   标记 → 下一键 `full=true` 强制全量重同步；`Busy` 不触发；
//! - **失效语义 A**（§4.5.4，P3b 范围）：连接断开 → `offline`，按键全部放行
//!   （等同没装输入法），P5 补快速重生（方案 C）。
//!
//! 客户端本地配置副本：路由判定（keymap/passthrough）与候选窗渲染所需字段
//! 在远端模式下读 `RemoteHandle::config`（Config::load 装配 + daemon 配置纪元热载），
//! **不加载词库/引擎**（49 §2「客户端无词库无引擎」）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use iuv_core::{Config, Key, SessionEnd};
use iuv_proto::{ImeState as WireImeState, KeyOutcome, KeyPhase, KeyToken, KeyVerdict, C2S, S2C};
use iuv_win::logger::log_line;
use iuv_win::transport::{
    connect, ClientConfig, TransportClient, TransportError, CONNECT_RETRY_MS, SERVICE_PIPE_NAME,
};

/// 每键请求截止（49 §4.5.2）：实测长连接 P99 = 13µs，20ms 已是百倍余量；
/// 超时 = 服务端异常，放行按键优于卡住宿主。
const KEY_DEADLINE_MS: u64 = 20;

/// 进程模式：true = 远端 iuv-server（薄客户端）。`init_mode` 一次性判定。
static USE_SERVER: AtomicBool = AtomicBool::new(false);
static MODE_INIT: AtomicBool = AtomicBool::new(false);
/// 远端连接（后台装配；None = 连接失败 → 恒透明放行）。
static REMOTE: OnceLock<Option<Arc<RemoteHandle>>> = OnceLock::new();

/// 路由/渲染所需配置（模式感知）：local = engine.config()；remote = 客户端副本。
/// None = 后端未就绪（透明放行——本地引擎加载中/远端未连接同语义）。
pub(crate) fn backend_config() -> Option<Config> {
    if use_server() {
        remote().filter(|r| r.ready()).map(|r| r.config())
    } else {
        super::engine_host::engine().map(|e| e.config())
    }
}

/// 当前修饰键 → 线上 Mods（key_routing 的 GetKeyState 语义）。
pub(crate) fn wire_mods(shift: bool, ctrl: bool, alt: bool) -> iuv_proto::Mods {
    iuv_proto::Mods { shift, ctrl, alt }
}

/// 线上候选 → 核心候选（客户端渲染用；code/weight/seg_len 不上线——
/// 续接/调权语义在服务端会话内，客户端只需 text/kind 展示）。
pub(crate) fn core_candidate(c: &iuv_proto::Candidate) -> iuv_core::Candidate {
    iuv_core::Candidate::new(
        c.text.clone(),
        match c.kind {
            iuv_proto::CandidateKind::Sentence => iuv_core::CandidateKind::Sentence,
            iuv_proto::CandidateKind::Word => iuv_core::CandidateKind::Word,
            iuv_proto::CandidateKind::Char => iuv_core::CandidateKind::Char,
        },
        String::new(),
        0,
        0,
    )
}

pub(crate) fn core_page(p: iuv_proto::PageInfo) -> iuv_core::PageInfo {
    iuv_core::PageInfo {
        page: p.page as usize,
        page_count: p.page_count as usize,
        page_size: p.page_size as usize,
        total: p.total as usize,
    }
}

pub(crate) fn core_session_end(e: iuv_proto::SessionEnd) -> SessionEnd {
    match e {
        iuv_proto::SessionEnd::Commit(text) => SessionEnd::Commit(text),
        iuv_proto::SessionEnd::Cancel => SessionEnd::Cancel,
    }
}

/// Activate 时调用（进程内首个实例）：读配置判定模式，随后调 `start_engine_load`
/// 或 `start_remote_load`。
pub(crate) fn init_mode() {
    if MODE_INIT.swap(true, Ordering::SeqCst) {
        return;
    }
    let use_server = Config::load().use_engine_server;
    USE_SERVER.store(use_server, Ordering::SeqCst);
    log_line(&format!(
        "[backend] 引擎模式：{}",
        if use_server {
            "远端 iuv-server（薄客户端）"
        } else {
            "本地（现状）"
        }
    ));
}

pub(crate) fn use_server() -> bool {
    USE_SERVER.load(Ordering::SeqCst)
}

/// 取远端句柄（未启动/连接失败 → None = 调用方透明放行）。
pub(crate) fn remote() -> Option<&'static Arc<RemoteHandle>> {
    REMOTE.get().and_then(|r| r.as_ref())
}

/// 后台连接 iuv-server（Activate 触发；加载中按键透明放行，语义同引擎后台加载）。
/// 失败 → REMOTE.set(None)：远端模式永久透明（P5 补快速重生后改为可重试）。
pub(crate) fn start_remote_load() {
    if REMOTE.get().is_some() {
        return;
    }
    std::thread::Builder::new()
        .name("iuv-remote-connect".into())
        .spawn(|| {
            let t0 = Instant::now();
            log_line("[backend] 连接 iuv-server（后台）");
            let handle = match connect_server() {
                Ok(h) => h,
                Err(e) => {
                    log_line(&format!(
                        "[backend] iuv-server 连接失败（{:.0} ms）：{e} → 远端模式透明",
                        t0.elapsed().as_millis()
                    ));
                    let _ = REMOTE.set(None);
                    return;
                }
            };
            log_line(&format!(
                "[backend] iuv-server 已连接（{:.0} ms）",
                t0.elapsed().as_millis()
            ));
            let _ = REMOTE.set(Some(Arc::new(handle)));
        })
        .expect("远端连接线程创建");
}

fn connect_server() -> Result<RemoteHandle, String> {
    let dir = iuv_core::paths::iuv_dir().unwrap_or_else(|| std::env::temp_dir().join("iuv"));
    let token = iuv_win::transport::load_or_create_token(&dir).map_err(|e| e.to_string())?;
    let cfg = ClientConfig {
        pipe_name: SERVICE_PIPE_NAME.to_string(),
        proto_min: iuv_proto::PROTO_MIN,
        proto_max: iuv_proto::PROTO_MAX,
        auth: token,
        caps: iuv_proto::Caps(iuv_proto::Caps::UIELEMENT),
        app: crate::log::module_name(),
        resume: None,
        handshake_timeout: Duration::from_millis(CONNECT_RETRY_MS),
    };
    let deadline = Instant::now() + Duration::from_millis(CONNECT_RETRY_MS);
    let (client, _ack, pushes) = loop {
        match connect(&cfg) {
            Ok(x) => break x,
            // 服务端 accept 间隙 / 未就绪：短重试至上界（正常 <1ms；server 缺席 = 走满）
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => return Err(e.to_string()),
        }
    };
    // 推送泵：SessionAttached/未来推送在此消费，防止通道堆积（P3 无业务推送）。
    std::thread::Builder::new()
        .name("iuv-remote-push".into())
        .spawn(move || loop {
            if pushes.recv_timeout(Duration::from_secs(3600)).is_err() {
                return; // 连接关闭
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(RemoteHandle {
        client: Mutex::new(Some(client)),
        config: Mutex::new(Config::load()),
        pending: Mutex::new(None),
        last_state: Mutex::new(None),
        degraded: AtomicBool::new(false),
        offline: AtomicBool::new(false),
        last_composition: Mutex::new(None),
    })
}

/// 远端引擎会话的客户端句柄（进程级单例，STA 线程们经它发请求）。
pub(crate) struct RemoteHandle {
    client: Mutex<Option<TransportClient>>,
    /// 客户端本地配置副本（路由 keymap/passthrough + 候选窗渲染字段）。
    config: Mutex<Config>,
    /// Test 裁定单槽缓存：`key_test` 写入、`key_down` 消费（键值匹配才复用）。
    pending: Mutex<Option<(Key, KeyOutcome)>>,
    last_state: Mutex<Option<WireImeState>>,
    /// 上次请求 Deadline → 下一 Key 带 `full=true`（§4.5.2 基线失效重同步）。
    degraded: AtomicBool,
    /// 连接断开（失效语义 A）：按键全部放行，直至进程重激活。
    offline: AtomicBool,
    /// 最近一次 composition（flush_session 原文上屏用；撇号为切分显示层）。
    last_composition: Mutex<Option<String>>,
}

impl RemoteHandle {
    pub(crate) fn ready(&self) -> bool {
        !self.offline.load(Ordering::Relaxed)
    }

    pub(crate) fn config(&self) -> Config {
        self.config
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// daemon 配置纪元热载（远端模式：更新客户端副本；服务端配置热载 P4 接管）。
    pub(crate) fn set_config(&self, cfg: Config) {
        *self.config.lock().unwrap_or_else(|e| e.into_inner()) = cfg;
    }

    /// OnTestKeyDown：Test 阶段**真正处理**并缓存裁定（§4.5.1）。None = 放行按键。
    pub(crate) fn key_test(&self, key: Key, mods: iuv_proto::Mods) -> Option<KeyOutcome> {
        if let Some((k, o)) = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            if *k == key {
                return Some(o.clone()); // 同键重复 Test：复用缓存（引擎只推进一次）
            }
        }
        let o = self.send_key(key, mods, KeyPhase::Test)?;
        *self.pending.lock().unwrap_or_else(|e| e.into_inner()) = Some((key, o.clone()));
        Some(o)
    }

    /// OnKeyDown：命中 Test 缓存零 IPC 复用；未命中（只发 OnKeyDown 的应用）现场处理。
    pub(crate) fn key_down(&self, key: Key, mods: iuv_proto::Mods) -> Option<KeyOutcome> {
        if let Some((k, o)) = self
            .pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            if k == key {
                return Some(o);
            }
            // 键不匹配（路由判定与 Test 阶段不一致的异常路径）：按未命中处理
        }
        self.send_key(key, mods, KeyPhase::Down)
    }

    fn send_key(&self, key: Key, mods: iuv_proto::Mods, phase: KeyPhase) -> Option<KeyOutcome> {
        let full = self.degraded.load(Ordering::Relaxed);
        let resp = self.request(C2S::Key {
            key: wire_key(&key),
            mods,
            token: KeyToken { seq: 0, phase },
            full,
        })?;
        let outcome = match resp {
            S2C::KeyResult(KeyVerdict::Consumed(o)) => o,
            S2C::KeyResult(KeyVerdict::Busy) => return None, // 引擎过载：放行（基线未失效）
            _ => return None,
        };
        self.degraded.store(false, Ordering::Relaxed);
        if outcome.composition.is_some() {
            *self
                .last_composition
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = outcome.composition.clone();
        }
        Some(outcome)
    }

    /// 客户端结束会话（flush/提交/取消收尾；尽力而为，失败不阻断）。
    pub(crate) fn end_session(&self) {
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        *self
            .last_composition
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        let _ = self.request(C2S::EndSession {
            end: iuv_proto::SessionEnd::Cancel,
        });
    }

    /// 四态同步（客户端是 OPENCLOSE 真相源）：与上次相同则跳过（省一次往返）。
    pub(crate) fn sync_state(&self, state: &iuv_core::ImeState) {
        let wire = wire_ime_state(state);
        {
            let mut last = self.last_state.lock().unwrap_or_else(|e| e.into_inner());
            if last.as_ref() == Some(&wire) {
                return;
            }
            *last = Some(wire);
        }
        let _ = self.request(C2S::ImeState(wire));
    }

    /// flush_session 原文上屏：最近 composition 去切分撇号（过渡近似：
    /// 用户手打引号的极端场景原文会少一个撇号，P4 服务端补 pending_text 后消除）。
    pub(crate) fn pending_raw_text(&self) -> Option<String> {
        self.last_composition
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
            .map(|c| c.replace('\'', ""))
    }

    /// 单请求（截止 [`KEY_DEADLINE_MS`]）。None = 降级放行（Deadline/Closed/协议错误）。
    fn request(&self, req: C2S) -> Option<S2C> {
        if self.offline.load(Ordering::Relaxed) {
            return None;
        }
        let client = self
            .client
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()?;
        match client.request(req, true, Duration::from_millis(KEY_DEADLINE_MS)) {
            Ok(resp) => Some(resp),
            Err(TransportError::Deadline) => {
                // §4.5.2：超时放行 + degraded → 下一 Key 带 full=true 全量重同步。
                // 连接仍活着（服务端只是慢），**不**置 offline。
                self.degraded.store(true, Ordering::Relaxed);
                log_line("[backend] 远端请求超时 → 放行本键（degraded，下键全量重同步）");
                None
            }
            Err(e) => {
                // Closed/IO/协议错误：失效语义 A——连接已坏，按键全部放行。
                self.offline.store(true, Ordering::Relaxed);
                log_line(&format!(
                    "[backend] 远端请求失败（{e}）→ 透明放行（失效语义 A）"
                ));
                None
            }
        }
    }
}

/// 核心 `Key` → 线上 `Key`（镜像变体集；与 iuv-server 的反向转换成对，P4 收敛后消失）。
fn wire_key(k: &Key) -> iuv_proto::Key {
    match *k {
        Key::Char(c) => iuv_proto::Key::Char(c),
        Key::ShiftChar(c) => iuv_proto::Key::ShiftChar(c),
        Key::Backspace => iuv_proto::Key::Backspace,
        Key::Space => iuv_proto::Key::Space,
        Key::Enter => iuv_proto::Key::Enter,
        Key::Esc => iuv_proto::Key::Esc,
        Key::Digit(n) => iuv_proto::Key::Digit(n),
        Key::Tab => iuv_proto::Key::Tab,
        Key::Delete => iuv_proto::Key::Delete,
        Key::Home => iuv_proto::Key::Home,
        Key::End => iuv_proto::Key::End,
        Key::Insert => iuv_proto::Key::Insert,
        Key::PageUp => iuv_proto::Key::PageUp,
        Key::PageDown => iuv_proto::Key::PageDown,
        Key::Up => iuv_proto::Key::Up,
        Key::Down => iuv_proto::Key::Down,
        Key::Left => iuv_proto::Key::Left,
        Key::Right => iuv_proto::Key::Right,
        Key::F1 => iuv_proto::Key::F1,
        Key::F2 => iuv_proto::Key::F2,
        Key::F3 => iuv_proto::Key::F3,
        Key::F4 => iuv_proto::Key::F4,
        Key::F5 => iuv_proto::Key::F5,
        Key::F6 => iuv_proto::Key::F6,
        Key::F7 => iuv_proto::Key::F7,
        Key::F8 => iuv_proto::Key::F8,
        Key::F9 => iuv_proto::Key::F9,
        Key::F10 => iuv_proto::Key::F10,
        Key::F11 => iuv_proto::Key::F11,
        Key::F12 => iuv_proto::Key::F12,
        Key::SwapLeft => iuv_proto::Key::SwapLeft,
        Key::SwapRight => iuv_proto::Key::SwapRight,
        Key::HideCandidate => iuv_proto::Key::HideCandidate,
    }
}

/// 核心 → 线上四态（iuv-server 有反向转换；此处 TSF 侧发送方向）。
fn wire_ime_state(s: &iuv_core::ImeState) -> WireImeState {
    WireImeState {
        mode: match s.mode {
            iuv_core::InitialMode::Chinese => iuv_proto::ImeMode::Chinese,
            iuv_core::InitialMode::English => iuv_proto::ImeMode::English,
        },
        width: match s.width {
            iuv_core::WidthMode::Half => iuv_proto::ImeWidth::Half,
            iuv_core::WidthMode::Full => iuv_proto::ImeWidth::Full,
        },
        script: match s.script {
            iuv_core::ScriptMode::Simplified => iuv_proto::ImeScript::Simplified,
            iuv_core::ScriptMode::Traditional => iuv_proto::ImeScript::Traditional,
        },
        punct: match s.punct {
            iuv_core::PunctMode::Chinese => iuv_proto::ImePunct::Chinese,
            iuv_core::PunctMode::English => iuv_proto::ImePunct::English,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iuv_core::Key;
    use iuv_proto::{Auth, BuildId, Caps, C2S};
    use iuv_win::transport::{ConnHandler, Reply, ServerConfig, Session, TransportServer};
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    struct EchoSession;

    impl Session for EchoSession {
        fn on_c2s(&mut self, req: C2S, reply: &mut Reply) {
            match req {
                C2S::Key { key, .. } => {
                    reply.respond(S2C::KeyResult(KeyVerdict::Consumed(KeyOutcome {
                        eaten: true,
                        composition: Some(format!("k:{key:?}")),
                        reading: None,
                        end: None,
                        candidates: None,
                        all_candidates: None,
                        page: None,
                        selected: None,
                    })));
                }
                C2S::Ping { nonce } => reply.respond(S2C::Pong { nonce }),
                _ => {}
            }
        }
    }

    struct Factory;

    impl ConnHandler for Factory {
        fn on_connect(&self, _client: &iuv_proto::ClientInfo, _caps: Caps) -> Box<dyn Session> {
            Box::new(EchoSession)
        }
    }

    /// 首键慢、后续即时（Deadline→degraded→full 重同步链路测试）。
    /// composition 回显 full 标志，供断言重同步真的发生。
    struct SlowFirstSession {
        first: AtomicBool,
    }

    impl Session for SlowFirstSession {
        fn on_c2s(&mut self, req: C2S, reply: &mut Reply) {
            if let C2S::Key { full, .. } = req {
                if self.first.swap(false, Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(300));
                }
                reply.respond(S2C::KeyResult(KeyVerdict::Consumed(KeyOutcome {
                    eaten: true,
                    composition: Some(format!("full={full}")),
                    reading: None,
                    end: None,
                    candidates: None,
                    all_candidates: None,
                    page: None,
                    selected: None,
                })));
            }
        }
    }

    struct SlowFactory;

    impl ConnHandler for SlowFactory {
        fn on_connect(&self, _client: &iuv_proto::ClientInfo, _caps: Caps) -> Box<dyn Session> {
            Box::new(SlowFirstSession {
                first: AtomicBool::new(true),
            })
        }
    }

    fn start_with(tag: &str, factory: Arc<dyn ConnHandler>) -> (String, TransportServer) {
        let pipe = format!(r"\\.\pipe\iuv-remote-test-{}-{tag}", std::process::id());
        let server = TransportServer::start(
            ServerConfig {
                pipe_name: pipe.clone(),
                auth: Auth([5u8; 32]),
                caps: Caps(Caps::UIELEMENT),
                build: BuildId("t".into()),
                max_connections: 4,
            },
            factory,
        )
        .expect("服务端");
        (pipe, server)
    }

    /// 直连一个 RemoteHandle 形状的连接（绕过进程级静态，专测去重/降级逻辑）。
    fn connect_handle(pipe: &str) -> RemoteHandle {
        let cfg = ClientConfig {
            pipe_name: pipe.to_string(),
            proto_min: iuv_proto::PROTO_MIN,
            proto_max: iuv_proto::PROTO_MAX,
            auth: Auth([5u8; 32]),
            caps: Caps(Caps::UIELEMENT),
            app: "remote-host-test".into(),
            resume: None,
            handshake_timeout: Duration::from_secs(5),
        };
        let (client, _ack, pushes) = connect(&cfg).expect("握手");
        std::thread::spawn(move || loop {
            if pushes.recv_timeout(Duration::from_secs(3600)).is_err() {
                return; // 连接关闭
            }
        });
        RemoteHandle {
            client: Mutex::new(Some(client)),
            config: Mutex::new(Config::default()),
            pending: Mutex::new(None),
            last_state: Mutex::new(None),
            degraded: AtomicBool::new(false),
            offline: AtomicBool::new(false),
            last_composition: Mutex::new(None),
        }
    }

    #[test]
    fn test_down_dedup_single_engine_advance() {
        let (pipe, _server) = start_with("dedup", Arc::new(Factory));
        let h = connect_handle(&pipe);
        let k = Key::Char('n');
        let mods = iuv_proto::Mods::default();

        // Test 阶段：发请求并缓存
        let t1 = h.key_test(k, mods).expect("Test 裁定");
        // 同键重复 Test：复用缓存（引擎只推进一次）
        let t2 = h.key_test(k, mods).expect("Test 裁定");
        assert_eq!(t1, t2);
        // Down：命中缓存零 IPC（值相同即视为复用）
        let d = h.key_down(k, mods).expect("Down 复用");
        assert_eq!(d, t1);
        // 缓存已消费：下一个 Down 是新请求（echo 形状相同，但缓存槽已空）
        let d2 = h.key_down(k, mods).expect("Down 新请求");
        assert_eq!(d2, t1);

        // 异键：Down 未命中现场处理
        let other = h.key_down(Key::Char('i'), mods).expect("新键");
        assert!(other.composition.as_deref().unwrap_or("").starts_with("k:"));
    }

    #[test]
    fn deadline_marks_degraded_then_full_resync() {
        let (pipe, _server) = start_with("slow", Arc::new(SlowFactory));
        let h = connect_handle(&pipe);
        let k = Key::Char('n');
        let mods = iuv_proto::Mods::default();

        // 首键：服务端 300ms > 截止 20ms → None（放行）+ degraded 标记
        assert_eq!(h.key_test(k, mods), None, "慢响应应超时放行");
        // 连接内串行处理：睡眠期间后续键也依次超时（真实形态），重同步在数键内完成；
        // degraded → full=true 强制全量（服务端回显 full 标志）。
        let mut o = None;
        for _ in 0..30 {
            if let Some(x) = h.key_test(k, mods) {
                o = Some(x);
                break;
            }
        }
        let o = o.expect("重同步应在数键内完成");
        assert_eq!(
            o.composition.as_deref(),
            Some("full=true"),
            "degraded 应触发全量重同步"
        );
        // 应答成功 → degraded 清除：Down 消费缓存后再 Test 新请求，应恢复正常增量（full=false）
        let d = h.key_down(k, mods).expect("Down 复用缓存");
        assert_eq!(d.composition.as_deref(), Some("full=true"));
        let o2 = h.key_test(k, mods).expect("应答");
        assert_eq!(
            o2.composition.as_deref(),
            Some("full=false"),
            "重同步后 degraded 应清除"
        );
    }

    #[test]
    fn missing_client_passes_through() {
        let (pipe, _server) = start_with("offline", Arc::new(Factory));
        let h = connect_handle(&pipe);
        // 模拟连接丢失（失效语义 A）：客户端槽清空 → 全部放行，绝不 panic/阻塞
        *h.client.lock().unwrap_or_else(|e| e.into_inner()) = None;
        assert_eq!(h.key_test(Key::Char('n'), iuv_proto::Mods::default()), None);
        assert_eq!(h.key_down(Key::Char('n'), iuv_proto::Mods::default()), None);
        assert_eq!(
            h.pending_raw_text(),
            None,
            "无 composition 时原文上屏应为 None（走 cancel 分支）"
        );
    }
}
