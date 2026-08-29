//! 翻译器（librime gear/script_translator.cc 的字符串键改编，39-rime-pipeline.md §Step2）。
//!
//! 派生自 librime（BSD-3-Clause）。核心流程对齐（行号为本仓 checkout）：
//! 1. 音节图（见 syllabifier.rs）；
//! 2. **词典游标引导 BFS 桶收集**（见 [`collect_buckets`]）——Table::Query 的
//!    字符串键同构（table.cc:571-634：BFS 携带词典游标、exhausted 即剪枝，
//!    只遍历词典中真实存在的路径）；段首起点(0,*)的桶喂词候选流，全部桶喂 Poet；
//! 3. **整句闸门**：仅当「无覆盖全段的可靠精确词」且存在跨词组合可能时造句
//!    （st.cc:482-507）；句候选排在一切词候选之前（st.cc:598-601）；
//! 4. **词候选流**：按消耗终点降序分桶输出（码长优先于一切，st.cc:582-589），
//!    桶内权重降序；前缀补全候选标 predictive；
//! 5. 文本去重（DistinctTranslation，translation.cc:191-207）。

use super::poet::{self, GraphEntry, WordGraph};
use super::syllabifier::{SyllableGraph, SpellingType};
use iuv_data::Dict;
use std::collections::BTreeMap;

/// 桶内条目。
#[derive(Clone, Debug)]
pub(crate) struct BucketEntry {
    pub entry: iuv_data::Entry,
    /// true = 编码精确命中；false = 尾前缀补全（predictive）
    pub exact: bool,
    /// 路径质量类：0=纯 Normal；1=含 Abbreviation；2=含 Completion。
    /// 词流分级输出——补全(全跨)置顶 → 纯全拼 → 含简拼沉底
    /// （2026-08-26 裁决，任务书 §13.4）。
    pub class: u8,
    /// 路径拼写可信度累计（log 域负值，Normal 0 / Abbreviation·Completion 各
    /// ln(0.05)，librime dictionary.cc:164 credibility 语义）。进 poet 词格与
    /// 候选 score；2026-08-29 λ 校准起真正消费（此前是死数据）。
    pub cred: f64,
}

/// 桶表：(起点字节位, 终点字节位) → 条目列表。
pub(crate) type Buckets = BTreeMap<(usize, usize), Vec<BucketEntry>>;

/// 从给定起点集做「**词典游标引导 BFS**」收集词条桶。
///
/// 学 librime Table::Query（table.cc:571-634）：BFS 携带词典游标走图，每个音节步
/// 用词典本身剪枝——扩展出的键若无任何词以其开头（`exact` 空且无更长前缀命中，
/// 即游标 exhausted），立即砍枝。遍历的只是「词典中真实存在的路径」，切分组合
/// 爆炸在结构上不可能发生；两族简拼键形（音节值 join' / 字母 concat）由键串构造
/// 自然统一，无需特判。`origins` = 音节边界起点集（调用方按重排切分给出——
/// 非边界起点只产跨字垃圾桶）。`blocked` 为 M2 屏蔽谓词，先行过滤。
pub(crate) fn collect_buckets(
    dict: &Dict,
    graph: &SyllableGraph,
    max_word_syllables: usize,
    origins: &std::collections::BTreeSet<usize>,
    blocked: impl Fn(&str, &str) -> bool,
) -> Buckets {
    let mut buckets: Buckets = BTreeMap::new();

    // 游标状态：键串即词典位置；origin 随行（桶键 = (起点, 终点)）
    #[derive(Clone)]
    struct Walk {
        origin: usize,
        v: usize,
        key: String,
        hops: usize,
        class: u8,
        cred: f64,
    }

    let mut visited: std::collections::HashSet<(usize, usize, String, bool)> =
        std::collections::HashSet::new();
    let mut queue: std::collections::VecDeque<Walk> = std::collections::VecDeque::new();
    // 待物化桶标记（librime 惰性 accessor 的字符串键等价：游标步零分配探针，
    // 命中只记 Marker，BFS 结束后统一取词条）
    struct Marker {
        origin: usize,
        end: usize,
        key: String,
        class: u8,
        completion: bool,
        cred: f64,
    }
    let mut markers: Vec<Marker> = Vec::new();

    for &start in origins {
        if start >= graph.farthest || (start != 0 && !graph.edges.contains_key(&start)) {
            continue;
        }
        queue.push_back(Walk {
            origin: start,
            v: start,
            key: String::new(),
            hops: 0,
            class: 0,
            cred: 0.0,
        });
        visited.insert((start, start, String::new(), false));
    }

    while let Some(w) = queue.pop_front() {
        if w.hops >= max_word_syllables {
            continue;
        }
        let Some(ends) = graph.edges.get(&w.v) else {
            continue;
        };
        // 简拼=兜底语义：顶点存在 Normal 出边时只走 Normal/Completion
        let has_normal = ends
            .values()
            .flat_map(|sps| sps.iter())
            .any(|sp| sp.spelling_type == SpellingType::Normal);
        for (&e, spellings) in ends.iter().rev() {
            let mut ordered: Vec<&super::syllabifier::Spelling> = spellings.iter().collect();
            if !has_normal {
                // 无 Normal：短拼写优先（字母先于音节展开，压缩键先查）
                ordered.sort_by_key(|sp| sp.syllable.chars().count());
            } else {
                ordered.retain(|sp| {
                    sp.spelling_type == SpellingType::Normal
                        || sp.spelling_type == SpellingType::Completion
                });
            }
            for sp in ordered {
                // 键串延长：字母串自身（单字母简拼边，压缩简拼键族）**直拼**；
                // 音节全名/补全段以 ' 连接（词库键族）——对齐任务书 §13#2
                // 「全字母路径用 concat 键、纯音节路径用 join' 键」（2026-08-29
                // 补：旧实现恒 join，纯简拼 `jj` 拼成 `j'j` 命中不了 concat 键）。
                let mut nkey = String::with_capacity(w.key.len() + sp.syllable.len() + 1);
                nkey.push_str(&w.key);
                let concat = sp.spelling_type == SpellingType::Abbreviation
                    && sp.syllable.chars().count() == 1;
                if !concat {
                    let last_byte = w.key.as_bytes().last().copied();
                    if last_byte.is_some_and(|b| b != b'\'') {
                        nkey.push('\'');
                    }
                }
                nkey.push_str(&sp.syllable);
                let completion = sp.spelling_type == SpellingType::Completion;
                let abbrev = sp.spelling_type == SpellingType::Abbreviation;
                let class = w.class.max(st_cls(completion, abbrev));
                let cred = w.cred + sp.credibility;
                if !visited.insert((e, w.origin, nkey.clone(), completion)) {
                    continue;
                }
                // —— 词典剪枝（rime exhausted 等价，零分配探针）——
                let (has_eq, deeper) = if completion {
                    // 补全段：前缀命中即收集（predictive）兼可继续走
                    let p = dict.has_prefix(&nkey);
                    (p, p)
                } else {
                    (dict.has_code(&nkey), dict.has_prefix(&nkey))
                };
                if has_eq {
                    // 标记待物化桶（第二阶段统一 exact/prefix 取词条）
                    markers.push(Marker {
                        origin: w.origin,
                        end: e,
                        key: nkey.clone(),
                        class: if completion { class.max(2) } else { class },
                        completion,
                        cred,
                    });
                }
                if deeper {
                    queue.push_back(Walk {
                        origin: w.origin,
                        v: e,
                        key: nkey,
                        hops: w.hops + 1,
                        class,
                        cred,
                    });
                }
            }
        }
    }

    // —— 第二阶段：按标记统一物化词条（惰性 accessor 的取词时刻）——
    for m in &markers {
        let got: Vec<iuv_data::Entry> = if m.completion {
            dict.prefix(&m.key, 64)
        } else {
            dict.exact(&m.key)
        };
        if got.is_empty() {
            continue;
        }
        let slot = buckets.entry((m.origin, m.end)).or_default();
        for entry in got {
            if blocked(&entry.code, &entry.word) {
                continue;
            }
            merge_into(
                slot,
                BucketEntry {
                    entry,
                    exact: !m.completion,
                    class: if m.completion { m.class.max(2) } else { m.class },
                    cred: m.cred,
                },
            );
        }
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

fn st_cls(completion: bool, abbrev: bool) -> u8 {
    if completion {
        2
    } else if abbrev {
        1
    } else {
        0
    }
}

/// 同词多路径合并：精确优先、其后权重优先；类别含补全恒 2，否则取更纯者；
/// cred 取两条路径中更优（较大，负值惩罚小者为优）。
fn merge_into(slot: &mut Vec<BucketEntry>, be: BucketEntry) {
    match slot.iter().position(|x| x.entry.word == be.entry.word) {
        Some(i) => {
            let merged_class = if slot[i].class == 2 || be.class == 2 {
                2
            } else {
                slot[i].class.min(be.class)
            };
            let merged_cred = slot[i].cred.max(be.cred);
            let replace = (!slot[i].exact && be.exact)
                || (slot[i].exact == be.exact && be.entry.weight > slot[i].entry.weight);
            if replace {
                slot[i] = be;
            }
            slot[i].class = merged_class;
            slot[i].cred = merged_cred;
        }
        None => slot.push(be),
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
                // credibility 累进 log 权重（dictionary.cc:164 语义）——
                // 补全/简拼边在组句 DP 中劣于纯全拼边（2026-08-29 λ 校准）
                log_weight: lm_log_prob(be.entry.weight) + be.cred,
            });
        }
    }
    if wg.is_empty() {
        return None;
    }
    Some(wg)
}

/// 由 Poet 结果构造整句候选（seg_len 置大数 = 全消费；score = 组句路径权重）。
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
