//! 按键 / 会话结果 / UI 快照类型。W0 完整实现，冻结。

use crate::Candidate;

/// 归一化按键。TSF/REPL 映射为它再喂给 Session。
///
/// 序列化格式（config.json）：`"PageUp"` / `"Up"` / `","` / `"3"` 等字符串。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    /// Shift/CapsLock 字母（大写；保形进序列——匹配只认小写、commit 原样上屏）。
    /// 仅 TSF 产生，不参与 config 序列化（from_name 不可达）。
    ShiftChar(char),
    Backspace,
    Space,
    Enter,
    Esc,
    Digit(u8),
    Tab,
    Delete,
    Home,
    End,
    Insert,
    PageUp,
    PageDown,
    Up,
    Down,
    Left,
    Right,
    F1,
    F2,
    F3,
    F4,
    F5,
    F6,
    F7,
    F8,
    F9,
    F10,
    F11,
    F12,
    /// 主动调权（M2，18-m2-user-dict.md）：与左侧/右侧**相邻候选**交换权重。
    /// 仅 TSF 产生（Alt+←/→），不参与 config 序列化（from_name 不可达，同 ShiftChar 先例）。
    SwapLeft,
    SwapRight,
    /// 隐藏候选（M2 二期）：Shift+Delete——先删用户库条目（自造词/覆盖），
    /// 否则屏蔽基础库词条。仅 TSF 产生，不参与 config 序列化（同 ShiftChar 先例）。
    HideCandidate,
}

impl Key {
    /// 展示名（config.json / 日志用）：`Char(',')` → `","`，`Digit(3)` → `"3"`。
    pub fn name(&self) -> String {
        match self {
            Key::Char(c) => c.to_string(),
            Key::ShiftChar(c) => c.to_string(),
            Key::Backspace => "Backspace".into(),
            Key::Space => "Space".into(),
            Key::Enter => "Enter".into(),
            Key::Esc => "Esc".into(),
            Key::Digit(n) => n.to_string(),
            Key::Tab => "Tab".into(),
            Key::Delete => "Delete".into(),
            Key::Home => "Home".into(),
            Key::End => "End".into(),
            Key::Insert => "Insert".into(),
            Key::PageUp => "PageUp".into(),
            Key::PageDown => "PageDown".into(),
            Key::Up => "Up".into(),
            Key::Down => "Down".into(),
            Key::Left => "Left".into(),
            Key::Right => "Right".into(),
            Key::F1 => "F1".into(),
            Key::F2 => "F2".into(),
            Key::F3 => "F3".into(),
            Key::F4 => "F4".into(),
            Key::F5 => "F5".into(),
            Key::F6 => "F6".into(),
            Key::F7 => "F7".into(),
            Key::F8 => "F8".into(),
            Key::F9 => "F9".into(),
            Key::F10 => "F10".into(),
            Key::F11 => "F11".into(),
            Key::F12 => "F12".into(),
            Key::SwapLeft => "SwapLeft".into(),
            Key::SwapRight => "SwapRight".into(),
            Key::HideCandidate => "HideCandidate".into(),
        }
    }

    /// 从字符串解析（config.json）：`","` → Char(',')，`"3"` → Digit(3)，`"PageUp"` → PageUp。
    pub fn from_name(s: &str) -> Option<Key> {
        match s {
            "Backspace" => Some(Key::Backspace),
            "Space" => Some(Key::Space),
            "Enter" => Some(Key::Enter),
            "Esc" => Some(Key::Esc),
            "PageUp" => Some(Key::PageUp),
            "PageDown" => Some(Key::PageDown),
            "Up" => Some(Key::Up),
            "Down" => Some(Key::Down),
            "Left" => Some(Key::Left),
            "Right" => Some(Key::Right),
            "Tab" => Some(Key::Tab),
            "Delete" => Some(Key::Delete),
            "Home" => Some(Key::Home),
            "End" => Some(Key::End),
            "Insert" => Some(Key::Insert),
            "F1" => Some(Key::F1),
            "F2" => Some(Key::F2),
            "F3" => Some(Key::F3),
            "F4" => Some(Key::F4),
            "F5" => Some(Key::F5),
            "F6" => Some(Key::F6),
            "F7" => Some(Key::F7),
            "F8" => Some(Key::F8),
            "F9" => Some(Key::F9),
            "F10" => Some(Key::F10),
            "F11" => Some(Key::F11),
            "F12" => Some(Key::F12),
            s if s.chars().count() == 1 => {
                let c = s.chars().next().unwrap();
                if c.is_ascii_digit() {
                    Some(Key::Digit(c as u8 - b'0'))
                } else {
                    Some(Key::Char(c))
                }
            }
            _ => None,
        }
    }
}

/// 翻页信息。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageInfo {
    pub page: usize,
    pub page_count: usize,
    pub page_size: usize,
    pub total: usize,
}

/// 会话结束方式。
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionEnd {
    /// 上屏文本
    Commit(String),
    /// 取消，不上屏
    Cancel,
}

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
