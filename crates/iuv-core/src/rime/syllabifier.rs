//! 音节图构建（librime algo/syllabifier.cc 的字符串键改编版，39-rime-pipeline.md §Step2）。
//!
//! 派生自 librime（BSD-3-Clause）。保留其「顶点=位置、边=(起,终)带拼写类型与可信度罚分」
//! 图模型；音节 ID trie 替换为 iuv-data 字符串键。MVP 三类拼写：
//! - **Normal** 完整音节，cred 0；
//! - **Abbreviation** 单字母简拼——一条边携带该字母开头的**全部合法音节**
//!   （librime 由拼写代数 achieve 同效：`nhao` 的 `n` 展开为所有 n* 音节，
//!   故 `n+hao` 可命中 `ni'hao`「你好」，混拼由此统一承载），cred = ln(0.05)；
//! - **Completion** 尾前缀补全——仅当图解释不到输入末尾时补一条 [farthest, len)
//!   边，内容 = 剩余串（非音节），查询侧走前缀查询展开（librime syllabifier.cc:207-248，
//!   cred += ln(0.05)，:26-29 权重阶梯）。
//! 模糊音/纠错（fuzzy/correction）留 M3。

use std::collections::{BTreeMap, BTreeSet};

/// 简拼/补全可信度罚分默认值（librime syllabifier.cc:28 硬编码 ln(0.05)）。
/// 简拼与补全同值，config `rime_spelling_penalty` 可调，此处仅是缺省。
pub(crate) const COMPLETION_PENALTY: f64 = -2.995_732_273_553_991;

/// 拼写类型（librime SpellingType 子集）。序 = 质量序（值小者优）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum SpellingType {
    Normal,
    Completion,
    Abbreviation,
}

/// 边上的一个拼写候选。
#[derive(Clone, Debug)]
pub(crate) struct Spelling {
    /// 音节规范形（Normal/Abbreviation）；Completion 时为剩余输入串（非音节）
    pub syllable: String,
    pub spelling_type: SpellingType,
    /// 可信度罚分（log 域，负值）
    pub credibility: f64,
}

/// 图的边集合：(起点 → 终点 → 拼写列表)。
pub(crate) type Edges = BTreeMap<usize, BTreeMap<usize, Vec<Spelling>>>;

/// 音节图：位置 0..=input.len()。
pub(crate) struct SyllableGraph {
    pub edges: Edges,
    /// 图能解释到的最远位置（≤ input.len()）
    pub farthest: usize,
}

/// 在 `input`（小写化字母串，可含 `'` 分隔符）上构建音节图。
/// `abbrev_penalty` / `completion_penalty`：简拼边与补全边的可信度罚分
/// （log 域负值；config `rime_spelling_penalty`，默认 ln(0.05)）。
///
/// 规则（自 librime 化简，依据见模块注释）：
/// 1. 位置升序扩展；每个到达位置先吞前导 `'`；
/// 2. 最长优先完整音节匹配（Normal）；
/// 3. 单字母 → Abbreviation 边（展开为该字母开头的全部音节）；
/// 4. 吞尾随 `'` 后落点为终点（边长含分隔符）；
/// 5. 最远点 < len 且剩余串是某音节前缀 → 补一条 Completion 直达边；
/// 6. 剪枝：仅保留能连通到 farthest 的边。
pub(crate) fn build_graph(
    input: &str,
    syllables: &BTreeSet<String>,
    max_syllable_len: usize,
    abbrev_penalty: f64,
    completion_penalty: f64,
) -> SyllableGraph {
    let bytes = input.as_bytes();
    let n = bytes.len();
    let mut edges: Edges = BTreeMap::new();
    let mut reached = vec![false; n + 1];
    reached[0] = true;

    for v in 0..n {
        if !reached[v] {
            continue;
        }
        let mut s = v;
        while s < n && bytes[s] == b'\'' {
            s += 1;
        }
        if s == n {
            continue;
        }
        // 完整音节匹配（最长优先）
        let upper = (n - s).min(max_syllable_len);
        for len in (1..=upper).rev() {
            let sub = &input[s..s + len];
            if !syllables.contains(sub) {
                continue;
            }
            let mut e = s + len;
            while e < n && bytes[e] == b'\'' {
                e += 1;
            }
            add_spelling(&mut edges, &mut reached, v, e, sub.to_string(), SpellingType::Normal, 0.0);
        }
        // 单字母简拼边：两族拼写——①该字母开头的全部音节（混拼展开，
        // n(i)+hao 命中 ni'hao）；②字母串自身（命中构建期简拼键，如
        // n'h'm's'x = 你还没睡醒）。同边共存，查询侧各自成键。
        let initial = &input[s..s + 1];
        let mut e = s + 1;
        while e < n && bytes[e] == b'\'' {
            e += 1;
        }
        for syl in syllables.iter().filter(|syl| syl.starts_with(initial)) {
            add_spelling(
                &mut edges,
                &mut reached,
                v,
                e,
                syl.clone(),
                SpellingType::Abbreviation,
                abbrev_penalty,
            );
        }
        // 族②字母串自身：仅小写字母（大写保形字符不产边——作为不可达分隔，
        // `Hello` 的 H 处无出边 → farthest=0 → 兜底原文，与 classic 语义一致）。
        if initial.as_bytes().first().is_some_and(|b| b.is_ascii_lowercase()) {
            add_spelling(
                &mut edges,
                &mut reached,
                v,
                e,
                initial.to_string(),
                SpellingType::Abbreviation,
                abbrev_penalty,
            );
        }
    }

    let farthest = (0..=n).rev().find(|&p| p == 0 || reached[p]).unwrap_or(0);

    // 图整体不可达尾部的兜底补全边（保留原语义：从最远点直达）
    if farthest < n {
        let tail = input[farthest..].trim_matches('\'');
        if !tail.is_empty() && syllables.iter().any(|syl| syl.starts_with(tail)) {
            add_spelling(
                &mut edges,
                &mut reached,
                farthest,
                n,
                tail.to_string(),
                SpellingType::Completion,
                completion_penalty,
            );
        }
    }

    prune(&mut edges, farthest);
    SyllableGraph { edges, farthest }
}

fn add_spelling(
    edges: &mut Edges,
    reached: &mut [bool],
    from: usize,
    to: usize,
    syllable: String,
    t: SpellingType,
    cred: f64,
) {
    reached[to] = true;
    let slot = edges.entry(from).or_default().entry(to).or_default();
    if let Some(existing) = slot.iter_mut().find(|sp| sp.syllable == syllable) {
        // 同 (起,终,拼写) 去重，保留更优类型（类型序即质量序）
        if t < existing.spelling_type {
            existing.spelling_type = t;
            existing.credibility = cred;
        }
    } else {
        slot.push(Spelling { syllable, spelling_type: t, credibility: cred });
    }
}

/// 2b 场景补全边（classic 对齐，2026-08-26 裁决）：切分尾段为**音节真前缀**
/// （`shigechengy` 的 `y`）时，从该段起点补一条直达输入末尾的 Completion 边，
/// 词条由查询侧前缀展开。简拼边会先把尾部「解释掉」而掩盖不可达性，
/// 故此边在图构建后由引擎显式注入（持有重排切分才能定位尾段字节起点）。
pub(crate) fn push_completion_edge(
    graph: &mut SyllableGraph,
    input_len: usize,
    tail_start: usize,
    tail: &str,
    completion_penalty: f64,
) {
    if tail.is_empty() || tail_start >= input_len {
        return;
    }
    let slot = graph.edges.entry(tail_start).or_default();
    slot.entry(input_len).or_default().push(Spelling {
        syllable: tail.to_string(),
        spelling_type: SpellingType::Completion,
        credibility: completion_penalty,
    });
    if input_len > graph.farthest {
        // 补全边直达 len：farthest 抬到 len（剪枝已跑过，此处只抬界）
        graph.farthest = input_len;
    }
}

/// 反向剪枝：只留能到达 `target` 的边（librime syllabifier.cc:156-205 化简版）。
fn prune(edges: &mut Edges, target: usize) {    let mut good: BTreeSet<usize> = BTreeSet::new();
    good.insert(target);
    let ends: Vec<(usize, usize)> = edges
        .iter()
        .flat_map(|(f, m)| m.keys().map(move |e| (*f, *e)))
        .collect();
    let mut changed = true;
    while changed {
        changed = false;
        for (f, e) in &ends {
            if good.contains(e) && !good.contains(f) {
                good.insert(*f);
                changed = true;
            }
        }
    }
    for (_, m) in edges.iter_mut() {
        m.retain(|e, _| good.contains(e));
    }
    edges.retain(|f, m| good.contains(f) && !m.is_empty());
}
