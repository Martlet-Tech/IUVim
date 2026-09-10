//! 全拼切分。契约 01-contract.md §4 schema.rs。
//!
//! 46 号 §3.1 重写：**切分决策 = 词库整跨词反查 + 贪心兜底**（api::best_seg 合成），
//! 本模块只产出贪心切分（O(n·L)，无枚举）。原「全枚举全部方案 + rank_plans 词频
//! 重排」模型已删除——其最优方案选取语义由反查段在词库侧闭式承载。
//! `'` 为用户强制分隔（硬边界，空段保留以便 display 显示尾/连续 `'`）。
//! 段内规则（2026-09-10 修订，见 [`Quanpin::greedy_group`]）：**优先「完整音节链 +
//! 可选尾段」的切法**（正是组句闸门认可的形态），做不到再退回纯最长音节匹配 +
//! 音节前缀/单字母兜底（保证永不失败）。

use std::collections::BTreeSet;

/// üe 去点输入形 → 词库规范形（v=ü，GB《通用键盘表示规范》）。
/// lue→lve、nue→nve：唯一归一单点，seg/plans/viterbi 键/dict 查询
/// 全部消费规范形（24-ue-input-alias.md）。
fn canonical(syl: &str) -> &str {
    match syl {
        "lue" => "lve",
        "nue" => "nve",
        _ => syl,
    }
}

/// 输入串归一（**长度不变**，46 号 §3.1 反查键与切分共用的唯一口径）：
/// 仅 üe 去点输入形 lue→lve / nue→nve；**不做大小写折叠**——大写保形要求
/// `niHAO` 原样进序列（大写不被音节表命中，切/查都自然落空 → 贪心兜底），
/// 若在此小写化会让预编辑显示从 `ni'H'A'O` 退化为 `ni'hao`。
pub fn normalize_input(raw: &str) -> String {
    if !raw.contains("lue") && !raw.contains("nue") {
        return raw.to_string();
    }
    raw.replace("lue", "lve").replace("nue", "nve")
}

/// 原始字母串 → 切分（音节序列）。
/// 全拼：`'` 为强制分隔（硬边界，空段保留）；段内优先「完整音节链 + 可选尾段」，
/// 无此切法时退回最长音节优先 + 音节前缀/单字母兜底，保证永不失败。
pub trait InputSchema: Send + Sync {
    fn segment(&self, raw: &str) -> Vec<String>;
    /// 切分 → 显示串：以 ' 连接（空段保留：`["x",""]` → `"x'"`）
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

    /// 段内切分：先试「完整音节链（+ 尾段）」形态，失败退回纯最长匹配。
    ///
    /// 2026-09-10（46 号后续，真机 + REPL 复现）：纯最长匹配会撞进 `den`（扽 dèn）这类
    /// **合法但极生僻**的音节，把本该属于后一个音节的字母吃掉——`zhendeniubi` →
    /// `zhen|den|i|u|bi`，段中冒出非音节 `i`/`u`；而组句闸门（`rime/mod.rs`
    /// `rest_all_syllables`）要求「除末段外全是完整音节」，于是**长句候选整条被关掉**，
    /// 只剩首段词（实测 `zhecixiugaishizhendeniubi` 坍缩成 这次/这词/这/着/者）。
    /// 带回溯选出 `zhen|de|niu|bi` 即让闸门保持打开；不存在这样的切法（简拼、大写保形、
    /// `sh`/`zho` 这类前缀串）时退回原纯最长匹配，行为与改造前逐字节一致。
    fn greedy_group(&self, s: &str) -> Vec<String> {
        self.syllable_chain(s)
            .unwrap_or_else(|| self.longest_match_group(s))
    }

    /// 纯最长匹配（2026-09-10 前的原实现，现为兜底路径）：每位置取最长合法音节；
    /// 无任何音节匹配时取最长音节前缀兜底（再无则单字母），保证永不失败。
    fn longest_match_group(&self, s: &str) -> Vec<String> {
        let b = s.as_bytes();
        let mut out = Vec::new();
        let mut pos = 0usize;
        while pos < b.len() {
            let rem = b.len() - pos;
            let upper = rem.min(self.max_len);
            let mut matched = false;
            for len in (1..=upper).rev() {
                if self.syllables.contains(&s[pos..pos + len]) {
                    matched = true;
                    out.push(canonical(&s[pos..pos + len]).to_string());
                    pos += len;
                    break;
                }
            }
            if !matched {
                // 微软对齐：`sh` 是 sha/shan/shi… 的前缀，整体为一段而非 s'h 两段；
                // `zho`/`zhon` 同理。无任何前缀才单字母兜底，保证有解永不失败。
                let mut plen = 1usize;
                for len in (1..=upper).rev() {
                    if self.syllables.iter().any(|syl| syl.starts_with(&s[pos..pos + len])) {
                        plen = len;
                        break;
                    }
                }
                out.push(s[pos..pos + plen].to_string());
                pos += plen;
            }
        }
        out
    }

    /// 「完整音节链 + 可选尾段」切分：除末段外**全部是完整音节**，末段允许是未闭合
    /// 音节（某音节的真前缀）或单字母——正是组句闸门认可的形态。
    /// 最长优先 DFS + 失败位置记忆化，复杂度 O(n·L)（与纯最长匹配同阶，无回溯爆炸）。
    /// 不存在这样的切法返回 None（调用方退回纯最长匹配）。
    fn syllable_chain(&self, s: &str) -> Option<Vec<String>> {
        let b = s.as_bytes();
        let mut failed = vec![false; b.len() + 1];
        let mut picked: Vec<&str> = Vec::new();
        if self.chain_dfs(s, b, 0, &mut failed, &mut picked) {
            Some(picked.iter().map(|p| canonical(p).to_string()).collect())
        } else {
            None
        }
    }

    /// 链式 DFS：`pos` 处优先吃**最长**完整音节；走不通时把「剩余全部」当尾段
    /// （要求已有至少一个音节段，且剩余是合法尾段）。`failed` 记失败位置以剪枝。
    fn chain_dfs<'a>(
        &self,
        s: &'a str,
        b: &[u8],
        pos: usize,
        failed: &mut [bool],
        picked: &mut Vec<&'a str>,
    ) -> bool {
        if pos == b.len() {
            return !picked.is_empty();
        }
        if failed[pos] {
            return false;
        }
        let upper = (b.len() - pos).min(self.max_len);
        for len in (1..=upper).rev() {
            let piece = &s[pos..pos + len];
            if self.syllables.contains(piece) {
                picked.push(piece);
                if self.chain_dfs(s, b, pos + len, failed, picked) {
                    return true;
                }
                picked.pop();
            }
        }
        // 尾段：打字过程中末段常常正是未闭合音节（`zh`/`yo`）或单字母（大写保形），
        // 不能因为它不是完整音节就否定整条链——闸门同样只要求「除末段外」是音节。
        if !picked.is_empty() && self.is_valid_tail(&s[pos..]) {
            picked.push(&s[pos..]);
            return true;
        }
        failed[pos] = true;
        false
    }

    /// 合法尾段：完整音节 / 某音节的真前缀 / 单字母（与兜底口径一致）。
    fn is_valid_tail(&self, part: &str) -> bool {
        part.len() == 1
            || self.syllables.contains(part)
            || self.syllables.iter().any(|syl| syl.starts_with(part))
    }
}

impl InputSchema for Quanpin {
    fn segment(&self, raw: &str) -> Vec<String> {
        // `'` 硬切分（空段保留：尾/连续 `'` 需在 display 中显示），逐段贪心
        let mut out = Vec::new();
        for g in raw.split('\'') {
            if g.is_empty() {
                out.push(String::new());
            } else {
                out.extend(self.greedy_group(g));
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
    use iuv_data::Dict;

    /// 测试音节集：覆盖基本切分、üe 归一、词频重排敏感串（fenge/dier/keneng）
    /// 与兜底段（sh/zho/nh）所需的全部音节。
    fn quanpin() -> Quanpin {
        let d = Dict::from_entries(vec![
            ("ni'hao".into(), "你好".into(), 8000),
            ("xian".into(), "先".into(), 500),
            ("xi".into(), "西".into(), 100),
            ("an".into(), "安".into(), 100),
            ("shi".into(), "是".into(), 90000),
            ("zhong".into(), "中".into(), 50000),
            ("gong".into(), "攻".into(), 500),
            ("lve".into(), "略".into(), 400),
            ("nve".into(), "虐".into(), 300),
            ("fen'ge".into(), "分割".into(), 8000),
            ("feng".into(), "风".into(), 5000),
            ("e".into(), "额".into(), 3000),
            ("di'er".into(), "第二".into(), 6000),
            ("die".into(), "跌".into(), 1000),
            ("ken".into(), "啃".into(), 500),
            ("eng".into(), "嗯".into(), 400),
            ("pu".into(), "普".into(), 200),
        ]);
        Quanpin::new(d.syllables().clone())
    }

    #[test]
    fn seg_basic() {
        let q = quanpin();
        assert_eq!(q.segment("nihao"), vec!["ni", "hao"]);
    }

    #[test]
    fn seg_apostrophe_forced_split() {
        let q = quanpin();
        // `'` 硬边界：用户强制分隔恒保留（不再产生第二方案——方案枚举已删，
        // 「xi'an 更优」这类判断改由词库反查段在 api::best_seg 裁决）。
        assert_eq!(q.segment("xi'an"), vec!["xi", "an"]);
    }

    #[test]
    fn seg_longest_syllable_wins() {
        let q = quanpin();
        // 贪心 = 最长音节优先：xian 是合法音节 → 单段（西安靠反查段胜出）。
        assert_eq!(q.segment("xian"), vec!["xian"]);
    }

    #[test]
    fn seg_invalid_char_fallback() {
        let q = quanpin();
        // 无合法音节前缀时：最长音节前缀兜底（q 仅 "q" 自身是前缀 → 单字母）
        assert_eq!(q.segment("qaz"), vec!["q", "a", "z"]);
        // 非法起始不 panic。
        assert_eq!(q.segment("xn"), vec!["x", "n"]);
        assert_eq!(q.segment("input"), vec!["i", "n", "pu", "t"]);
    }

    #[test]
    fn seg_longest_prefix_fallback_single_segment() {
        let q = quanpin();
        // 微软对齐：`sh` 是 sha/shan/shi… 的前缀 → 整体一段，而非 s'h 两段。
        assert_eq!(q.segment("sh"), vec!["sh"]);
        // `zho`/`zhon` 是 zhong/zhou 的前缀 → 单段。
        assert_eq!(q.segment("zho"), vec!["zho"]);
        assert_eq!(q.segment("zhon"), vec!["zhon"]);
    }

    #[test]
    fn seg_abbrev_not_prefix_keeps_single_letters() {
        let q = quanpin();
        // `nh` 不是任何音节的前缀（无音节以 nh 开头）→ 仍拆为 n/h 两段（简拼档）。
        assert_eq!(q.segment("nh"), vec!["n", "h"]);
        // 前缀段（n）之后继续正常切分完整音节（hao）。
        assert_eq!(q.segment("nhao"), vec!["n", "hao"]);
    }

    #[test]
    fn seg_keeps_empty_groups_for_display() {
        let q = quanpin();
        // 尾/连续 `'`：空段保留，display 时 join 出来。
        assert_eq!(q.segment("x'"), vec!["x", ""]);
        assert_eq!(q.segment("x''y"), vec!["x", "", "y"]);
        assert_eq!(q.display(&q.segment("x'")), "x'");
        assert_eq!(q.display(&q.segment("x''y")), "x''y");
    }

    #[test]
    fn display_joins_with_apostrophe() {
        let q = quanpin();
        assert_eq!(q.display(&["ni".into(), "hao".into()]), "ni'hao");
        assert_eq!(q.display(&[]), "");
    }

    // 24-ue-input-alias.md：üe 去点输入形 lue/nue → 词库规范形 lve/nve（唯一归一单点）。
    #[test]
    fn seg_ue_alias_canonical() {
        let q = quanpin();
        assert_eq!(q.segment("lue"), vec!["lve"]);
        assert_eq!(q.segment("nue"), vec!["nve"]);
        assert_eq!(q.segment("gonglue"), vec!["gong", "lve"]);
    }

    #[test]
    fn seg_ue_regression() {
        let q = quanpin();
        // 规范形直通（不二次改写）。
        assert_eq!(q.segment("gonglve"), vec!["gong", "lve"]);
        assert_eq!(q.segment("lve"), vec!["lve"]);
        assert_eq!(q.segment("nve"), vec!["nve"]);
        // j/q/x/y 侧 jue/que/xue/yue 是正字法音节，保持原样（ve 形非法，不映射）。
        let d = Dict::from_entries(vec![
            ("jue".into(), "决".into(), 1000),
            ("que".into(), "却".into(), 1000),
            ("xue".into(), "学".into(), 1000),
            ("yue".into(), "月".into(), 1000),
        ]);
        let q2 = Quanpin::new(d.syllables().clone());
        assert_eq!(q2.segment("jue"), vec!["jue"]);
        assert_eq!(q2.segment("que"), vec!["que"]);
        assert_eq!(q2.segment("xue"), vec!["xue"]);
        assert_eq!(q2.segment("yue"), vec!["yue"]);
    }

    /// 46 号 §3.1：归一单点只做 üe 别名，**绝不做大小写折叠**（大写保形）。
    #[test]
    fn normalize_input_only_ue_alias() {
        assert_eq!(normalize_input("gonglue"), "gonglve");
        assert_eq!(normalize_input("nue"), "nve");
        // 规范形幂等（不二次改写）。
        assert_eq!(normalize_input("gonglve"), "gonglve");
        // 大小写原样（niHAO 的 H/A/O 必须保留，否则预编辑与上屏都失真）。
        assert_eq!(normalize_input("niHAO"), "niHAO");
        assert_eq!(normalize_input("LUE"), "LUE");
        // 撇号不动。
        assert_eq!(normalize_input("xi'an"), "xi'an");
        assert_eq!(normalize_input("lue'"), "lve'");
    }

    /// 46 号 §6.1 对拍钉子的替代（枚举已删，无法再与 `enumerate_inner` 对拍）：
    /// 固定语料冻结**黄金期望值**，任何切分口径漂移在此暴露。
    #[test]
    fn greedy_golden_corpus() {
        let q = quanpin();
        let cases: &[(&str, &[&str])] = &[
            ("nihao", &["ni", "hao"]),
            ("xian", &["xian"]),
            ("xi'an", &["xi", "an"]),
            // 词频重排敏感串的**贪心**形（旧 rank_plans 会改成 fen'ge/di'er/ke'neng）
            ("fenge", &["feng", "e"]),
            ("dier", &["die", "r"]),
            ("keneng", &["ken", "eng"]),
            // 兜底段
            ("qaz", &["q", "a", "z"]),
            ("xn", &["x", "n"]),
            ("sh", &["sh"]),
            ("zho", &["zho"]),
            ("zhon", &["zhon"]),
            ("nh", &["n", "h"]),
            ("nhao", &["n", "hao"]),
            ("input", &["i", "n", "pu", "t"]),
            // 大写保形（大写不被音节表命中 → 单字母兜底段）
            ("niHAO", &["ni", "H", "A", "O"]),
            // 撇号/空段
            ("x'", &["x", ""]),
            ("x''y", &["x", "", "y"]),
            // üe 归一
            ("lue", &["lve"]),
            ("nue", &["nve"]),
            ("gonglue", &["gong", "lve"]),
            ("gonglve", &["gong", "lve"]),
        ];
        for (raw, want) in cases {
            assert_eq!(q.segment(raw), want.to_vec(), "切分漂移: {raw:?}");
        }
    }

    /// 2026-09-10 回归钉子（真机 + REPL 双复现）：`den`（扽 dèn）是**合法**音节，
    /// 纯最长匹配会吃掉 `de` 的 n → `zhen|den|i|u|bi`，段中冒出非音节 `i`/`u` →
    /// 组句闸门（`rime/mod.rs` `rest_all_syllables`「除末段外全是完整音节」）关闭 →
    /// 长句候选整条消失、只剩首段词（用户实测 `zhecixiugaishizhendeniubi` 坍缩）。
    /// 带回溯必须选出 `zhen|de|niu|bi` 保住闸门。
    #[test]
    fn seg_backtracks_to_keep_syllable_chain() {
        let d = Dict::from_entries(vec![
            ("zhen".into(), "真".into(), 5000),
            ("de".into(), "的".into(), 90000),
            ("den".into(), "扽".into(), 28), // 生僻但合法：最长匹配陷阱
            ("niu".into(), "牛".into(), 4000),
            ("bi".into(), "比".into(), 3000),
        ]);
        let q = Quanpin::new(d.syllables().clone());
        assert_eq!(
            q.segment("zhendeniubi"),
            vec!["zhen", "de", "niu", "bi"],
            "应回溯保住「除末段外全是完整音节」的链"
        );
        // 对照：纯最长匹配（兜底路径）确实会踩陷阱——两实现并存，差异可见。
        assert_eq!(
            q.longest_match_group("zhendeniubi"),
            vec!["zhen", "den", "i", "u", "bi"],
            "纯最长匹配的陷阱行为（仅作对照，不进生产路径）"
        );

        // 无此链可走时行为逐字节不变（简拼/大写/前缀串仍走原兜底）。
        let q2 = quanpin();
        assert_eq!(q2.segment("sh"), vec!["sh"]);
        assert_eq!(q2.segment("nh"), vec!["n", "h"]);
        assert_eq!(q2.segment("nhao"), vec!["n", "hao"]);
        assert_eq!(q2.segment("niHAO"), vec!["ni", "H", "A", "O"]);
        assert_eq!(q2.segment("qaz"), vec!["q", "a", "z"]);
    }

    /// 结构不变量 + 幂等（对固定语料与 LCG 伪随机串；真随机串无期望值可比，
    /// 用可证明的性质替代——覆盖「重建性 / 段非空 / 兜底合法 / 幂等」四性质）。
    #[test]
    fn greedy_structural_invariants() {
        let q = quanpin();
        let check = |s: &str| {
            let seg = q.segment(s);
            // ① 重建性：各段拼接 == 输入去撇号（撇号是硬边界，不进入段内容）
            assert_eq!(
                seg.concat(),
                normalize_input(s).replace('\'', ""),
                "重建性被破坏: {s:?} -> {seg:?}"
            );
            for part in &seg {
                // ② 段非空（空段唯一来源：撇号切分 / 空输入 `segment("") == [""]`）
                assert!(
                    !part.is_empty() || s.contains('\'') || s.is_empty(),
                    "出现空段: {s:?} -> {seg:?}"
                );
                if part.is_empty() {
                    continue;
                }
                // ③ 兜底合法：非音节段必为「某音节的真前缀」，否则必为单字母
                assert!(
                    q.syllables.contains(part)
                        || part.len() == 1
                        || q.syllables.iter().any(|syl| syl.starts_with(part.as_str())),
                    "非法段: {s:?} -> {seg:?}"
                );
            }
            // ④ 幂等：对无撇号输入再切一次结果不变（段内容 = 规范形，重切稳定）
            if !s.contains('\'') {
                assert_eq!(q.segment(&seg.concat()), seg, "幂等被破坏: {s:?}");
            }
        };

        for s in [
            "nihao", "xian", "shigechengy", "nhao", "nhmsx", "qaz", "xn", "sh", "zho", "zhon",
            "lue", "nue", "gonglue", "gonglve", "jue", "chuangqianmingyueguang", "", "x'", "x''y",
            "beiguofengguangqianlibingfengwanlixuepiaowangchang",
        ] {
            check(s);
        }

        // LCG 伪随机（确定性、零依赖）：拼音字母表上的短串（含大量歧义/孤点），len 1..=24
        const ALPHABET: &[u8] = b"aeioubpmfdtnlgkhjqxzhcsrwy";
        let mut seed: u64 = 0x46_4c4f_57_45_52;
        let mut next = || {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (seed >> 33) as usize
        };
        for _ in 0..400 {
            let len = 1 + next() % 24;
            let s: String =
                (0..len).map(|_| ALPHABET[next() % ALPHABET.len()] as char).collect();
            check(&s);
        }
    }
}
