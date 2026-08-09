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

    /// 按契约 §4.2 生成候选。`squashed` 需与 seg 一致。
    pub(crate) fn generate_candidates(&self, raw: &str, seg: &[String]) -> Vec<crate::Candidate> {
        let squashed: String = seg.concat();
        let mut cands: Vec<crate::Candidate> = Vec::new();

        // 1. unigram Viterbi 最优路径（seg.len() >= 2）
        if let Some(sentence) = crate::viterbi::best_sentence(&self.dict, seg, &*self.lm, &self.config)
        {
            cands.push(sentence);
        }

        // 2. exact 查询，前 50
        for e in self.dict.exact(&squashed).iter().take(50) {
            let kind = if e.word.chars().count() >= 2 {
                crate::CandidateKind::Word
            } else {
                crate::CandidateKind::Char
            };
            cands.push(crate::Candidate::new(e.word.clone(), kind, e.code.clone(), e.weight));
        }

        // 3. 前缀补全，20 条
        for e in self.dict.prefix(&squashed, 20) {
            let kind = if e.word.chars().count() >= 2 {
                crate::CandidateKind::Word
            } else {
                crate::CandidateKind::Char
            };
            cands.push(crate::Candidate::new(e.word.clone(), kind, e.code.clone(), e.weight));
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
