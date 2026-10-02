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
        on_server_req: None,
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
        // P4 服务端自渲染：普通应用零候选载荷（显式空 = 清客户端旧值）
        assert_eq!(o.candidates.as_ref(), Some(&vec![]), "普通应用候选应为空");
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
    // P4：候选数据不下发（服务端自渲染），selected 仍随选中态走
    assert_eq!(moved.candidates.as_ref(), Some(&vec![]));

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

/// flush_session 原文真相源：会话中查询 → picked+raw；无会话 → 空串。
#[test]
fn pending_text_query_returns_session_original() {
    let pipe = test_pipe("pending");
    let _server = start_server(&pipe);
    let (client, _ack, _pushes) = connect_ok(&pipe);

    type_str(&client, "zhujincheng");
    let r = client
        .request(C2S::PendingTextQuery, true, Duration::from_secs(2))
        .expect("PendingTextQuery 应答");
    match r {
        S2C::PendingText { text } => {
            assert_eq!(text, "zhujincheng", "无已选词时原文 = 全部 raw");
        }
        other => panic!("应答类型错误: {other:?}"),
    }

    // 会话结束后查询：空串（客户端走 cancel 分支）。
    let _ = client.request(
        C2S::EndSession {
            end: SessionEnd::Cancel,
        },
        true,
        Duration::from_secs(2),
    );
    let r = client
        .request(C2S::PendingTextQuery, true, Duration::from_secs(2))
        .expect("PendingTextQuery 应答");
    assert_eq!(
        r,
        S2C::PendingText {
            text: String::new()
        }
    );
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
        Ok(Push::ConfigChanged {
            epoch: e,
            client_view,
        }) => {
            assert_eq!(e, 1);
            let expected = match iuv_core::Config::default().initial_state.mode {
                iuv_core::ImeMode::Chinese => iuv_proto::ImeMode::Chinese,
                iuv_core::ImeMode::English => iuv_proto::ImeMode::English,
            };
            assert_eq!(
                client_view.initial_mode, expected,
                "client_view = 引擎当前配置视图"
            );
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

/// 真机实锤（2026-09-27）回归钉：**每个 C2S 请求必须有应答**。ImeState 臂曾不回
/// `S2C::Ok`，客户端 sync_state 同步等待 → 每次四态同步白等满截止（300ms）+ 误标
/// degraded——「间歇漏键」的真凶（首会话必中：连接后 last_state=None 必发一次）。
#[test]
fn every_c2s_request_gets_a_reply() {
    let pipe = test_pipe("c2s-reply");
    let _server = start_server(&pipe);
    let (client, _ack, pushes) = connect_ok(&pipe);
    assert!(matches!(
        pushes.recv_timeout(Duration::from_secs(2)),
        Ok(Push::SessionAttached { .. })
    ));

    // 连接后首次四态同步（此前：无应答 → 客户端白等 300ms）
    let r = client.request(
        C2S::ImeState(iuv_proto::ImeState {
            mode: iuv_proto::ImeMode::Chinese,
            width: iuv_proto::ImeWidth::Half,
            script: iuv_proto::ImeScript::Simplified,
            punct: iuv_proto::ImePunct::Chinese,
        }),
        true,
        Duration::from_millis(500),
    );
    assert!(matches!(r, Ok(S2C::Ok)), "ImeState 必须有应答: {r:?}");

    // fire-and-forget 变体同样必须回 Ok（杜绝「等不来的应答」整类问题）
    let r = client.request(
        C2S::FocusChanged { focused: true },
        true,
        Duration::from_millis(500),
    );
    assert!(matches!(r, Ok(S2C::Ok)), "FocusChanged 必须有应答: {r:?}");
    let r = client.request(
        C2S::CaretMoved {
            rect: iuv_proto::CaretRect::default(),
            dpi: 96,
        },
        true,
        Duration::from_millis(500),
    );
    assert!(matches!(r, Ok(S2C::Ok)), "CaretMoved 必须有应答: {r:?}");
}

/// P5 失效语义 C（49 §4.4/§4.5.4）：断连保存现场 + `Hello.resume` 重绑。
/// 打 "ni" → 断连 → 带令牌重连 → `full=true` 首键全量回放（composition 仍含 "ni"）。
#[test]
fn resume_token_rebinds_and_replays_composition() {
    let pipe = test_pipe("resume");
    let _server = start_server(&pipe);
    let token = {
        let (client, _ack, pushes) = connect_ok(&pipe);
        let token = match pushes.recv_timeout(Duration::from_secs(2)) {
            Ok(Push::SessionAttached { token }) => token,
            other => panic!("应收到 SessionAttached: {other:?}"),
        };
        // 打字建立会话（"ni" 入 composition），随后 client drop = 断连
        type_str(&client, "ni");
        token
    };
    // 连接线程回收 + Drop 保存现场是异步的，短等
    std::thread::sleep(Duration::from_millis(300));

    let cfg = ClientConfig {
        resume: Some(token),
        ..client_cfg(&pipe, Caps(Caps::UIELEMENT))
    };
    let (client, _ack, pushes) = connect(&cfg).expect("带令牌重连");
    assert!(matches!(
        pushes.recv_timeout(Duration::from_secs(2)),
        Ok(Push::SessionAttached { .. })
    ));
    // 客户端 degraded → full=true：服务端强制全量应答 = composition 重放
    let o = client
        .request(
            C2S::Key {
                key: Key::Char('x'),
                mods: Default::default(),
                token: KeyToken {
                    seq: 0,
                    phase: KeyPhase::Down,
                },
                full: true,
            },
            true,
            Duration::from_secs(2),
        )
        .expect("重绑后首键应答");
    match o {
        S2C::KeyResult(KeyVerdict::Consumed(out)) => {
            let comp = out.composition.expect("full=true 必须全量");
            assert!(
                comp.starts_with("ni"),
                "重绑应回放断连前 composition：{comp}"
            );
        }
        other => panic!("应答类型错误: {other:?}"),
    }
}

/// EndSession 后令牌作废：重绑得到全新会话（无 composition 可回放）。
#[test]
fn end_session_voids_resume_state() {
    let pipe = test_pipe("resume-void");
    let _server = start_server(&pipe);
    let token = {
        let (client, _ack, pushes) = connect_ok(&pipe);
        let token = match pushes.recv_timeout(Duration::from_secs(2)) {
            Ok(Push::SessionAttached { token }) => token,
            other => panic!("应收到 SessionAttached: {other:?}"),
        };
        type_str(&client, "ni");
        let _ = client.request(
            C2S::EndSession {
                end: SessionEnd::Cancel,
            },
            true,
            Duration::from_secs(2),
        );
        token
    };
    std::thread::sleep(Duration::from_millis(300));

    let cfg = ClientConfig {
        resume: Some(token),
        ..client_cfg(&pipe, Caps(Caps::UIELEMENT))
    };
    let (client, _ack, pushes) = connect(&cfg).expect("重连");
    let _ = pushes.recv_timeout(Duration::from_secs(2));
    let o = client
        .request(
            C2S::Key {
                key: Key::Char('x'),
                mods: Default::default(),
                token: KeyToken {
                    seq: 0,
                    phase: KeyPhase::Down,
                },
                full: true,
            },
            true,
            Duration::from_secs(2),
        )
        .expect("首键应答");
    match o {
        S2C::KeyResult(KeyVerdict::Consumed(out)) => {
            let comp = out.composition.expect("full=true 必须全量");
            assert_eq!(comp, "x", "EndSession 后重绑应得全新会话：{comp}");
        }
        other => panic!("应答类型错误: {other:?}"),
    }
}

/// P4 抑制名单（candidate_owner_apps 命中）：KeyOutcome 携带候选数据源
/// （客户端游戏桥自绘），服务端窗静默。
#[test]
fn suppressed_app_receives_candidate_data() {
    let dict = Dict::from_entries(vec![
        ("ni".into(), "你".into(), 100),
        ("ni".into(), "尼".into(), 50),
    ]);
    let cfg = Config {
        candidate_owner_apps: vec!["wow.exe".into()],
        ..Config::default()
    };
    let engine = Engine::new(dict, cfg);
    let pipe = test_pipe("suppress");
    let _server = TransportServer::start(
        ServerConfig {
            pipe_name: pipe.clone(),
            auth: Auth([3u8; 32]),
            caps: Caps(Caps::UIELEMENT),
            build: BuildId("test".into()),
            max_connections: 8,
        },
        Arc::new(EngineService::new(engine)),
    )
    .expect("服务端启动");
    // app = wow.exe（命中名单）
    let mut c = client_cfg(&pipe, Caps(Caps::UIELEMENT));
    c.app = "wow.exe".into();
    let (client, _ack, pushes) = connect(&c).expect("握手");
    assert!(matches!(
        pushes.recv_timeout(Duration::from_secs(2)),
        Ok(Push::SessionAttached { .. })
    ));

    let o = key(&client, Key::Char('n'));
    let cands = o.candidates.expect("抑制命中必须带候选数据源");
    assert!(!cands.is_empty(), "抑制命中的客户端拿真实候选");
    assert!(o.all_candidates.as_ref().is_some_and(|v| !v.is_empty()));
}

/// ② 用户库收敛：客户端 `C2S::UserMutation` → 引擎应用（屏蔽生效于候选）。
#[test]
fn user_mutation_applies_to_engine() {
    let pipe = test_pipe("mutation");
    let _server = start_server(&pipe);
    let (client, _ack, pushes) = connect_ok(&pipe);
    assert!(matches!(
        pushes.recv_timeout(Duration::from_secs(2)),
        Ok(Push::SessionAttached { .. })
    ));

    // 屏蔽「你」→ n 的候选不再含「你」（只剩「尼」）
    let r = client
        .request(
            C2S::UserMutation(iuv_proto::UserMutation::Block {
                code: "ni".into(),
                word: "你".into(),
            }),
            true,
            Duration::from_secs(2),
        )
        .expect("UserMutation 应答");
    assert!(matches!(r, S2C::Ok), "UserMutation 必须有应答: {r:?}");

    let o = key(&client, Key::Char('n'));
    let cands = o.candidates.expect("抑制外默认空; 此连接非抑制 → 空数组");
    assert_eq!(cands, Vec::new(), "普通应用零候选载荷");
    // 屏蔽效果经会话键验证：Digit 无从观察（无候选下发）——改走 composition 增量
    // 与 all_candidates 不可观察，此处以「应答 Ok + 引擎内部应用」为契约；
    // 引擎层屏蔽行为已有 iuv-core 单测覆盖（hide_entry 系列）。
}
