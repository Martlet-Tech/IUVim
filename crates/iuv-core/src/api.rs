//! 顶层引擎接口（39-rime-pipeline.md §4）：唯一签名面。
//!
//! 本质两份输入输出：
//! ① 待输入串 → 分段视图 + 候选列表（[`ImeEngine::translate`]）；
//! ② 高亮候选 → 预编辑显示串（[`ImeEngine::preedit`]，如输入 `jian` 导航到
//!    「吉安」时返回 `ji'an`）。
//!
//! classic 与 rime 两个核心都实现此 trait；会话层只认它，不感知核心差异。
//! `EngineCtx::preceding_text` 为 Step 3 预埋钩子：classic 忽略，rime 核心喂给
//! 组句打分（poet 的 preceding_text 机制）。

/// 一次 translate/preedit 的上下文。
pub struct EngineCtx<'a> {
    /// 已确认前文（悬空选词拼接的汉字）。classic 忽略；rime 组句上下文用。
    pub preceding_text: &'a str,
}

/// 待输入串：用户敲的原始字母串（可能含用户强制撇号 `'`）。
pub struct PendingInput<'a> {
    pub raw: &'a str,
}

/// 分段视图的一段：音节序列 + 标签。
///
/// `syllables` 保留空段（尾撇号 display 语义，与既有 seg 一致）；
/// `tags` 决定哪些翻译器参与该段（rime 核心的 tag 机制；classic 恒 `["pinyin"]`）。
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub syllables: Vec<String>,
    pub tags: Vec<&'static str>,
}

/// translate 输出：分段视图 + 活动段候选列表。
///
/// classic 阶段 `segmentation` 恒为整串一段（现有行为零变化）；rime 核心落地后
/// 才出现真正的多段视图（Step 3 会话层开始消费）。
#[derive(Clone, Debug, PartialEq)]
pub struct Translation {
    pub segmentation: Vec<Span>,
    pub candidates: Vec<Candidate>,
}

/// 顶层引擎接口。实现须线程安全（进程级单例跨线程共享）。
pub trait ImeEngine: Send + Sync {
    /// 输入方向①：待输入串 → 分段视图 + 候选列表。
    fn translate(&self, ctx: &EngineCtx, pending: &PendingInput) -> Translation;

    /// 输入方向②：当前高亮候选 → 该候选视角下的预编辑显示串（只含未消费尾巴，
    /// 已确认前文由会话层拼接）。`selected = None` 时返回默认切分显示。
    fn preedit(
        &self,
        ctx: &EngineCtx,
        pending: &PendingInput,
        selected: Option<&Candidate>,
    ) -> String;
}

use crate::Candidate;
