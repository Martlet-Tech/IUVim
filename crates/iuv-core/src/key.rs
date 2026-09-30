//! UI 快照类型（③-2 镜像归一后仅剩 `Effect`——Key/PageInfo/SessionEnd 已沉底
//! iuv-data，经本模块 re-export 维持 `crate::key::` 路径）。

pub use iuv_data::{Key, PageInfo, SessionEnd};

use crate::Candidate;

/// 一次按键后的完整 UI 快照 + 副作用。TSF/REPL 只消费它，不读引擎内部。
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Effect {
    /// 内嵌预编辑文本：拼音分段（如 "ce'shi"，保留用户按下的强制分隔符 `'`，
    /// 与 reading 同值）——微软式：拼音留在预编辑，候选窗只放候选；
    /// commit 时由 end.text 替换上屏
    pub composition: String,
    /// 切分显示，如 "ni'hao"（保留用户 `'`）
    pub reading: String,
    /// 当前页候选（页内索引 0 起）
    pub candidates: Vec<Candidate>,
    /// 全量候选（所有页，按页内序）。TSF 候选 UI 元素（WoW 游戏内候选栏）数据源：
    /// 桥按全量构造 IMM CANDIDATELIST，游戏翻页从全量切片——当前页候选不够翻页
    /// （2026-08-16 实测：翻页后游戏内候选栏消失，回第 0 页恢复）。
    pub all_candidates: Vec<Candidate>,
    /// 页内高亮索引
    pub selected: usize,
    pub page: PageInfo,
    /// Some → 会话结束（Commit 上屏 / Cancel 取消）
    pub end: Option<SessionEnd>,
}
