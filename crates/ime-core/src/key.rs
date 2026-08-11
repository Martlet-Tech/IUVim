//! 按键 / 会话结果 / UI 快照类型。W0 完整实现，冻结。

use crate::Candidate;

/// 归一化按键。TSF/REPL 映射为它再喂给 Session。
///
/// 序列化格式（config.json）：`"PageUp"` / `"Up"` / `","` / `"3"` 等字符串。
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

impl Key {
    /// 展示名（config.json / 日志用）：`Char(',')` → `","`，`Digit(3)` → `"3"`。
    pub fn name(&self) -> String {
        match self {
            Key::Char(c) => c.to_string(),
            Key::Backspace => "Backspace".into(),
            Key::Space => "Space".into(),
            Key::Enter => "Enter".into(),
            Key::Esc => "Esc".into(),
            Key::Digit(n) => n.to_string(),
            Key::PageUp => "PageUp".into(),
            Key::PageDown => "PageDown".into(),
            Key::Up => "Up".into(),
            Key::Down => "Down".into(),
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

impl serde::Serialize for Key {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.name())
    }
}

impl<'de> serde::Deserialize<'de> for Key {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(d)?;
        Key::from_name(&raw).ok_or_else(|| serde::de::Error::custom(format!("未知按键：{raw}")))
    }
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
    /// 内嵌预编辑文本：拼音分段（如 "ce'shi"，保留用户按下的强制分隔符 `'`，
    /// 与 reading 同值）——微软式：拼音留在预编辑，候选窗只放候选；
    /// commit 时由 end.text 替换上屏
    pub composition: String,
    /// 切分显示，如 "ni'hao"（保留用户 `'`）
    pub reading: String,
    /// 当前页候选（页内索引 0 起）
    pub candidates: Vec<Candidate>,
    /// 页内高亮索引
    pub selected: usize,
    pub page: PageInfo,
    /// Some → 会话结束（Commit 上屏 / Cancel 取消）
    pub end: Option<SessionEnd>,
    /// 续接选词的部分上屏词（M1 后期契约演进）：选中中间级词时上屏该词、
    /// 尾巴留预编辑继续；TSF 收到后 commit 该词并重建 composition。None = 无。
    pub part_commit: Option<String>,
}
