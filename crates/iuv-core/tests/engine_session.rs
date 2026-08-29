//! iuv-core 引擎与会话集成测试。任务书 11 §4 全部用例。
//! 用 Dict::from_entries 造小词典，不依赖真实词库文件。

use iuv_core::{
    Candidate, CandidateKind, Config, Engine, ImeState, Key, Session, SessionEnd, WidthMode,
};
use iuv_data::Dict;
use std::sync::Arc;

/// 任务书建议 fixture。
pub fn fixture_dict() -> Dict {
    Dict::from_entries(vec![
        ("de".into(), "的".into(), 100000),
        ("de".into(), "得".into(), 300),
        ("de".into(), "地".into(), 200),
        ("ni'hao".into(), "你好".into(), 8000),
        ("ni'hao".into(), "泥嚎".into(), 100),
        ("ni".into(), "泥".into(), 500),
        ("ni".into(), "你".into(), 50000),
        ("hao".into(), "好".into(), 40000),
        ("shi'jie".into(), "世界".into(), 6000),
        ("shi".into(), "世".into(), 3000),
        ("jie".into(), "界".into(), 2500),
        ("gong'lve".into(), "攻略".into(), 9279),
        ("lu".into(), "路".into(), 3000),
    ])
}

pub fn default_engine() -> Arc<Engine> {
    Engine::new(fixture_dict(), Config::default())
}

// ===== engine 候选生成 =====

#[test]
fn candidates_word_first() {
    // rime 词优先（39-rime-pipeline.md §Step2：可靠精确词在场不组句）：
    // nihao 有精确词「你好」→ 候选[0] 为词条而非 Sentence（classic 曾整句置顶）。
    let engine = default_engine();
    let mut s = engine.start_session();
    for c in "nihao".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.effect();
    assert_eq!(e.candidates[0].text, "你好");
    assert_eq!(e.candidates[0].kind, CandidateKind::Word);
}

#[test]
fn exact_words_order_by_weight() {
    let engine = default_engine();
    let mut s = engine.start_session();
    for c in "de".chars() {
        s.on_key(Key::Char(c));
    }
    let binding = s.effect();
    let texts: Vec<&str> = binding.candidates.iter().map(|c| c.text.as_str()).collect();
    // 单音节无 Sentence；exact 顺序 = weight 降序：的/得/地
    assert!(
        texts.iter().position(|t| *t == "的").unwrap()
            < texts.iter().position(|t| *t == "得").unwrap()
    );
    assert!(
        texts.iter().position(|t| *t == "得").unwrap()
            < texts.iter().position(|t| *t == "地").unwrap()
    );
    assert_eq!(s.effect().candidates[0].text, "的");
}

// ===== 24-ue-input-alias.md：üe 去点输入形 lue/nue → v 形 =====

/// gonglue/gonglve 同出「攻略」，seg_len=2 整词上屏（u/v 两种输入形归一）。
#[test]
fn ue_alias_gonglue_gonglve() {
    let engine = default_engine();
    for raw in ["gonglue", "gonglve"] {
        let mut s = engine.start_session();
        for c in raw.chars() {
            s.on_key(Key::Char(c));
        }
        let e = s.effect();
        let texts: Vec<&str> = e.candidates.iter().map(|c| c.text.as_str()).collect();
        assert!(
            texts.contains(&"攻略"),
            "{raw} 应出「攻略」，实际：{texts:?}"
        );
        let hit = e.candidates.iter().find(|c| c.text == "攻略").unwrap();
        assert_eq!(hit.seg_len, 2, "{raw} 攻略应整词上屏");
    }
}

/// lu 仍真实对立音节（路/芦…，非律 lv）；jv 非音节非前缀 → 无候选（原文兜底）。
#[test]
fn ue_alias_lu_jv_unaffected() {
    let engine = default_engine();
    let mut s = engine.start_session();
    for c in "lu".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.effect();
    assert!(
        e.candidates.iter().any(|c| c.text == "路"),
        "lu 应出「路」（lu≠lv），实际：{:?}",
        e.candidates.iter().map(|c| c.text.as_str()).collect::<Vec<_>>()
    );
    assert!(!e.candidates.iter().any(|c| c.text == "律"));

    let mut s2 = engine.start_session();
    for c in "jv".chars() {
        s2.on_key(Key::Char(c));
    }
    let e2 = s2.effect();
    // rime 简拼边展开（j→jie…）出字而非原文兜底（librime 语义）；核心语义不变：
    // ve 形不映射（jv 不得出「略」）。
    assert!(
        !e2.candidates.iter().any(|c| c.text == "略"),
        "ve 形不归一连累（jv 不得出略）：{:?}",
        e2.candidates.iter().map(|c| c.text.as_str()).collect::<Vec<_>>()
    );
}

/// 前缀联想开关：默认关闭（候选仅 exact，微软化）；开启时追加以当前码为前缀的长词。
#[test]
fn candidate_prefix_switch() {
    let dict = Dict::from_entries(vec![
        ("ni'hao".into(), "你好".into(), 8000),
        ("ni'hao".into(), "泥嚎".into(), 100),
        ("ni'hao'a".into(), "你好啊".into(), 9000),
        ("ni'hao'a'a".into(), "你好啊啊".into(), 1000),
    ]);
    // 默认（关）：无联想长词
    let engine = Engine::new(dict.clone(), Config::default());
    let mut s = engine.start_session();
    for c in "nihao".chars() {
        s.on_key(Key::Char(c));
    }
    let texts: Vec<String> = s
        .effect()
        .candidates
        .iter()
        .map(|c| c.text.clone())
        .collect();
    assert!(texts.contains(&"你好".to_string()));
    assert!(
        !texts.contains(&"你好啊".to_string()),
        "默认不应出现前缀联想词，实际：{texts:?}"
    );
    // 开启：追加联想长词
    let cfg = Config {
        candidate_prefix: true,
        ..Config::default()
    };
    let engine2 = Engine::new(dict, cfg);
    let mut s2 = engine2.start_session();
    for c in "nihao".chars() {
        s2.on_key(Key::Char(c));
    }
    let texts2: Vec<String> = s2
        .effect()
        .candidates
        .iter()
        .map(|c| c.text.clone())
        .collect();
    assert!(
        texts2.contains(&"你好啊".to_string()),
        "开启后应出现前缀联想词，实际：{texts2:?}"
    );
}

#[test]
fn prefix_completion_recalled() {
    // 联想开启时，未完成码 "nih" 也能通过前缀补全召回 "你好"
    let cfg = Config {
        candidate_prefix: true,
        ..Config::default()
    };
    let engine = Engine::new(fixture_dict(), cfg);
    let mut s = engine.start_session();
    for c in "nih".chars() {
        s.on_key(Key::Char(c));
    }
    assert!(s.effect().candidates.iter().any(|c| c.text == "你好"));
}

#[test]
fn dedup_by_text() {
    let engine = default_engine();
    let mut s = engine.start_session();
    for c in "nihao".chars() {
        s.on_key(Key::Char(c));
    }
    let count = s
        .effect()
        .candidates
        .iter()
        .filter(|c| c.text == "你好")
        .count();
    assert_eq!(count, 1);
}

#[test]
fn max_candidates_capped() {
    let cfg = Config {
        max_candidates: 3,
        ..Config::default()
    };
    let engine = Engine::new(fixture_dict(), cfg);
    let mut s = engine.start_session();
    for c in "nihao".chars() {
        s.on_key(Key::Char(c));
    }
    assert!(s.effect().candidates.len() <= 3);
}

// ===== 砍尾巴逐级前缀（契约 §4.2，M1 后期）=====

/// 长句逐级：整句 → 次长句 → … → 词 → 单字，按从长到短全部出现在候选。
#[test]
fn tail_cutting_lists_all_levels_longest_first() {
    let dict = Dict::from_entries(vec![
        (
            "chuang'qian'ming'yue'guang".into(),
            "床前明月光".into(),
            8000,
        ),
        ("chuang'qian'ming'yue".into(), "床前明月".into(), 7000),
        ("chuang'qian'ming".into(), "床前明".into(), 6000),
        ("chuang'qian".into(), "床前".into(), 5000),
        ("chuang".into(), "床".into(), 4000),
        ("qian".into(), "前".into(), 3000),
        ("ming".into(), "明".into(), 2000),
        ("yue".into(), "月".into(), 1000),
        ("guang".into(), "光".into(), 500),
    ]);
    let engine = Engine::new(dict, Config::default());
    let mut s = engine.start_session();
    for c in "chuangqianmingyueguang".chars() {
        s.on_key(Key::Char(c));
    }
    let texts: Vec<String> = s
        .effect()
        .candidates
        .iter()
        .map(|c| c.text.clone())
        .collect();
    let expect = ["床前明月光", "床前明月", "床前明", "床前", "床"];
    let mut pos = 0usize;
    for want in expect {
        let at = texts
            .iter()
            .position(|t| t == want)
            .expect(&format!("候选应含 {want}，实际：{texts:?}"));
        assert!(at >= pos, "{want} 应排在更早层级之后，实际：{texts:?}");
        pos = at;
    }
    assert_eq!(texts[0], "床前明月光", "整句应为候选 1，实际：{texts:?}");
}

/// 短码的词/单字可及：zheshi → "这是/知识"（2 段词）+ "这"（1 段单字）。
#[test]
fn tail_cutting_single_char_reachable() {
    let dict = Dict::from_entries(vec![
        ("zhe'shi".into(), "这是".into(), 8000),
        ("zhe'shi".into(), "知识".into(), 7000),
        ("zhe".into(), "这".into(), 50000),
        ("shi".into(), "是".into(), 40000),
    ]);
    let engine = Engine::new(dict, Config::default());
    let mut s = engine.start_session();
    for c in "zheshi".chars() {
        s.on_key(Key::Char(c));
    }
    let texts: Vec<String> = s
        .effect()
        .candidates
        .iter()
        .map(|c| c.text.clone())
        .collect();
    assert!(texts.contains(&"这是".to_string()), "实际：{texts:?}");
    assert!(texts.contains(&"知识".to_string()), "实际：{texts:?}");
    let zhe = texts
        .iter()
        .position(|t| t == "这")
        .expect("单字应可及，实际：{texts:?}");
    let zheshi = texts.iter().position(|t| t == "这是").unwrap();
    assert!(
        zheshi < zhe,
        "词级应在单字级之前（从长到短），实际：{texts:?}"
    );
}

/// 无撇号 xian：枚举切分在 k=1 级内合并 [xian]+[xi,an]，跨组按权重（先/线/西安…）——
/// "顺其自然"的规则结果，与历史混排一致。
#[test]
fn tail_cutting_xian_merge_by_weight() {
    let dict = Dict::from_entries(vec![
        ("xian".into(), "先".into(), 75337),
        ("xian".into(), "线".into(), 24039),
        ("xi'an".into(), "西安".into(), 6091),
    ]);
    let engine = Engine::new(dict, Config::default());
    let mut s = engine.start_session();
    for c in "xian".chars() {
        s.on_key(Key::Char(c));
    }
    let texts: Vec<String> = s
        .effect()
        .candidates
        .iter()
        .map(|c| c.text.clone())
        .collect();
    assert_eq!(texts[0], "先");
    assert!(texts.iter().position(|t| t == "西安").unwrap() > 0);
    // 西安（6091）按权重在 先/线 之后。
    let xi_an = texts.iter().position(|t| t == "西安").unwrap();
    let xian = texts.iter().position(|t| t == "先").unwrap();
    let xian2 = texts.iter().position(|t| t == "线").unwrap();
    assert!(xian < xi_an && xian2 < xi_an, "实际：{texts:?}");
}

/// 无撇号 jian：替代切分 ji'an 的低权重词不被同音字挤出（2026-08-19 删 20 截断回归）。
/// 旧实现 PER_LEVEL_EXACT=20 按权重合并只推前 20，吉安(33)/集安(47) 翻页不可达。
#[test]
fn tail_cutting_jian_ji_an_reachable_beyond_top20() {
    let mut items: Vec<(String, String, u32)> = vec![
        ("jian".into(), "件".into(), 99999),
        ("ji'an".into(), "吉安".into(), 160),
        ("ji'an".into(), "集安".into(), 31),
    ];
    // 30 个高权重 jian 单字压住词频前 20——旧截断必把吉安/集安挤出
    for i in 0..30u32 {
        items.push((
            "jian".into(),
            char::from_u32(0x4e00 + i).unwrap().to_string(),
            50000 - i,
        ));
    }
    let cfg = Config {
        page_size: 100,
        ..Config::default()
    };
    let engine = Engine::new(Dict::from_entries(items), cfg);
    let mut s = engine.start_session();
    for c in "jian".chars() {
        s.on_key(Key::Char(c));
    }
    let texts: Vec<String> = s
        .effect()
        .candidates
        .iter()
        .map(|c| c.text.clone())
        .collect();
    assert!(
        texts.contains(&"吉安".to_string()),
        "吉安应可达（权重 160 < 前 20 单字），实际：{texts:?}"
    );
    assert!(
        texts.contains(&"集安".to_string()),
        "集安应可达（权重 31），实际：{texts:?}"
    );
}

/// fenge：无撇号多段歧义——枚举源必须是 plain 前缀而非 join(') 键，
/// 否则段内枚举被 `'` 强制切分扼杀（只出 feng'e），fen'ge（分割）不可及。
#[test]
fn full_pinyin_enumerates_multi_seg_variants_fenge() {
    let dict = Dict::from_entries(vec![
        ("feng'e".into(), "风额".into(), 1),
        ("fen'ge".into(), "分割".into(), 8000),
        ("feng".into(), "风".into(), 5000),
        ("e".into(), "额".into(), 4000),
    ]);
    let engine = Engine::new(dict, Config::default());
    let mut s = engine.start_session();
    for c in "fenge".chars() {
        s.on_key(Key::Char(c));
    }
    let texts: Vec<String> = s
        .effect()
        .candidates
        .iter()
        .map(|c| c.text.clone())
        .collect();
    assert!(texts.contains(&"分割".to_string()), "实际：{texts:?}");
}

/// keneng：同上——ke'neng（可能）须经枚举命中（贪心 [ken,eng] 挡不住枚举）。
#[test]
fn full_pinyin_enumerates_multi_seg_variants_keneng() {
    let dict = Dict::from_entries(vec![
        ("ken'eng".into(), "啃嗯".into(), 1),
        ("ke'neng".into(), "可能".into(), 9000),
        ("ken".into(), "啃".into(), 3000),
    ]);
    let engine = Engine::new(dict, Config::default());
    let mut s = engine.start_session();
    for c in "keneng".chars() {
        s.on_key(Key::Char(c));
    }
    let texts: Vec<String> = s
        .effect()
        .candidates
        .iter()
        .map(|c| c.text.clone())
        .collect();
    assert!(texts.contains(&"可能".to_string()), "实际：{texts:?}");
}

/// xi'an 强制输入不枚举变体：不得出"先"（xian 键的词），强制语义保持。
#[test]
fn forced_apostrophe_does_not_enumerate() {
    let dict = Dict::from_entries(vec![
        ("xian".into(), "先".into(), 99999),
        ("xi'an".into(), "西安".into(), 100),
    ]);
    let engine = Engine::new(dict, Config::default());
    let mut s = engine.start_session();
    for c in "xi'an".chars() {
        s.on_key(Key::Char(c));
    }
    let texts: Vec<String> = s
        .effect()
        .candidates
        .iter()
        .map(|c| c.text.clone())
        .collect();
    assert!(texts.contains(&"西安".to_string()), "实际：{texts:?}");
    assert!(
        !texts.contains(&"先".to_string()),
        "强制切分不应枚举出 xian 键，实际：{texts:?}"
    );
}

/// `xi'`（尾空段）后空格：消费边界按有效段数（非空段）判定——
/// 否则"系"（seg_len=1）被判成部分消费，悬空 + 空尾巴导致"空候选表"（实测 2026-08-13）。
#[test]
fn trailing_apostrophe_space_commits_first_candidate() {
    let dict = Dict::from_entries(vec![
        ("xi".into(), "西".into(), 4125),
        ("xi".into(), "系".into(), 11730),
    ]);
    let engine = Engine::new(dict, Config::default());
    let mut s = engine.start_session();
    for c in "xi'".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.on_key(Key::Space);
    assert_eq!(e.end, Some(SessionEnd::Commit("系".into())));
}

/// 左右键页内移动 selected（边界环绕翻页）。
#[test]
fn arrow_keys_move_selected_in_page() {
    let dict = Dict::from_entries(vec![
        ("ni'hao".into(), "你好".into(), 8000),
        ("ni'hao".into(), "泥嚎".into(), 7000),
        ("ni'hao".into(), "拟好".into(), 6000),
        ("ni".into(), "你".into(), 50000),
        ("hao".into(), "好".into(), 40000),
    ]);
    let engine = Engine::new(dict, Config::default());
    let mut s = engine.start_session();
    for c in "nihao".chars() {
        s.on_key(Key::Char(c));
    }
    assert_eq!(s.effect().selected, 0);
    assert_eq!(s.on_key(Key::Right).selected, 1);
    assert_eq!(s.on_key(Key::Right).selected, 2);
    assert_eq!(s.on_key(Key::Left).selected, 1);
    assert_eq!(s.on_key(Key::Left).selected, 0);
    // 页首回退：首页夹紧 0（无上一页）
    assert_eq!(s.on_key(Key::Left).selected, 0);
}

/// 页内导航边界环绕：页尾继续 → 下一页（selected=0）；页首回退 → 上一页（selected=页尾）。
#[test]
fn arrow_keys_wrap_across_pages() {
    let dict = Dict::from_entries(vec![
        ("ni'hao".into(), "你好".into(), 8000),
        ("ni'hao".into(), "泥嚎".into(), 7000),
        ("ni'hao".into(), "拟好".into(), 6000),
        ("ni'hao".into(), "你好啊".into(), 5000),
        ("ni'hao".into(), "泥嚎哦".into(), 4000),
        ("ni'hao".into(), "拟好吧".into(), 3000),
        ("ni".into(), "你".into(), 50000),
        ("hao".into(), "好".into(), 40000),
    ]);
    let engine = Engine::new(dict, Config::default());
    let mut s = engine.start_session();
    for c in "nihao".chars() {
        s.on_key(Key::Char(c));
    }
    // 第一页 5 个（page_size=5）；多翻几页到最后一页
    while s.effect().page.page + 1 < s.effect().page.page_count {
        s.on_key(Key::PageDown);
    }
    let last_page = s.effect().page.page;
    // 页尾继续 → 夹紧（已是末页）
    let len = s.effect().candidates.len();
    for _ in 0..len {
        s.on_key(Key::Right);
    }
    let e = s.effect();
    assert_eq!(e.page.page, last_page);
    assert_eq!(e.selected, e.candidates.len() - 1, "末页页尾夹紧");
    // 回第一页
    while s.effect().page.page > 0 {
        s.on_key(Key::PageUp);
    }
    // 页首回退 → 夹紧 0（已是首页）
    assert_eq!(s.on_key(Key::Left).selected, 0);
    // 页尾继续 → 翻到下一页 selected=0
    for _ in 0..s.effect().candidates.len() {
        s.on_key(Key::Right);
    }
    let e = s.effect();
    assert_eq!(e.page.page, 1, "页尾继续应翻到下一页");
    assert_eq!(e.selected, 0, "下一页从页首开始");
    // 页首回退 → 翻回上一页 selected=页尾
    let e = s.on_key(Key::Left);
    assert_eq!(e.page.page, 0, "页首回退应翻回上一页");
    assert_eq!(e.selected, e.candidates.len() - 1, "回上一页选中页尾");
}

/// 连续 `'` 忽略（不允许 `''`）：`xi'` 后按 `'` 预览不变、不产生空段怪态；
/// 继续输入等效 `xi'an` → 出"西安"（第二个 `'` 被吞）。
#[test]
fn consecutive_apostrophe_ignored() {
    let dict = Dict::from_entries(vec![
        ("xi".into(), "西".into(), 100),
        ("xi'an".into(), "西安".into(), 200),
    ]);
    let engine = Engine::new(dict, Config::default());
    let mut s = engine.start_session();
    for c in "xi'".chars() {
        s.on_key(Key::Char(c));
    }
    // 第二个 `'`：忽略，预览保持 xi'
    let e = s.on_key(Key::Char('\''));
    assert_eq!(e.composition, "xi'");
    assert_eq!(s.effect().composition, "xi'");
    // 继续 an：等效 xi'an → 候选含"西安"
    for c in "an".chars() {
        s.on_key(Key::Char(c));
    }
    let texts: Vec<String> = s
        .effect()
        .candidates
        .iter()
        .map(|c| c.text.clone())
        .collect();
    assert!(texts.contains(&"西安".to_string()), "实际：{texts:?}");
}

// ===== 续接（picked 栈 + 尾巴续接，契约 §4.1 选词行，M1 后期）=====

/// 长句词库：整句/两字词/单字 + 尾巴整词，供续接用例。
fn tail_dict() -> Dict {
    Dict::from_entries(vec![
        (
            "chuang'qian'ming'yue'guang".into(),
            "床前明月光".into(),
            8000,
        ),
        ("chuang'qian".into(), "床前".into(), 5000),
        ("chuang".into(), "床".into(), 4000),
        ("qian".into(), "前".into(), 3000),
        ("ming'yue'guang".into(), "明月光".into(), 7000),
        ("ming'yue".into(), "明月".into(), 6000),
        ("ming".into(), "明".into(), 2000),
        ("yue".into(), "月".into(), 1000),
        ("guang".into(), "光".into(), 500),
    ])
}

fn type_long(s: &mut Session) {
    for c in "chuangqianmingyueguang".chars() {
        s.on_key(Key::Char(c));
    }
}

/// 选中间级词：悬空入栈 + 尾巴续接（会话不结束、无 commit 信号）。
#[test]
fn pick_middle_keeps_tail() {
    let engine = Engine::new(tail_dict(), Config::default());
    let mut s = engine.start_session();
    type_long(&mut s);
    // 选 k=2 级"床前"（候选 1 是整句，找"床前"位置）。
    let idx = s
        .effect()
        .candidates
        .iter()
        .position(|c| c.text == "床前")
        .unwrap();
    let e = s.on_key(Key::Digit((idx + 1) as u8));
    assert_eq!(e.end, None, "续接不应结束会话");
    assert_eq!(
        e.composition, "床前ming'yue'guang",
        "混合预编辑：已选汉字+尾巴拼音，实际：{}",
        e.composition
    );
    assert_eq!(e.reading, "床前ming'yue'guang");
    // 尾巴候选：整词"明月光"居首。
    assert_eq!(e.candidates[0].text, "明月光");
    assert!(s.is_active());
}

/// 续接后继续选词直至全部上屏。
#[test]
fn continue_then_commit_all() {
    let engine = Engine::new(tail_dict(), Config::default());
    let mut s = engine.start_session();
    type_long(&mut s);
    let idx = s
        .effect()
        .candidates
        .iter()
        .position(|c| c.text == "床前")
        .unwrap();
    s.on_key(Key::Digit((idx + 1) as u8));
    // 空格上屏首选（明月光）→ 全量结束：床前明月光。
    let e = s.on_key(Key::Space);
    assert_eq!(e.end, Some(SessionEnd::Commit("床前明月光".into())));
    assert!(!s.is_active());
}

/// 续接尾巴首段歧义音节可枚举替代切分（2026-08-18 修复）：
/// 手选 老师(k=2) 在(k=1) 后尾巴 xian 的撇号是引擎注入的（非用户强制），
/// 此前被 `raw.contains('\'')` 门控锁死 → 「西安」（xi'an）进不了候选；
/// 现 k=1 对砍尾结果重新分音节逐 variant exact（与独立输入 xian 同通道）。
#[test]
fn resumed_tail_ambiguous_syllable_reachable() {
    let dict = Dict::from_entries(vec![
        ("lao'shi".into(), "老师".into(), 9000),
        ("zai".into(), "在".into(), 5000),
        ("zai".into(), "再".into(), 1000),
        ("xi'an".into(), "西安".into(), 8000),
        ("xian".into(), "先".into(), 7000),
        ("xian".into(), "现".into(), 1000),
        ("lao".into(), "老".into(), 3000),
        ("shi".into(), "师".into(), 4000),
        ("zai".into(), "载".into(), 500),
    ]);
    let engine = Engine::new(dict, Config::default());
    let mut s = engine.start_session();
    for c in "laoshizaixian".chars() {
        s.on_key(Key::Char(c));
    }
    // 选 老师（k=2）→ 尾巴 zai'xian
    let e = s.effect();
    let idx = e.candidates.iter().position(|c| c.text == "老师").unwrap();
    s.on_key(Key::Digit((idx + 1) as u8));
    // 选 在（k=1）→ 尾巴 xian
    let e = s.effect();
    let idx = e.candidates.iter().position(|c| c.text == "在").unwrap();
    s.on_key(Key::Digit((idx + 1) as u8));
    let e = s.effect();
    assert_eq!(
        e.reading, "老师在xi'an",
        "预编辑跟随居首候选切分（西安 xi'an），实际：{}",
        e.reading
    );
    let texts: Vec<String> = e.candidates.iter().map(|c| c.text.clone()).collect();
    assert!(
        texts.contains(&"西安".to_string()),
        "续接尾巴首段歧义应重新分音节出 xi'an：{texts:?}"
    );
    assert!(
        texts.contains(&"先".to_string()),
        "xian 单字应保留：{texts:?}"
    );
}

/// 退格回退已选词（39-rime-pipeline.md §6 rime 式逐字退）：
/// 多字词退**末字**——末字音节还原回未确认区，其余留栈；再退整词。
#[test]
fn backspace_pops_picked() {
    let engine = Engine::new(tail_dict(), Config::default());
    let mut s = engine.start_session();
    type_long(&mut s);
    let idx = s
        .effect()
        .candidates
        .iter()
        .position(|c| c.text == "床前")
        .unwrap();
    s.on_key(Key::Digit((idx + 1) as u8));
    // 第一次退格：床前 → 床 + qian 回未确认区
    let e = s.on_key(Key::Backspace);
    assert_eq!(e.end, None, "回退不结束会话");
    assert_eq!(
        e.composition, "床qian'ming'yue'guang",
        "逐字退：前 字还原为 qian，实际：{}",
        e.composition
    );
    assert!(s.is_active());
    // 第二次退格：床 为单字词 → 整词退，raw 完整恢复
    let e = s.on_key(Key::Backspace);
    assert_eq!(
        e.composition, "chuang'qian'ming'yue'guang",
        "单字词整词退，实际：{}",
        e.composition
    );
    assert!(
        e.candidates.iter().any(|c| c.text == "床前明月光"),
        "候选恢复整句"
    );
    assert!(s.is_active());
}

/// 悬空状态下按 Esc：已选词上屏（尾巴随之取消），非整句取消。
#[test]
fn esc_with_picked_commits_picked() {
    let engine = Engine::new(tail_dict(), Config::default());
    let mut s = engine.start_session();
    type_long(&mut s);
    let idx = s
        .effect()
        .candidates
        .iter()
        .position(|c| c.text == "床前")
        .unwrap();
    s.on_key(Key::Digit((idx + 1) as u8));
    let e = s.on_key(Key::Esc);
    assert_eq!(e.end, Some(SessionEnd::Commit("床前".into())));
    assert!(!s.is_active());
}

/// 无已选词时 Esc = 整句取消（现状不变）。
#[test]
fn esc_without_picked_cancels() {
    let engine = Engine::new(tail_dict(), Config::default());
    let mut s = engine.start_session();
    type_long(&mut s);
    let e = s.on_key(Key::Esc);
    assert_eq!(e.end, Some(SessionEnd::Cancel));
    assert!(!s.is_active());
}

/// 选整句（k=5）：全部消费，会话结束。
#[test]
fn select_full_consumes_all() {
    let engine = Engine::new(tail_dict(), Config::default());
    let mut s = engine.start_session();
    type_long(&mut s);
    let e = s.on_key(Key::Digit(1));
    assert_eq!(e.end, Some(SessionEnd::Commit("床前明月光".into())));
    assert!(!s.is_active());
}

#[test]
fn static_order_is_deterministic() {
    let engine = default_engine();
    let mut a = engine.start_session();
    let mut b = engine.start_session();
    for c in "nihao".chars() {
        a.on_key(Key::Char(c));
        b.on_key(Key::Char(c));
    }
    assert_eq!(a.effect().candidates, b.effect().candidates);
}

// ===== M1.5 三路路由（微软实测对齐）=====

/// M1.5 词典：模拟 dictc 产物——全拼键 + 简拼键同表（简拼键为 dictc 自动生成，
/// 引擎测试里手工给出等价数据）。
fn m15_dict() -> Dict {
    Dict::from_entries(vec![
        // 单段档单字
        ("de".into(), "的".into(), 100000),
        ("de".into(), "得".into(), 300),
        ("shi".into(), "是".into(), 90000),
        ("shi".into(), "时".into(), 50000),
        ("shi".into(), "十".into(), 40000),
        ("shi".into(), "事".into(), 30000),
        ("shi".into(), "市".into(), 20000),
        ("shi".into(), "世".into(), 10000),
        ("shang".into(), "上".into(), 40000),
        ("ca".into(), "擦".into(), 2000),
        ("cai".into(), "才".into(), 10000),
        ("cai".into(), "财".into(), 3000),
        // 混拼全拼键
        ("ni'hao".into(), "你好".into(), 8000),
        ("ni'hao".into(), "泥嚎".into(), 100),
        ("ni".into(), "你".into(), 50000),
        ("na".into(), "那".into(), 40000),
        ("hao".into(), "好".into(), 30000),
        // 简拼键（dictc 同款）
        ("nh".into(), "你好".into(), 8000),
        ("nh".into(), "泥嚎".into(), 100),
        ("nhm".into(), "你还没".into(), 6000),
        ("nhms".into(), "你还没说".into(), 5000),
        ("nhmsx".into(), "你还没睡醒".into(), 7000),
    ])
}

/// 单字母档：c → 纯单字（才/财/擦），无词，词频序。
#[test]
fn single_letter_chars_only() {
    let engine = Engine::new(m15_dict(), Config::default());
    let mut s = engine.start_session();
    s.on_key(Key::Char('c'));
    let e = s.effect();
    let texts: Vec<&str> = e.candidates.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, vec!["才", "财", "擦"]);
    assert!(
        e.candidates.iter().all(|c| c.kind == CandidateKind::Char),
        "单字母档应纯单字"
    );
}

/// 部分音节档：sh → 纯单字（是/时/上/十/事…），无词。
#[test]
fn prefix_segment_chars_only() {
    let cfg = Config {
        page_size: 10,
        ..Config::default()
    };
    let engine = Engine::new(m15_dict(), cfg);
    let mut s = engine.start_session();
    for c in "sh".chars() {
        s.on_key(Key::Char(c));
    }
    let binding = s.effect();
    let texts: Vec<&str> = binding.candidates.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, vec!["是", "时", "上", "十", "事", "市", "世"]);
    assert!(s
        .effect()
        .candidates
        .iter()
        .all(|c| c.kind == CandidateKind::Char));
}

/// 部分音节档选中候选：整串消费、上屏、会话结束（切分器前缀兜底修正后 sh 为单段，
/// 不残留尾巴 h 续接——微软实测：sh 选"时"直接上屏）。
#[test]
fn prefix_select_commits_all() {
    let engine = Engine::new(m15_dict(), Config::default());
    let mut s = engine.start_session();
    for c in "sh".chars() {
        s.on_key(Key::Char(c));
    }
    let idx = s
        .effect()
        .candidates
        .iter()
        .position(|c| c.text == "时")
        .unwrap();
    let e = s.on_key(Key::Digit((idx + 1) as u8));
    assert_eq!(e.end, Some(SessionEnd::Commit("时".into())));
    assert!(!s.is_active());
}

/// 完整音节档：de → 纯单字（的/得），无"的的"类词（微软 B 组实测：shi→是时十使）。
#[test]
fn complete_syllable_chars_only() {
    let engine = Engine::new(m15_dict(), Config::default());
    let mut s = engine.start_session();
    for c in "de".chars() {
        s.on_key(Key::Char(c));
    }
    let binding = s.effect();
    let texts: Vec<&str> = binding.candidates.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, vec!["的", "得"]);
}

/// 完整音节档走 exact 全量同音字：shi → 全部 shi 单字按词频序（是时十事市世），
/// 不被首字母桶 top-N 截成 5 个（修正：桶混收多字词导致 shi 只剩 5 字）。
#[test]
fn complete_syllable_exact_full_pool() {
    let cfg = Config {
        page_size: 10,
        ..Config::default()
    };
    let engine = Engine::new(m15_dict(), cfg);
    let mut s = engine.start_session();
    for c in "shi".chars() {
        s.on_key(Key::Char(c));
    }
    let binding = s.effect();
    let texts: Vec<&str> = binding.candidates.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, vec!["是", "时", "十", "事", "市", "世"]);
    assert!(binding
        .candidates
        .iter()
        .all(|c| c.kind == CandidateKind::Char));
}

/// 单段非前缀（v）：无词库候选，兜底原文候选（微软 A 组实测：i/u/v 只有字面）。
#[test]
fn non_prefix_single_letter_fallback() {
    let engine = Engine::new(m15_dict(), Config::default());
    let mut s = engine.start_session();
    for c in "v".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.effect();
    let texts: Vec<&str> = e.candidates.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, vec!["v"], "无匹配输入 → 兜底原文候选");
    assert_eq!(
        e.candidates[0].kind,
        CandidateKind::Char,
        "单字符兜底用 Char"
    );
    assert_eq!(e.candidates[0].seg_len, 1, "seg_len=段数 → 全消费");
    let e = s.on_key(Key::Space);
    assert_eq!(e.end, Some(SessionEnd::Commit("v".into())));
}

/// 简拼档：nh → 词优先（你好/泥嚎），单字（你/那）由简拼边展开自然排在词后
/// （librime 拼写代数语义：简拼边展开为该字母开头全部音节，含单音节词条；
/// 微软「简拼纯词」由词频天然把词排前，不额外过滤）。
#[test]
fn abbrev_words_first_chars_follow() {
    let engine = Engine::new(m15_dict(), Config::default());
    let mut s = engine.start_session();
    for c in "nh".chars() {
        s.on_key(Key::Char(c));
    }
    let binding = s.effect();
    let texts: Vec<&str> = binding.candidates.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(&texts[..2], &["你好", "泥嚎"], "词优先，实际：{texts:?}");
    assert!(texts.contains(&"你"), "单字由简拼展开可达：{texts:?}");
}

/// 简拼无长度上限 + 逐级砍尾巴：nhmsx → 你还没睡醒(k5)/你还没说(k4)/你还没(k3)/你好(k2)。
#[test]
fn abbrev_tail_levels_longest_first() {
    let engine = Engine::new(m15_dict(), Config::default());
    let mut s = engine.start_session();
    for c in "nhmsx".chars() {
        s.on_key(Key::Char(c));
    }
    let texts: Vec<String> = s
        .effect()
        .candidates
        .iter()
        .map(|c| c.text.clone())
        .collect();
    let expect = ["你还没睡醒", "你还没说", "你还没", "你好"];
    let mut pos = 0usize;
    for want in expect {
        let at = texts
            .iter()
            .position(|t| t == want)
            .unwrap_or_else(|| panic!("候选应含 {want}，实际：{texts:?}"));
        assert!(at >= pos, "{want} 应排在更长层级之后，实际：{texts:?}");
        pos = at;
    }
}

/// 简拼每级全量不截断：键命中 >20 条时低频词翻页可达（2026-08-19 删 20 截断回归；
/// 简拼键是首字母严格匹配，jj 只出 j-j 词，无替代切分可枚举）。
#[test]
fn abbrev_over_20_low_freq_reachable() {
    let mut items: Vec<(String, String, u32)> = Vec::new();
    for i in 0..30u32 {
        items.push(("jj".into(), format!("词{i}"), 1000 - i * 10));
    }
    let cfg = Config {
        page_size: 100,
        ..Config::default()
    };
    let engine = Engine::new(Dict::from_entries(items), cfg);
    let mut s = engine.start_session();
    for c in "jj".chars() {
        s.on_key(Key::Char(c));
    }
    let texts: Vec<String> = s
        .effect()
        .candidates
        .iter()
        .map(|c| c.text.clone())
        .collect();
    assert_eq!(texts.len(), 30, "简拼每级全量不截断，实际：{texts:?}");
    assert_eq!(texts[29], "词29", "低频词翻页可达，实际：{texts:?}");
}

/// 简拼部分消费：选中"你还没说"（k=4）→ 词上屏 + 尾巴 x 续接（悬空续接复用）。
#[test]
fn abbrev_partial_commit_keeps_tail() {
    let engine = Engine::new(m15_dict(), Config::default());
    let mut s = engine.start_session();
    for c in "nhmsx".chars() {
        s.on_key(Key::Char(c));
    }
    let idx = s
        .effect()
        .candidates
        .iter()
        .position(|c| c.text == "你还没说")
        .unwrap();
    let e = s.on_key(Key::Digit((idx + 1) as u8));
    assert_eq!(e.end, None, "部分消费不结束会话");
    assert_eq!(
        e.composition, "你还没说x",
        "词上屏+尾巴拼音，实际：{}",
        e.composition
    );
    assert!(s.is_active());
}

/// 混拼：nhao → 你好（n 简拼段展开 × hao 完整段），词前字后。
#[test]
fn mixed_nhao_finds_nihao() {
    let engine = Engine::new(m15_dict(), Config::default());
    let mut s = engine.start_session();
    for c in "nhao".chars() {
        s.on_key(Key::Char(c));
    }
    let texts: Vec<String> = s
        .effect()
        .candidates
        .iter()
        .map(|c| c.text.clone())
        .collect();
    assert_eq!(texts[0], "你好", "混拼词应居首，实际：{texts:?}");
    assert!(texts.contains(&"泥嚎".to_string()));
    // 单字（你/那）排在词后
    let ni = texts.iter().position(|t| t == "你").unwrap();
    let nihao = texts.iter().position(|t| t == "你好").unwrap();
    assert!(nihao < ni, "词前字后，实际：{texts:?}");
}

/// 单段档全量返回、无 30 截断（微软对齐：候选全给、翻页可达；全局 max_candidates 兜底）。
#[test]
fn single_segment_no_truncation() {
    let mut items: Vec<(String, String, u32)> = Vec::new();
    for i in 0..40u32 {
        let w = char::from_u32(0x4e00 + i).unwrap().to_string(); // 40 个唯一单字
        items.push((format!("shi"), w, 1000 - i));
    }
    let dict = Dict::from_entries(items);
    let cfg = Config {
        page_size: 10,
        ..Config::default()
    };
    let engine = Engine::new(dict, cfg);
    let mut s = engine.start_session();
    for c in "shi".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.effect();
    // e.candidates = 当前页（page_size 10）；全量由 page_count 体现：40 候选 = 4 页
    assert_eq!(e.candidates.len(), 10);
    assert_eq!(
        e.page.page_count, 4,
        "40 候选应为 4 页（无 30 截断），实际：{}",
        e.page.page_count
    );
}

/// 简拼键整串消费：选中"你好"（k=2=n）→ 全部上屏、会话结束。
#[test]
fn abbrev_full_commit_ends_session() {
    let engine = Engine::new(m15_dict(), Config::default());
    let mut s = engine.start_session();
    for c in "nh".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.on_key(Key::Space);
    assert_eq!(e.end, Some(SessionEnd::Commit("你好".into())));
    assert!(!s.is_active());
}

#[test]
fn type_shows_candidates() {
    let engine = default_engine();
    let mut s = engine.start_session();
    for c in "de".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.effect();
    assert!(s.is_active());
    // 微软式：预编辑 = 拼音分段，候选只进候选窗。
    assert_eq!(e.composition, "de");
    assert_eq!(e.reading, "de");
    assert!(!e.candidates.is_empty());
}

#[test]
fn backspace_to_empty_cancels() {
    let engine = default_engine();
    let mut s = engine.start_session();
    for c in "de".chars() {
        s.on_key(Key::Char(c));
    }
    assert!(s.is_active());
    s.on_key(Key::Backspace);
    assert!(s.is_active());
    let e = s.on_key(Key::Backspace);
    assert!(!s.is_active());
    assert_eq!(e.end, Some(SessionEnd::Cancel));
}

#[test]
fn space_commits_selected() {
    let engine = default_engine();
    let mut s = engine.start_session();
    for c in "de".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.on_key(Key::Space);
    assert_eq!(e.end, Some(SessionEnd::Commit("的".into())));
}

#[test]
fn space_without_candidates_commits_raw() {
    let engine = default_engine();
    let mut s = engine.start_session();
    // "w" 无词库命中 → 兜底原文候选；空格选中兜底 → 原文上屏（与旧行为一致）
    s.on_key(Key::Char('w'));
    let binding = s.effect();
    let texts: Vec<&str> = binding.candidates.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, vec!["w"]);
    let e = s.on_key(Key::Space);
    assert_eq!(e.end, Some(SessionEnd::Commit("w".into())));
}

/// 英文串（input/window）：所有路由无命中 → 兜底原文候选，可 1/Space 直接上屏。
#[test]
fn english_input_falls_back_to_raw_candidate() {
    let engine = Engine::new(m15_dict(), Config::default());
    let mut s = engine.start_session();
    for c in "input".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.effect();
    let texts: Vec<&str> = e.candidates.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, vec!["input"], "英文串兜底为去撇号原文");
    assert_eq!(
        e.candidates[0].kind,
        CandidateKind::Word,
        "多字符兜底用 Word"
    );
    assert_eq!(
        e.candidates[0].seg_len, 5,
        "seg_len=段数（[i,n,p,u,t]）→ 全消费"
    );

    let e = s.on_key(Key::Digit(1));
    assert_eq!(e.end, Some(SessionEnd::Commit("input".into())));
}

#[test]
fn english_input_digit_and_space_commit_raw() {
    let engine = Engine::new(m15_dict(), Config::default());
    let mut s = engine.start_session();
    for c in "window".chars() {
        s.on_key(Key::Char(c));
    }
    assert_eq!(
        s.effect()
            .candidates
            .iter()
            .map(|c| c.text.as_str())
            .collect::<Vec<_>>(),
        vec!["window"]
    );
    let e = s.on_key(Key::Space);
    assert_eq!(e.end, Some(SessionEnd::Commit("window".into())));
}

#[test]
fn english_forced_apostrophes_fallback_squashed() {
    let engine = Engine::new(m15_dict(), Config::default());
    let mut s = engine.start_session();
    for c in "i'n'pu't".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.effect();
    assert_eq!(
        e.candidates[0].text, "input",
        "强制撇号输入兜底 text 为去撇号原文"
    );
    assert_eq!(
        e.reading, "i'n'p'u't",
        "composition 显示切分后的分段（fixture 无 pu 音节 → p/u 两段）"
    );
    let e = s.on_key(Key::Space);
    assert_eq!(e.end, Some(SessionEnd::Commit("input".into())));
}

#[test]
fn digit_selects_nth_in_page() {
    let engine = default_engine();
    let mut s = engine.start_session();
    for c in "de".chars() {
        s.on_key(Key::Char(c));
    }
    // 第 2 个候选 = "得"（exact 第 2）
    let e = s.on_key(Key::Digit(2));
    assert_eq!(e.end, Some(SessionEnd::Commit("得".into())));
}

#[test]
fn digit_out_of_range_noop() {
    let engine = default_engine();
    let mut s = engine.start_session();
    for c in "de".chars() {
        s.on_key(Key::Char(c));
    }
    let before = s.effect();
    let e = s.on_key(Key::Digit(9));
    assert!(e.end.is_none());
    assert!(s.is_active());
    assert_eq!(e.candidates, before.candidates);
}

#[test]
fn enter_commits_raw() {
    let engine = default_engine();
    let mut s = engine.start_session();
    for c in "de".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.on_key(Key::Enter);
    assert_eq!(e.end, Some(SessionEnd::Commit("de".into())));
}

/// 大写保形进序列：ni + CapsLock HAO（ShiftChar 大写）→ raw=niHAO 原样；
/// 匹配只认小写（大写段不命中音节表），候选仍从 ni 前缀出，Enter 原样上屏。
#[test]
fn shiftchar_uppercase_preserves_case_through_session() {
    let engine = default_engine();
    let mut s = engine.start_session();
    for c in "ni".chars() {
        s.on_key(Key::Char(c));
    }
    for c in ['H', 'A', 'O'] {
        s.on_key(Key::ShiftChar(c));
    }
    let e = s.effect();
    assert_eq!(
        e.reading, "ni'H'A'O",
        "大写原样进序列，切分按不可匹配字符单字母段"
    );
    let texts: Vec<&str> = e.candidates.iter().map(|c| c.text.as_str()).collect();
    assert!(texts.contains(&"你"), "候选仍从 ni 前缀出：{texts:?}");
    let e = s.on_key(Key::Enter);
    assert_eq!(
        e.end,
        Some(SessionEnd::Commit("niHAO".into())),
        "commit 原样含大写"
    );
}

/// 悬空 + ShiftChar：选中 ni 候选（部分消费）→ 尾巴 HAO 悬空续接，commit 组合原样。
#[test]
fn shiftchar_partial_consume_keeps_uppercase_tail() {
    let engine = default_engine();
    let mut s = engine.start_session();
    for c in "ni".chars() {
        s.on_key(Key::Char(c));
    }
    s.on_key(Key::ShiftChar('H'));
    // 选"你"（ni 前缀词，seg_len=1 < n=2）→ 悬空：你 + 尾巴 H
    let e = s.on_key(Key::Digit(1));
    assert!(e.end.is_none(), "部分消费不产生 commit 信号");
    assert_eq!(s.effect().composition, "你H", "已选词汉字 + 尾巴大写");
    let e = s.on_key(Key::Enter);
    assert_eq!(e.end, Some(SessionEnd::Commit("你H".into())));
}

/// ShiftChar 可被 Backspace 正常回退。
#[test]
fn shiftchar_backspace_removes_uppercase() {
    let engine = default_engine();
    let mut s = engine.start_session();
    s.on_key(Key::Char('n'));
    s.on_key(Key::ShiftChar('I'));
    let e = s.on_key(Key::Backspace);
    assert!(e.end.is_none());
    assert_eq!(s.effect().reading, "n");
}

/// 大写开会话首键：Shift+H + ello → Hello 全程进序列，commit 原样上屏。
#[test]
fn shiftchar_starts_session_hello() {
    let engine = default_engine();
    let mut s = engine.start_session();
    s.on_key(Key::ShiftChar('H'));
    assert!(s.is_active(), "ShiftChar 是开会话键，H 进序列而非直接上屏");
    for c in ['e', 'l', 'l', 'o'] {
        s.on_key(Key::Char(c));
    }
    let e = s.effect();
    assert_eq!(
        e.reading, "Hello",
        "大写段无匹配（兜底原文候选）→ 原样显示不分节（对齐主流：input 无匹配原样）"
    );
    let texts: Vec<&str> = e.candidates.iter().map(|c| c.text.as_str()).collect();
    assert!(
        texts.contains(&"Hello"),
        "全不命中 → 兜底原文候选：{texts:?}"
    );
    let e = s.on_key(Key::Enter);
    assert_eq!(
        e.end,
        Some(SessionEnd::Commit("Hello".into())),
        "commit 原样含大写"
    );
}

#[test]
fn esc_cancels() {
    let engine = default_engine();
    let mut s = engine.start_session();
    for c in "de".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.on_key(Key::Esc);
    assert_eq!(e.end, Some(SessionEnd::Cancel));
}

#[test]
fn paging_clamps_and_resets_selected() {
    let cfg = Config {
        page_size: 1,
        ..Config::default()
    };
    let engine = Engine::new(fixture_dict(), cfg);
    let mut s = engine.start_session();
    for c in "de".chars() {
        s.on_key(Key::Char(c));
    }
    let mut e = s.on_key(Key::Down);
    assert_eq!(e.page.page_count, 3);
    assert_eq!(e.page.page, 1, "单候选页页尾继续 → 翻到下一页");
    assert_eq!(e.selected, 0); // 下一页从页首开始
    e = s.on_key(Key::PageDown);
    assert_eq!(e.page.page, 2);
    assert_eq!(e.selected, 0);
    e = s.on_key(Key::PageDown);
    assert_eq!(e.page.page, 2); // clamp 到上限
    e = s.on_key(Key::PageUp);
    assert_eq!(e.page.page, 1);
    assert_eq!(e.selected, 0); // 翻页归零
    e = s.on_key(Key::PageUp);
    assert_eq!(e.page.page, 0);
    e = s.on_key(Key::PageUp);
    assert_eq!(e.page.page, 0); // clamp 到下限
}

#[test]
fn updown_clamps_within_page() {
    let engine = default_engine();
    let mut s = engine.start_session();
    for c in "de".chars() {
        s.on_key(Key::Char(c));
    }
    let mut e = s.on_key(Key::Down);
    assert_eq!(e.selected, 1);
    e = s.on_key(Key::Down);
    assert_eq!(e.selected, 2);
    e = s.on_key(Key::Down);
    assert_eq!(e.selected, 2); // clamp
    e = s.on_key(Key::Up);
    assert_eq!(e.selected, 1);
}

#[test]
fn ended_session_is_inert() {
    let engine = default_engine();
    let mut s = engine.start_session();
    for c in "de".chars() {
        s.on_key(Key::Char(c));
    }
    s.on_key(Key::Esc);
    // 会话结束后 on_key 兜底返回 Cancel，不 panic
    let e = s.on_key(Key::Char('x'));
    assert_eq!(e.end, Some(SessionEnd::Cancel));
}

// ===== M2 主动调权（Alt+←/→ 相邻交换权重，18-m2-user-dict.md）=====

/// 输入 "de" 后的候选 text 序列。
fn de_texts(s: &Session) -> Vec<String> {
    s.effect()
        .candidates
        .iter()
        .map(|c| c.text.clone())
        .collect()
}

fn user_dict_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("iuv-{name}-{}.imedic", std::process::id()))
}

#[test]
fn swap_right_moves_candidate_up_and_follows() {
    let engine = default_engine();
    let mut s = engine.start_session();
    for c in "de".chars() {
        s.on_key(Key::Char(c));
    }
    // 初始：的/得/地，selected=0
    assert_eq!(de_texts(&s), vec!["的", "得", "地"]);
    // Alt+→：高亮"的"与右侧"得"交换
    s.on_key(Key::SwapRight);
    let e = s.effect();
    let texts: Vec<String> = e.candidates.iter().map(|c| c.text.clone()).collect();
    assert_eq!(texts[0], "得", "交换后得升到 1 号位，实际：{texts:?}");
    assert_eq!(texts[1], "的");
    assert_eq!(texts[2], "地");
    assert_eq!(e.selected, 1, "高亮跟随被调词（的）");
    assert!(e.end.is_none(), "交换不结束会话");
    // 会话未结束：还能继续导航并上屏
    s.on_key(Key::Left); // 高亮回到 得
    let e2 = s.on_key(Key::Space);
    assert_eq!(e2.end, Some(SessionEnd::Commit("得".into())));
}

#[test]
fn swap_left_boundary_ignored() {
    let engine = default_engine();
    let mut s = engine.start_session();
    for c in "de".chars() {
        s.on_key(Key::Char(c));
    }
    let before = de_texts(&s);
    s.on_key(Key::SwapLeft); // 1 号位无左邻：忽略
    assert_eq!(de_texts(&s), before);
}

#[test]
fn swap_right_boundary_ignored() {
    let engine = default_engine();
    let mut s = engine.start_session();
    for c in "de".chars() {
        s.on_key(Key::Char(c));
    }
    s.on_key(Key::Right); // 高亮移到最后（地）
    s.on_key(Key::Right);
    let before = de_texts(&s);
    s.on_key(Key::SwapRight); // 末位无右邻：忽略
    assert_eq!(de_texts(&s), before);
}

#[test]
fn swap_repeatedly_moves_up_two_steps() {
    let engine = default_engine();
    let mut s = engine.start_session();
    for c in "de".chars() {
        s.on_key(Key::Char(c));
    }
    // 高亮 地（2 号）→ Alt+← 两次 → 地升到 1 号位，高亮始终跟随
    s.on_key(Key::Right);
    s.on_key(Key::Right);
    assert_eq!(s.effect().selected, 2);
    s.on_key(Key::SwapLeft);
    assert_eq!(de_texts(&s)[0], "的", "一次交换只上移一格：地到 1 号位");
    assert_eq!(de_texts(&s)[1], "地");
    assert_eq!(s.effect().selected, 1, "第一次交换后高亮跟随地（1 号）");
    s.on_key(Key::SwapLeft);
    let e = s.effect();
    let texts: Vec<String> = e.candidates.iter().map(|c| c.text.clone()).collect();
    assert_eq!(texts[0], "地", "第二次交换后地到 1 号位，实际：{texts:?}");
    assert_eq!(texts[1], "的");
    assert_eq!(texts[2], "得");
    assert_eq!(e.selected, 0);
}

#[test]
fn swap_without_user_dict_does_not_crash() {
    // 未 attach 用户库：交换走空库路径（不写盘），内存态也生效
    let engine = default_engine();
    let mut s = engine.start_session();
    for c in "de".chars() {
        s.on_key(Key::Char(c));
    }
    s.on_key(Key::SwapRight);
    let e = s.effect();
    let texts: Vec<String> = e.candidates.iter().map(|c| c.text.clone()).collect();
    assert_eq!(texts[0], "得");
    assert_eq!(e.selected, 1);
}

#[test]
fn swap_persists_across_sessions_in_process() {
    let path = user_dict_path("swap-persist");
    let _ = std::fs::remove_file(&path);
    let engine = default_engine();
    let _ = engine.attach_user_dict(path.clone()); // 首次无文件：降级空库（Err 仅日志）
                                                   // 会话 1：交换
    let mut s = engine.start_session();
    for c in "de".chars() {
        s.on_key(Key::Char(c));
    }
    s.on_key(Key::SwapRight);
    // 会话 2：新会话候选序保持（内存态 + 写盘）
    let mut s2 = engine.start_session();
    for c in "de".chars() {
        s2.on_key(Key::Char(c));
    }
    assert_eq!(de_texts(&s2)[0], "得");
    assert!(path.exists(), "swap 后用户库应写盘");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn swap_persists_to_file_for_new_process() {
    let path = user_dict_path("swap-file");
    let _ = std::fs::remove_file(&path);
    // 进程 A：attach + 交换（写盘）
    let engine = default_engine();
    let _ = engine.attach_user_dict(path.clone()); // 首次无文件：降级空库（Err 仅日志）
    let mut s = engine.start_session();
    for c in "de".chars() {
        s.on_key(Key::Char(c));
    }
    s.on_key(Key::SwapRight);
    // 进程 B：新 Engine attach 同一文件 → 覆盖生效
    let engine2 = default_engine();
    let _ = engine2.attach_user_dict(path.clone());
    let mut s2 = engine2.start_session();
    for c in "de".chars() {
        s2.on_key(Key::Char(c));
    }
    assert_eq!(de_texts(&s2)[0], "得");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn swap_reloads_on_mtime_change() {
    let path = user_dict_path("swap-reload");
    let _ = std::fs::remove_file(&path);
    let engine = default_engine();
    let _ = engine.attach_user_dict(path.clone()); // 首次无文件：降级空库（Err 仅日志）
                                                   // 会话 1：本进程写盘（得 升 1 号）
    let mut s = engine.start_session();
    for c in "de".chars() {
        s.on_key(Key::Char(c));
    }
    s.on_key(Key::SwapRight);
    assert_eq!(de_texts(&s)[0], "得");
    // 外部进程改写：把"地"调成最高
    let ext = iuv_data::UserDict::empty().apply_swap("de", "的", 100, "de", "地", 999999);
    ext.save(&path).unwrap();
    // 新会话：mtime 检测 → 重载外部内容（地 升 1 号）
    let mut s2 = engine.start_session();
    for c in "de".chars() {
        s2.on_key(Key::Char(c));
    }
    assert_eq!(de_texts(&s2)[0], "地", "外部写盘后新会话应重载生效");
    let _ = std::fs::remove_file(&path);
}

#[test]
fn swap_exchanges_word_candidate() {
    // rime 词优先：nihao 首候选是精确词（Word，词库条目）→ 调权交换正常生效
    // （classic 曾首候选为 Sentence 不可交换；rime 词优先下 Word 可交换）。
    let engine = default_engine();
    let mut s = engine.start_session();
    for c in "nihao".chars() {
        s.on_key(Key::Char(c));
    }
    let before = s.effect().candidates.clone();
    assert_eq!(before[0].kind, CandidateKind::Word);
    s.on_key(Key::SwapRight);
    let after = s.effect().candidates.clone();
    assert_eq!(after.len(), before.len());
    assert_ne!(
        after[0].text, before[0].text,
        "词条候选可交换：你好 与相邻候选互调"
    );
}

// ===== M2 修复：多段全拼 k=1 单字全量（低频字可达）=====

#[test]
fn multisegment_k1_single_chars_full_pool() {
    // zhangweiwei → 选张 → weiwei 续接，低频单字「葳」翻页可达。
    // 旧实现 PER_LEVEL_EXACT=20 把单字截在词频前 20（低频字卡死边界）；
    // 新实现 k=1 走单段档逻辑：单字全量（微软对齐：多段输入翻页可达单字）。
    let mut items: Vec<(String, String, u32)> = vec![
        ("zhang".into(), "张".into(), 90000),
        ("zhang".into(), "章".into(), 8000),
        ("zhang".into(), "长".into(), 5000),
        ("zhang'wei".into(), "张威".into(), 6000),
        ("zhang'wei'wei".into(), "张威威".into(), 5000),
        ("zhang'wei'wei".into(), "张薇薇".into(), 4000),
        ("wei'wei".into(), "薇薇".into(), 8000),
        ("wei'wei".into(), "巍巍".into(), 7000),
    ];
    // wei 单字 30 个（词频降序）+ 低频「葳」= 第 31 位：旧 20 上限必砍，新全量可达
    for i in 0..30u32 {
        items.push((
            "wei".into(),
            char::from_u32(0x4E00 + i).unwrap().to_string(),
            1000 - i * 10,
        ));
    }
    items.push(("wei".into(), "葳".into(), 50));
    let mut cfg = Config::default();
    cfg.page_size = 100; // 一页全显，断言免翻页
    let engine = Engine::new(Dict::from_entries(items), cfg);

    // zhangweiwei：k3 词 2 + k2 词 1 + k1 zhang 单字全量 3 = 6 候选；张 = 第 4 位
    let mut s = engine.start_session();
    for c in "zhangweiwei".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.effect();
    assert_eq!(
        e.page.total, 6,
        "zhangweiwei 候选总量，实际：{}",
        e.page.total
    );
    assert_eq!(e.candidates[3].text, "张");

    // 选张 → 悬空续接 weiwei：k2 词 2 + k1 wei 单字全量 31 = 33 候选；葳 = 第 33 位
    s.on_key(Key::Digit(4));
    let e = s.effect();
    assert_eq!(
        e.page.total, 33,
        "wei 单字全量应进候选（旧 20 上限只有 22）"
    );
    assert!(
        e.candidates.iter().any(|c| c.text == "葳"),
        "低频单字应可达（全量词频序第 31 位）"
    );
    // 上屏链路完整：选葳 → 张+葳 commit，尾巴 wei 续接
    let idx = e.candidates.iter().position(|c| c.text == "葳").unwrap();
    let e2 = s.on_key(Key::Digit((idx + 1) as u8));
    assert!(
        e2.end.is_none(),
        "部分消费：张葳 + wei 尾巴续接，会话未结束"
    );
}

// ===== M2 二期：自造词（逐字选择记录）+ 隐藏（Shift+Delete）=====

/// 逐字选出一个整词并 commit（模拟 zhangweiwei → 张/藳/藳 或 shouxuan → 手/选）。
/// 内嵌翻页：目标字不在当前页则 PageDown 循环（任意 page_size 下可用）。
fn select_by_chars(engine: &Arc<Engine>, input: &str, chars: &[&str]) -> Session {
    let mut s = engine.start_session();
    for c in input.chars() {
        s.on_key(Key::Char(c));
    }
    for ch in chars {
        let mut e = s.effect();
        let mut guard = 0;
        let pos = loop {
            if let Some(p) = e.candidates.iter().position(|c| c.text == *ch) {
                break p;
            }
            guard += 1;
            assert!(guard < 200, "单字不在候选：{ch}");
            s.on_key(Key::PageDown);
            e = s.effect();
        };
        s.on_key(Key::Digit((pos + 1) as u8));
    }
    s
}

#[test]
fn phrase_recording_scenario_a_no_hit() {
    // 场景 a：zhangweiwei 词库无整词（只有单字）→ 自造「张藳藳」权重 = 8000
    let dict = Dict::from_entries(vec![
        ("zhang".into(), "张".into(), 90000),
        ("wei".into(), "威".into(), 1000),
        ("wei".into(), "藳".into(), 50),
    ]);
    let engine = Engine::new(dict, Config::default());
    let s = select_by_chars(&engine, "zhangweiwei", &["张", "藳", "藳"]);
    assert_eq!(s.effect().end, Some(SessionEnd::Commit("张藳藳".into())));
    // 用户库出现自造词：再打整串直接出词
    let mut s2 = engine.start_session();
    for c in "zhangweiwei".chars() {
        s2.on_key(Key::Char(c));
    }
    let texts: Vec<String> = s2
        .effect()
        .candidates
        .iter()
        .map(|c| c.text.clone())
        .collect();
    assert!(
        texts.iter().any(|t| t == "张藳藳"),
        "自造词应出现在候选：{texts:?}"
    );
}

#[test]
fn phrase_recording_scenario_b1_weight() {
    // 场景 b1：shouxuan 命中 2 条（n=2 < page_size）→ 权重 < 手癖(300)，词位第 3
    let dict = Dict::from_entries(vec![
        ("shou'xuan".into(), "首选".into(), 8000),
        ("shou'xuan".into(), "手癖".into(), 300),
        ("shou".into(), "手".into(), 50000),
        ("shou".into(), "首".into(), 40000),
        ("xuan".into(), "选".into(), 30000),
        ("xuan".into(), "癖".into(), 200),
    ]);
    let engine = Engine::new(dict, Config::default());
    let s = select_by_chars(&engine, "shouxuan", &["手", "选"]);
    assert_eq!(s.effect().end, Some(SessionEnd::Commit("手选".into())));
    // 再打整串：手选出现在候选（viterbi 整句或词位）
    let mut s2 = engine.start_session();
    for c in "shouxuan".chars() {
        s2.on_key(Key::Char(c));
    }
    let texts: Vec<String> = s2
        .effect()
        .candidates
        .iter()
        .map(|c| c.text.clone())
        .collect();
    let pos = |t: &str| texts.iter().position(|x| x == t);
    assert!(pos("手选").is_some(), "自造词应出现在候选：{texts:?}");
    // classic 下「手选先于手癖」靠整句置顶；rime 词优先下手选为词条
    // （b1 权重 = 手癖−1 = 299）按词频排位——保留自造词可达性 + 先于单字即可。
    assert!(
        pos("手选").unwrap() < pos("手").unwrap(),
        "手选先于单字手：{texts:?}"
    );
}

#[test]
fn phrase_recording_scenario_b2_weight() {
    // 场景 b2：命中 6 条（n >= page_size）→ 权重 = avg(第4, 第5位) → 词位第 5
    let dict = Dict::from_entries(vec![
        ("zhong'xin".into(), "中心".into(), 9000),
        ("zhong'xin".into(), "衷心".into(), 7000),
        ("zhong'xin".into(), "钟鑫".into(), 5000),
        ("zhong'xin".into(), "中芯".into(), 3000),
        ("zhong'xin".into(), "众心".into(), 1000),
        ("zhong'xin".into(), "忠信".into(), 800),
        ("zhong".into(), "中".into(), 50000),
        ("xin".into(), "心".into(), 40000),
        ("xin".into(), "信".into(), 30000),
    ]);
    let engine = Engine::new(dict, Config::default());
    // 「中信」不在词库 → b2：权重 = avg(中芯3000, 众心1000) = 2000 → 词位 index 4
    let s = select_by_chars(&engine, "zhongxin", &["中", "信"]);
    assert_eq!(s.effect().end, Some(SessionEnd::Commit("中信".into())));
    let mut s2 = engine.start_session();
    for c in "zhongxin".chars() {
        s2.on_key(Key::Char(c));
    }
    let texts: Vec<String> = s2
        .effect()
        .candidates
        .iter()
        .map(|c| c.text.clone())
        .collect();
    let pos = |t: &str| texts.iter().position(|x| x == t);
    assert_eq!(
        pos("中信"),
        Some(4),
        "b2 权重 = avg(中芯3000, 众心1000) = 2000 → 词位 index 4：{texts:?}"
    );
}

#[test]
fn phrase_recording_scenario_0_skips_existing() {
    // 场景 0：词库已有整词（张威威），逐字选出来 → 不记录（权重不被覆盖）
    let dict = Dict::from_entries(vec![
        ("zhang".into(), "张".into(), 90000),
        ("wei".into(), "威".into(), 1000),
        ("zhang'wei'wei".into(), "张威威".into(), 6000),
    ]);
    let engine = Engine::new(dict, Config::default());
    let s = select_by_chars(&engine, "zhangweiwei", &["张", "威", "威"]);
    assert_eq!(s.effect().end, Some(SessionEnd::Commit("张威威".into())));
    let mut s2 = engine.start_session();
    for c in "zhangweiwei".chars() {
        s2.on_key(Key::Char(c));
    }
    assert!(s2.effect().candidates.iter().any(|c| c.text == "张威威"));
}

#[test]
fn phrase_recording_skips_non_char_selection() {
    // 边界：picked 含词（非单字）→ 不记录
    let dict = Dict::from_entries(vec![
        ("zhang".into(), "张".into(), 90000),
        ("wei'wei".into(), "威威".into(), 5000),
        ("wei".into(), "威".into(), 1000),
    ]);
    let engine = Engine::new(dict, Config::default());
    let mut s = engine.start_session();
    for c in "zhangweiwei".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.effect();
    let pos = e.candidates.iter().position(|c| c.text == "张").unwrap();
    s.on_key(Key::Digit((pos + 1) as u8)); // 张（k1 单字）
    let e = s.effect();
    let pos = e.candidates.iter().position(|c| c.text == "威威").unwrap();
    s.on_key(Key::Digit((pos + 1) as u8)); // 威威 → 全消费 commit
    assert!(s.effect().end.is_some());
    // 单音节单字直接选 → 不记录（picked 空）
    let mut s3 = engine.start_session();
    for c in "wei".chars() {
        s3.on_key(Key::Char(c));
    }
    let e = s3.effect();
    let pos = e.candidates.iter().position(|c| c.text == "威").unwrap();
    s3.on_key(Key::Digit((pos + 1) as u8));
    assert!(s3.effect().end.is_some());
}

#[test]
fn hide_candidate_removes_override_then_blocks_base() {
    // 隐藏语义：先删用户库条目（自造词），否则屏蔽基础库
    let dict = Dict::from_entries(vec![
        ("shou'xuan".into(), "首选".into(), 8000),
        ("shou'xuan".into(), "手癖".into(), 300),
        ("shou".into(), "手".into(), 50000),
        ("shou".into(), "首".into(), 40000),
        ("xuan".into(), "选".into(), 30000),
        ("xuan".into(), "癖".into(), 200),
    ]);
    let engine = Engine::new(dict, Config::default());
    // 先自造"手选"（b1）→ 用户库有条目
    let s = select_by_chars(&engine, "shouxuan", &["手", "选"]);
    assert!(s.effect().end.is_some());
    // 导航到"手选"并隐藏 → 从用户库删除
    let mut s2 = engine.start_session();
    for c in "shouxuan".chars() {
        s2.on_key(Key::Char(c));
    }
    let e = s2.effect();
    let pos = e.candidates.iter().position(|c| c.text == "手选").unwrap();
    for _ in 0..pos {
        s2.on_key(Key::Right);
    }
    s2.on_key(Key::HideCandidate);
    // 隐藏自造词 = 撤销自造（用户决策 3）：词位条目删除，整句仍可组出
    let e = s2.effect();
    let texts: Vec<String> = e.candidates.iter().map(|c| c.text.clone()).collect();
    let hs: Vec<&Candidate> = e.candidates.iter().filter(|c| c.text == "手选").collect();
    assert!(
        hs.iter().all(|c| c.kind == CandidateKind::Sentence),
        "自造词条目应被删除（只剩整句）：{texts:?}"
    );
    // 新会话：手选不再以词位出现
    let mut s3 = engine.start_session();
    for c in "shouxuan".chars() {
        s3.on_key(Key::Char(c));
    }
    let e3 = s3.effect();
    assert!(!e3
        .candidates
        .iter()
        .any(|c| c.text == "手选" && c.kind != CandidateKind::Sentence));
    // 隐藏基础库词"手癖" → 屏蔽
    let mut s4 = engine.start_session();
    for c in "shouxuan".chars() {
        s4.on_key(Key::Char(c));
    }
    let e4 = s4.effect();
    let pos = e4.candidates.iter().position(|c| c.text == "手癖").unwrap();
    for _ in 0..pos {
        s4.on_key(Key::Right);
    }
    s4.on_key(Key::HideCandidate);
    let texts4: Vec<String> = s4
        .effect()
        .candidates
        .iter()
        .map(|c| c.text.clone())
        .collect();
    assert!(
        !texts4.contains(&"手癖".to_string()),
        "基础库词被屏蔽后剔除：{texts4:?}"
    );
    assert!(texts4.contains(&"首选".to_string()));
    // 新会话持久生效
    let mut s5 = engine.start_session();
    for c in "shouxuan".chars() {
        s5.on_key(Key::Char(c));
    }
    assert!(!s5.effect().candidates.iter().any(|c| c.text == "手癖"));
}

#[test]
fn hide_candidate_selected_follows_position() {
    // 隐藏后高亮落在原位置附近（不越界、不崩溃）
    let dict = Dict::from_entries(vec![
        ("shou'xuan".into(), "首选".into(), 8000),
        ("shou'xuan".into(), "手癖".into(), 300),
        ("shou'xuan".into(), "手选".into(), 100),
    ]);
    let engine = Engine::new(dict, Config::default());
    let mut s = engine.start_session();
    for c in "shouxuan".chars() {
        s.on_key(Key::Char(c));
    }
    s.on_key(Key::HideCandidate); // 隐藏首位（首选）
    let e = s.effect();
    let texts: Vec<String> = e.candidates.iter().map(|c| c.text.clone()).collect();
    assert_eq!(texts, vec!["手癖", "手选"]);
    assert!(
        e.selected < e.candidates.len(),
        "selected 不越界：{}",
        e.selected
    );
}

// ===== M2.5 消费端多方案（2026-08-14）：dier 第二可达 / keneng 可能第一 =====

#[test]
fn dier_second_available_first() {
    // dier 贪心 [die,r]（r 是音节前缀被误判 Mixed 展开出"跌入"）；
    // 修复：classify 看全部方案（[di,er] 全完整 → FullPinyin）+ rank_plans
    // （di'er 词条权重最高 → 方案[0]）→ 「第二」第一，分节 di'er。
    let dict = Dict::from_entries(vec![
        ("di'er".into(), "第二".into(), 34485),
        ("di".into(), "地".into(), 50000),
        ("di".into(), "第".into(), 40000),
        ("die".into(), "跌".into(), 2932),
        ("die'ru".into(), "跌入".into(), 302),
    ]);
    let engine = Engine::new(dict, Config::default());
    let mut s = engine.start_session();
    for c in "dier".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.effect();
    assert_eq!(
        e.reading, "di'er",
        "分节显示应跟随词频最优方案，实际：{}",
        e.reading
    );
    let texts: Vec<String> = e.candidates.iter().map(|c| c.text.clone()).collect();
    let pos = |t: &str| texts.iter().position(|x| x == t);
    assert_eq!(
        pos("第二"),
        Some(0),
        "第二应第一（词条 34485），实际：{texts:?}"
    );
    // classic 曾把 die+r 判为 Mixed 误判不产出「跌入」；rime 简拼=兜底语义下
    // die+r→ru 走 class1 简拼路径，候选沉底（不出现在词条之前）——接受该差异。
    assert!(
        pos("跌入").is_some_and(|p| p > pos("第二").unwrap()),
        "跌入为简拼展开应沉底在第二之后：{texts:?}"
    );
}

#[test]
fn keneng_only_one_sentence_combos_removed() {
    // 2026-08-18 新规则：词库负责"词"、Viterbi 只负责"唯一最佳句子"。
    // keneng 只出唯一整句「可能」；[ken,eng] 的单字临时拼句「啃嗯」
    // （啃+嗯，非词库词条）不再作为候选（旧实现遍历所有方案各出整句）。
    let dict = Dict::from_entries(vec![
        ("ke'neng".into(), "可能".into(), 8000),
        ("ken".into(), "啃".into(), 500),
        ("eng".into(), "嗯".into(), 400),
        ("ke".into(), "可".into(), 60000),
        ("neng".into(), "能".into(), 30000),
    ]);
    let engine = Engine::new(dict, Config::default());
    let mut s = engine.start_session();
    for c in "keneng".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.effect();
    assert_eq!(
        e.reading, "ke'neng",
        "分节显示应跟随词频最优方案，实际：{}",
        e.reading
    );
    let texts: Vec<String> = e.candidates.iter().map(|c| c.text.clone()).collect();
    let pos = |t: &str| texts.iter().position(|x| x == t);
    assert_eq!(
        pos("可能"),
        Some(0),
        "可能应第一（词条命中），实际：{texts:?}"
    );
    // rime 词优先：可靠精确词「可能」在场 → 整句通道让位（classic 曾唯一 Sentence 置顶）
    let sentences: Vec<&Candidate> = e
        .candidates
        .iter()
        .filter(|c| c.kind == CandidateKind::Sentence)
        .collect();
    assert!(sentences.is_empty(), "词优先不组句：{texts:?}");
    assert!(
        !texts.iter().any(|t| t == "啃嗯"),
        "单字临时拼句不得作为候选：{texts:?}"
    );
}

#[test]
fn rank_plans_prefers_weightiest_plan() {
    // fenge 顺带受益：贪心 [feng,e]（风额）→ 词频重排 [fen,ge]（分割 8000）第一
    let dict = Dict::from_entries(vec![
        ("feng'e".into(), "风额".into(), 1),
        ("fen'ge".into(), "分割".into(), 8000),
        ("feng".into(), "风".into(), 5000),
        ("fen".into(), "分".into(), 40000),
        ("ge".into(), "个".into(), 50000),
        ("e".into(), "额".into(), 3000),
    ]);
    let engine = Engine::new(dict, Config::default());
    let mut s = engine.start_session();
    for c in "fenge".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.effect();
    assert_eq!(e.reading, "fen'ge", "分节应 fen'ge，实际：{}", e.reading);
    let texts: Vec<String> = e.candidates.iter().map(|c| c.text.clone()).collect();
    // unigram LM 特性：组合单字分可能高于词条（"分个" 在 "分割" 前）——M3 语言模型治本；
    // 本次保证：分节正确 + 分割可达（旧贪心 [feng,e] 下分割在词条路径第二，仍可达）。
    assert!(texts.iter().any(|t| t == "分割"), "分割应可达：{texts:?}");
}

#[test]
fn sentence_only_for_full_syllable_plans() {
    // 2026-08-18 新规则：只跑唯一一次 Viterbi（词频最优方案），至多一条 Sentence。
    // keneng 唯一整句「可能」；单字临时拼句「啃嗯」（啃+嗯）与含兜底段的组合
    // （可嫩g/啃嗯g/可呢ng）全部不再出现。
    let dict = Dict::from_entries(vec![
        ("ke'neng".into(), "可能".into(), 8000),
        ("ken".into(), "啃".into(), 500),
        ("eng".into(), "嗯".into(), 400),
        ("ke".into(), "可".into(), 60000),
        ("neng".into(), "能".into(), 30000),
        ("nen".into(), "嫩".into(), 100),
        ("ne".into(), "呢".into(), 50),
    ]);
    let engine = Engine::new(dict, Config::default());
    let mut s = engine.start_session();
    for c in "keneng".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.effect();
    let texts: Vec<String> = e.candidates.iter().map(|c| c.text.clone()).collect();
    let sentences: Vec<&Candidate> = e
        .candidates
        .iter()
        .filter(|c| c.kind == CandidateKind::Sentence)
        .collect();
    assert_eq!(texts[0], "可能", "词优先：词条置顶：{texts:?}");
    assert!(sentences.is_empty(), "词优先不组句：{texts:?}");
    for bad in ["啃嗯", "可嫩g", "啃嗯g", "可呢ng", "可嫩"] {
        assert!(
            !texts.iter().any(|t| t == bad),
            "单字拼句/劣质整句不应出现（{bad}）：{texts:?}"
        );
    }
    // dier 的"跌r"（[die,r] 含兜底 r）同样消失，词条「第二」置顶
    let dict2 = Dict::from_entries(vec![
        ("di'er".into(), "第二".into(), 34485),
        ("di".into(), "地".into(), 50000),
        ("die".into(), "跌".into(), 2932),
    ]);
    let engine2 = Engine::new(dict2, Config::default());
    let mut s2 = engine2.start_session();
    for c in "dier".chars() {
        s2.on_key(Key::Char(c));
    }
    let e2 = s2.effect();
    let texts2: Vec<String> = e2.candidates.iter().map(|c| c.text.clone()).collect();
    let sentences2: Vec<&Candidate> = e2
        .candidates
        .iter()
        .filter(|c| c.kind == CandidateKind::Sentence)
        .collect();
    assert_eq!(texts2[0], "第二", "词优先：词条置顶：{texts2:?}");
    assert!(sentences2.is_empty(), "词优先不组句：{texts2:?}");
    assert!(
        !texts2.iter().any(|t| t == "跌r"),
        "跌r 是含兜底段的劣质整句：{texts2:?}"
    );
}

#[test]
fn tail_completion_2b_produces_extended_sentence() {
    // 2026-08-18 新规则 2b：末段为音节前缀时补全为所有合法音节，每个补齐跑一次 Viterbi，
    // 取路径分最高的一条作为唯一 Sentence。句子文本可超出已敲字母（预编辑仍按输入切分）。
    let dict = Dict::from_entries(vec![
        ("shi'ge'cheng'yu".into(), "是一个成语".into(), 10000),
        ("shi'ge'cheng'yi".into(), "是一个意外".into(), 500),
        ("cheng'yu".into(), "成语".into(), 5000),
        ("shi".into(), "是".into(), 50000),
        ("ge".into(), "个".into(), 100000),
        ("cheng".into(), "程".into(), 1000),
        ("yu".into(), "鱼".into(), 800),
        ("yi".into(), "一".into(), 9000),
    ]);
    let engine = Engine::new(dict, Config::default());
    let mut s = engine.start_session();
    for c in "shigechengy".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.effect();
    assert_eq!(
        e.reading, "shi'ge'cheng'y",
        "预编辑仍按输入切分不扩展，实际：{}",
        e.reading
    );
    let texts: Vec<String> = e.candidates.iter().map(|c| c.text.clone()).collect();
    let sentences: Vec<&Candidate> = e
        .candidates
        .iter()
        .filter(|c| c.kind == CandidateKind::Sentence)
        .collect();
    assert_eq!(texts[0], "是一个成语", "2b 补全类2置顶：{texts:?}");
    assert!(sentences.is_empty(), "词优先不组句：{texts:?}");
    // rime 2b 补全为「类2 全量词条」（词条在补全键上均收集），非 classic 唯一句
    assert!(texts.iter().any(|t| t == "是一个意外"), "是一个意外应可达：{texts:?}");
    // 词条通道按输入砍（砍 y 而非补 yu）：单独「成语」（cheng'yu）不从补全键出
    assert!(
        !texts.iter().any(|t| t == "成语"),
        "词条通道不得从补全后的键出词：{texts:?}"
    );
    // 单字仍可达
    assert!(texts.iter().any(|t| t == "是"), "单字应可达：{texts:?}");
}

#[test]
fn word_channel_only_real_entries() {
    // 2026-08-18 新规则：词条通道只出词库真实词条（exact），不再有按切分方案临时组词；
    // 唯一 Sentence 由 Viterbi 给出（可为单字组合，如 安建）。
    let dict = Dict::from_entries(vec![
        ("an'jian".into(), "案件".into(), 10000),
        ("an'jian".into(), "按肩".into(), 500),
        ("an'jian".into(), "暗箭".into(), 300),
        ("an".into(), "安".into(), 80000),
        ("jian".into(), "建".into(), 60000),
    ]);
    let engine = Engine::new(dict, Config::default());
    let mut s = engine.start_session();
    for c in "anjian".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.effect();
    let texts: Vec<String> = e.candidates.iter().map(|c| c.text.clone()).collect();
    let sentences: Vec<&Candidate> = e
        .candidates
        .iter()
        .filter(|c| c.kind == CandidateKind::Sentence)
        .collect();
    assert!(sentences.is_empty(), "词优先不组句：{texts:?}");
    // 词条通道三词全部可达（an'jian，seg_len=2）
    for w in ["案件", "按肩", "暗箭"] {
        assert!(texts.iter().any(|t| t == w), "{w} 应可达：{texts:?}");
    }
    // 非词库拼法（按键/案肩 等方案级临时词）不得出现
    for bad in ["按键", "案肩", "安j"] {
        assert!(!texts.iter().any(|t| t == bad), "不应出现（{bad}）：{texts:?}");
    }
}

#[test]
fn long_input_single_sentence() {
    // 2026-08-18 新规则：Viterbi 与长度无关，超长输入同样只出一条 Sentence。
    let dict = Dict::from_entries(vec![
        ("xi'huan'zhong'guo".into(), "喜欢中国".into(), 40000),
        ("xi".into(), "喜".into(), 20000),
        ("huan".into(), "欢".into(), 30000),
        ("zhong".into(), "中".into(), 100000),
        ("guo".into(), "国".into(), 90000),
    ]);
    let engine = Engine::new(dict, Config::default());
    let mut s = engine.start_session();
    for c in "xihuanzhongguo".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.effect();
    let texts: Vec<String> = e.candidates.iter().map(|c| c.text.clone()).collect();
    let sentences: Vec<&Candidate> = e
        .candidates
        .iter()
        .filter(|c| c.kind == CandidateKind::Sentence)
        .collect();
    assert_eq!(
        texts[0], "喜欢中国",
        "词优先：整词词条置顶：{texts:?}"
    );
    assert!(sentences.is_empty(), "词优先不组句：{texts:?}");
}

// ===== 预编辑显示规则（2026-08-16，对齐主流）：只显示用户输入+分节；有匹配分节、无匹配原样；分节可跟随候选（基于输入）=====

#[test]
fn preview_follows_candidate_when_code_matches() {
    // fenge：分割(fen'ge)/风额(feng'e)/分隔(fen'ge)/分个(fen'ge 整句)/分(fen+尾巴)
    let dict = Dict::from_entries(vec![
        ("feng'e".into(), "风额".into(), 5000),
        ("fen'ge".into(), "分割".into(), 8000),
        ("fen'ge".into(), "分隔".into(), 3000),
        ("feng".into(), "风".into(), 5000),
        ("fen".into(), "分".into(), 40000),
        ("ge".into(), "个".into(), 50000),
        ("e".into(), "额".into(), 3000),
    ]);
    let engine = Engine::new(dict, Config::default());
    let mut s = engine.start_session();
    for c in "fenge".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.effect();
    assert_eq!(
        e.reading, "fen'ge",
        "初始高亮候选 0（分个/分割均 fen'ge），实际：{}",
        e.reading
    );
    // 导航到"风额"（feng'e 词条，code==输入 fenge）→ 跟随候选切分 feng'e（主流行为）
    let mut guard = 0;
    while s.effect().candidates[s.effect().selected].text != "风额" {
        s.on_key(Key::Right);
        guard += 1;
        assert!(guard < 10, "候选应含风额");
    }
    assert_eq!(
        s.effect().reading,
        "feng'e",
        "导航风额应跟随切分 feng'e，实际：{}",
        s.effect().reading
    );
    // 导航到单字"分"（code=fen≠输入 fenge）→ 输入切分 fen'ge（不跟随单字）
    let mut guard = 0;
    while s.effect().candidates[s.effect().selected].text != "分" {
        s.on_key(Key::Right);
        guard += 1;
        assert!(guard < 10, "候选应含单字分");
    }
    assert_eq!(
        s.effect().reading,
        "fen'ge",
        "单字分 code≠输入，预编辑仍输入切分 fen'ge，实际：{}",
        s.effect().reading
    );
    // 导航回候选 0 上屏（预览不影响 commit）
    while s.effect().selected != 0 {
        s.on_key(Key::Left);
    }
    let first = s.effect().candidates[0].text.clone();
    let e = s.on_key(Key::Space);
    assert_eq!(e.end, Some(SessionEnd::Commit(first)));
}

#[test]
fn editorial_preview_uses_schema_display() {
    // 前缀档（n → 你/ni）：预编辑 = 切分显示 "n"
    let dict = Dict::from_entries(vec![
        ("ni".into(), "你".into(), 50000),
        ("ni".into(), "泥".into(), 100),
        ("na".into(), "那".into(), 40000),
    ]);
    let engine = Engine::new(dict, Config::default());
    let mut s = engine.start_session();
    s.on_key(Key::Char('n'));
    let e = s.effect();
    assert_eq!(
        e.reading, "n",
        "前缀档预编辑 = 切分显示，实际：{}",
        e.reading
    );
    // 简拼（nh）：预编辑 = 引擎切分 n'h（不跟随候选 code "nh"）
    let dict2 = Dict::from_entries(vec![
        ("ni".into(), "你".into(), 50000),
        ("nh".into(), "你好".into(), 8000),
        ("nh".into(), "泥嚎".into(), 100),
    ]);
    let engine2 = Engine::new(dict2, Config::default());
    let mut s2 = engine2.start_session();
    for c in "nh".chars() {
        s2.on_key(Key::Char(c));
    }
    let e2 = s2.effect();
    assert_eq!(e2.reading, "n'h", "简拼预编辑 = 切分显示，实际：{}", e2.reading);
}

#[test]
fn editorial_preview_keeps_forced_apostrophes() {
    // 原文兜底候选（强制撇号）：预编辑保留切分显示（i'n'p'u't）
    let dict = Dict::from_entries(vec![("ni".into(), "你".into(), 50000)]);
    let engine = Engine::new(dict, Config::default());
    let mut s = engine.start_session();
    for c in "i'n'p'u't".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.effect();
    assert_eq!(
        e.reading, "i'n'p'u't",
        "预编辑保留强制撇号切分，实际：{}",
        e.reading
    );
}

/// 预编辑规则对齐主流（2026-08-16）：只显示用户输入+分节；有匹配分节、无匹配原样；
/// 分节可跟随候选（基于输入）；用户强制撇号保留。
#[test]
fn preview_editorial_rules_aligned_with_mainstream() {
    // jisb → 简拼整词场景（词条 code=jishiben≠jisb，fixture 无简拼键不产整词候选——
    // 用"几"（ji 前缀，code≠输入）验证同一规则）：输入切分 ji's'b（不跟随——核心修复）
    let dict = Dict::from_entries(vec![
        ("jishiben".into(), "记事本".into(), 50000),
        ("ji".into(), "几".into(), 5000),
    ]);
    let engine = Engine::new(dict, Config::default());
    let mut s = engine.start_session();
    for c in "jisb".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.effect();
    assert_eq!(
        e.reading, "ji's'b",
        "简拼整词不跟随（jishiben≠jisb），实际：{}",
        e.reading
    );

    // xian → 导航到"西安"（code=xi'an==xian）：跟随切分 xi'an
    let dict2 = Dict::from_entries(vec![
        ("xi'an".into(), "西安".into(), 50000),
        ("xian".into(), "先".into(), 40000),
        ("xian".into(), "线".into(), 30000),
    ]);
    let engine2 = Engine::new(dict2, Config::default());
    let mut s2 = engine2.start_session();
    for c in "xian".chars() {
        s2.on_key(Key::Char(c));
    }
    let mut guard = 0;
    while s2.effect().candidates[s2.effect().selected].text != "西安" {
        s2.on_key(Key::Right);
        guard += 1;
        assert!(guard < 10, "候选应含西安");
    }
    assert_eq!(
        s2.effect().reading, "xi'an",
        "西安跟随切分 xi'an，实际：{}",
        s2.effect().reading
    );

    // input 无匹配：原样不分节
    let dict3 = Dict::from_entries(vec![("ni".into(), "你".into(), 50000)]);
    let engine3 = Engine::new(dict3, Config::default());
    let mut s3 = engine3.start_session();
    for c in "input".chars() {
        s3.on_key(Key::Char(c));
    }
    assert_eq!(
        s3.effect().reading, "input",
        "无匹配原样不分节，实际：{}",
        s3.effect().reading
    );

    // 用户强制撇号保留参与分节：zhu'jincheng → zhu'jin'cheng
    let dict4 = Dict::from_entries(vec![
        ("zhujincheng".into(), "朱金成".into(), 50000),
        ("zhu".into(), "朱".into(), 40000),
        ("jin".into(), "金".into(), 30000),
        ("cheng".into(), "成".into(), 30000),
    ]);
    let engine4 = Engine::new(dict4, Config::default());
    let mut s4 = engine4.start_session();
    for c in "zhu'jincheng".chars() {
        s4.on_key(Key::Char(c));
    }
    assert_eq!(
        s4.effect().reading, "zhu'jin'cheng",
        "用户'保留参与分节，实际：{}",
        s4.effect().reading
    );
}

/// 全角模式：Enter 提交预编辑原文 → 全角（微软对齐，28-initial-state-settings.md §8 影响点 1）。
#[test]
fn full_width_enter_commits_fullwidth_raw() {
    let cfg = Config {
        initial_state: ImeState {
            width: WidthMode::Full,
            ..ImeState::default()
        },
        ..Config::default()
    };
    let engine = Engine::new(fixture_dict(), cfg);
    let mut s = engine.start_session();
    for c in "nihao".chars() {
        s.on_key(Key::Char(c));
    }
    assert!(s.effect().candidates.iter().any(|c| c.text == "你好"), "有候选才验证 Enter 原文上屏");
    // pending_text（flush 路径）活动期即全角
    assert_eq!(s.pending_text(), "ｎｉｈａｏ", "flush 原文上屏应全角");
    let e = s.on_key(Key::Enter);
    assert_eq!(
        e.end,
        Some(SessionEnd::Commit("ｎｉｈａｏ".into())),
        "全角下 Enter 应提交全角原文，实际：{:?}",
        e.end
    );
    assert!(!s.is_active());
}

/// 全角模式：Space 选候选 → 汉字不受宽度影响；无候选原文兜底 → 全角。
#[test]
fn full_width_space_candidate_vs_fallback() {
    let cfg = Config {
        initial_state: ImeState {
            width: WidthMode::Full,
            ..ImeState::default()
        },
        ..Config::default()
    };
    let engine = Engine::new(fixture_dict(), cfg);
    // 有候选：Space 提交候选 你好（汉字 no-op）
    let mut s1 = engine.start_session();
    for c in "nihao".chars() {
        s1.on_key(Key::Char(c));
    }
    let e1 = s1.on_key(Key::Space);
    assert_eq!(e1.end, Some(SessionEnd::Commit("你好".into())));
    // 无候选：原文兜底（window 不匹配词库）→ 全角
    let mut s2 = engine.start_session();
    for c in "window".chars() {
        s2.on_key(Key::Char(c));
    }
    assert!(s2.effect().candidates.iter().all(|c| c.text != "你好"), "window 应无候选");
    let e2 = s2.on_key(Key::Space);
    assert_eq!(e2.end, Some(SessionEnd::Commit("ｗｉｎｄｏｗ".into())));
}

/// 半角回归：原文上屏保持半角（行为不变）。
#[test]
fn half_width_raw_commit_unchanged() {
    let engine = Engine::new(fixture_dict(), Config::default());
    let mut s = engine.start_session();
    for c in "nihao".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.on_key(Key::Enter);
    assert_eq!(e.end, Some(SessionEnd::Commit("nihao".into())));
}

// ===== 31-script-traditional.md：简→繁转换 =====

/// 繁体模式：候选文本显示繁体（词条「网」显示为「網」，单字表兜底）。
#[test]
fn traditional_candidates_converted() {
    let dict = Dict::from_entries(vec![
        ("wang".into(), "网".into(), 500),
        ("wang".into(), "旺".into(), 300),
    ]);
    let cfg = Config {
        initial_state: ImeState {
            script: iuv_core::ScriptMode::Traditional,
            ..ImeState::default()
        },
        ..Config::default()
    };
    let engine = Engine::new(dict, cfg);
    let table = iuv_data::opencc::from_text("", "网\t網\n").unwrap();
    engine.attach_script_converter(Some(std::sync::Arc::new(
        iuv_core::ScriptConverter::new(table),
    )));
    let mut s = engine.start_session();
    for c in "wang".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.effect();
    assert!(e.candidates.iter().any(|c| c.text == "網"), "网应显示为網");
    // 未入表的字（旺）原样
    assert!(e.candidates.iter().any(|c| c.text == "旺"));
}

/// 繁体模式：候选「网」单字被转换为「網」（单字表兜底）。
#[test]
fn traditional_single_char_converted() {
    // 需要词典里有一个网字词条；fixture_dict 无网 → 用 from_entries 注入
    let dict = Dict::from_entries(vec![("wang".into(), "网".into(), 500)]);
    let cfg = Config {
        initial_state: ImeState {
            script: iuv_core::ScriptMode::Traditional,
            ..ImeState::default()
        },
        ..Config::default()
    };
    let engine = Engine::new(dict, cfg);
    let table = iuv_data::opencc::from_text("", "网\t網\n").unwrap();
    engine.attach_script_converter(Some(std::sync::Arc::new(
        iuv_core::ScriptConverter::new(table),
    )));
    let mut s = engine.start_session();
    for c in "wang".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.effect();
    assert!(e.candidates.iter().any(|c| c.text == "網"), "候选应显示網");
}

/// 繁体模式：整词上屏转换为繁体（以后 → 以後）。
#[test]
fn traditional_commit_converted() {
    // fixture_dict 无「以后」，注入
    let dict = Dict::from_entries(vec![("yi'hou".into(), "以后".into(), 8000)]);
    let cfg = Config {
        initial_state: ImeState {
            script: iuv_core::ScriptMode::Traditional,
            ..ImeState::default()
        },
        ..Config::default()
    };
    let engine = Engine::new(dict, cfg);
    let table = iuv_data::opencc::from_text("以后\t以後\n", "").unwrap();
    engine.attach_script_converter(Some(std::sync::Arc::new(
        iuv_core::ScriptConverter::new(table),
    )));
    let mut s = engine.start_session();
    for c in "yihou".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.on_key(Key::Space);
    assert_eq!(e.end, Some(SessionEnd::Commit("以後".into())));
}

/// 繁体模式：自造词记录简体原文（commit 前不转换；录音 = 简体，与全角「录原文不录全角」同构）。
#[test]
fn traditional_selfmade_records_simplified() {
    // 逐字选择自造：网+络 → 记录"网络"（简体），commit 上屏"網絡"（繁体）
    let dict = Dict::from_entries(vec![
        ("wang".into(), "网".into(), 500),
        ("luo".into(), "络".into(), 400),
    ]);
    let cfg = Config {
        initial_state: ImeState {
            script: iuv_core::ScriptMode::Traditional,
            ..ImeState::default()
        },
        ..Config::default()
    };
    let engine = Engine::new(dict, cfg);
    let table = iuv_data::opencc::from_text("网络\t網絡\n", "网\t網\n络\t絡\n").unwrap();
    engine.attach_script_converter(Some(std::sync::Arc::new(
        iuv_core::ScriptConverter::new(table),
    )));
    let mut s = engine.start_session();
    for c in "wangluo".chars() {
        s.on_key(Key::Char(c));
    }
    // 逐字选：網 → 絡（悬空续接 → 全消费 commit；候选显示繁体）
    let mut guard = 0;
    let pos = loop {
        let e = s.effect();
        if let Some(p) = e.candidates.iter().position(|c| c.text == "網") {
            break p;
        }
        guard += 1;
        assert!(guard < 200, "「網」不在候选");
        s.on_key(Key::PageDown);
    };
    s.on_key(Key::Digit((pos + 1) as u8));
    let mut guard = 0;
    let pos = loop {
        let e = s.effect();
        if let Some(p) = e.candidates.iter().position(|c| c.text == "絡") {
            break p;
        }
        guard += 1;
        assert!(guard < 200, "「絡」不在候选");
        s.on_key(Key::PageDown);
    };
    s.on_key(Key::Digit((pos + 1) as u8));
    assert_eq!(s.effect().end, Some(SessionEnd::Commit("網絡".into())));
    // 用户库应记录简体（内部键不变）
    let user = engine.user_dict().expect("自造词后用户库应已装配");
    assert!(user
        .adjusted("wang'luo")
        .iter()
        .any(|(word, _)| word == "网络"));
}

/// 简体回归：默认模式转换器装配也不生效（零行为变化）。
#[test]
fn simplified_mode_converter_inert() {
    let dict = Dict::from_entries(vec![("yi'hou".into(), "以后".into(), 8000)]);
    let engine = Engine::new(dict, Config::default());
    let table = iuv_data::opencc::from_text("以后\t以後\n", "").unwrap();
    engine.attach_script_converter(Some(std::sync::Arc::new(
        iuv_core::ScriptConverter::new(table),
    )));
    let mut s = engine.start_session();
    for c in "yihou".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.on_key(Key::Space);
    assert_eq!(e.end, Some(SessionEnd::Commit("以后".into())));
}

/// 繁体模式数据缺失：降级简体输出（不 panic）。
#[test]
fn traditional_no_converter_degraded() {
    let cfg = Config {
        initial_state: ImeState {
            script: iuv_core::ScriptMode::Traditional,
            ..ImeState::default()
        },
        ..Config::default()
    };
    let engine = Engine::new(fixture_dict(), cfg); // 未装配转换器
    let mut s = engine.start_session();
    for c in "nihao".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.on_key(Key::Space);
    assert_eq!(e.end, Some(SessionEnd::Commit("你好".into())));
}

// ===== 会话内符号字面尾巴（issue「d冒号表现不一致」） =====

#[test]
fn colon_enters_literal_tail_and_enter_commits_verbatim() {
    let engine = default_engine();
    let mut s = engine.start_session();
    s.on_key(Key::Char('d'));
    s.on_key(Key::Char(':'));
    let e = s.effect();
    // 字面态：预编辑显示 拼音+尾巴，无汉字候选（对齐搜狗）
    assert_eq!(e.composition, "d:");
    assert!(e.reading.is_empty());
    assert!(e.candidates.is_empty());
    // Enter 原样上屏
    match s.on_key(Key::Enter).end {
        Some(SessionEnd::Commit(text)) => assert_eq!(text, "d:"),
        other => panic!("期望提交 d:，实际 {other:?}"),
    }
}

#[test]
fn literal_backspace_restores_pinyin_candidates() {
    let engine = default_engine();
    let mut s = engine.start_session();
    s.on_key(Key::Char('d'));
    s.on_key(Key::Char(':'));
    // Backspace 删 ':' → 回拼音态，候选恢复
    let e = s.on_key(Key::Backspace);
    assert_eq!(e.composition, "d");
    assert!(!e.candidates.is_empty());
    match s.on_key(Key::Space).end {
        Some(SessionEnd::Commit(text)) => assert_eq!(text, "的"),
        other => panic!("期望回拼音态后 Space 提交 的，实际 {other:?}"),
    }
}

#[test]
fn literal_mode_locks_following_keys_until_cleared() {
    let engine = default_engine();
    let mut s = engine.start_session();
    for k in [Key::Char('d'), Key::Char(':'), Key::Char('\\'), Key::Char('t')] {
        s.on_key(k);
    }
    assert_eq!(s.effect().composition, "d:\\t");
    // 字面锁定：数字/字母照常追加，不选词不进拼音
    s.on_key(Key::Digit(2));
    s.on_key(Key::Char('x'));
    assert_eq!(s.effect().composition, "d:\\t2x");
    // Enter 原样提交
    match s.on_key(Key::Enter).end {
        Some(SessionEnd::Commit(text)) => assert_eq!(text, "d:\\t2x"),
        other => panic!("期望字面整体原样提交，实际 {other:?}"),
    }
}

#[test]
fn literal_esc_cancels_whole_session() {
    let engine = default_engine();
    let mut s = engine.start_session();
    for k in [Key::Char('d'), Key::Char(':')] {
        s.on_key(k);
    }
    match s.on_key(Key::Esc).end {
        Some(SessionEnd::Cancel) => {}
        other => panic!("期望字面态 Esc 取消会话，实际 {other:?}"),
    }
}
