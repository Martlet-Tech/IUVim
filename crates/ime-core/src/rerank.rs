//! 排序管线。契约 01-contract.md §4 rerank.rs。
//! 【Agent B】W1 实现。W0 仅空壳。

use crate::{Candidate, Config, UserDataStore};
use std::time::SystemTime;

/// 排序上下文。
pub struct RerankCtx<'a> {
    pub raw: &'a str,
    pub seg: &'a [String],
    pub store: &'a dyn UserDataStore,
    pub config: &'a Config,
    pub now: SystemTime,
}

/// 排序阶段。M2 的滞回/钉选实现为新增 Stage。
pub trait RerankStage: Send + Sync {
    fn rerank(&self, ctx: &RerankCtx, cands: &mut Vec<Candidate>);
}

/// 静态序：候选生成顺序即展示顺序（no-op）。
pub struct StaticOrder;

impl RerankStage for StaticOrder {
    fn rerank(&self, _ctx: &RerankCtx, _cands: &mut Vec<Candidate>) {}
}
