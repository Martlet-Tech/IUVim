//! 组句器（librime gear/poet.cc 的 Rust arena 移植，39-rime-pipeline.md §Step2）。
//!
//! 派生自 librime（BSD-3-Clause，Copyright RIME Developers）。原始文件头注明
//! "simplistic sentence-making"：词格上求最优词序列。本移植保留（**纯 DP 版**）：
//! - DP 策略（无语法组件时，每位置单状态）——poet.cc:106-186；
//! - 排除「单词贯穿全程」的退化解（start==0 && end==total 跳过，poet.cc:207-208）；
//! - 每词权重累加 = 前缀累计 + Grammar::Evaluate(context, word, entry_weight)，
//!   context 回看两词 + preceding_text（:215-219）；本引擎无语法组件时
//!   Evaluate = entry_weight + λ（grammar.h:18-27 的 kPenalty=ln(1e-6) 即 librime
//!   原生 λ）——每多一个词多扣一次 λ，长词路径占优，即 rime 的长度偏好来源。
//!   λ 已参数化（config `rime_lambda`，39 号 §W2 校准），常数仅作默认值。
//!
//! 改写点：
//! - C++ 版 Line.predecessor 是依赖 std::map 指针稳定性的裸指针——Rust 版改用
//!   arena（Vec<Line> + u32 索引），语义等价；
//! - 平局决胜按 poet.cc:88-109 CompareWeight/LeftAssociateCompare 重建：
//!   权重降序 → 词数少者优 → 词长（编码字符数）序列字典序（左结合）；
//! - 2026-08-26 大幅裁剪（死代码清理）：**BeamSearch 策略 / MakeSentences(mlti) /
//!   词条 code 追踪链 / 路径滚动哈希** 均已移除——当前无语法组件、行主流程只用单句
//!   DP。M3 引入语法组件 / 多整句候选时按 librime 源码重建（BSD-3 可再派生）。

/// λ 默认值 = librime 无语法组件时的每词罚分（grammar.h:13 kPenalty ≈ ln(1e-6)）。
/// 校准经 config `rime_lambda` 调整（39 号 W2），此常数仅是缺省值。
pub(crate) const DEFAULT_LAMBDA: f64 = -13.815_510_557_964_274;

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
    /// 词数（平局决胜：少词优先）
    n_words: u32,
    /// 各词编码字符数序列（平局决胜：左结合字典序）
    lens: Vec<u32>,
}

impl Line {
    fn empty() -> Line {
        Line {
            prev: None,
            word: String::new(),
            end_pos: 0,
            weight: 0.0,
            n_words: 0,
            lens: Vec::new(),
        }
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

/// 组句结果：词文本序列 + 路径权重（log 域，上浮为整句候选 score）。
#[derive(Clone, Debug)]
pub(crate) struct Sentence {
    pub words: Vec<String>,
    pub weight: f64,
}

/// 单最优句（poet.cc MakeSentence，纯 DP）。`lambda` = 每词长度惩罚（log 域）。
pub(crate) fn make_sentence(
    graph: &WordGraph,
    total_length: usize,
    preceding_text: &str,
    lambda: f64,
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
            let (line_weight, ctx, n_prev, lens_prev) = {
                let line = &arena[source_idx as usize];
                (
                    line.weight,
                    line.context(&arena, preceding_text),
                    line.n_words,
                    line.lens.clone(),
                )
            };
            for e in entries {
                let w = line_weight + evaluate(&ctx, &e.word, e.log_weight, is_rear, lambda);
                let n_words = n_prev + 1;
                let mut lens = lens_prev.clone();
                lens.push(e.word.chars().count() as u32);
                let new_line = Line {
                    prev: Some(source_idx),
                    word: e.word.clone(),
                    end_pos,
                    weight: w,
                    n_words,
                    lens,
                };
                let slot = states.entry(end_pos).or_insert(None);
                let better = match slot {
                    None => true,
                    // 平局决胜（poet.cc:88-109）：权重降序 → 少词优先 →
                    // 词长序列字典序（左结合）
                    Some(b) => compare_line(&arena[*b as usize], &new_line) == std::cmp::Ordering::Less,
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
    let weight = line.weight;
    Some(Sentence { words: line.path_words(&arena), weight })
}

/// 线比较（CompareWeight + LeftAssociateCompare 的合并，poet.cc:88-109）：
/// 返回 Greater = `a` 优于 `b`。权重高者优；等权时词数少者优；
/// 再同则词长序列字典序，短序列在前（左结合偏好）。
fn compare_line(a: &Line, b: &Line) -> std::cmp::Ordering {
    a.weight
        .partial_cmp(&b.weight)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then(b.n_words.cmp(&a.n_words))
        .then(a.lens.cmp(&b.lens))
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

/// Grammar::Evaluate（grammar.h:18-27）：entry_weight + Query 或长度惩罚。
/// 本引擎暂无语法组件 → 恒走罚分支（unigram 信息已含在 entry_weight 内）；
/// `lambda` 为每词长度惩罚（log 域负值，config `rime_lambda`）。
pub(crate) fn evaluate(
    _context: &str,
    _word: &str,
    entry_log_weight: f64,
    _is_rear: bool,
    lambda: f64,
) -> f64 {
    entry_log_weight + lambda
}
