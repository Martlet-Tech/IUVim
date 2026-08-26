//! 组句器（librime gear/poet.cc 的 Rust arena 移植，39-rime-pipeline.md §Step2）。
//!
//! 派生自 librime（BSD-3-Clause，Copyright RIME Developers）。原始文件头注明
//! "simplistic sentence-making"：词格上求最优词序列。本移植保留：
//! - DP 策略（无语法组件时，每位置单状态）与 BeamSearch 策略（有语法组件时，
//!   按「末词」分组保 top-7）——poet.cc:106-186；
//! - 排除「单词贯穿全程」的退化解（start==0 && end==total 跳过，poet.cc:207-208）；
//! - 每词权重累加 = 前缀累计 + Grammar::Evaluate(context, word, entry_weight)，
//!   context 回看两词 + preceding_text（:215-219）；本引擎无语法组件时
//!   Evaluate = entry_weight + kPenalty（grammar.h:18-27，kPenalty=ln(1e-6)）——
//!   常数罚分使「少词路径」一致占优，即 rime 的长度偏好来源；
//! - 多句 MakeSentences 的束宽 3×N、滚动哈希去重、cutoff 阈值衰减调度（:258-353）。
//!
//! 改写点：C++ 版 Line.predecessor 是依赖 std::map 指针稳定性的裸指针——
//! Rust 版改用 arena（Vec<Line> + u32 索引），语义等价。

/// 无语法组件时的每词罚分（librime grammar.h:13 kPenalty ≈ ln(1e-6)）。
pub(crate) const GRAMMAR_PENALTY: f64 = -13.815_510_557_964_274;

/// BeamSearch 每位置保留的候选线数上限（poet.cc:137 kMaxLineCandidates）。
#[allow(dead_code)]
pub(crate) const MAX_LINE_CANDIDATES: usize = 7;

/// 词条轻量视图（避免物化整个 Entry）。
#[derive(Clone, Debug)]
pub(crate) struct GraphEntry {
    pub word: String,
    /// 产生该词条的音节路径键（撇号分隔，如 "ni'hao"；学习 key / 屏蔽 key 用）
    pub code: String,
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
    code: String,
    end_pos: usize,
    weight: f64,
    /// 路径全文本滚动哈希（MakeSentences 去重用；DP 策略不消费）
    #[allow(dead_code)]
    hash: u64,
}

impl Line {
    fn empty() -> Line {
        Line { prev: None, word: String::new(), code: String::new(), end_pos: 0, weight: 0.0, hash: 0 }
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
    /// 回放路径各词编码（同序）。
    fn path_codes(&self, arena: &[Line]) -> Vec<String> {
        let mut out = Vec::new();
        let mut cur = Some(self);
        while let Some(l) = cur {
            if l.is_empty() {
                break;
            }
            out.push(l.code.clone());
            cur = l.prev.map(|p| &arena[p as usize]);
        }
        out.reverse();
        out
    }
    /// 回放路径各词 end_pos（同序）。
    fn path_ends(&self, arena: &[Line]) -> Vec<usize> {
        let mut out = Vec::new();
        let mut cur = Some(self);
        while let Some(l) = cur {
            if l.is_empty() {
                break;
            }
            out.push(l.end_pos);
            cur = l.prev.map(|p| &arena[p as usize]);
        }
        out.reverse();
        out
    }
}

fn text_hash(s: &str) -> u64 {
    // poet.cc 的 31 进制滚动哈希
    let mut h: u64 = 0;
    for b in s.bytes() {
        h = h.wrapping_mul(31).wrapping_add(b as u64);
    }
    h
}

/// 路径全文本哈希（含新词；poet.cc MakeSentences 的滚动 text_hash）。
#[allow(dead_code)]
fn path_hash(line: &Line, arena: &[Line], new_word: &str) -> u64 {
    let mut h: u64 = 0;
    for wd in line.path_words(arena) {
        h = h.wrapping_mul(31).wrapping_add(text_hash(&wd));
    }
    h.wrapping_mul(31).wrapping_add(text_hash(new_word))
}

/// 组句策略（poet.cc:106-186）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Strategy {
    /// 无语法组件：每位置单状态纯 DP。
    DynamicProgramming,
    /// 有语法组件：按末词分组、全局 top-7 扩展。（M3 octagram 类组件落地后启用。）
    #[allow(dead_code)]
    BeamSearch,
}

/// 组句结果：词文本序列 + 各词编码 + 各词终点位置 + 路径总权重。
#[derive(Clone, Debug)]
pub(crate) struct Sentence {
    pub words: Vec<String>,
    /// 各词产生路径键（学习/屏蔽 key 备用，M3 消费）
    #[allow(dead_code)]
    pub codes: Vec<String>,
    pub ends: Vec<usize>,
    pub weight: f64,
}

/// 单最优句（poet.cc MakeSentence）。
pub(crate) fn make_sentence(
    graph: &WordGraph,
    total_length: usize,
    preceding_text: &str,
    strategy: Strategy,
) -> Option<Sentence> {
    make_inner(graph, total_length, preceding_text, strategy).map(|(s, _)| s)
}

/// 多句（poet.cc MakeSentences）：束宽 3×max_sentences，cutoff 衰减截断。
/// （M3 学习/多整句候选启用；当前主流程走 make_sentence。）
#[allow(dead_code)]
pub(crate) fn make_sentences(
    graph: &WordGraph,
    total_length: usize,
    preceding_text: &str,
    max_sentences: usize,
    cutoff_threshold: f64,
) -> Vec<Sentence> {
    if max_sentences == 0 || total_length == 0 {
        return Vec::new();
    }
    let beam_width = max_sentences * 3;
    let mut arena: Vec<Line> = vec![Line::empty()];
    // states[pos] = 线索引列表（按权重降序维护）
    let mut states: std::collections::BTreeMap<usize, Vec<u32>> = Default::default();
    states.insert(0, vec![0]);

    for (&start_pos, ends_map) in graph.iter() {
        let source = match states.get(&start_pos) {
            Some(v) => v.clone(),
            None => continue,
        };
            for (&end_pos, entries) in ends_map.iter() {
                if start_pos == 0 && end_pos == total_length {
                    continue; // 单词解排除
                }
                let is_rear = end_pos == total_length;
                let target_slot = states.entry(end_pos).or_default();
                // 快照线信息（借用不跨 arena.push；见下方 Line 构造）
                let snapshots: Vec<(u32, String, f64, Vec<String>)> = source
                    .iter()
                    .map(|&idx| {
                        let l = &arena[idx as usize];
                        (
                            idx,
                            l.context(&arena, preceding_text),
                            l.weight,
                            l.path_words(&arena),
                        )
                    })
                    .collect();
                for (line_idx, ctx, line_weight, prior_words) in snapshots {
                    for e in entries {
                        let w = line_weight + evaluate(&ctx, &e.word, e.log_weight, is_rear);
                        let h = {
                            let mut hh: u64 = 0;
                            for wd in &prior_words {
                                hh = hh.wrapping_mul(31).wrapping_add(text_hash(wd));
                            }
                            hh.wrapping_mul(31).wrapping_add(text_hash(&e.word))
                        };
                        // 同路径文本留高权重（poet.cc:289-305）
                        if let Some(dup) =
                            target_slot.iter().copied().find(|&i| arena[i as usize].hash == h)
                        {
                            if arena[dup as usize].weight >= w {
                                continue;
                            }
                            target_slot.retain(|&i| i != dup);
                        }
                        let pos = target_slot
                            .iter()
                            .position(|&i| arena[i as usize].weight < w)
                            .unwrap_or(target_slot.len());
                        target_slot.insert(
                            pos,
                            Line {
                                prev: Some(line_idx),
                                word: e.word.clone(),
                                code: e.code.clone(),
                                end_pos,
                                weight: w,
                                hash: h,
                            }
                            .into_id(&mut arena),
                        );
                        if target_slot.len() > beam_width {
                            target_slot.pop();
                        }
                    }
                }
            }
    }

    let final_state = match states.get(&total_length) {
        Some(v) => v,
        None => return Vec::new(),
    };
    let mut results = Vec::new();
    let mut last_weight = 0.0_f64;
    let acceleration = 1.0 - 1.0 / max_sentences as f64;
    let mut threshold = cutoff_threshold;
    for (i, &idx) in final_state.iter().enumerate() {
        if i >= max_sentences {
            break;
        }
        let line = &arena[idx as usize];
        if line.is_empty() {
            break;
        }
        let cur = line.weight;
        if i > 0 {
            if last_weight != 0.0 && (cur - last_weight).abs() / last_weight.abs() > threshold {
                break;
            }
            threshold *= acceleration;
        }
        last_weight = cur;
        results.push(Sentence {
            words: line.path_words(&arena),
            codes: line.path_codes(&arena),
            ends: line.path_ends(&arena),
            weight: cur,
        });
    }
    results
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

fn make_inner(
    graph: &WordGraph,
    total_length: usize,
    preceding_text: &str,
    strategy: Strategy,
) -> Option<(Sentence, f64)> {
    let mut arena: Vec<Line> = vec![Line::empty()];
    enum State {
        Dp(Option<u32>),
        Beam(std::collections::HashMap<String, u32>),
    }
    let mut states: std::collections::BTreeMap<usize, State> = Default::default();
    states.insert(
        0,
        match strategy {
            Strategy::DynamicProgramming => State::Dp(Some(0)),
            Strategy::BeamSearch => {
                let mut m = std::collections::HashMap::new();
                m.insert(String::new(), 0u32);
                State::Beam(m)
            }
        },
    );

    for (&start_pos, ends_map) in graph.iter() {
        let source_lines: Vec<u32> = match states.get(&start_pos) {
            Some(State::Dp(idx)) => idx.iter().copied().collect(),
            Some(State::Beam(map)) => {
                let mut all: Vec<u32> = map.values().copied().collect();
                all.sort_by(|&a, &b| {
                    arena[b as usize]
                        .weight
                        .partial_cmp(&arena[a as usize].weight)
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
                all.truncate(MAX_LINE_CANDIDATES);
                all
            }
            None => continue,
        };
        for (&end_pos, entries) in ends_map.iter() {
            if start_pos == 0 && end_pos == total_length {
                continue; // 单词解排除（poet.cc:207-208）
            }
            let is_rear = end_pos == total_length;
            // 快照线信息（借用不跨 arena.push）
            let snapshots: Vec<(u32, String, f64, Vec<String>)> = source_lines
                .iter()
                .map(|&idx| {
                    let l = &arena[idx as usize];
                    (
                        idx,
                        l.context(&arena, preceding_text),
                        l.weight,
                        l.path_words(&arena),
                    )
                })
                .collect();
            for (line_idx, ctx, line_weight, prior_words) in snapshots {
                for e in entries {
                    let w = line_weight + evaluate(&ctx, &e.word, e.log_weight, is_rear);
                    let h = {
                        let mut hh: u64 = 0;
                        for wd in &prior_words {
                            hh = hh.wrapping_mul(31).wrapping_add(text_hash(wd));
                        }
                        hh.wrapping_mul(31).wrapping_add(text_hash(&e.word))
                    };
                    let new_line = Line {
                        prev: Some(line_idx),
                        word: e.word.clone(),
                        code: e.code.clone(),
                        end_pos,
                        weight: w,
                        hash: h,
                    };
                    let slot = states.entry(end_pos).or_insert_with(|| match strategy {
                        Strategy::DynamicProgramming => State::Dp(None),
                        Strategy::BeamSearch => State::Beam(Default::default()),
                    });
                    match slot {
                        State::Dp(best) => {
                            let better = match best {
                                None => true,
                                Some(b) => arena[*b as usize].weight < w,
                            };
                            if better {
                                *best = Some(new_line.into_id(&mut arena));
                            }
                        }
                        State::Beam(map) => {
                            // 按「末词」分组各保一条最优线（poet.cc BestLineToUpdate）
                            let dominated = match map.get(&new_line.word) {
                                Some(&b) => arena[b as usize].weight >= w,
                                None => false,
                            };
                            if !dominated {
                                let word = new_line.word.clone();
                                let id = new_line.into_id(&mut arena);
                                map.insert(word, id);
                            }
                        }
                    }
                }
            }
        }
    }

    let best_idx = match states.get(&total_length)? {
        State::Dp(idx) => match idx {
            Some(i) => *i,
            None => return None,
        },
        State::Beam(map) => map
            .values()
            .copied()
            .max_by(|&a, &b| {
                arena[a as usize]
                    .weight
                    .partial_cmp(&arena[b as usize].weight)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })?,
    };
    let line = &arena[best_idx as usize];
    if line.is_empty() || line.end_pos != total_length {
        return None;
    }
    let s = Sentence {
        words: line.path_words(&arena),
        codes: line.path_codes(&arena),
        ends: line.path_ends(&arena),
        weight: line.weight,
    };
    Some((s, line.weight))
}

/// Grammar::Evaluate（grammar.h:18-27）：entry_weight + Query 或常数罚分。
/// 本引擎暂无语法组件 → 恒走罚分支（unigram 信息已含在 entry_weight 内）。
pub(crate) fn evaluate(_context: &str, _word: &str, entry_log_weight: f64, _is_rear: bool) -> f64 {
    entry_log_weight + GRAMMAR_PENALTY
}
