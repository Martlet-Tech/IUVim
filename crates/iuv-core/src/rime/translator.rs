//! 翻译器（librime gear/script_translator.cc 的字符串键改编，39-rime-pipeline.md §Step2）。
//!
//! 派生自 librime（BSD-3-Clause）。核心流程对齐（行号为本仓 checkout）：
//! 1. 音节图（见 syllabifier.rs）；
//! 2. **桶收集**：从每个可达顶点出发沿图枚举音节路径查词，按「(起点,词条编码消耗
//!    终点)」分组——对应 Dictionary::Lookup 的 DictEntryCollector（dictionary.cc:271-297）
//!    与 PrepareForMakingSentence 的逐位置查询（st.cc:698-716）。其中**段首起点(0,*)
//!    的桶喂词候选流，全部桶喂 Poet 词格**；
//! 3. **整句闸门**：仅当「无覆盖全段的可靠精确词」且存在跨词组合可能时造句
//!    （st.cc:482-507）；句候选排在一切词候选之前（st.cc:598-601）；
//! 4. **词候选流**：按消耗终点降序分桶输出（码长优先于一切，st.cc:582-589），
//!    桶内权重降序；前缀补全候选标 predictive；
//! 5. 文本去重（DistinctTranslation，translation.cc:191-207）。

use super::poet::{self, GraphEntry, WordGraph};
use super::syllabifier::{SyllableGraph, SpellingType};
use iuv_data::Dict;
use std::collections::BTreeMap;

/// 路径查询预算：每起点独立配额（防简拼组合爆炸饿死后续分支；
/// 总量另有硬顶）。与 classic MAX_EXPAND_QUERIES 同量级思路。
const QUERY_BUDGET_PER_ORIGIN: usize = 1024;
const QUERY_BUDGET_TOTAL: usize = 16_384;

/// 桶内条目。
#[derive(Clone, Debug)]
pub(crate) struct BucketEntry {
    pub entry: iuv_data::Entry,
    /// true = 编码精确命中；false = 尾前缀补全（predictive）
    pub exact: bool,
    /// 产生路径的音节段数（部分消费推进用）
    pub parts: usize,
    /// 路径质量类：0=纯 Normal；1=含 Abbreviation；2=含 Completion。
    /// 词流分级输出（类内终点降序）——纯全拼桶先于简拼/补全污染桶，
    /// 对齐 classic「exact 全量在前」的用户预期（2026-08-26 裁决，任务书附录）。
    pub class: u8,
}

/// 桶表：(起点字节位, 终点字节位) → 条目列表。
pub(crate) type Buckets = BTreeMap<(usize, usize), Vec<BucketEntry>>;

/// 从**每个可达顶点**出发枚举路径收集词条桶（librime 逐位置 Lookup 的等价）。
/// `blocked` 为 M2 屏蔽谓词，先行过滤。
pub(crate) fn collect_buckets(
    dict: &Dict,
    graph: &SyllableGraph,
    max_word_syllables: usize,
    blocked: impl Fn(&str, &str) -> bool,
) -> Buckets {
    let mut buckets: Buckets = BTreeMap::new();
    let mut total_budget = QUERY_BUDGET_TOTAL;
    let reachable: Vec<usize> = (0..graph.farthest)
        .filter(|&v| v == 0 || graph.edges.contains_key(&v))
        .collect();

    for &start in &reachable {
        if total_budget == 0 {
            break;
        }
        let mut parts: Vec<String> = Vec::new();
        let mut origin_budget = QUERY_BUDGET_PER_ORIGIN;
        walk(
            dict, graph, start, start, &mut parts, true, false, 0,
            max_word_syllables, &mut buckets, &mut origin_budget, &mut total_budget, &blocked,
        );
    }

    // 桶内排序：精确优先，其次权重降序、字序稳定（librime 组内三规则的化简）
    for slot in buckets.values_mut() {
        slot.sort_by(|a, b| {
            b.exact
                .cmp(&a.exact)
                .then(b.entry.weight.cmp(&a.entry.weight))
                .then(a.entry.word.cmp(&b.entry.word))
        });
    }
    buckets
}

#[allow(clippy::too_many_arguments)]
fn walk(
    dict: &Dict,
    graph: &SyllableGraph,
    origin: usize,
    v: usize,
    parts: &mut Vec<String>,
    all_letters: bool,
    tail_completion: bool,
    cur_class: u8,
    max_word_syllables: usize,
    buckets: &mut Buckets,
    budget: &mut usize,
    total_budget: &mut usize,
    blocked: &impl Fn(&str, &str) -> bool,
) {
    if *budget == 0 || *total_budget == 0 || parts.len() > max_word_syllables {
        return;
    }
    // 当前路径查询（键形三态，2026-08-26 裁决：白霜简拼键=压缩首字母串）：
    //   全字母路径 → exact(concat)（命中构建期简拼键 nhm/nhmsx）；
    //   含字母混合路径 → 不查（无此键形，噪音）；
    //   纯音节值路径 → exact(join ')；Completion 尾段 → prefix(join ')。
    if !parts.is_empty() && (all_letters || !parts.iter().any(|p| p.chars().count() == 1)) || tail_completion {
        *budget -= 1;
        *total_budget -= 1;
        let key = parts.join("'");
        let got: Vec<BucketEntry> = if tail_completion {
            dict.prefix(&key, 64)
                .into_iter()
                .map(|e| BucketEntry {
                    entry: e,
                    exact: false,
                    parts: parts.len(),
                    class: cur_class.max(2),
                })
                .collect()
        } else if all_letters {
            let squashed: String = parts.concat();
            dict.exact(&squashed)
                .into_iter()
                .map(|e| BucketEntry { entry: e, exact: true, parts: parts.len(), class: cur_class })
                .collect()
        } else {
            dict.exact(&key)
                .into_iter()
                .map(|e| BucketEntry { entry: e, exact: true, parts: parts.len(), class: cur_class })
                .collect()
        };
        let slot = buckets.entry((origin, v)).or_default();
        for mut be in got {
            if blocked(&be.entry.code, &be.entry.word) {
                continue;
            }
            let be_class = be.class;
            match slot.iter().position(|x| x.entry.word == be.entry.word) {
                Some(i) => {
                    // 同词多路径合并：精确优先、其后权重优先。
                    // 类别归并（非序数）：含补全路径(2)恒保留 2（置顶语义），
                    // 否则取更纯者（0 纯全拼 < 1 含简拼）。
                    let merged_class = if slot[i].class == 2 || be_class == 2 {
                        2
                    } else {
                        slot[i].class.min(be_class)
                    };
                    let replace = (!slot[i].exact && be.exact)
                        || (slot[i].exact == be.exact
                            && be.entry.weight > slot[i].entry.weight);
                    if replace {
                        slot[i] = be;
                    }
                    slot[i].class = merged_class;
                }
                None => slot.push(be),
            }
        }
    }
    if parts.len() >= max_word_syllables {
        return;
    }
    if let Some(ends) = graph.edges.get(&v) {
        // 简拼=兜底语义（2026-08-26 裁决）：顶点存在 Normal 出边时只走 Normal——
        // 消灭「你会(ni'hui)」类中途简拼垃圾路径，组合爆炸随之消失；
        // 全简拼输入（nhmsx）沿途无 Normal，自然全链放行。
        let has_normal = ends
            .values()
            .flat_map(|sps| sps.iter())
            .any(|sp| sp.spelling_type == SpellingType::Normal);
        for (&e, spellings) in ends {
            let mut ordered: Vec<&super::syllabifier::Spelling> =
                spellings.iter().collect();
            if !has_normal {
                // 无 Normal：字母串拼写优先于音节展开（简拼键先查）
                ordered.sort_by_key(|sp| sp.syllable.chars().count());
            } else {
                // 有 Normal：只走 Normal + Completion（补全边是尾部唯一消费途径）
                ordered.retain(|sp| {
                    sp.spelling_type == SpellingType::Normal
                        || sp.spelling_type == SpellingType::Completion
                });
            }
            for sp in ordered {
                let completion = sp.spelling_type == SpellingType::Completion;
                let letter = sp.syllable.chars().count() == 1
                    && sp.spelling_type == SpellingType::Abbreviation;
                let abbrev = sp.spelling_type == SpellingType::Abbreviation;
                let next_class =
                    cur_class.max(if completion { 2 } else if abbrev { 1 } else { 0 });
                let next_letters = all_letters && letter;
                parts.push(sp.syllable.clone());
                walk(
                    dict, graph, origin, e, parts, next_letters, completion, next_class,
                    max_word_syllables, buckets, budget, total_budget, blocked,
                );
                parts.pop();
            }
        }
    }
}

/// 整句闸门 + Poet 词格构建（st.cc:482-507；EnrollEntries 默认 max_homophones=1：
/// 每 (起,终) 只取桶首最高频条目）。
pub(crate) fn build_poet_graph(
    buckets: &Buckets,
    total_length: usize,
    lm_log_prob: impl Fn(u32) -> f64,
) -> Option<WordGraph> {
    // 闸门：无覆盖全段的可靠精确词（has_reliable_exact_phrase，st.cc:445-449）
    let has_reliable = buckets
        .get(&(0, total_length))
        .map(|slot| slot.iter().any(|b| b.exact))
        .unwrap_or(false);
    if has_reliable {
        return None;
    }
    // 闸门：需存在「不从段首开始」的词条边（即 ≥2 词的组合空间；
    // 对应 librime syllable_graph.edges.size() >= 2 的意图）
    let multi_hop = buckets.keys().any(|&(s, _)| s > 0);
    if !multi_hop {
        return None;
    }
    let mut wg: WordGraph = BTreeMap::new();
    for (&(s, e), slot) in buckets.iter() {
        if e > total_length {
            continue;
        }
        if let Some(be) = slot.first() {
            wg.entry(s).or_default().entry(e).or_default().push(GraphEntry {
                word: be.entry.word.clone(),
                code: be.entry.code.clone(),
                log_weight: lm_log_prob(be.entry.weight),
            });
        }
    }
    if wg.is_empty() {
        return None;
    }
    Some(wg)
}

/// 由 Poet 结果构造整句候选（score = 路径总权重；seg_len 置大数 = 全消费）。
pub(crate) fn sentence_candidate(s: &poet::Sentence, display_code: String) -> crate::Candidate {
    let mut c = crate::Candidate::new(
        s.words.concat(),
        crate::CandidateKind::Sentence,
        display_code,
        0,
        999,
    );
    c.score = s.weight;
    c
}

#[cfg(test)]
pub(crate) fn dbg_walk_trace() {}
