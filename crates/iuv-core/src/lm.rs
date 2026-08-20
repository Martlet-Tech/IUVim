//! 语言模型。契约 01-contract.md §4 lm.rs。

/// prev = 前一个词（整句上下文）。MVP unigram 实现忽略 prev —— n-gram 槽位，签名不得改。
pub trait LmProvider: Send + Sync {
    fn log_prob(&self, prev: Option<&str>, word: &str, weight: u32) -> f64;
}

/// OOV（词典查不到）惩罚，由 viterbi 层加到兜底边。
pub(crate) const OOV_PENALTY: f64 = -10.0;

/// unigram 模型：ln(weight+1) - ln(total_weight)。
pub struct UnigramLm {
    total: u64,
    _entry_count: usize,
}

impl UnigramLm {
    pub fn new(total_weight: u64, entry_count: usize) -> Self {
        UnigramLm { total: total_weight, _entry_count: entry_count }
    }
}

impl LmProvider for UnigramLm {
    fn log_prob(&self, _prev: Option<&str>, _word: &str, weight: u32) -> f64 {
        if self.total == 0 {
            return -30.0;
        }
        ((weight as f64 + 1.0).ln()) - (self.total as f64).ln()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formula_and_zero_total() {
        let lm = UnigramLm::new(1000, 100);
        let p = lm.log_prob(None, "x", 99);
        assert!((p - ((100f64).ln() - (1000f64).ln())).abs() < 1e-9);
        let lm0 = UnigramLm::new(0, 0);
        assert_eq!(lm0.log_prob(None, "x", 5), -30.0);
    }
}
