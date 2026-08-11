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
    /// 砍尾巴逐级匹配：for k = n..1，对前缀 `seg[0..k]` 跑 viterbi（每级 0 或 1 句）
    /// + 前缀枚举切分查 exact（词/单字）；候选按前缀长度**从长到短**排列，
    /// 同 k 内 Sentence 在前、词按权重降序。viterbi.rs 算法零改动。
    pub(crate) fn generate_candidates(&self, raw: &str, seg: &[String]) -> Vec<crate::Candidate> {
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

        // 3. 前缀补全（联想）：默认关闭（微软化，候选仅 exact）；config 开启时追加。
        //    用方案[0] 的 `'` 键做前缀匹配（词库键已分隔化）。
        //    联想词消费全部当前段（seg_len = n），选中即整词上屏。
        if self.config.candidate_prefix {
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

        // 4. 按 text 去重（保序，先见先留）
        let mut seen = std::collections::HashSet::new();
        cands.retain(|c| seen.insert(c.text.clone()));

        // 5. 截断到 max_candidates
        cands.truncate(self.config.max_candidates);

        // 6. 依次过 stages 管线
        let now = SystemTime::now();
        let store = self.store.lock().expect("store lock poisoned");
        let ctx = RerankCtx { raw, seg, store: store.as_ref(), config: &self.config, now };
        for stage in &self.stages {
            stage.rerank(&ctx, &mut cands);
        }
        cands
    }

    /// 用户选择记录（commit 时调用）。
    pub(crate) fn record_selection(&self, code: &str, text: &str) {
        let mut store = self.store.lock().expect("store lock poisoned");
        store.record_selection(code, text, SystemTime::now());
    }
}
