//! Step3 双引擎开关集成测试（39-rime-pipeline.md §6）。
//! 模拟 TSF load_engine 装配路径：config.engine == Rime → attach_core → 会话走 rime 核心。

use iuv_core::config::EngineChoice;
use iuv_core::{Config, Engine, Key, RimeEngine, Session, SessionEnd};

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

/// 39-rime-pipeline.md §6 回归：degua 长句逐词上屏不丢字（2026-08-26 实测丢 xi 的
/// 根因 = 续接态 raw 带撇号导致 origins/边界表错位，修复后钉死）。
#[test]
fn rime_deguo_flow_no_segment_loss() {
    let dict = iuv_data::Dict::from_entries(vec![
        ("de".into(), "的".into(), 15378475),
        ("de'guo".into(), "德国".into(), 22806),
        ("de'guo".into(), "得过".into(), 1227),
        ("guo'lao'shi".into(), "郭老师".into(), 300),
        ("lao'shi".into(), "老师".into(), 130337),
        ("xi'huan".into(), "喜欢".into(), 9000),
        ("huan".into(), "换".into(), 800),
        ("chi".into(), "吃".into(), 30000),
        ("shui'guo".into(), "水果".into(), 5000),
        ("chi'shui'guo".into(), "吃水果".into(), 400),
    ]);
    let cfg = Config { engine: EngineChoice::Rime, ..Config::default() };
    let engine = Engine::new(dict, cfg);
    let rime = RimeEngine::new(engine.shared_dict(), &engine.config());
    engine.attach_core(rime);

    let mut s = engine.start_session();
    for c in "deguolaoshixihuanchishuiguo".chars() {
        s.on_key(Key::Char(c));
    }
    // 选 德国（数字键按当前页定位）
    pick(&mut s, "德国");
    let e = s.effect();
    assert!(
        e.composition.starts_with("德国"),
        "选德国后组合文本应以德国开头：{}",
        e.composition
    );
    // 选 老师
    pick(&mut s, "老师");
    let e = s.effect();
    assert!(
        e.composition.starts_with("德国老师") && e.composition.contains("xi'huan"),
        "选老师后尾巴应从 xi 继续（不吞 xi）：{}",
        e.composition
    );
    // 尾巴 xihuan'chishuiguo 上：句通道应恢复，喜欢吃水果 可达
    let texts: Vec<String> = e.candidates.iter().map(|c| c.text.clone()).collect();
    assert!(
        texts.iter().any(|t| t.contains("喜欢")),
        "喜欢必须可达（不得只剩 huan 单字汤）：{texts:?}"
    );
    // 吃水果 收尾
    if let Some(i) = e.candidates.iter().position(|c| c.text == "吃水果") {
        s.on_key(Key::Digit((i + 1) as u8));
    } else {
        s.on_key(Key::Space); // 整句候选直接收尾
    }
    let e = s.effect();
    match e.end {
        Some(SessionEnd::Commit(t)) => {
            assert_eq!(t, "德国老师喜欢吃水果", "最终上屏：{t}");
        }
        other => panic!("应提交完成，实际 {other:?} / comp={}", e.composition),
    }
}

fn pick(s: &mut iuv_core::Session, text: &str) {
    let e = s.effect();
    if let Some(i) = e.candidates.iter().position(|c| c.text == text) {
        s.on_key(Key::Digit((i + 1) as u8));
        return;
    }
    panic!("候选中找不到 {text}：{:?}", e.candidates.iter().map(|c| &c.text).take(9).collect::<Vec<_>>());
}

/// 39-rime-pipeline.md §6/§13 回归：用户**独有**词条对 rime 游标探针可见
/// （2026-08-26 实测：野猪皮 仅存于用户库、基础库无此码 → has_code 只查基础库
/// 返回 false → 永不收集。修复 = Dict::has_code/has_prefix 兼查 user()）。
#[test]
fn rime_user_only_word_visible() {
    use iuv_data::UserDict;
    // 基础库不含 ye'zhu'pi；用户库注入 8000 权重的 野猪皮
    let dict = iuv_data::Dict::from_entries(vec![
        ("ye'zhu".into(), "野猪".into(), 5000),
        ("ye'zhu".into(), "业主".into(), 9000),
        ("ye".into(), "也".into(), 30000),
        ("pi".into(), "皮".into(), 1000),
    ]);
    dict.set_user(std::sync::Arc::new(
        UserDict::empty().set_entry("ye'zhu'pi", "野猪皮", 8000),
    ));
    let cfg = Config { engine: EngineChoice::Rime, ..Config::default() };
    let engine = Engine::new(dict, cfg);
    let rime = RimeEngine::new(engine.shared_dict(), &engine.config());
    engine.attach_core(rime);

    let mut s = engine.start_session();
    for c in "yezhupi".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.effect();
    let texts: Vec<String> = e.candidates.iter().map(|c| c.text.clone()).collect();
    assert!(
        texts.iter().any(|t| t == "野猪皮"),
        "用户独有词 野猪皮 必须可达（游标探针须感知用户库）：{texts:?}"
    );
}
