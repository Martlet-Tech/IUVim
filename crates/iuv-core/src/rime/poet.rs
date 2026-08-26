//! 组句器（librime gear/poet.cc 的 Rust arena 移植，39-rime-pipeline.md §Step2）。
//!
//! 派生自 librime（BSD-3-Clause，Copyright RIME Developers）。原始文件头注明
//! "simplistic sentence-making"：词格上求最优词序列。本移植保留（**纯 DP 版**）：
//! - DP 策略（无语法组件时，每位置单状态）——poet.cc:106-186；
//! - 排除「单词贯穿全程」的退化解（start==0 && end==total 跳过，poet.cc:207-208）；
//! - 每词权重累加 = 前缀累计 + Grammar::Evaluate(context, word, entry_weight)，
//!   context 回看两词 + preceding_text（:215-219）；本引擎无语法组件时
//!   Evaluate = entry_weight + kPenalty（grammar.h:18-27，kPenalty=ln(1e-6)）——
//!   常数罚分使「少词路径」一致占优，即 rime 的长度偏好来源。
//!
//! 改写点：
//! - C++ 版 Line.predecessor 是依赖 std::map 指针稳定性的裸指针——Rust 版改用
//!   arena（Vec<Line> + u32 索引），语义等价；
//! - 2026-08-26 大幅裁剪（死代码清理）：**BeamSearch 策略 / MakeSentences(mlti) /
//!   词条 code 追踪链 / 路径滚动哈希** 均已移除——当前无语法组件、行主流程只用单句
//!   DP。M3 引入语法组件 / 多整句候选时按 librime 源码重建（BSD-3 可再派生）。

/// 无语法组件时的每词罚分（librime grammar.h:13 kPenalty ≈ ln(1e-6)）。
pub(crate) const GRAMMAR_PENALTY: f64 = -13.815_510_557_964_274;

/// 词条轻量视图（避免物化整个 Entry）。
#[derive(Clone, Debug)]
pub(crate) struct GraphEntry {
    pub word: String,
    /// log 域词条权重（ln((freq+1)/total)，与 LmProvider 同一口径）
    pub log_weight: f64,
}

/// 词格：起点 → 终点 → 词条列表（poet.h:20 WordGraph 的同构）。
pub(crate) type WordGraph =
    std::collections::BTreeMap<usize, std::collections::BTreeMap<usize, Vec<GraphEntry>>>;

/// 一条组句线（arena 存储单元；对应 poet.cc struct Line）。
#[derive(Clone, Debug)]
struct Line {
    prev: Option<u32>,
    word: String,
    end_pos: usize,
    weight: f64,
}

impl Line {
    fn empty() -> Line {
        Line { prev: None, word: String::new(), end_pos: 0, weight: 0.0 }
    }
    fn is_empty(&self) -> bool {
        self.prev.is_none() && self.word.is_empty()
    }
    /// poet.cc Line::context：回看两词拼接（前一词+当前词）；空线返回 preceding。
    fn context(&self, arena: &[Line], preceding: &str) -> String {
        if self.is_empty() {
            return preceding.to_string();
        }
        match self.prev.map(|p| &arena[p as usize]) {
            None => self.word.clone(),
            Some(prev_line) => format!("{}{}", prev_line.word, self.word),
        }
    }
    /// 回放路径词文本（自尾向头收集后反转）。空线不含在内。
    fn path_words(&self, arena: &[Line]) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = Some(self);
        while let Some(l) = cur {
            if l.is_empty() {
                break;
            }
            out.push(l.word.clone());
            cur = l.prev.map(|p| &arena[p as usize]);
        }
        out.reverse();
        out
    }
}

/// 组句结果：词文本序列 + 路径总权重。
#[derive(Clone, Debug)]
pub(crate) struct Sentence {
    pub words: Vec<String>,
    pub weight: f64,
}

/// 单最优句（poet.cc MakeSentence，纯 DP）。
pub(crate) fn make_sentence(
    graph: &WordGraph,
    total_length: usize,
    preceding_text: &str,
) -> Option<Sentence> {
    let mut arena: Vec<Line> = vec![Line::empty()];
    // states[pos] = 该位置最优线索引（单状态）
    let mut states: std::collections::BTreeMap<usize, Option<u32>> = Default::default();
    states.insert(0, Some(0));

    for (&start_pos, ends_map) in graph.iter() {
        let Some(source_idx) = states.get(&start_pos).copied().flatten() else {
            continue;
        };
        for (&end_pos, entries) in ends_map.iter() {
            if start_pos == 0 && end_pos == total_length {
                continue; // 单词解排除（poet.cc:207-208）
            }
            let is_rear = end_pos == total_length;
            // 快照线信息（借用不跨 arena.push）
            let line = &arena[source_idx as usize];
            let ctx = line.context(&arena, preceding_text);
            let line_weight = line.weight;
            for e in entries {
                let w = line_weight + evaluate(&ctx, &e.word, e.log_weight, is_rear);
                let new_line = Line {
                    prev: Some(source_idx),
                    word: e.word.clone(),
                    end_pos,
                    weight: w,
                };
                let slot = states.entry(end_pos).or_insert(None);
                let better = match slot {
                    None => true,
                    Some(b) => arena[*b as usize].weight < w,
                };
                if better {
                    *slot = Some(new_line.into_id(&mut arena));
                }
            }
        }
    }

    let best_idx = match states.get(&total_length) {
        Some(Some(i)) => *i,
        _ => return None,
    };
    let line = &arena[best_idx as usize];
    if line.is_empty() || line.end_pos != total_length {
        return None;
    }
    Some(Sentence { words: line.path_words(&arena), weight: line.weight })
}

trait IntoId {
    fn into_id(self, arena: &mut Vec<Line>) -> u32;
}
impl IntoId for Line {
    fn into_id(self, arena: &mut Vec<Line>) -> u32 {
        arena.push(self);
        (arena.len() - 1) as u32
    }
}

/// Grammar::Evaluate（grammar.h:18-27）：entry_weight + Query 或常数罚分。
/// 本引擎暂无语法组件 → 恒走罚分支（unigram 信息已含在 entry_weight 内）。
pub(crate) fn evaluate(_context: &str, _word: &str, entry_log_weight: f64, _is_rear: bool) -> f64 {
    entry_log_weight + GRAMMAR_PENALTY
}
