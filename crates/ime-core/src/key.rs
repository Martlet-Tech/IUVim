//! 按键 / 会话结果 / UI 快照类型。W0 完整实现，冻结。

use crate::Candidate;

/// 归一化按键。TSF/REPL 映射为它再喂给 Session。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Backspace,
    Space,
    Enter,
    Esc,
    Digit(u8),
    PageUp,
    PageDown,
    Up,
    Down,
}

/// 翻页信息。
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PageInfo {
    pub page: usize,
    pub page_count: usize,
    pub page_size: usize,
    pub total: usize,
}

/// 会话结束方式。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SessionEnd {
    /// 上屏文本
    Commit(String),
    /// 取消，不上屏
    Cancel,
}

/// 一次按键后的完整 UI 快照 + 副作用。TSF/REPL 只消费它，不读引擎内部。
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Effect {
    /// 内嵌预编辑文本：首选候选文本；无候选时为原始拼音 raw
    pub composition: String,
    /// 切分显示，如 "ni'hao"（以 ' 连接各音节）
    pub reading: String,
    /// 当前页候选（页内索引 0 起）
    pub candidates: Vec<Candidate>,
    /// 页内高亮索引
    pub selected: usize,
    pub page: PageInfo,
    /// Some → 会话结束（Commit 上屏 / Cancel 取消）
    pub end: Option<SessionEnd>,
}
