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
}
