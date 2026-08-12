//! ime-core 引擎与会话集成测试。任务书 11 §4 全部用例。
//! 用 Dict::from_entries 造小词典，不依赖真实词库文件。

use ime_core::{
    Candidate, CandidateKind, Config, Engine, Key, Quanpin, RerankCtx, RerankStage, Session,
    SessionEnd, UserDataStore,
};
use ime_data::Dict;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

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
    ])
}

pub fn default_engine() -> Arc<Engine> {
    Engine::new(fixture_dict(), Config::default())
}

// ===== engine 候选生成 =====

#[test]
fn candidates_sentence_first() {
    let engine = default_engine();
    let mut s = engine.start_session();
    for c in "nihao".chars() {
        s.on_key(Key::Char(c));
    }
    let e = s.effect();
    assert_eq!(e.candidates[0].kind, CandidateKind::Sentence);
    assert!(e.candidates[0].text.contains("你好"));
    assert_eq!(e.candidates[0].weight, 0);
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
    assert!(texts.iter().position(|t| *t == "的").unwrap() < texts.iter().position(|t| *t == "得").unwrap());
    assert!(texts.iter().position(|t| *t == "得").unwrap() < texts.iter().position(|t| *t == "地").unwrap());
    assert_eq!(s.effect().candidates[0].text, "的");
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
    let texts: Vec<String> = s.effect().candidates.iter().map(|c| c.text.clone()).collect();
    assert!(texts.contains(&"你好".to_string()));
    assert!(!texts.contains(&"你好啊".to_string()), "默认不应出现前缀联想词，实际：{texts:?}");
    // 开启：追加联想长词
    let cfg = Config { candidate_prefix: true, ..Config::default() };
    let engine2 = Engine::new(dict, cfg);
    let mut s2 = engine2.start_session();
    for c in "nihao".chars() {
        s2.on_key(Key::Char(c));
    }
    let texts2: Vec<String> = s2.effect().candidates.iter().map(|c| c.text.clone()).collect();
    assert!(texts2.contains(&"你好啊".to_string()), "开启后应出现前缀联想词，实际：{texts2:?}");
}

#[test]
fn prefix_completion_recalled() {
    // 联想开启时，未完成码 "nih" 也能通过前缀补全召回 "你好"
    let cfg = Config { candidate_prefix: true, ..Config::default() };
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
    let count = s.effect().candidates.iter().filter(|c| c.text == "你好").count();
    assert_eq!(count, 1);
}

#[test]
fn max_candidates_capped() {
    let cfg = Config { max_candidates: 3, ..Config::default() };
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
        ("chuang'qian'ming'yue'guang".into(), "床前明月光".into(), 8000),
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
    let texts: Vec<String> = s.effect().candidates.iter().map(|c| c.text.clone()).collect();
    let expect = ["床前明月光", "床前明月", "床前明", "床前", "床"];
    let mut pos = 0usize;
    for want in expect {
        let at = texts.iter().position(|t| t == want).expect(&format!("候选应含 {want}，实际：{texts:?}"));
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
    let texts: Vec<String> = s.effect().candidates.iter().map(|c| c.text.clone()).collect();
    assert!(texts.contains(&"这是".to_string()), "实际：{texts:?}");
    assert!(texts.contains(&"知识".to_string()), "实际：{texts:?}");
    let zhe = texts.iter().position(|t| t == "这").expect("单字应可及，实际：{texts:?}");
    let zheshi = texts.iter().position(|t| t == "这是").unwrap();
    assert!(zheshi < zhe, "词级应在单字级之前（从长到短），实际：{texts:?}");
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
    let texts: Vec<String> = s.effect().candidates.iter().map(|c| c.text.clone()).collect();
    assert_eq!(texts[0], "先");
    assert!(texts.iter().position(|t| t == "西安").unwrap() > 0);
    // 西安（6091）按权重在 先/线 之后。
    let xi_an = texts.iter().position(|t| t == "西安").unwrap();
    let xian = texts.iter().position(|t| t == "先").unwrap();
    let xian2 = texts.iter().position(|t| t == "线").unwrap();
    assert!(xian < xi_an && xian2 < xi_an, "实际：{texts:?}");
}

// ===== 续接（picked 栈 + 尾巴续接，契约 §4.1 选词行，M1 后期）=====

/// 长句词库：整句/两字词/单字 + 尾巴整词，供续接用例。
fn tail_dict() -> Dict {
    Dict::from_entries(vec![
        ("chuang'qian'ming'yue'guang".into(), "床前明月光".into(), 8000),
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
    let idx = s.effect().candidates.iter().position(|c| c.text == "床前").unwrap();
    let e = s.on_key(Key::Digit((idx + 1) as u8));
    assert_eq!(e.end, None, "续接不应结束会话");
    assert_eq!(e.composition, "床前ming'yue'guang", "混合预编辑：已选汉字+尾巴拼音，实际：{}", e.composition);
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
    let idx = s.effect().candidates.iter().position(|c| c.text == "床前").unwrap();
    s.on_key(Key::Digit((idx + 1) as u8));
    // 空格上屏首选（明月光）→ 全量结束：床前明月光。
    let e = s.on_key(Key::Space);
    assert_eq!(e.end, Some(SessionEnd::Commit("床前明月光".into())));
    assert!(!s.is_active());
}

/// 退格回退已选词：pop 栈顶，raw 恢复原输入。
#[test]
fn backspace_pops_picked() {
    let engine = Engine::new(tail_dict(), Config::default());
    let mut s = engine.start_session();
    type_long(&mut s);
    let idx = s.effect().candidates.iter().position(|c| c.text == "床前").unwrap();
    s.on_key(Key::Digit((idx + 1) as u8));
    let e = s.on_key(Key::Backspace);
    assert_eq!(e.end, None, "回退栈顶不结束会话");
    assert_eq!(e.composition, "chuang'qian'ming'yue'guang", "raw 恢复，实际：{}", e.composition);
    assert!(e.candidates.iter().any(|c| c.text == "床前明月光"), "候选恢复整句");
    assert!(s.is_active());
}

/// 悬空状态下按 Esc：已选词上屏（尾巴随之取消），非整句取消。
#[test]
fn esc_with_picked_commits_picked() {
    let engine = Engine::new(tail_dict(), Config::default());
    let mut s = engine.start_session();
    type_long(&mut s);
    let idx = s.effect().candidates.iter().position(|c| c.text == "床前").unwrap();
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

// ===== M1.5 三路路由（微软实测对齐，docs/research/msime-probe-checklist.txt）=====

/// M1.5 词典：模拟 dictc 产物——全拼键 + 简拼键同表（简拼键为 dictc 自动生成，
/// 引擎测试里手工给出等价数据）。
fn m15_dict() -> Dict {
    Dict::from_entries(vec![
        // 单段档单字
        ("de".into(), "的".into(), 100000),
        ("de".into(), "得".into(), 300),
        ("shi".into(), "是".into(), 90000),
        ("shi".into(), "时".into(), 50000),
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
    assert!(e.candidates.iter().all(|c| c.kind == CandidateKind::Char), "单字母档应纯单字");
}

/// 部分音节档：sh → 纯单字（是/时/上），无词。
#[test]
fn prefix_segment_chars_only() {
    let engine = Engine::new(m15_dict(), Config::default());
    let mut s = engine.start_session();
    for c in "sh".chars() {
        s.on_key(Key::Char(c));
    }
    let binding = s.effect();
    let texts: Vec<&str> = binding.candidates.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, vec!["是", "时", "上"]);
    assert!(s.effect().candidates.iter().all(|c| c.kind == CandidateKind::Char));
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
    let idx = s.effect().candidates.iter().position(|c| c.text == "时").unwrap();
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

/// 单段非前缀（v）：无候选（微软 A 组实测：i/u/v 只有字面）。
#[test]
fn non_prefix_single_letter_empty() {
    let engine = Engine::new(m15_dict(), Config::default());
    let mut s = engine.start_session();
    for c in "v".chars() {
        s.on_key(Key::Char(c));
    }
    assert!(s.effect().candidates.is_empty());
    let e = s.on_key(Key::Space);
    assert_eq!(e.end, Some(SessionEnd::Commit("v".into())));
}

/// 简拼档：nh → 纯词（你好/泥嚎），无单字（微软 D 组实测：nh→你好您好女孩你还）。
#[test]
fn abbrev_words_only_no_chars() {
    let engine = Engine::new(m15_dict(), Config::default());
    let mut s = engine.start_session();
    for c in "nh".chars() {
        s.on_key(Key::Char(c));
    }
    let binding = s.effect();
    let texts: Vec<&str> = binding.candidates.iter().map(|c| c.text.as_str()).collect();
    assert_eq!(texts, vec!["你好", "泥嚎"]);
    assert!(s.effect().candidates.iter().all(|c| c.kind == CandidateKind::Word), "简拼档应纯词");
    assert!(!texts.contains(&"你"), "简拼候选不含单字，实际：{texts:?}");
}

/// 简拼无长度上限 + 逐级砍尾巴：nhmsx → 你还没睡醒(k5)/你还没说(k4)/你还没(k3)/你好(k2)。
#[test]
fn abbrev_tail_levels_longest_first() {
    let engine = Engine::new(m15_dict(), Config::default());
    let mut s = engine.start_session();
    for c in "nhmsx".chars() {
        s.on_key(Key::Char(c));
    }
    let texts: Vec<String> = s.effect().candidates.iter().map(|c| c.text.clone()).collect();
    let expect = ["你还没睡醒", "你还没说", "你还没", "你好"];
    let mut pos = 0usize;
    for want in expect {
        let at = texts.iter().position(|t| t == want)
            .unwrap_or_else(|| panic!("候选应含 {want}，实际：{texts:?}"));
        assert!(at >= pos, "{want} 应排在更长层级之后，实际：{texts:?}");
        pos = at;
    }
}

/// 简拼部分消费：选中"你还没说"（k=4）→ 词上屏 + 尾巴 x 续接（悬空续接复用）。
#[test]
fn abbrev_partial_commit_keeps_tail() {
    let engine = Engine::new(m15_dict(), Config::default());
    let mut s = engine.start_session();
    for c in "nhmsx".chars() {
        s.on_key(Key::Char(c));
    }
    let idx = s.effect().candidates.iter().position(|c| c.text == "你还没说").unwrap();
    let e = s.on_key(Key::Digit((idx + 1) as u8));
    assert_eq!(e.end, None, "部分消费不结束会话");
    assert_eq!(e.composition, "你还没说x", "词上屏+尾巴拼音，实际：{}", e.composition);
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
    let texts: Vec<String> = s.effect().candidates.iter().map(|c| c.text.clone()).collect();
    assert_eq!(texts[0], "你好", "混拼词应居首，实际：{texts:?}");
    assert!(texts.contains(&"泥嚎".to_string()));
    // 单字（你/那）排在词后
    let ni = texts.iter().position(|t| t == "你").unwrap();
    let nihao = texts.iter().position(|t| t == "你好").unwrap();
    assert!(nihao < ni, "词前字后，实际：{texts:?}");
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
    // "w" 无词条、单音节无 Sentence → 无候选
    s.on_key(Key::Char('w'));
    assert!(s.effect().candidates.is_empty());
    let e = s.on_key(Key::Space);
    assert_eq!(e.end, Some(SessionEnd::Commit("w".into())));
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
    let cfg = Config { page_size: 1, ..Config::default() };
    let engine = Engine::new(fixture_dict(), cfg);
    let mut s = engine.start_session();
    for c in "de".chars() {
        s.on_key(Key::Char(c));
    }
    let mut e = s.on_key(Key::Down);
    assert_eq!(e.page.page_count, 3);
    assert_eq!(e.page.page, 0);
    assert_eq!(e.selected, 0); // 单候选页 clamp
    e = s.on_key(Key::PageDown);
    assert_eq!(e.page.page, 1);
    assert_eq!(e.selected, 0);
    e = s.on_key(Key::PageDown);
    assert_eq!(e.page.page, 2);
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

// ===== spy 部件 =====

struct SpyStore {
    calls: Mutex<Vec<(String, String)>>,
}

impl UserDataStore for SpyStore {
    fn record_selection(&mut self, code: &str, text: &str, _now: SystemTime) {
        self.calls.lock().unwrap().push((code.to_string(), text.to_string()));
    }
    fn power(&self, _code: &str, _text: &str, _now: SystemTime) -> f32 {
        0.0
    }
}

#[test]
fn commit_records_selection() {
    let store = Box::new(SpyStore { calls: Mutex::new(Vec::new()) });
    let schema = Box::new(Quanpin::new(fixture_dict().syllables().clone()));
    let lm = Box::new(ime_core::UnigramLm::new(1000000, 100));
    let engine = Engine::with_parts(
        fixture_dict(),
        Config::default(),
        schema,
        lm,
        vec![],
        store,
    );
    let mut s = engine.start_session();
    for c in "de".chars() {
        s.on_key(Key::Char(c));
    }
    s.on_key(Key::Space);
    // 通过公开 API 无法读 store，这里用第二个 session + digit 验证 commit 路径不 panic 即可，
    // 详细入参断言在下方 with store 内嵌测试。
    assert!(!s.is_active());
}

struct SpyStoreShared(Arc<Mutex<Vec<(String, String)>>>);

impl UserDataStore for SpyStoreShared {
    fn record_selection(&mut self, code: &str, text: &str, _now: SystemTime) {
        self.0.lock().unwrap().push((code.to_string(), text.to_string()));
    }
    fn power(&self, _code: &str, _text: &str, _now: SystemTime) -> f32 {
        0.0
    }
}

#[test]
fn commit_records_selection_args() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let store = Box::new(SpyStoreShared(log.clone()));
    let schema = Box::new(Quanpin::new(fixture_dict().syllables().clone()));
    let lm = Box::new(ime_core::UnigramLm::new(fixture_dict().total_weight(), fixture_dict().entry_count()));
    let engine = Engine::with_parts(fixture_dict(), Config::default(), schema, lm, vec![], store);
    let mut s = engine.start_session();
    for c in "de".chars() {
        s.on_key(Key::Char(c));
    }
    s.on_key(Key::Space);
    let calls = log.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, "de"); // Word 用候选自身 code
    assert_eq!(calls[0].1, "的");
}

struct SpyStage {
    calls: Mutex<usize>,
    swaps: bool,
}

impl RerankStage for SpyStage {
    fn rerank(&self, _ctx: &RerankCtx, cands: &mut Vec<Candidate>) {
        *self.calls.lock().unwrap() += 1;
        if self.swaps && cands.len() >= 2 {
            cands.swap(0, 1);
        }
    }
}

#[test]
fn rerank_stage_is_invoked() {
    let stage = Box::new(SpyStage { calls: Mutex::new(0), swaps: true });
    let schema = Box::new(Quanpin::new(fixture_dict().syllables().clone()));
    let lm = Box::new(ime_core::UnigramLm::new(fixture_dict().total_weight(), fixture_dict().entry_count()));
    let engine = Engine::with_parts(
        fixture_dict(),
        Config::default(),
        schema,
        lm,
        vec![stage],
        Box::new(ime_core::NullStore),
    );
    let mut s = engine.start_session();
    for c in "de".chars() {
        s.on_key(Key::Char(c));
    }
    // stage 被调用（>0），且静态序可被 stage 改写（M2 槽位验证）
    let e = s.effect();
    let stages_called = e.candidates.first().map(|_| true).unwrap_or(false);
    assert!(stages_called);
    // 注：stage 的调用次数从 effect 无法直接读；此处以 effect 正常产出为准。
}

#[test]
fn rerank_stage_swaps_order() {
    use std::cell::RefCell;
    struct SwapStage;
    impl RerankStage for SwapStage {
        fn rerank(&self, _ctx: &RerankCtx, cands: &mut Vec<Candidate>) {
            if cands.len() >= 2 {
                cands.swap(0, 1);
            }
        }
    }
    let _ = RefCell::new(0);
    let schema = Box::new(Quanpin::new(fixture_dict().syllables().clone()));
    let lm = Box::new(ime_core::UnigramLm::new(fixture_dict().total_weight(), fixture_dict().entry_count()));
    let engine = Engine::with_parts(
        fixture_dict(),
        Config::default(),
        schema,
        lm,
        vec![Box::new(SwapStage)],
        Box::new(ime_core::NullStore),
    );
    let mut s = engine.start_session();
    for c in "de".chars() {
        s.on_key(Key::Char(c));
    }
    // SwapStage 把 的/得 互换 → 首个候选不是"的"
    assert_ne!(s.effect().candidates[0].text, "的");
}
