//! Step3 双引擎开关集成测试（39-rime-pipeline.md §6）。
//! 模拟 TSF load_engine 装配路径：config.engine == Rime → attach_core → 会话走 rime 核心。

use iuv_core::config::EngineChoice;
use iuv_core::{Config, Engine, Key, RimeEngine};

fn rime_wired() -> Arc<Engine> {
    let dict = iuv_data::Dict::from_entries(vec![
        ("ni'hao".into(), "你好".into(), 8000),
        ("ni".into(), "你".into(), 50000),
        ("hao".into(), "好".into(), 40000),
        ("xian".into(), "先".into(), 500),
        ("xi'an".into(), "西安".into(), 6091),
    ]);
    let cfg = Config {
        engine: EngineChoice::Rime,
        ..Config::default()
    };
    let engine = Engine::new(dict, cfg);
    let rime = RimeEngine::new(engine.shared_dict(), &engine.config());
    engine.attach_core(rime);
    engine
}

use std::sync::Arc;

/// config.engine=Rime 装配后，start_session 产出的会话走 rime 核心：
/// 候选带 score（rime 全量填充）、词优先无 Sentence（可靠精确词闸门）。
#[test]
fn engine_choice_rime_routes_to_rime_core() {
    let engine = rime_wired();
    let mut s = engine.start_session();
    for c in "nihao".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.effect();
    assert_eq!(e.candidates[0].text, "你好");
    assert!(
        e.candidates.iter().all(|c| c.kind != iuv_core::CandidateKind::Sentence),
        "rime 核心：可靠精确词在场不组句"
    );
}

/// 默认（Classic）装配行为不变：整句通道在场。
#[test]
fn engine_choice_default_classic_unchanged() {
    let dict = iuv_data::Dict::from_entries(vec![
        ("ni'hao".into(), "你好".into(), 8000),
        ("ni".into(), "你".into(), 50000),
        ("hao".into(), "好".into(), 40000),
    ]);
    let engine = Engine::new(dict, Config::default());
    assert_eq!(engine.config().engine, EngineChoice::Classic);
    let mut s = engine.start_session();
    for c in "nihao".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.effect();
    assert_eq!(e.candidates[0].text, "你好");
    assert_eq!(e.candidates[0].kind, iuv_core::CandidateKind::Sentence);
}

/// rime 核心下 M2 调权跨核心生效（共享 Dict）：交换后候选序变化。
#[test]
fn rime_core_shares_user_dict_swap() {
    use iuv_core::CandidateKind;
    let engine = rime_wired();
    let mut s = engine.start_session();
    for c in "xian".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.effect();
    let xi = e.candidates.iter().position(|c| c.text == "西安").unwrap();
    let _ = CandidateKind::Word;
    // 西安(6091) 与相邻候选调权一次后应移动位置
    s.on_key(if xi == 0 { Key::SwapRight } else { Key::SwapLeft });
    let e2 = s.effect();
    let xi2 = e2.candidates.iter().position(|c| c.text == "西安").unwrap();
    assert_ne!(xi, xi2, "调权后西安位置应变化：{} -> {}", xi, xi2);
}
