//! 全拼切分。契约 01-contract.md §4 schema.rs。
//!
//! segment 输出二维数组：第一维 = 所有可能的切分方案，第二维 = 各方案的音节序列。
//! `'` 为用户强制分隔（硬边界，空段保留以便 display 显示尾/连续 `'`）；
//! 每段内部递归枚举全部合法音节切分（有合法音节前缀时不兜底单字母，无则兜底保证有解）。
//! 方案按贪心优先排序（方案[0] = 贪心/强制切分，供 viterbi 整句与 display 使用）。

use std::collections::BTreeSet;

/// 切分方案总数上限：超限只保留贪心方案（退化为单一切分，防长输入组合爆炸）。
const MAX_PLANS: usize = 128;

/// 原始字母串 → 全部可能切分方案（每方案 = 音节序列）。
/// 全拼：`'` 为强制分隔（硬边界，空段保留）；其余段内枚举合法音节切分。
/// 非法前缀按单字母原样保留，保证永不失败（无合法音节时兜底）。
pub trait InputSchema: Send + Sync {
    fn segment(&self, raw: &str) -> Vec<Vec<String>>;
    /// 单个方案 → 显示串：以 ' 连接
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

    /// 段内枚举：把 `s` 切成合法音节序列的全部方案（含单字母兜底，保证完整消费）。
    /// 顺序 = 贪心优先（递归时最长音节先试，方案[0] 即贪心切分）。
    fn enumerate_inner(&self, s: &str) -> Vec<Vec<String>> {
        let mut out = Vec::new();
        let mut cur = Vec::new();
        self.backtrack(s, 0, &mut cur, &mut out);
        out
    }

    fn backtrack(
        &self,
        s: &str,
        pos: usize,
        cur: &mut Vec<String>,
        out: &mut Vec<Vec<String>>,
    ) {
        if pos == s.len() {
            out.push(cur.clone());
            return;
        }
        let rem = s.len() - pos;
        let mut matched = false;
        for len in (1..=rem.min(self.max_len)).rev() {
            if self.syllables.contains(&s[pos..pos + len]) {
                matched = true;
                cur.push(s[pos..pos + len].to_string());
                self.backtrack(s, pos + len, cur, out);
                cur.pop();
            }
        }
        if !matched {
            // 无合法音节前缀 → 单字母兜底（保证有解，永不失败）
            cur.push(s[pos..pos + 1].to_string());
            self.backtrack(s, pos + 1, cur, out);
            cur.pop();
        }
    }
}

impl InputSchema for Quanpin {
    fn segment(&self, raw: &str) -> Vec<Vec<String>> {
        // 1. `'` 硬切分（空段保留：尾/连续 `'` 需在 display 中显示）
        let groups: Vec<&str> = raw.split('\'').collect();
        // 2. 各段枚举笛卡尔积 → 全部方案（方案[0] = 逐段贪心 = 贪心/强制切分）
        let mut plans: Vec<Vec<String>> = vec![Vec::new()];
        for g in groups {
            let inner = if g.is_empty() {
                vec![vec![String::new()]]
            } else {
                self.enumerate_inner(g)
            };
            let mut next = Vec::with_capacity(plans.len() * inner.len());
            for plan in &plans {
                for seg in &inner {
                    let mut p = plan.clone();
                    p.extend(seg.clone());
                    next.push(p);
                }
            }
            plans = next;
            if plans.len() > MAX_PLANS {
                // 超限：只保留贪心方案（plans[0]），退化为单一切分。
                plans.truncate(1);
            }
        }
        plans
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
            ("ni'hao".into(), "你好".into(), 8000),
            ("xian".into(), "先".into(), 500),
            ("xi".into(), "西".into(), 100),
            ("an".into(), "安".into(), 100),
        ]);
        Quanpin::new(d.syllables().clone())
    }

    #[test]
    fn seg_basic() {
        let q = quanpin();
        assert_eq!(q.segment("nihao"), vec![vec!["ni", "hao"]]);
    }

    #[test]
    fn seg_apostrophe_forced_single_plan() {
        let q = quanpin();
        // `'` 硬边界：只有一种方案。
        assert_eq!(q.segment("xi'an"), vec![vec!["xi", "an"]]);
    }

    #[test]
    fn seg_enumerates_xian() {
        let q = quanpin();
        // 无撇号：枚举 [xian]（贪心，方案[0]）与 [xi,an]。
        assert_eq!(q.segment("xian"), vec![vec!["xian"], vec!["xi", "an"]]);
    }

    #[test]
    fn seg_invalid_char_fallback() {
        let q = quanpin();
        // 无合法音节前缀按单字母保留，不 panic。
        assert_eq!(q.segment("qaz"), vec![vec!["q", "a", "z"]]);
        // 非法起始不 panic。
        assert_eq!(q.segment("xn"), vec![vec!["x", "n"]]);
    }

    #[test]
    fn seg_keeps_empty_groups_for_display() {
        let q = quanpin();
        // 尾/连续 `'`：空段保留，display 时 join 出来。
        assert_eq!(q.segment("x'"), vec![vec!["x", ""]]);
        assert_eq!(q.segment("x''y"), vec![vec!["x", "", "y"]]);
        assert_eq!(q.display(&q.segment("x'")[0]), "x'");
        assert_eq!(q.display(&q.segment("x''y")[0]), "x''y");
    }

    #[test]
    fn display_joins_with_apostrophe() {
        let q = quanpin();
        assert_eq!(q.display(&["ni".into(), "hao".into()]), "ni'hao");
        assert_eq!(q.display(&[]), "");
    }
}
