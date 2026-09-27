//! iuv-proto 契约测试：往返 / 拒绝 / 预算（49 §5 P1 出口条件）。

use iuv_proto::*;

// ---------- 样本构造 ----------

fn key_samples() -> Vec<Key> {
    vec![
        Key::Char('a'),
        Key::ShiftChar('A'),
        Key::Char('，'),
        Key::Backspace,
        Key::Space,
        Key::Enter,
        Key::Esc,
        Key::Digit(3),
        Key::Tab,
        Key::Delete,
        Key::Home,
        Key::End,
        Key::Insert,
        Key::PageUp,
        Key::PageDown,
        Key::Up,
        Key::Down,
        Key::Left,
        Key::Right,
        Key::F1,
        Key::F12,
        Key::SwapLeft,
        Key::SwapRight,
        Key::HideCandidate,
    ]
}

fn c2s_samples() -> Vec<C2S> {
    vec![
        C2S::Hello {
            proto_min: PROTO_MIN,
            proto_max: PROTO_MAX,
            auth: Auth([7u8; 32]),
            caps: Caps(Caps::UIELEMENT),
            resume: Some(ResumeToken(42)),
            client: ClientInfo {
                pid: 1234,
                tid: 5678,
                app: "weixin.exe".into(),
            },
        },
        C2S::Key {
            key: Key::Char('n'),
            mods: Mods {
                shift: false,
                ctrl: false,
                alt: true,
            },
            token: KeyToken {
                seq: 9,
                phase: KeyPhase::Test,
            },
            full: false,
        },
        C2S::EndSession {
            end: SessionEnd::Commit("你好".into()),
        },
        C2S::EndSession {
            end: SessionEnd::Cancel,
        },
        C2S::CaretMoved {
            rect: CaretRect {
                left: -1920,
                top: 100,
                right: -1800,
                bottom: 132,
            },
            dpi: 144,
        },
        C2S::FocusChanged { focused: false },
        C2S::ImeState(ImeState {
            mode: ImeMode::English,
            ..Default::default()
        }),
        C2S::SetMaintenance { on: true },
        C2S::UserMutation(UserMutation::Swap {
            a_code: "ni".into(),
            a_word: "你".into(),
            a_eff: 3,
            b_code: "li".into(),
            b_word: "李".into(),
            b_eff: 1,
        }),
        C2S::UserMutation(UserMutation::Set {
            code: "ceshi".into(),
            word: "测试".into(),
            adj: 5,
        }),
        C2S::UserMutation(UserMutation::Remove {
            code: "ceshi".into(),
            word: "测".into(),
        }),
        C2S::UserMutation(UserMutation::Block {
            code: "fa".into(),
            word: "方案".into(),
        }),
        C2S::CtlResult(CtlResult::Ok {
            state: ImeState {
                mode: ImeMode::English,
                width: ImeWidth::Full,
                script: ImeScript::Traditional,
                punct: ImePunct::English,
            },
        }),
        C2S::CtlResult(CtlResult::Err),
        C2S::Ping { nonce: 77 },
        C2S::Pong { nonce: 78 },
        C2S::Ok,
        C2S::Err(ProtoError::Deadline {
            op: "Key".into(),
            elapsed_ms: 25,
        }),
    ]
}

fn s2c_samples() -> Vec<S2C> {
    vec![
        S2C::HelloAck {
            proto: 1,
            caps: Caps(Caps::UIELEMENT | Caps::PET),
            server_build: BuildId("6b34917".into()),
        },
        S2C::KeyResult(KeyVerdict::Consumed(KeyOutcome {
            eaten: true,
            composition: Some("ni'hao".into()),
            reading: Some("ni'hao".into()),
            end: Some(SessionEnd::Commit("你好".into())),
            candidates: None,
            all_candidates: None,
            page: None,
            selected: None,
        })),
        S2C::KeyResult(KeyVerdict::Busy),
        S2C::Ok,
        S2C::Err(ProtoError::FrameTooLarge {
            len: 70000,
            max: 65536,
        }),
        S2C::Ctl {
            cmd: CtlCmd::SetScript(true),
        },
        S2C::Ping { nonce: 1 },
        S2C::Pong { nonce: 2 },
    ]
}

fn push_samples() -> Vec<Push> {
    let cand = |t: &str| Candidate {
        text: t.into(),
        kind: CandidateKind::Word,
    };
    vec![
        Push::SessionAttached {
            token: ResumeToken(u64::MAX),
        },
        Push::ImeState(ImeState::default()),
        Push::FocusBound { focused: true },
        Push::ConfigChanged {
            epoch: 3,
            client_view: ClientConfig {
                initial_mode: ImeMode::Chinese,
            },
        },
        Push::UserDictChanged { version: u32::MAX },
        Push::UiElement(Effect {
            composition: "ni'hao".into(),
            reading: "ni'hao".into(),
            candidates: vec![cand("你好"), cand("拟好")],
            all_candidates: vec![cand("你好"), cand("拟好"), cand("你号")],
            selected: 0,
            page: PageInfo {
                page: 0,
                page_count: 2,
                page_size: 5,
                total: 7,
            },
        }),
        Push::TypingActivity { active: false },
        Push::Shutdown { grace_ms: 500 },
    ]
}

// ---------- 往返 ----------

#[test]
fn roundtrip_all_c2s_s2c_push_variants() {
    for (i, s) in c2s_samples().iter().enumerate() {
        let frame = encode_frame(2, false, &Payload::ClientReq(s.clone()))
            .unwrap_or_else(|e| panic!("C2S #{i} 编码失败: {e:?}"));
        let (h, Payload::ClientReq(back)) = decode_frame(&frame).unwrap() else {
            panic!("C2S #{i} kind 错配");
        };
        assert_eq!(h.stream_id, 2);
        assert!(!h.urgent);
        assert_eq!(&back, s, "C2S #{i} 往返不一致");
    }
    for (i, s) in s2c_samples().iter().enumerate() {
        let frame = encode_frame(3, false, &Payload::ServerResp(s.clone()))
            .unwrap_or_else(|e| panic!("S2C #{i} 编码失败: {e:?}"));
        let (_, Payload::ServerResp(back)) = decode_frame(&frame).unwrap() else {
            panic!("S2C #{i} kind 错配");
        };
        assert_eq!(&back, s, "S2C #{i} 往返不一致");
    }
    for (i, s) in push_samples().iter().enumerate() {
        let frame = encode_frame(0, false, &Payload::Push(s.clone()))
            .unwrap_or_else(|e| panic!("Push #{i} 编码失败: {e:?}"));
        let (_, Payload::Push(back)) = decode_frame(&frame).unwrap() else {
            panic!("Push #{i} kind 错配");
        };
        assert_eq!(&back, s, "Push #{i} 往返不一致");
    }
}

#[test]
fn roundtrip_all_key_variants() {
    for k in key_samples() {
        let frame = encode_frame(
            0,
            false,
            &Payload::ClientReq(C2S::Key {
                key: k,
                mods: Mods::default(),
                token: KeyToken {
                    seq: 1,
                    phase: KeyPhase::Down,
                },
                full: true,
            }),
        )
        .unwrap();
        let (_, Payload::ClientReq(C2S::Key { key, .. })) = decode_frame(&frame).unwrap() else {
            panic!("{k:?} kind 错配");
        };
        assert_eq!(key, k);
    }
}

// ---------- 帧头线格式（锁死 wire 布局） ----------

#[test]
fn header_wire_layout_is_frozen() {
    let frame = encode_frame(
        0x0102,
        true,
        &Payload::Push(Push::FocusBound { focused: true }),
    )
    .unwrap();
    // 载荷 = 双层枚举变体索引 + 字段：Payload::Push(0x04) + FocusBound(0x02) + bool(0x01) = 3B
    assert_eq!(
        &frame[..8],
        &[0x03, 0x00, 0x00, 0x00, 0x04, 0x01, 0x02, 0x01]
    );
    assert_eq!(&frame[8..], &[0x04, 0x02, 0x01]);
}

#[test]
fn urgent_flag_and_stream_id_roundtrip() {
    let frame = encode_frame(
        0xBEEF,
        true,
        &Payload::ServerResp(S2C::KeyResult(KeyVerdict::Busy)),
    )
    .unwrap();
    let (h, _) = decode_frame(&frame).unwrap();
    assert!(h.urgent);
    assert_eq!(h.stream_id, 0xBEEF);
    assert_eq!(h.kind, FrameKind::ServerResp);
}

// ---------- 拒绝纪律（49 §4.8：拒整帧、不猜不截断） ----------

#[test]
fn reject_residual_bytes() {
    let mut frame =
        encode_frame(0, false, &Payload::Push(Push::FocusBound { focused: true })).unwrap();
    frame.push(0xAA);
    match decode_frame(&frame) {
        Err(ProtoError::Malformed { .. }) => {}
        other => panic!("残留字节应拒整帧, got {other:?}"),
    }
}

#[test]
fn reject_truncated_payload() {
    let mut frame =
        encode_frame(0, false, &Payload::Push(Push::Shutdown { grace_ms: 100 })).unwrap();
    frame.pop();
    assert!(matches!(
        decode_frame(&frame),
        Err(ProtoError::Malformed { .. })
    ));
}

#[test]
fn reject_truncated_header() {
    assert!(matches!(
        decode_frame(&[0u8; 7]),
        Err(ProtoError::Malformed { .. })
    ));
}

#[test]
fn reject_unknown_kind() {
    let mut frame =
        encode_frame(0, false, &Payload::Push(Push::FocusBound { focused: true })).unwrap();
    frame[4] = 0xFF;
    match decode_frame(&frame) {
        Err(ProtoError::UnknownTag { kind: 0xFF, .. }) => {}
        other => panic!("未知 kind 应报 UnknownTag, got {other:?}"),
    }
}

#[test]
fn reject_reserved_flag_bits() {
    let mut frame =
        encode_frame(0, false, &Payload::Push(Push::FocusBound { focused: true })).unwrap();
    frame[5] = 0x02;
    assert!(matches!(
        decode_frame(&frame),
        Err(ProtoError::Malformed { .. })
    ));
}

#[test]
fn reject_garbage_payload() {
    // 头合法（kind=4 PUSH, len=4），载荷全 0xFF → serde 变体索引非法。
    let mut frame = Vec::new();
    frame.extend_from_slice(&4u32.to_le_bytes());
    frame.extend_from_slice(&[0x04]); // kind = Push
    frame.extend_from_slice(&[0x00]); // flags
    frame.extend_from_slice(&[0x01, 0x02]); // stream_id
    frame.extend_from_slice(&[0xFF; 4]); // 载荷：非法变体索引
    assert!(matches!(
        decode_frame(&frame),
        Err(ProtoError::Malformed { .. })
    ));
}

#[test]
fn reject_oversize_payload_in_header() {
    let mut head = [0u8; 8];
    FrameHeader {
        kind: FrameKind::Push,
        urgent: false,
        stream_id: 0,
        payload_len: (MAX_PAYLOAD + 1) as u32,
    }
    .encode_into(&mut head);
    match FrameHeader::decode(&head) {
        Err(ProtoError::FrameTooLarge { len, max }) => {
            assert_eq!((len as usize, max as usize), (MAX_PAYLOAD + 1, MAX_PAYLOAD));
        }
        other => panic!("超限头应报 FrameTooLarge, got {other:?}"),
    }
}

#[test]
fn reject_oversize_payload_in_encode() {
    // ~70KiB 文本进 UiEffect 全量候选 → 编码侧超限拒绝。
    let big = Push::UiElement(Effect {
        composition: String::new(),
        reading: String::new(),
        candidates: vec![],
        all_candidates: vec![Candidate {
            text: "字".repeat(40_000),
            kind: CandidateKind::Sentence,
        }],
        selected: 0,
        page: PageInfo::default(),
    });
    match encode_frame(0, false, &Payload::Push(big)) {
        Err(ProtoError::FrameTooLarge { .. }) => {}
        other => panic!("超限载荷应拒, got {:?}", other.map(|_| ())),
    }
}

// ---------- 热路径预算（49 §4.5.3：每键几十字节） ----------

#[test]
fn hot_path_key_frame_is_tiny() {
    // 中文态普通字母键：最常见的热路径帧。
    let frame = encode_frame(
        0,
        true,
        &Payload::ClientReq(C2S::Key {
            key: Key::Char('n'),
            mods: Mods::default(),
            token: KeyToken {
                seq: 1,
                phase: KeyPhase::Test,
            },
            full: false,
        }),
    )
    .unwrap();
    assert!(frame.len() <= 24, "Key 请求帧应 ≤24B, got {}", frame.len());

    // 增量应答（composition None）：服务端 → 客户端。
    let frame = encode_frame(
        0,
        true,
        &Payload::ServerResp(S2C::KeyResult(KeyVerdict::Consumed(KeyOutcome {
            eaten: true,
            composition: None,
            reading: None,
            end: None,
            candidates: None,
            all_candidates: None,
            page: None,
            selected: None,
        }))),
    )
    .unwrap();
    assert!(
        frame.len() <= 24,
        "None 增量应答帧应 ≤24B, got {}",
        frame.len()
    );
}

// ---------- 版本协商（49 §4.4） ----------

#[test]
fn negotiate_takes_max_common() {
    assert_eq!(negotiate(1, 3, 2, 5).unwrap(), 3);
    assert_eq!(negotiate(1, 1, 1, 1).unwrap(), 1);
}

#[test]
fn negotiate_no_overlap_is_typed_error() {
    match negotiate(1, 2, 3, 4) {
        Err(ProtoError::VersionMismatch { client, server }) => {
            assert_eq!((client, server), (1, 3));
        }
        other => panic!("无交集应报 VersionMismatch, got {other:?}"),
    }
}

// ---------- stream_id 分配（49 §4.2：双向分家 + 回绕跳过在途） ----------

#[test]
fn client_even_server_odd() {
    let mut c = StreamIdAlloc::client();
    let mut s = StreamIdAlloc::server();
    for _ in 0..100 {
        assert_eq!(c.alloc().unwrap() % 2, 0);
        assert_eq!(s.alloc().unwrap() % 2, 1);
    }
}

#[test]
fn stream_id_wraps_across_zero() {
    // 起点贴着号段顶：65534 → 回绕 0，回绕前后都落在偶数段。
    let mut c = StreamIdAlloc::with_start(false, 65534);
    assert_eq!(c.alloc().unwrap(), 65534);
    assert_eq!(c.alloc().unwrap(), 0);
    c.release(65534);
    assert_eq!(c.in_flight(), 1);
}

#[test]
fn stream_id_skips_in_flight_on_wrap_and_exhausts_cleanly() {
    // 耗尽全部 32768 个偶数号：分配器返回 None（不 panic 兜底）；
    // 释放一个后可复用——复用路径要跨过整个在途号段（回绕 + 跳过在途的实测路径）。
    let mut c = StreamIdAlloc::client();
    let mut all = Vec::new();
    for _ in 0..32768 {
        all.push(c.alloc().expect("号段应足够 32768 个"));
    }
    assert!(c.alloc().is_none(), "号段耗尽应返回 None");
    let freed = all[12345];
    c.release(freed);
    assert_eq!(c.alloc().unwrap(), freed, "唯一空闲号应被跨段复用");
    assert!(c.alloc().is_none());
}
