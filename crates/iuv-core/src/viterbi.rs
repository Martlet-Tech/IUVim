//! unigram Viterbi 最优路径。契约 01-contract.md §4.2。
//! 内部 API，不 re-export。

use crate::{Candidate, CandidateKind, Config, LmProvider};
use crate::lm::OOV_PENALTY;
use iuv_data::Dict;

/// 带路径分的版本（M2.5 消费端多方案整句排序用，2026-08-14）：
/// 返回 (候选, 路径总 log_prob)——分高 = 词条直接命中或高词频组合。
pub fn best_sentence_scored(
    dict: &Dict,
    seg: &[String],
    lm: &dyn LmProvider,
    config: &Config,
) -> Option<(Candidate, f64)> {
    let n = seg.len();
    if n < 2 {
        return None;
    }
    let max_w = config
        .max_word_syllables
        .min(dict.max_word_syllables())
        .max(1);

    // dp[i] = (log_prob, word, prev_idx, weight)——路径到位置 i 最优
    let mut dp: Vec<(f64, Option<String>, Option<usize>, u32)> =
        vec![(f64::NEG_INFINITY, None, None, 0); n + 1];
    dp[0] = (0.0, None, None, 0);

    for i in 0..n {
        if dp[i].0 == f64::NEG_INFINITY {
            continue;
        }
        for j in (i + 1)..=(n.min(i + max_w)) {
            // 词库键已分隔化（空格→'），边 key 以 ' 连接各音节（如 ["xi","an"] → "xi'an"）。
            let code = seg[i..j].join("'");
            let entries = dict.exact(&code);
            if entries.is_empty() {
                if j == i + 1 {
                    // 兜底边：单音节原样
                    let word = seg[i].clone();
                    let score = dp[i].0 + lm.log_prob(dp[i].1.as_deref(), &word, 0) + OOV_PENALTY;
                    if score > dp[j].0 {
                        dp[j] = (score, Some(word), Some(i), 0);
                    }
                }
                continue;
            }
            let prev_word: Option<String> = dp[i].1.clone();
            for e in entries {
                let score = dp[i].0 + lm.log_prob(prev_word.as_deref(), &e.word, e.weight);
                if score > dp[j].0 {
                    dp[j] = (score, Some(e.word.clone()), Some(i), e.weight);
                }
            }
        }
    }

    let mut path = Vec::new();
    let mut k = n;
    while k > 0 {
        let (_, word, prev, _w) = &dp[k];
        let word = word.clone()?;
        path.push(word);
        k = prev.clone()?;
    }
    path.reverse();
    let text = path.join("");
    // Sentence 权重恒 0（契约 candidate.rs）；seg_len = 组句段数（消费全部 vseg 段）。
    // 路径总 log_prob 作为返回值供消费端排序，不上浮到候选（排序未统一前候选不带分）。
    let cand = Candidate::new(text, CandidateKind::Sentence, seg.join("'"), 0, seg.len());
    Some((cand, dp[n].0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        schema::{InputSchema, Quanpin},
        UnigramLm,
    };
    use iuv_data::Dict;

    fn dict() -> Dict {
        Dict::from_entries(vec![
            ("ni".into(), "你".into(), 50000),
            ("hao".into(), "好".into(), 40000),
            ("ni'hao".into(), "你好".into(), 8000),
            ("shi'jie".into(), "世界".into(), 6000),
            ("shi".into(), "世".into(), 3000),
            ("jie".into(), "界".into(), 2500),
            ("de".into(), "的".into(), 100000),
        ])
    }

    fn seg(raw: &str) -> Vec<String> {
        let d = dict();
        Quanpin::new(d.syllables().clone()).segment(raw)[0].clone()
    }

    #[test]
    fn sentence_prefers_high_freq_path() {
        let d = dict();
        let lm = UnigramLm::new(d.total_weight());
        // "shijie": 世界(6000) vs 世(3000)+界(2500)。世界更高频。
        let s = seg("shijie");
        let c = best_sentence_scored(&d, &s, &lm, &Config::default()).unwrap().0;
        assert_eq!(c.text, "世界");
        // "nihao": 你好(8000) vs 你(50000)+好(40000)。单独字频率更高，但整句路径得分：
        // ln(8001)-ln(total) vs ln(50001)-ln(total)+ln(40001)-ln(total)
        // 单字路径=2ln(W)-2ln(T)，整词=ln(8001)-ln(T)。W 很大时单字更优。
        let s2 = seg("nihao");
        let c2 = best_sentence_scored(&d, &s2, &lm, &Config::default()).unwrap().0;
        assert_eq!(c2.text, "你好");
    }

    #[test]
    fn oov_syllable_falls_back() {
        let d = dict();
        let lm = UnigramLm::new(d.total_weight());
        let s = seg("wode");
        let c = best_sentence_scored(&d, &s, &lm, &Config::default()).unwrap().0;
        // "wo" 无词条 → 兜底原样 "wo"；"de" → "的"
        assert!(c.text.contains("wo"));
        assert!(c.text.contains('的'));
    }

    #[test]
    fn single_syllable_no_sentence() {
        let d = dict();
        let lm = UnigramLm::new(d.total_weight());
        let s = seg("de");
        assert!(best_sentence_scored(&d, &s, &lm, &Config::default()).is_none());
    }
}
