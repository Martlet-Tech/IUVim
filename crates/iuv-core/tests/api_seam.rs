//! 顶层引擎接口（api.rs）契约测试。39-rime-pipeline.md §Step1。

use iuv_core::api::{EngineCtx, ImeEngine, PendingInput};
use iuv_core::{Config, Engine, Key, RimeEngine};
use std::sync::Arc;

fn ctx() -> EngineCtx<'static> {
    EngineCtx { preceding_text: "" }
}

fn jian_dict() -> iuv_data::Dict {
    // 吉安权重最高（词频重排后方案[0]=[ji,an]），间/见为单音节词条
    iuv_data::Dict::from_entries(vec![
        ("ji'an".into(), "吉安".into(), 6091),
        ("jian".into(), "间".into(), 5000),
        ("jian".into(), "见".into(), 800),
        ("jian".into(), "件".into(), 300),
        ("ni'hao".into(), "你好".into(), 8000),
    ])
}

/// 接口契约测的是 rime 核心（classic 已删，rime 为唯一 ImeEngine 实现）。
fn rime_engine(dict: iuv_data::Dict) -> Arc<RimeEngine> {
    RimeEngine::new(Arc::new(dict), &Config::default())
}

/// 输入方向①：待输入串 → 分段视图 + 候选列表。
#[test]
fn translate_returns_segmentation_and_candidates() {
    let e = rime_engine(jian_dict());
    let tr = e.translate(&ctx(), &PendingInput { raw: "nihao" });
    assert_eq!(tr.segmentation.len(), 1, "rime 打字期单活动段：segmentation 恒单段");
    assert_eq!(tr.segmentation[0].syllables, vec!["ni", "hao"]);
    assert!(tr
        .candidates
        .iter()
        .any(|c| c.text == "你好"), "nihao 应出「你好」");
}

/// 输入方向②：高亮候选 → 预编辑跟随候选切分（管理员点名的快赢：
/// `jian` 导航到吉安时预编辑必须显示 `ji'an`）。
#[test]
fn preedit_follows_highlighted_candidate() {
    let e = rime_engine(jian_dict());
    let tr = e.translate(&ctx(), &PendingInput { raw: "jian" });
    let jian = tr
        .candidates
        .iter()
        .find(|c| c.text == "吉安")
        .expect("吉安应在候选中");
    assert_eq!(
        e.preedit(&ctx(), &PendingInput { raw: "jian" }, Some(jian)),
        "ji'an",
        "预编辑应跟随候选切分为 ji'an"
    );
    // 无高亮：返回方案重排后的默认切分（吉安权重最高 → [ji,an] 排方案[0]）
    assert_eq!(
        e.preedit(&ctx(), &PendingInput { raw: "jian" }, None),
        "ji'an",
    );
}

/// 会话层端到端：键入 jian、导航到吉安，effect().composition 显示 ji'an。
#[test]
fn session_composition_updates_on_navigation() {
    let e = Engine::new(jian_dict(), Config::default());
    let mut s = e.start_session();
    for c in "jian".chars() {
        s.on_key(Key::Char(c));
    }
    // 逐个候选导航，命中吉安时断言预编辑
    let mut hit = false;
    for _ in 0..10 {
        let eff = s.effect();
        if eff.candidates.is_empty() {
            break;
        }
        let selected_text = eff.candidates[eff.selected].text.clone();
        if selected_text == "吉安" {
            assert_eq!(eff.composition, "ji'an", "导航到吉安应显 ji'an");
            hit = true;
            break;
        }
        s.on_key(Key::Right);
    }
    assert!(hit, "翻页应可达吉安");
}

/// 强制撇号不跟随候选（规则 1）：raw 含 `'` 时恒按输入切分显示。
#[test]
fn preedit_respects_user_apostrophe() {
    let e = rime_engine(jian_dict());
    let tr = e.translate(&ctx(), &PendingInput { raw: "ji'an" });
    let jian = tr
        .candidates
        .iter()
        .find(|c| c.text == "吉安")
        .expect("吉安应在候选中");
    assert_eq!(
        e.preedit(&ctx(), &PendingInput { raw: "ji'an" }, Some(jian)),
        "ji'an",
        "用户撇号参与分节，显示不变"
    );
}
