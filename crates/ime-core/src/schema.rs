//! 全拼切分。契约 01-contract.md §4 schema.rs。

use std::collections::BTreeSet;

/// 原始字母串 → 音节序列。全拼：' 为强制分隔；其余贪心最长合法音节。
/// 非法前缀按单字母原样保留，保证永不失败。
pub trait InputSchema: Send + Sync {
    fn segment(&self, raw: &str) -> Vec<String>;
    /// 音节序列 → 显示串：以 ' 连接
    fn display(&self, seg: &[String]) -> String;
}

/// 全拼切分器：合法音节集由 Dict::syllables() 构造。
pub struct Quanpin {
    syllables: BTreeSet<String>,
    max_len: usize,
}

impl Quanpin {
    pub fn new(syllables: BTreeSet<String>) -> Self {
        let max_len = syllables.iter().map(|s| s.len()).max().unwrap_or(6).min(6);
        Quanpin { syllables, max_len }
    }
}

impl InputSchema for Quanpin {
    fn segment(&self, raw: &str) -> Vec<String> {
        let b = raw.as_bytes();
        let mut out = Vec::new();
        let mut i = 0;
        while i < b.len() {
            if b[i] == b'\'' {
                i += 1; // 强制分隔
                continue;
            }
            let rem = b.len() - i;
            let mut matched = false;
            for len in (1..=rem.min(self.max_len)).rev() {
                if self.syllables.contains(&raw[i..i + len]) {
                    out.push(raw[i..i + len].to_string());
                    i += len;
                    matched = true;
                    break;
                }
            }
            if !matched {
                // 非法前缀按单字母保底，永不失败
                out.push(raw[i..i + 1].to_string());
                i += 1;
            }
        }
        out
    }

    fn display(&self, seg: &[String]) -> String {
        seg.join("'")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ime_data::Dict;

    fn quanpin() -> Quanpin {
        let d = Dict::from_entries(vec![
            ("nihao".into(), "你好".into(), 8000),
            ("xian".into(), "先".into(), 500),
            ("xi".into(), "西".into(), 100),
            ("an".into(), "安".into(), 100),
        ]);
        Quanpin::new(d.syllables().clone())
    }

    #[test]
    fn seg_basic() {
        let q = quanpin();
        assert_eq!(q.segment("nihao"), vec!["ni", "hao"]);
    }

    #[test]
    fn seg_apostrophe() {
        let q = quanpin();
        assert_eq!(q.segment("xi'an"), vec!["xi", "an"]);
        assert_eq!(q.segment("xian"), vec!["xian"]);
    }

    #[test]
    fn seg_invalid_char_fallback() {
        let q = quanpin();
        // 非音节前缀按单字母保留，不 panic
        let seg = q.segment("qaz");
        assert_eq!(seg, vec!["q", "a", "z"]);
        // 非法起始不 panic
        let seg = q.segment("xn");
        assert_eq!(seg, vec!["x", "n"]);
    }

    #[test]
    fn display_joins_with_apostrophe() {
        let q = quanpin();
        assert_eq!(q.display(&["ni".into(), "hao".into()]), "ni'hao");
        assert_eq!(q.display(&[]), "");
    }
}
