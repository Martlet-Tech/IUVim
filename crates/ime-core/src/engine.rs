//! 引擎：候选生成。契约 01-contract.md §4 engine.rs / §4.2 算法。

use crate::{
    rerank::RerankCtx, schema::Quanpin, session::Session, store::NullStore, Config, InputSchema,
    LmProvider, RerankStage, UnigramLm, UserDataStore,
};
use ime_data::{Dict, Entry};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// 引擎：进程级单例，跨线程共享。
pub struct Engine {
    pub(crate) dict: Dict,
    pub(crate) config: Config,
    pub(crate) schema: Box<dyn InputSchema>,
    pub(crate) lm: Box<dyn LmProvider>,
    pub(crate) stages: Vec<Box<dyn RerankStage>>,
    pub(crate) store: Mutex<Box<dyn UserDataStore>>,
}

impl Engine {
    /// 默认装配：Quanpin + UnigramLm + [StaticOrder] + NullStore。
    pub fn new(dict: Dict, config: Config) -> Arc<Engine> {
        let syllables = dict.syllables().clone();
        let lm = UnigramLm::new(dict.total_weight(), dict.entry_count());
        Self::with_parts(
            dict,
            config,
            Box::new(Quanpin::new(syllables)),
            Box::new(lm),
            vec![Box::new(crate::rerank::StaticOrder)],
            Box::new(NullStore),
        )
    }

    /// 全注入构造器（测试与后续里程碑用）。
    pub fn with_parts(
        dict: Dict,
        config: Config,
        schema: Box<dyn InputSchema>,
        lm: Box<dyn LmProvider>,
        stages: Vec<Box<dyn RerankStage>>,
        store: Box<dyn UserDataStore>,
    ) -> Arc<Engine> {
        Arc::new(Engine { dict, config, schema, lm, stages, store: Mutex::new(store) })
    }

    pub fn start_session(self: &Arc<Self>) -> Session {
        Session::new(self.clone())
    }

    pub fn config(&self) -> &Config {
        &self.config
    }

    /// 调试/REPL 用精确查询。
    pub fn lookup(&self, squashed_code: &str) -> &[Entry] {
        self.dict.exact(squashed_code)
    }

    /// 按契约 §4.2 生成候选。
    ///
    /// 路由（M1.5，微软实测对齐，见 docs/research/msime-probe-checklist.txt）：
    /// - 整串为音节前缀（`c`/`sh`/`zho`）→ 纯单字（首字母桶，词频序）
    /// - 完整单音节：无歧义（`shi`）→ 纯单字；歧义（`xian`→[xi,an]）→ 全拼 k-loop（替代切分词混排）
    /// - 单段非前缀（`i`/`u`/`v`）→ 无候选
    /// - 多段全完整（`nihao`/`xi'an`）→ 现有全拼 k-loop（viterbi 组句 + 逐级枚举）
    /// - 多段全不完整（`nh`/`nhm`/`nhmsx`）→ 简拼键逐级砍尾巴（构建期键，O(1) exact）
    /// - 多段混合（`nhao`）→ 不完整段展开音节配对查询（上限内，超限降级）
    ///
    /// 部分消费：候选 seg_len=k，选中间级词经 session 悬空续接把尾巴重建为组合。
    pub(crate) fn generate_candidates(&self, raw: &str, seg: &[String]) -> Vec<crate::Candidate> {
        let mut cands = {
            // 整串（去强制分隔符）判定：是否是某音节的前缀
            let plain: String = raw.chars().filter(|c| *c != '\'').collect();
            if !plain.is_empty() && self.is_syllable_prefix(&plain) && !self.is_syllable(&plain) {
                // 严格前缀（c/sh/zho…）：切分器可能把它切成多段，但按微软实测只出单字
                self.single_segment_candidates(&plain)
            } else if seg.len() == 1 {
                // 完整单音节 或 单段非前缀
                let plans = self.schema.segment(raw);
                if self.is_syllable(&seg[0]) {
                    if plans.len() > 1 {
                        // 歧义单音节（xian 类）：替代切分（xi,an）的词也要混排
                        self.full_pinyin_candidates(seg)
                    } else {
                        self.single_segment_candidates(&seg[0])
                    }
                } else {
                    Vec::new() // i/u/v 等非前缀：无候选
                }
            } else {
                // 多段：按段完整性分派
                let kinds: Vec<bool> =
                    seg.iter().map(|s| !s.is_empty() && self.is_syllable(s)).collect();
                if kinds.iter().all(|&c| c) {
                    self.full_pinyin_candidates(seg)
                } else if kinds.iter().all(|&c| !c) {
                    self.abbrev_candidates(seg)
                } else {
                    self.mixed_candidates(seg)
                }
            }
        };

        // 前缀补全（联想）：默认关闭（微软化，候选仅 exact）；config 开启时追加。
        //    用方案[0] 的 `'` 键做前缀匹配（词库键已分隔化）。
        //    联想词消费全部当前段（seg_len = n），选中即整词上屏。
        if self.config.candidate_prefix {
            let n = seg.len();
            let squashed = seg.join("'");
            for e in self.dict.prefix(&squashed, 20) {
                let kind = if e.word.chars().count() >= 2 {
                    crate::CandidateKind::Word
                } else {
                    crate::CandidateKind::Char
                };
                cands.push(crate::Candidate::new(
                    e.word.clone(),
                    kind,
                    e.code.clone(),
                    e.weight,
                    n,
                ));
            }
        }

        // 按 text 去重（保序，先见先留）
        let mut seen = std::collections::HashSet::new();
        cands.retain(|c| seen.insert(c.text.clone()));

        // 截断到 max_candidates
        cands.truncate(self.config.max_candidates);

        // 依次过 stages 管线
        let now = SystemTime::now();
        let store = self.store.lock().expect("store lock poisoned");
        let ctx = RerankCtx { raw, seg, store: store.as_ref(), config: &self.config, now };
        for stage in &self.stages {
            stage.rerank(&ctx, &mut cands);
        }
        cands
    }

    fn is_syllable(&self, s: &str) -> bool {
        self.dict.syllables().contains(s)
    }

    fn is_syllable_prefix(&self, s: &str) -> bool {
        self.dict.syllables().iter().any(|syl| syl.starts_with(s))
    }

    /// 单段档：完整音节或音节前缀 → 纯单字（词频序，首字母桶过滤）；否则空。
    /// 微软实测：单段输入无论完整与否只出单字（shi→是时十使，无"时间/时候"）；
    /// i/u/v 等非前缀无候选。
    fn single_segment_candidates(&self, s: &str) -> Vec<crate::Candidate> {
        const PER_LEVEL_EXACT: usize = 20;
        let mut cands = Vec::new();
        if s.is_empty() {
            return cands;
        }
        if !self.is_syllable(s) && !self.is_syllable_prefix(s) {
            return cands;
        }
        let first = s.chars().next().unwrap();
        let mut pushed = 0usize;
        for e in self.dict.initial_top(first, ime_data::INITIAL_BUCKET_SIZE) {
            if e.code.starts_with(s) && e.word.chars().count() == 1 {
                cands.push(crate::Candidate::new(
                    e.word.clone(),
                    crate::CandidateKind::Char,
                    e.code.clone(),
                    e.weight,
                    1,
                ));
                pushed += 1;
                if pushed >= PER_LEVEL_EXACT {
                    break;
                }
            }
        }
        cands
    }

    /// 简拼键档：多段全不完整（`nh`/`nhm`/`nhmsx`）→ 构建期简拼键逐级砍尾巴。
    /// 每级 k：exact(前 k 段首字母串)；尾巴段由 session 悬空续接重建为组合。
    /// 微软实测：简拼只出词（纯 exact 匹配，无单字、无更长词前缀）。
    fn abbrev_candidates(&self, seg: &[String]) -> Vec<crate::Candidate> {
        const PER_LEVEL_EXACT: usize = 20;
        let n = seg.len();
        let mut cands = Vec::new();
        for k in (1..=n).rev() {
            let key: String =
                seg[..k].iter().filter(|s| !s.is_empty()).map(|s| s.as_str()).collect();
            if key.is_empty() {
                continue;
            }
            let mut pushed = 0usize;
            for e in self.dict.exact(&key) {
                let kind = if e.word.chars().count() >= 2 {
                    crate::CandidateKind::Word
                } else {
                    crate::CandidateKind::Char
                };
                cands.push(crate::Candidate::new(
                    e.word.clone(),
                    kind,
                    e.code.clone(),
                    e.weight,
                    k,
                ));
                pushed += 1;
                if pushed >= PER_LEVEL_EXACT {
                    break;
                }
            }
        }
        cands
    }

    /// 混拼档：多段混合（`nhao` → n 简拼 + hao 完整）→ 不完整段展开为音节列表，
    /// 逐级笛卡尔积 exact 查询（词频合并）；组合数超限该级降级为空。
    fn mixed_candidates(&self, seg: &[String]) -> Vec<crate::Candidate> {
        const PER_LEVEL_EXACT: usize = 20;
        const MAX_EXPAND_QUERIES: usize = 2000;
        let n = seg.len();
        let mut cands = Vec::new();
        for k in (1..=n).rev() {
            // 展开前 k 段：完整段→自身；不完整段→音节前缀列表
            let mut lists: Vec<Vec<&str>> = Vec::new();
            let mut product: usize = 1;
            let mut ok = true;
            for s in &seg[..k] {
                if s.is_empty() {
                    ok = false;
                    break;
                }
                if self.is_syllable(s) {
                    lists.push(vec![s.as_str()]);
                } else {
                    let l: Vec<&str> = self
                        .dict
                        .syllables()
                        .iter()
                        .filter(|syl| syl.starts_with(s))
                        .map(|x| x.as_str())
                        .collect();
                    if l.is_empty() {
                        ok = false;
                        break;
                    }
                    product *= l.len();
                    if product > MAX_EXPAND_QUERIES {
                        ok = false;
                        break;
                    }
                    lists.push(l);
                }
            }
            if !ok {
                continue; // 该级降级为空
            }
            // 笛卡尔积 → exact 查询 → 词频合并
            let mut combos: Vec<Vec<&str>> = vec![Vec::new()];
            for l in &lists {
                let mut next = Vec::with_capacity(combos.len() * l.len());
                for c in &combos {
                    for syl in l {
                        let mut cc = c.clone();
                        cc.push(syl);
                        next.push(cc);
                    }
                }
                combos = next;
            }
            let mut entries: Vec<&ime_data::Entry> = Vec::new();
            for combo in &combos {
                entries.extend(self.dict.exact(&combo.join("'")).iter());
            }
            entries.sort_by(|a, b| b.weight.cmp(&a.weight).then(a.word.cmp(&b.word)));
            let mut seen = std::collections::HashSet::new();
            let mut pushed = 0usize;
            for e in entries {
                if !seen.insert(e.word.as_str()) {
                    continue;
                }
                let kind = if e.word.chars().count() >= 2 {
                    crate::CandidateKind::Word
                } else {
                    crate::CandidateKind::Char
                };
                cands.push(crate::Candidate::new(
                    e.word.clone(),
                    kind,
                    e.code.clone(),
                    e.weight,
                    k,
                ));
                pushed += 1;
                if pushed >= PER_LEVEL_EXACT {
                    break;
                }
            }
        }
        cands
    }

    /// 现有全拼路径（含歧义单音节）：砍尾巴逐级匹配。
    /// for k = n..1，对前缀 `seg[0..k]` 跑 viterbi（每级 0 或 1 句）
    /// 加前缀枚举切分查 exact（词/单字）；候选按前缀长度从长到短排列，
    /// 同 k 内 Sentence 在前、词按权重降序。viterbi.rs 算法零改动。
    fn full_pinyin_candidates(&self, seg: &[String]) -> Vec<crate::Candidate> {
        // 每级词候选上限（"2/3 字词时几个/十几个候选词"的规模；全局另有 max_candidates 截断）。
        const PER_LEVEL_EXACT: usize = 20;

        let mut cands: Vec<crate::Candidate> = Vec::new();
        let n = seg.len();

        for k in (1..=n).rev() {
            let prefix = &seg[..k];

            // 1. 每级一句整句（k >= 2）。空段（尾/连续 `'`）过滤后组句，防兜底空词。
            if k >= 2 {
                let vseg: Vec<String> = prefix.iter().filter(|s| !s.is_empty()).cloned().collect();
                if vseg.len() >= 2 {
                    if let Some(sentence) =
                        crate::viterbi::best_sentence(&self.dict, &vseg, &*self.lm, &self.config)
                    {
                        cands.push(sentence);
                    }
                }
            }

            // 2. 前缀枚举切分 → exact 词/单字（join 键；前缀含 `'` 时强制切分，
            //    无 `'` 时枚举变体如 [xian] → [xian]+[xi,an]）。seg_len = k。
            let mut entries: Vec<&ime_data::Entry> = Vec::new();
            for plan in self.schema.segment(&prefix.join("'")) {
                let key = plan.join("'");
                entries.extend(self.dict.exact(&key).iter());
            }
            entries.sort_by(|a, b| b.weight.cmp(&a.weight).then(a.word.cmp(&b.word)));
            let mut seen = std::collections::HashSet::new();
            let mut pushed = 0usize;
            for e in entries {
                if !seen.insert(e.word.as_str()) {
                    continue;
                }
                let kind = if e.word.chars().count() >= 2 {
                    crate::CandidateKind::Word
                } else {
                    crate::CandidateKind::Char
                };
                cands.push(crate::Candidate::new(
                    e.word.clone(),
                    kind,
                    e.code.clone(),
                    e.weight,
                    k,
                ));
                pushed += 1;
                if pushed >= PER_LEVEL_EXACT {
                    break;
                }
            }
        }

        cands
    }

    /// 用户选择记录（commit 时调用）。
    pub(crate) fn record_selection(&self, code: &str, text: &str) {
        let mut store = self.store.lock().expect("store lock poisoned");
        store.record_selection(code, text, SystemTime::now());
    }
}
