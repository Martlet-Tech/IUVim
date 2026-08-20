//! 候选类型。W0 完整实现，冻结。

/// 候选种类。M3+ 可扩：English / Symbol…
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CandidateKind {
    Sentence,
    Word,
    Char,
}

/// 一个候选。
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Candidate {
    pub text: String,
    pub kind: CandidateKind,
    /// squashed 编码（学习 key 用）；Sentence 为 seg 拼接
    pub code: String,
    /// 词典 weight；Sentence 恒 0
    pub weight: u32,
    /// 该候选消费的音节段数（所在前缀级 k；续接选词时推进用，M1 后期契约演进）
    pub seg_len: usize,
}

impl CandidateKind {
    /// 按词长定种类：≥2 字 → Word，否则 Char（整句/原文兜底候选由调用方显式传）。
    pub(crate) fn for_word(text: &str) -> CandidateKind {
        if text.chars().count() >= 2 {
            CandidateKind::Word
        } else {
            CandidateKind::Char
        }
    }
}

impl Candidate {
    /// 构造一个候选。
    pub fn new(
        text: impl Into<String>,
        kind: CandidateKind,
        code: impl Into<String>,
        weight: u32,
        seg_len: usize,
    ) -> Self {
        Candidate { text: text.into(), kind, code: code.into(), weight, seg_len }
    }

    /// 由词库词条构造候选（P1.6 抽取：引擎 5 处词条 → 候选样板收敛）。
    /// `kind` 建议用 `CandidateKind::for_word(&e.word)`；克隆 word/code（词条可能复用）。
    pub fn for_entry(e: &iuv_data::Entry, kind: CandidateKind, seg_len: usize) -> Self {
        Candidate {
            text: e.word.clone(),
            kind,
            code: e.code.clone(),
            weight: e.weight,
            seg_len,
        }
    }
}
