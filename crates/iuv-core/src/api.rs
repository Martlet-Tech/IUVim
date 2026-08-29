//! 顶层引擎接口（39-rime-pipeline.md §4）：唯一签名面。
//!
//! 本质两份输入输出：
//! ① 待输入串 → 分段视图 + 候选列表（[`ImeEngine::translate`]）；
//! ② 高亮候选 → 预编辑显示串（[`ImeEngine::preedit`]，如输入 `jian` 导航到
//!    「吉安」时返回 `ji'an`）。
//!
//! classic 与 rime 两个核心都实现此 trait；会话层只认它，不感知核心差异。
//! `EngineCtx::preceding_text` 为 Step 3 预埋钩子：classic 忽略，rime 核心喂给
//! 组句打分（poet 的 preceding_text 机制）。

/// 一次 translate/preedit 的上下文。
pub struct EngineCtx<'a> {
    /// 已确认前文（悬空选词拼接的汉字）。classic 忽略；rime 组句上下文用。
    pub preceding_text: &'a str,
}

/// 待输入串：用户敲的原始字母串（可能含用户强制撇号 `'`）。
pub struct PendingInput<'a> {
    pub raw: &'a str,
}

/// 分段视图的一段：音节序列。
///
/// `syllables` 保留空段（尾撇号 display 语义，与既有 seg 一致）。
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub syllables: Vec<String>,
}

/// translate 输出：分段视图 + 活动段候选列表。
///
/// classic 阶段 `segmentation` 恒为整串一段（现有行为零变化）；rime 核心落地后
/// 才出现真正的多段视图（Step 3 会话层开始消费）。
#[derive(Clone, Debug, PartialEq)]
pub struct Translation {
    pub segmentation: Vec<Span>,
    pub candidates: Vec<Candidate>,
}

/// 顶层引擎接口。实现须线程安全（进程级单例跨线程共享）。
pub trait ImeEngine: Send + Sync {
    /// 输入方向①：待输入串 → 分段视图 + 候选列表。
    fn translate(&self, ctx: &EngineCtx, pending: &PendingInput) -> Translation;

    /// 输入方向②：当前高亮候选 → 该候选视角下的预编辑显示串（只含未消费尾巴，
    /// 已确认前文由会话层拼接）。`selected = None` 时返回默认切分显示。
    fn preedit(
        &self,
        ctx: &EngineCtx,
        pending: &PendingInput,
        selected: Option<&Candidate>,
    ) -> String;
}

use crate::{Candidate, CandidateKind};

/// 预编辑显示五规则（classic/rime 两核心共用；判定顺序即契约顺序）：
/// 1. 用户强制撇号（raw 含 `'`）：恒输入切分，不跟随候选；
/// 2. 原文兜底（候选 text == 输入去撇号）：原样 plain 不分节；
/// 3. 消费段不完整（简拼 jisb/nh、前缀档）：输入切分；
/// 4. 消费段完整 且 候选 code（去撇号）== 输入：跟随候选切分（jian+吉安 → ji'an）；
/// 5. 其余：输入切分。
pub(crate) fn preview_rules(
    raw: &str,
    seg: &[String],
    is_syllable: &dyn Fn(&str) -> bool,
    display: &dyn Fn(&[String]) -> String,
    selected: Option<&Candidate>,
) -> String {
    let Some(c) = selected else {
        return display(seg);
    };
    if raw.contains('\'') {
        return display(seg);
    }
    let plain = crate::strip_apostrophes(raw);
    if c.text == plain {
        return plain;
    }
    let consumed = c.seg_len.max(1).min(seg.len());
    let consumed_full = seg[..consumed].iter().all(|s| !s.is_empty() && is_syllable(s));
    if !consumed_full {
        return display(seg);
    }
    let code_plain = crate::strip_apostrophes(&c.code);
    if code_plain == plain {
        let mut s = c.code.clone();
        if consumed < seg.len() {
            s.push('\'');
            s.push_str(&display(&seg[consumed..]));
        }
        return s;
    }
    display(seg)
}

/// 单字桶查询的共享实现（classic `single_segment_candidates` 与 rime
/// `prefix_chars_translation` 共用，2026-08-26 去重）：完整音节 → exact_single 全量；
/// 严格前缀 → 首字母桶过滤 starts_with。
pub(crate) fn single_char_entries(dict: &iuv_data::Dict, s: &str) -> Vec<iuv_data::Entry> {
    if s.is_empty() {
        return Vec::new();
    }
    if dict.is_syllable(s) {
        dict.exact_single(s)
    } else {
        let first = s.chars().next().unwrap();
        dict.initial_top(first, iuv_data::INITIAL_BUCKET_SIZE)
            .into_iter()
            .filter(|e| e.code.starts_with(s))
            .collect()
    }
}

/// 原文兜底候选（"不认识"语义，classic 尾部与 rime fallback 共用）：
/// 多字符 → Word，单字符 → Char；text == code == plain。
pub(crate) fn raw_fallback_candidate(plain: &str, seg_len: usize) -> Candidate {
    let kind = if plain.chars().count() >= 2 {
        CandidateKind::Word
    } else {
        CandidateKind::Char
    };
    Candidate::new(plain, kind, plain, 0, seg_len)
}
