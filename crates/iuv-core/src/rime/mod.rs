//! rime 引擎核心（39-rime-pipeline.md §Step2）。
//!
//! 派生自 librime（BSD-3-Clause）。模块构成：
//! - [`syllabifier`]：音节图（Normal/Abbreviation/Completion 三类拼写边）；
//! - [`translator`]：词条桶收集 + 整句闸门 + 词候选流；
//! - [`poet`]：组句 DP/Beam 双策略。
//!
//! 架构裁决（2026-08-26，记录于任务书附录）：librime 的 Segmentation/Context
//! 状态机**不移植**——ImeEngine 接缝按「单活动段」切分，打字期 rime 本就单段
//! 覆盖重译（navigator.cc 规格实证），段生命周期归会话层（Step 3 分段确认模型），
//! 与 librime 的 engine/translators 分层同构。

pub(crate) mod poet;
pub(crate) mod syllabifier;
pub(crate) mod translator;

use crate::api::{EngineCtx, ImeEngine, PendingInput, Span, Translation};
use crate::schema::InputSchema;
use crate::LmProvider;
use iuv_data::Dict;
use std::collections::BTreeSet;
use std::sync::Arc;
/// 音节匹配最大长度（与 Quanpin 同规，封顶 6）。
const MAX_SYLLABLE_LEN: usize = 6;

/// 组词最大音节数（lattice 宽度上限，对齐 Config.max_word_syllables 默认值 7）。
const MAX_WORD_SYLLABLES: usize = 7;

/// rime 核心引擎。持有词库与切分器；线程安全（进程级单例跨线程共享）。
pub struct RimeEngine {
    dict: Arc<Dict>,
    schema: crate::schema::Quanpin,
    lm: crate::UnigramLm,
    syllables: BTreeSet<String>,
    /// 候选截断上限（对齐 Config.max_candidates 语义）
    max_candidates: usize,
}

impl RimeEngine {
    /// 从共享词库装配（与 classic Engine 共享同一 Dict 实例：
    /// M2 用户库调权/屏蔽跨核心一致）。
    pub fn new(dict: Arc<Dict>, config: &crate::Config) -> Arc<RimeEngine> {
        let syllables = dict.syllables().clone();
        let lm = crate::UnigramLm::new(dict.total_weight(), dict.entry_count());
        Arc::new(RimeEngine {
            dict,
            schema: crate::schema::Quanpin::new(syllables.clone()),
            lm,
            syllables,
            max_candidates: config.max_candidates,
        })
    }

    fn is_syllable(&self, s: &str) -> bool {
        self.syllables.contains(s)
    }

    fn is_syllable_prefix(&self, s: &str) -> bool {
        self.syllables.iter().any(|syl| syl.starts_with(s))
    }

    /// 纯单字政策（classic::single_segment_candidates 同款）：完整音节 →
    /// exact_single 全量；严格前缀 → 首字母桶过滤。
    fn prefix_chars_translation(&self, pending: &PendingInput, seg: &[String]) -> Translation {
        let plain = crate::strip_apostrophes(pending.raw);
        let entries: Vec<iuv_data::Entry> = if self.is_syllable(&plain) {
            self.dict.exact_single(&plain)
        } else if let Some(first) = plain.chars().next() {
            self.dict
                .initial_top(first, iuv_data::INITIAL_BUCKET_SIZE)
                .into_iter()
                .filter(|e| e.code.starts_with(&plain))
                .collect()
        } else {
            Vec::new()
        };
        let cands: Vec<crate::Candidate> = entries
            .into_iter()
            .map(|e| {
                let mut c = crate::Candidate::for_entry(
                    &e,
                    crate::CandidateKind::Char,
                    1.min(seg.iter().filter(|s| !s.is_empty()).count().max(1)),
                );
                c.score = self.lm.log_prob(None, "", e.weight);
                c
            })
            .collect();
        if cands.is_empty() {
            return self.fallback_translation(pending, seg);
        }
        Translation {
            segmentation: vec![Span { syllables: seg.to_vec(), tags: vec!["pinyin"] }],
            candidates: cands,
        }
    }

    fn blocked(&self) -> impl Fn(&str, &str) -> bool + '_ {
        move |code: &str, word: &str| {
            self.dict
                .user()
                .map(|u| u.is_blocked(code, word))
                .unwrap_or(false)
        }
    }

    /// 分段视图首段 = 方案词频重排后的贪心切分（与会话层既有 seg 口径一致，
    /// 保证部分消费推进的段数语义在双引擎下不变）。
    fn ranked_seg(&self, raw: &str) -> Vec<String> {
        let mut plans = self.schema.segment(raw);
        if plans.len() > 1 {
            let mut scored: Vec<(u32, usize)> = plans
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let key = p.join("'");
                    let w = self.dict.exact(&key).first().map(|e| e.weight).unwrap_or(0);
                    (w, i)
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            let idx: Vec<usize> = scored.into_iter().map(|(_, i)| i).collect();
            plans = idx.into_iter().map(|i| plans[i].clone()).collect();
        }
        plans.into_iter().next().unwrap_or_default()
    }

    /// 原文兜底候选（与 classic generate_candidates 尾部同款："不认识"语义）。
    fn fallback_translation(&self, pending: &PendingInput, seg: &[String]) -> Translation {
        let plain = crate::strip_apostrophes(pending.raw);
        let mut cands = Vec::new();
        if !plain.is_empty() {
            let kind = if plain.chars().count() >= 2 {
                crate::CandidateKind::Word
            } else {
                crate::CandidateKind::Char
            };
            let n_seg = seg.iter().filter(|s| !s.is_empty()).count();
            cands.push(crate::Candidate::new(
                plain.clone(),
                kind,
                plain,
                0,
                n_seg.max(1),
            ));
        }
        Translation { segmentation: vec![], candidates: cands }
    }
}

impl ImeEngine for RimeEngine {
    fn translate(&self, ctx: &EngineCtx, pending: &PendingInput) -> Translation {
        if pending.raw.is_empty() {
            return Translation { segmentation: vec![], candidates: vec![] };
        }
        // 图构建用小写视图（ASCII 一一对应；大写保形显示由会话层既有路径处理）
        let lower = pending.raw.to_lowercase();
        let seg = self.ranked_seg(pending.raw);
        let n_seg = seg.iter().filter(|s| !s.is_empty()).count();

        let graph = syllabifier::build_graph(&lower, &self.syllables, MAX_SYLLABLE_LEN);
        // —— 微软对齐政策（classic PrefixChars，档位降级为核心内部政策）：
        // 整串为音节真前缀且非完整音节 → 纯单字，不走图流。——
        let plain_l = lower.trim_matches('\'');
        if !plain_l.is_empty() && !self.is_syllable(plain_l) && self.is_syllable_prefix(plain_l) {
            return self.prefix_chars_translation(pending, &seg);
        }
        let mut graph = graph;
        // —— 2b 补全边注入（守卫 n_seg≥2，与 classic 句通道守卫同规）——
        {
            let lens: Vec<usize> =
                seg.iter().filter(|s| !s.is_empty()).map(|s| s.chars().count()).collect();
            let n_ne = lens.len();
            if n_ne >= 2 {
                let total: usize = lens.iter().sum();
                let tail_start = total - lens[n_ne - 1];
                if let Some(last) = seg.iter().filter(|s| !s.is_empty()).last() {
                    if !self.is_syllable(last)
                        && self.syllables.iter().any(|syl| syl.starts_with(last.as_str()))
                    {
                        syllabifier::push_completion_edge(&mut graph, lower.len(), tail_start, last);
                    }
                }
            }
        }
        if graph.edges.is_empty() || graph.farthest == 0 {
            return self.fallback_translation(pending, &seg);
        }
        let buckets = translator::collect_buckets(
            &self.dict,
            &graph,
            MAX_WORD_SYLLABLES,
            self.blocked(),
        );

        // —— 词候选流（st.cc 码长优先 + 2026-08-26 裁决分级）：
        //    类 2（尾前缀补全，恒覆盖全跨度——对齐 classic 2b 整句置顶与
        //    rime end-desc 铁律）→ 类 0（纯全拼桶）→ 类 1（含简拼，保守沉底），
        //    类内消耗终点降序，桶内已按精确优先 + 权重降序。——
        // seg_len 按字节跨度映射到贪心分段数（librime end_pos 语义：候选消费 =
        // 其图终点覆盖的段数；简拼边值长≠字节跨度的错位由此归位）。
        let mut cum = vec![0usize];
        for s in seg.iter().filter(|s| !s.is_empty()) {
            let last = *cum.last().unwrap_or(&0);
            cum.push(last + s.len());
        }
        let consumed_parts = |end_byte: usize| -> usize {
            cum.iter()
                .rposition(|&c| c <= end_byte)
                .unwrap_or(0)
                .min(n_seg.max(1))
        };

        let mut cands: Vec<crate::Candidate> = Vec::new();
        for class in [2u8, 0, 1] {
            let mut ends: Vec<usize> = buckets
                .iter()
                .filter(|(&(_, e), slot)| {
                    e <= graph.farthest && e > 0 && slot.iter().any(|b| b.class == class)
                })
                .map(|(&(_, e), _)| e)
                .collect();
            ends.sort_unstable_by(|a, b| b.cmp(a));
            ends.dedup();
            for end in ends {
                if let Some(slot) = buckets.get(&(0, end)) {
                    for be in slot.iter().filter(|b| b.class == class) {
                        // 预测匹配（尾前缀补全）覆盖全输入 → 恒全消费
                        let seg_len = if be.exact { consumed_parts(end) } else { 999 };
                        let kind = crate::CandidateKind::for_word(&be.entry.word);
                        let mut c = crate::Candidate::for_entry(&be.entry, kind, seg_len);
                        c.score = self.lm.log_prob(None, &be.entry.word, be.entry.weight);
                        cands.push(c);
                    }
                }
            }
        }

        // —— 整句通道：闸门 + Poet DP；句候选置最前（st.cc:598-601）。
        // classic 2b 守卫对齐：除末段外全为完整音节才组句（全简拼输入无句通道）——
        // 同时要求 Poet 词格只用纯全拼桶（class≤... 过滤含简拼的桶，垃圾组合不入围）——
        let rest_all_syllables = {
            let ne: Vec<&String> = seg.iter().filter(|s| !s.is_empty()).collect();
            ne.len() >= 2 && ne[..ne.len() - 1].iter().all(|s| self.is_syllable(s))
        };
        if rest_all_syllables {
            let mut wg_filtered: translator::Buckets = buckets
                .iter()
                .map(|(k, slot)| {
                    (*k, slot.iter().filter(|b| b.class != 1).cloned().collect::<Vec<_>>())
                })
                .filter(|(_, slot)| !slot.is_empty())
                .collect();
            if let Some(wg) = translator::build_poet_graph(
                &mut wg_filtered,
                graph.farthest,
                |w| self.lm.log_prob(None, "", w),
            ) {
                if let Some(sentence) = poet::make_sentence(
                    &wg,
                    graph.farthest,
                    ctx.preceding_text,
                    poet::Strategy::DynamicProgramming,
                ) {
                    cands.insert(0, translator::sentence_candidate(&sentence, seg.join("'")));
                }
            }
        }

        // 文本去重（DistinctTranslation：静默丢重、先见先留）
        let mut seen = std::collections::HashSet::new();
        cands.retain(|c| seen.insert(c.text.clone()));

        // 截断 + 原文兜底
        cands.truncate(self.max_candidates);
        if cands.is_empty() {
            return self.fallback_translation(pending, &seg);
        }

        Translation {
            segmentation: vec![Span { syllables: seg, tags: vec!["pinyin"] }],
            candidates: cands,
        }
    }

    /// 预编辑显示：与 classic 共用五规则（api::preview_rules），seg = 重排后贪心切分。
    fn preedit(
        &self,
        _ctx: &EngineCtx,
        pending: &PendingInput,
        selected: Option<&crate::Candidate>,
    ) -> String {
        let seg = self.ranked_seg(pending.raw);
        crate::api::preview_rules(
            pending.raw,
            &seg,
            &|s| self.is_syllable(s),
            &|s| self.schema.display(s),
            selected,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::ImeEngine;
    use std::sync::Arc as StdArc;

    fn engine(items: Vec<(&str, &str, u32)>) -> StdArc<RimeEngine> {
        let d = Dict::from_entries(
            items
                .into_iter()
                .map(|(c, w, wt)| (c.to_string(), w.to_string(), wt))
                .collect(),
        );
        RimeEngine::new(StdArc::new(d), &crate::Config::default())
    }

    fn texts(e: &RimeEngine, raw: &str) -> Vec<String> {
        e.translate(
            &EngineCtx { preceding_text: "" },
            &PendingInput { raw },
        )
        .candidates
        .iter()
        .map(|c| c.text.clone())
        .collect()
    }

    /// 词优先：全段精确词存在 → 无整句，最长码桶在前（st.cc 码长优先）。
    #[test]
    fn full_pinyin_word_first_no_sentence() {
        let e = engine(vec![
            ("ni'hao", "你好", 8000),
            ("ni", "你", 50000),
            ("hao", "好", 40000),
        ]);
        let t = texts(&e, "nihao");
        assert_eq!(t.first().map(String::as_str), Some("你好"));
        assert!(
            !t.contains(&"你号".to_string()),
            "无该词条不应出现：{t:?}"
        );
        // 无 Sentence（可靠精确词闸门）
        let tr = e.translate(&EngineCtx { preceding_text: "" }, &PendingInput { raw: "nihao" });
        assert!(tr.candidates.iter().all(|c| c.kind != crate::CandidateKind::Sentence));
    }

    /// 歧义音节：西安（xi'an 路径）与 先（xian 路径）同桶竞争，权重降序。
    #[test]
    fn ambiguous_syllable_buckets_compete() {
        let e = engine(vec![
            ("xian", "先", 500),
            ("xi'an", "西安", 6091),
            ("xi", "西", 100),
            ("an", "安", 100),
        ]);
        let t = texts(&e, "xian");
        assert_eq!(t.first().map(String::as_str), Some("西安"));
        assert!(t.contains(&"先".to_string()), "先应可达：{t:?}");
    }

    /// 混拼：简拼边展开 n* 音节 → n(i)+hao 命中 ni'hao。
    #[test]
    fn mixed_abbrev_hits_word() {
        let e = engine(vec![("ni'hao", "你好", 8000)]);
        let t = texts(&e, "nhao");
        assert_eq!(t.first().map(String::as_str), Some("你好"), "{t:?}");
    }

    /// 尾前缀补全：shigechengy → shi'ge'cheng'y 前缀命中 是一个成语。
    #[test]
    fn tail_completion_predictive_top() {
        let e = engine(vec![
            ("shi'ge'cheng'yu", "是一个成语", 3000),
            ("shi", "是", 90000),
            ("ge", "个", 50000),
            ("cheng", "成", 8000),
            ("yu", "与", 6000),
        ]);
        let t = texts(&e, "shigechengy");
        assert_eq!(t.first().map(String::as_str), Some("是一个成语"), "{t:?}");
    }

    /// M2 屏蔽跨核心生效。
    #[test]
    fn blocked_entry_filtered() {
        use iuv_data::UserDict;
        let d = Dict::from_entries(vec![
            ("xian".to_string(), "先".to_string(), 500),
            ("xi'an".to_string(), "西安".to_string(), 6091),
        ]);
        d.set_user(StdArc::new(UserDict::empty().block("xi'an", "西安")));
        let e = RimeEngine::new(StdArc::new(d), &crate::Config::default());
        let t = texts(&e, "xian");
        assert!(!t.contains(&"西安".to_string()), "屏蔽后不可见：{t:?}");
        assert!(t.contains(&"先".to_string()));
    }

    /// 预编辑快赢在 rime 核心下同样成立：jian 导航吉安 → ji'an。
    #[test]
    fn preedit_follows_candidate_rime() {
        let e = engine(vec![
            ("ji'an", "吉安", 6091),
            ("jian", "间", 5000),
        ]);
        let tr = e.translate(&EngineCtx { preceding_text: "" }, &PendingInput { raw: "jian" });
        let jian = tr.candidates.iter().find(|c| c.text == "吉安").expect("吉安应在候选中");
        assert_eq!(
            e.preedit(&EngineCtx { preceding_text: "" }, &PendingInput { raw: "jian" }, Some(jian)),
            "ji'an"
        );
    }

    /// 部分消费推进：nihao 选「你」（parts=1 < 贪心段数 2）→ seg_len=1。
    #[test]
    fn partial_consumption_seg_len() {
        let e = engine(vec![
            ("ni'hao", "你好", 8000),
            ("ni", "你", 50000),
            ("hao", "好", 40000),
        ]);
        let tr = e.translate(&EngineCtx { preceding_text: "" }, &PendingInput { raw: "nihao" });
        let ni = tr.candidates.iter().find(|c| c.text == "你").expect("你应在候选中");
        assert_eq!(ni.seg_len, 1, "单字候选消费 1 段");
        let nihao = tr.candidates.iter().find(|c| c.text == "你好").unwrap();
        assert_eq!(nihao.seg_len, 2);
    }
}

#[cfg(test)]
mod tests2 {
    use super::*;
    use crate::api::ImeEngine;
    use std::sync::Arc as StdArc;

    fn engine(items: Vec<(&str, &str, u32)>) -> StdArc<RimeEngine> {
        let d = Dict::from_entries(
            items
                .into_iter()
                .map(|(c, w, wt)| (c.to_string(), w.to_string(), wt))
                .collect(),
        );
        RimeEngine::new(StdArc::new(d), &crate::Config::default())
    }

    /// 简拼键（构建期首字母串）：nhmsx → 你还没睡醒。
    #[test]
    fn built_abbrev_keys_hit() {
        let e = engine(vec![
            ("ni'hai'mei'shui'xing", "你还没睡醒", 30),
            ("ni'hao", "你好", 8000),
        ]);
        let tr = e.translate(&EngineCtx { preceding_text: "" }, &PendingInput { raw: "nhmsx" });
        let texts: Vec<String> = tr.candidates.iter().map(|c| c.text.clone()).collect();
        assert_eq!(texts.first().map(String::as_str), Some("你还没睡醒"), "{texts:?}");
    }
}

#[cfg(test)]
mod tests3 {
    use super::*;
    use crate::api::ImeEngine;
    use std::sync::Arc as StdArc;

    fn engine(items: Vec<(&str, &str, u32)>) -> StdArc<RimeEngine> {
        let d = Dict::from_entries(
            items
                .into_iter()
                .map(|(c, w, wt)| (c.to_string(), w.to_string(), wt))
                .collect(),
        );
        RimeEngine::new(StdArc::new(d), &crate::Config::default())
    }

    /// 微软对齐政策：严格音节前缀（sh）→ 纯单字（exact 命中的 shi 键单字）。
    #[test]
    fn strict_prefix_chars_only() {
        let e = engine(vec![
            ("shi", "是", 90000),
            ("shi", "时", 800),
            ("shi'hou", "时候", 5000),
        ]);
        let tr = e.translate(&EngineCtx { preceding_text: "" }, &PendingInput { raw: "sh" });
        let texts: Vec<String> = tr.candidates.iter().map(|c| c.text.clone()).collect();
        assert!(texts.contains(&"是".to_string()), "{texts:?}");
        assert!(!texts.contains(&"时候".to_string()), "前缀档不出词：{texts:?}");
    }
}
