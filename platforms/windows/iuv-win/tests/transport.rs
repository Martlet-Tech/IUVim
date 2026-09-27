//! transport 集成测试（49 §5 P2 出口条件）：双侧互连、版本不匹配明确报错、
//! 未认证连接被拒、推送送达、请求超时与恢复、并发多路复用。

use std::sync::Arc;
use std::time::Duration;

use iuv_proto::{
    Auth, BuildId, Caps, KeyOutcome, KeyPhase, KeyToken, KeyVerdict, ProtoError, Push, ResumeToken,
    C2S, PROTO_MAX, PROTO_MIN, S2C,
};
use iuv_win::transport::{
    connect, ClientConfig, ConnHandler, Reply, ServerConfig, Session, TransportError,
    TransportServer,
};

fn test_pipe(tag: &str) -> String {
    format!(r"\\.\pipe\iuv-transport-{}-{tag}", std::process::id())
}

fn test_auth() -> Auth {
    Auth([9u8; 32])
}

fn client_cfg(pipe: &str) -> ClientConfig {
    ClientConfig {
        pipe_name: pipe.to_string(),
        proto_min: PROTO_MIN,
        proto_max: PROTO_MAX,
        auth: test_auth(),
        caps: Caps(Caps::UIELEMENT),
        app: "test.exe".into(),
        resume: None,
        handshake_timeout: Duration::from_secs(5),
    }
}

/// 回声会话：Ping→Pong、Key→eaten、UserMutation→Ok。
struct EchoSession;

impl Session for EchoSession {
    fn on_c2s(&mut self, req: C2S, reply: &mut Reply) {
        match req {
            C2S::Ping { nonce } => reply.respond(S2C::Pong { nonce }),
            C2S::Key { .. } => reply.respond(S2C::KeyResult(KeyVerdict::Consumed(KeyOutcome {
                eaten: true,
                composition: None,
                reading: None,
                end: None,
                candidates: None,
                page: None,
                selected: None,
            }))),
            C2S::UserMutation(_) => reply.respond(S2C::Ok),
            _ => reply.respond(S2C::Err(ProtoError::Malformed {
                offset: 0,
                reason: "EchoSession 未定义".into(),
            })),
        }
    }
}

/// 慢会话：Ping 延迟后应答（超时测试用）。
struct SlowSession {
    delay: Duration,
}

impl Session for SlowSession {
    fn on_c2s(&mut self, req: C2S, reply: &mut Reply) {
        if let C2S::Ping { nonce } = req {
            std::thread::sleep(self.delay);
            reply.respond(S2C::Pong { nonce });
        }
    }
}

struct Factory;

impl ConnHandler for Factory {
    fn on_connect(&self, _client: &iuv_proto::ClientInfo, _caps: Caps) -> Box<dyn Session> {
        Box::new(EchoSession)
    }
}

struct SlowFactory(Duration);

impl ConnHandler for SlowFactory {
    fn on_connect(&self, _client: &iuv_proto::ClientInfo, _caps: Caps) -> Box<dyn Session> {
        Box::new(SlowSession { delay: self.0 })
    }
}

fn start_echo_server(pipe: &str) -> TransportServer {
    TransportServer::start(
        ServerConfig {
            pipe_name: pipe.to_string(),
            auth: test_auth(),
            caps: Caps(Caps::UIELEMENT | Caps::PET),
            build: BuildId("test-build".into()),
            max_connections: 8,
        },
        Arc::new(Factory),
    )
    .expect("服务端启动")
}

fn connect_ok(
    pipe: &str,
) -> (
    iuv_win::transport::TransportClient,
    iuv_win::transport::HelloAck,
    iuv_win::transport::PushStream,
) {
    connect(&client_cfg(pipe)).expect("客户端握手")
}

// ---------- P2 出口 1：双侧互连 + 握手协商 ----------

#[test]
fn handshake_and_session_attached_push() {
    let pipe = test_pipe("ok");
    let server = start_echo_server(&pipe);
    let (client, ack, pushes) = connect_ok(&pipe);

    assert_eq!(ack.proto, PROTO_MAX.min(PROTO_MIN.max(PROTO_MIN)));
    assert_eq!(ack.proto, PROTO_MIN, "双方都支持 [1,1] → 定版 1");
    assert_eq!(ack.build.0, "test-build");
    // caps 取交集：客户端只请求 UIELEMENT → ack 不含 PET
    assert!(ack.caps.has(Caps::UIELEMENT));
    assert!(!ack.caps.has(Caps::PET));

    // 会话建立推送必达（SessionAttached）
    let push = pushes
        .recv_timeout(Duration::from_secs(2))
        .expect("SessionAttached 应送达");
    assert!(matches!(push, Push::SessionAttached { token: ResumeToken(t) } if t != 0));

    assert!(!client.is_closed());
    server.stop();
}

#[test]
fn ping_pong_roundtrip_and_hot_path_key() {
    let pipe = test_pipe("echo");
    let _server = start_echo_server(&pipe);
    let (client, _ack, _pushes) = connect_ok(&pipe);

    let pong = client
        .request(C2S::Ping { nonce: 42 }, false, Duration::from_secs(2))
        .expect("Ping 应答");
    assert_eq!(pong, S2C::Pong { nonce: 42 });

    let hot = client
        .request(
            C2S::Key {
                key: iuv_proto::Key::Char('n'),
                mods: Default::default(),
                token: KeyToken {
                    seq: 1,
                    phase: KeyPhase::Test,
                },
                full: false,
            },
            true,
            Duration::from_secs(2),
        )
        .expect("Key 应答");
    match hot {
        S2C::KeyResult(KeyVerdict::Consumed(o)) => assert!(o.eaten),
        other => panic!("热路径应答错误: {other:?}"),
    }

    let ok = client
        .request(
            C2S::UserMutation(iuv_proto::UserMutation::Set {
                code: "t".into(),
                word: "测".into(),
                adj: 1,
            }),
            false,
            Duration::from_secs(2),
        )
        .expect("UserMutation 应答");
    assert_eq!(ok, S2C::Ok);
}

// ---------- P2 出口 2：版本不匹配有明确报错 ----------

#[test]
fn version_mismatch_is_typed_error() {
    let pipe = test_pipe("ver");
    let _server = start_echo_server(&pipe);
    let mut cfg = client_cfg(&pipe);
    // 客户端只支持 99-100，服务端 [1,1] → 无交集
    cfg.proto_min = 99;
    cfg.proto_max = 100;
    match connect(&cfg) {
        Err(TransportError::Proto(ProtoError::VersionMismatch { client, server })) => {
            assert_eq!((client, server), (99, PROTO_MIN));
        }
        other => panic!("应报 VersionMismatch, got {:?}", other.map(|_| ())),
    }
}

// ---------- P2 出口 3：未认证连接被拒 ----------

#[test]
fn wrong_auth_rejected() {
    let pipe = test_pipe("auth");
    let _server = start_echo_server(&pipe);
    let mut cfg = client_cfg(&pipe);
    cfg.auth = Auth([1u8; 32]); // 密钥不符
    match connect(&cfg) {
        Err(TransportError::Proto(ProtoError::Unauthenticated)) => {}
        other => panic!("应报 Unauthenticated, got {:?}", other.map(|_| ())),
    }
}

// ---------- 推送 / 超时 / 并发 ----------

#[test]
fn request_deadline_then_recover() {
    let pipe = test_pipe("slow");
    let server = TransportServer::start(
        ServerConfig {
            pipe_name: pipe.clone(),
            auth: test_auth(),
            caps: Caps(0),
            build: BuildId("slow".into()),
            max_connections: 4,
        },
        Arc::new(SlowFactory(Duration::from_millis(300))),
    )
    .expect("慢服务端启动");
    let (client, _ack, _pushes) = connect_ok(&pipe);

    // 50ms 截止 < 300ms 服务端延迟 → Deadline
    let err = client
        .request(C2S::Ping { nonce: 1 }, false, Duration::from_millis(50))
        .expect_err("应超时");
    assert!(matches!(err, TransportError::Deadline));

    // 恢复：下一请求正常（迟到应答被丢弃，不串号）
    let pong = client
        .request(C2S::Ping { nonce: 2 }, false, Duration::from_secs(2))
        .expect("超时后应恢复");
    assert_eq!(pong, S2C::Pong { nonce: 2 });
    server.stop();
}

#[test]
fn concurrent_requests_multiplexed() {
    let pipe = test_pipe("multi");
    let _server = start_echo_server(&pipe);
    let (client, _ack, _pushes) = connect_ok(&pipe);

    // 4 线程同连一条连接并发请求（stream_id 偶数号复用；应答不串号）
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let c = client.clone();
            std::thread::spawn(move || {
                let r = c
                    .request(C2S::Ping { nonce: i }, false, Duration::from_secs(3))
                    .expect("并发请求应答");
                assert_eq!(r, S2C::Pong { nonce: i });
            })
        })
        .collect();
    for h in handles {
        h.join().expect("并发线程");
    }
}

#[test]
fn max_connections_enforced() {
    let pipe = test_pipe("cap");
    let server = TransportServer::start(
        ServerConfig {
            pipe_name: pipe.clone(),
            auth: test_auth(),
            caps: Caps(0),
            build: BuildId("cap".into()),
            max_connections: 1,
        },
        Arc::new(Factory),
    )
    .expect("上限服务端启动");
    let (_c1, _a1, _p1) = connect_ok(&pipe);
    // 第 2 条连接：握手发不出/收不到应答 → Closed/Deadline 类错误（连接被服务端关闭）
    let mut cfg = client_cfg(&pipe);
    cfg.handshake_timeout = Duration::from_millis(1500);
    match connect(&cfg) {
        Err(TransportError::Closed)
        | Err(TransportError::Deadline)
        | Err(TransportError::Io(_)) => {}
        Ok(_) => panic!("超出连接上限应被拒绝"),
        Err(e) => panic!("意外错误: {e}"),
    }
    server.stop();
}
