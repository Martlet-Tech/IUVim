//! iuv-server 热路径无头测试（49 §5 P3 出口条件的可自动化部分）：
//! 合成按键序列经 transport 打到 EngineService，验证 composition/commit/增量语义/
//! 候选推送。「往返数 = 1/键」的去重属客户端逻辑（P3b），此处验证服务端侧契约。

use std::sync::Arc;
use std::time::Duration;

use iuv_core::{Config, Engine};
use iuv_data::Dict;
use iuv_proto::{
    Auth, BuildId, Caps, Key, KeyOutcome, KeyPhase, KeyToken, KeyVerdict, Push, SessionEnd, C2S,
    PROTO_MAX, PROTO_MIN, S2C,
};
use iuv_win::transport::{
    connect, ClientConfig, HelloAck, PushStream, ServerConfig, TransportClient, TransportServer,
};

use iuv_server::EngineService;

fn test_pipe(tag: &str) -> String {
    format!(r"\\.\pipe\iuv-server-{}-{tag}", std::process::id())
}

/// 小词典（02-conventions §4：引擎测试禁读真实词库）。
fn test_engine() -> Arc<Engine> {
    let dict = Dict::from_entries(vec![
        ("ni".into(), "你".into(), 100),
        ("ni".into(), "尼".into(), 50),
        ("ni'hao".into(), "你好".into(), 90),
    ]);
    Engine::new(dict, Config::default())
}

fn start_server(pipe: &str) -> TransportServer {
    TransportServer::start(
        ServerConfig {
            pipe_name: pipe.to_string(),
            auth: Auth([3u8; 32]),
            caps: Caps(Caps::UIELEMENT),
            build: BuildId("test".into()),
            max_connections: 8,
        },
        Arc::new(EngineService::new(test_engine())),
    )
    .expect("服务端启动")
}

fn client_cfg(pipe: &str, caps: Caps) -> ClientConfig {
    ClientConfig {
        pipe_name: pipe.to_string(),
        proto_min: PROTO_MIN,
        proto_max: PROTO_MAX,
        auth: Auth([3u8; 32]),
        caps,
        app: "hot-path-test".into(),
        resume: None,
        handshake_timeout: Duration::from_secs(5),
    }
}

fn connect_ok(pipe: &str) -> (TransportClient, HelloAck, PushStream) {
    connect(&client_cfg(pipe, Caps(Caps::UIELEMENT))).expect("客户端握手")
}

/// 发一个字母键并取回瘦身应答。
fn key(client: &TransportClient, k: Key) -> KeyOutcome {
    match client
        .request(
            C2S::Key {
                key: k,
                mods: Default::default(),
                token: KeyToken {
                    seq: 0,
                    phase: KeyPhase::Test,
                },
                full: false,
            },
            true,
            Duration::from_secs(2),
        )
        .expect("按键应答")
    {
        S2C::KeyResult(KeyVerdict::Consumed(o)) => o,
        other => panic!("按键应答类型错误: {other:?}"),
    }
}

fn type_str(client: &TransportClient, s: &str) -> Vec<KeyOutcome> {
    s.chars().map(|c| key(client, Key::Char(c))).collect()
}

#[test]
fn type_nihao_and_commit_via_space() {
    let pipe = test_pipe("commit");
    let _server = start_server(&pipe);
    let (client, _ack, pushes) = connect_ok(&pipe);
    // 连接建立推送先到（SessionAttached），先消费
    assert!(matches!(
        pushes.recv_timeout(Duration::from_secs(2)),
        Ok(Push::SessionAttached { .. })
    ));

    let outs = type_str(&client, "nihao");
    assert_eq!(outs[0].composition.as_deref(), Some("n"));
    assert_eq!(outs[1].composition.as_deref(), Some("ni"));
    assert_eq!(outs[4].composition.as_deref(), Some("ni'hao"));
    for (i, o) in outs.iter().enumerate() {
        assert!(o.eaten);
        assert!(o.end.is_none(), "第 {i} 键不应结束会话");
        assert!(o.candidates.as_ref().is_some_and(|c| !c.is_empty()));
        assert!(o.all_candidates.as_ref().is_some_and(|c| !c.is_empty()));
    }
    // 每键 UiElement 推送已裁撤（载荷实测顶破客户端截止）：全量候选单份走 KeyOutcome
    assert!(
        pushes.recv_timeout(Duration::from_millis(150)).is_err(),
        "不应再有推送"
    );

    // 空格提交首候选
    let last = key(&client, Key::Space);
    assert_eq!(last.end, Some(SessionEnd::Commit("你好".into())));
}

#[test]
fn composition_delta_and_candidate_move() {
    let pipe = test_pipe("delta");
    let _server = start_server(&pipe);
    let (client, _ack, _pushes) = connect_ok(&pipe);

    type_str(&client, "ni");
    // Right = 候选页内移动（首页 Left 夹紧 0，引擎语义）：composition 不变 → 增量 None
    let moved = key(&client, Key::Right);
    assert_eq!(moved.composition, None, "composition 未变应回 None（增量）");
    assert_eq!(moved.reading, None);
    assert_eq!(moved.selected, Some(1), "ni 有两个候选：你/尼");
    let cands = moved.candidates.expect("候选应随选中态重发");
    assert_eq!(cands[0].text, "你");
    assert_eq!(cands[1].text, "尼");

    // 继续输入：composition 变化 → 恢复全量
    let h = key(&client, Key::Char('h'));
    assert_eq!(h.composition.as_deref(), Some("ni'h"));
}

#[test]
fn full_flag_forces_full_composition() {
    let pipe = test_pipe("full");
    let _server = start_server(&pipe);
    let (client, _ack, _pushes) = connect_ok(&pipe);

    type_str(&client, "ni");
    // full=true：即使 composition 未变（Left 移动选中）也强制全量（基线失效重同步，49 §4.5.2）
    let moved = match client
        .request(
            C2S::Key {
                key: Key::Left,
                mods: Default::default(),
                token: KeyToken {
                    seq: 0,
                    phase: KeyPhase::Test,
                },
                full: true,
            },
            true,
            Duration::from_secs(2),
        )
        .expect("应答")
    {
        S2C::KeyResult(KeyVerdict::Consumed(o)) => o,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        moved.composition.as_deref(),
        Some("ni"),
        "full=true 强制全量"
    );
}

#[test]
fn end_session_message_and_restart() {
    let pipe = test_pipe("end");
    let _server = start_server(&pipe);
    let (client, _ack, _pushes) = connect_ok(&pipe);

    type_str(&client, "n");
    // 客户端显式结束会话（如 Esc 路径收尾）：服务端丢 Session
    let ack = client
        .request(
            C2S::EndSession {
                end: SessionEnd::Cancel,
            },
            false,
            Duration::from_secs(2),
        )
        .expect("EndSession 应答");
    assert_eq!(ack, S2C::Ok);

    // 新键开新会话：composition 从头开始（非"n"+旧尾巴）
    let fresh = key(&client, Key::Char('n'));
    assert_eq!(fresh.composition.as_deref(), Some("n"));
    assert!(fresh.end.is_none());
}

#[test]
fn cancel_via_esc_ends_session() {
    let pipe = test_pipe("esc");
    let _server = start_server(&pipe);
    let (client, _ack, _pushes) = connect_ok(&pipe);

    type_str(&client, "ni");
    let esc = key(&client, Key::Esc);
    assert_eq!(esc.end, Some(SessionEnd::Cancel), "Esc = 取消会话");
}

#[test]
fn no_uielement_caps_means_no_candidate_payload() {
    let pipe = test_pipe("nocand");
    let _server = start_server(&pipe);
    // caps = 0：无候选数据能力
    let (client, ack, pushes) = connect(&client_cfg(&pipe, Caps(0))).expect("握手");
    assert!(!ack.caps.has(Caps::UIELEMENT), "服务端能力 ∩ 客户端 0 = 空");

    // 连接建立推送（SessionAttached）先到；之后按键不应再有任何推送
    assert!(matches!(
        pushes.recv_timeout(Duration::from_secs(2)),
        Ok(Push::SessionAttached { .. })
    ));

    let o = key(&client, Key::Char('n'));
    assert_eq!(o.composition.as_deref(), Some("n"));
    assert!(o.candidates.is_none() && o.page.is_none() && o.selected.is_none());
    assert!(
        pushes.recv_timeout(Duration::from_millis(150)).is_err(),
        "无 UIELEMENT 能力不应收到任何推送"
    );
}

/// P4 配置热载（49 §5）：纪元变化 → 下一请求捎带 `Push::ConfigChanged`
///（latest-wins，同一纪元只推一次）；无变化零推送。
/// （真实 config_watch 的 mtime 监视是文件 IO，本机测试环境存量红——此处
/// 直接驱动纪元句柄，覆盖会话侧契约。）
#[test]
fn config_epoch_change_pushes_config_changed_once() {
    use std::sync::atomic::Ordering;

    let pipe = test_pipe("config-push");
    let service = Arc::new(EngineService::new(test_engine()));
    let epoch = service.config_epoch();
    let _server = TransportServer::start(
        ServerConfig {
            pipe_name: pipe.clone(),
            auth: Auth([3u8; 32]),
            caps: Caps(Caps::UIELEMENT),
            build: BuildId("test".into()),
            max_connections: 8,
        },
        service,
    )
    .expect("服务端启动");
    let (client, _ack, pushes) = connect_ok(&pipe);
    assert!(matches!(
        pushes.recv_timeout(Duration::from_secs(2)),
        Ok(Push::SessionAttached { .. })
    ));

    // 无变更：请求只回应答
    let _ = key(&client, Key::Char('n'));
    assert!(
        pushes.recv_timeout(Duration::from_millis(150)).is_err(),
        "无配置变更不应有推送"
    );

    // 纪元自增（真实服务里由 config_watch 驱动）：下一请求捎带 ConfigChanged
    epoch.store(1, Ordering::SeqCst);
    let _ = key(&client, Key::Char('i'));
    match pushes.recv_timeout(Duration::from_secs(2)) {
        Ok(Push::ConfigChanged { epoch: e, client_view }) => {
            assert_eq!(e, 1);
            let expected = match iuv_core::Config::default().initial_state.mode {
                iuv_core::InitialMode::Chinese => iuv_proto::ImeMode::Chinese,
                iuv_core::InitialMode::English => iuv_proto::ImeMode::English,
            };
            assert_eq!(client_view.initial_mode, expected, "client_view = 引擎当前配置视图");
        }
        other => panic!("应收到 ConfigChanged，实际 {other:?}"),
    }

    // 同一纪元不重复推（会话记住 seen_epoch）
    let _ = key(&client, Key::Char('h'));
    assert!(
        pushes.recv_timeout(Duration::from_millis(150)).is_err(),
        "同一纪元只推一次"
    );
}
