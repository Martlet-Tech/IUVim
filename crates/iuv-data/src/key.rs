//! 归一化按键 / 翻页信息 / 会话结束（③-2 镜像归一：全仓唯一定义，iuv-core/iuv-proto
//! 双方 re-export）。

use serde::{Deserialize, Serialize};

/// 归一化按键。TSF/REPL 映射为它再喂给 Session。
///
/// 序列化格式（config.json）：`"PageUp"` / `"Up"` / `","` / `"3"` 等字符串
/// （`name()`/`from_name()`）；线上为 serde 派生（postcard 变体序号）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

/// 翻页信息。定宽 u32——线格式不随平台位宽漂移（49 §4.3 纪律）。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageInfo {
    pub page: u32,
    pub page_count: u32,
    pub page_size: u32,
    pub total: u32,
}

/// 会话结束方式。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SessionEnd {
    /// 上屏文本
    Commit(String),
    /// 取消，不上屏
    Cancel,
}
