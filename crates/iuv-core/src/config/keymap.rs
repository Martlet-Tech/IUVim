//! 快捷键映射。默认：上翻页 = PageUp / `,` / ↑；下翻页 = PageDown / `.` / ↓；
//! 前一个候选项 = ←；后一个候选项 = →；数字 1-9 选中上屏（无映射）。
//! 键位语义全部由配置文件决定，与候选窗布局方向解耦（改布局不改键位）。

use crate::Key;

/// 快捷键映射表。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Keymap {
    /// 前页（上翻页）键
    pub page_prev: Vec<Key>,
    /// 后页（下翻页）键
    pub page_next: Vec<Key>,
    /// 前一个候选项键（页内 selected 左移/上移）
    pub candidate_prev: Vec<Key>,
    /// 后一个候选项键（页内 selected 右移/下移）
    pub candidate_next: Vec<Key>,
}

impl Default for Keymap {
    fn default() -> Self {
        Keymap {
            page_prev: vec![Key::PageUp, Key::Char(','), Key::Up],
            page_next: vec![Key::PageDown, Key::Char('.'), Key::Down],
            candidate_prev: vec![Key::Left],
            candidate_next: vec![Key::Right],
        }
    }
}

impl Keymap {
    /// 命中映射表则返回归一化键（PageUp/PageDown/Left/Right），否则返回 None。
    pub fn map(&self, key: Key) -> Option<Key> {
        if self.page_prev.contains(&key) {
            Some(Key::PageUp)
        } else if self.page_next.contains(&key) {
            Some(Key::PageDown)
        } else if self.candidate_prev.contains(&key) {
            Some(Key::Left)
        } else if self.candidate_next.contains(&key) {
            Some(Key::Right)
        } else {
            None
        }
    }
}

/// 应用快捷键映射：命中四组表则归一化（翻页/候选移动），否则原样返回。
pub fn apply_keymap(key: Key, keymap: &Keymap) -> Key {
    keymap.map(key).unwrap_or(key)
}

/// 会话外是否可用该键开启新会话（字母键——小写拼音 / Shift/CapsLock 大写保形进序列；
/// `,`/`.`/`'` 等标点放行给应用——`'` 只在会话内作强制分隔符，开场按 `'` 应直接上屏）。
pub fn is_session_start_key(key: Key) -> bool {
    matches!(key, Key::Char(c) | Key::ShiftChar(c) if c.is_ascii_alphabetic())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let k = Keymap::default();
        assert!(k.page_prev.contains(&Key::PageUp));
        assert!(k.page_prev.contains(&Key::Char(',')));
        assert!(k.page_prev.contains(&Key::Up));
        assert!(k.page_next.contains(&Key::PageDown));
        assert!(k.page_next.contains(&Key::Char('.')));
        assert!(k.page_next.contains(&Key::Down));
        assert!(k.candidate_prev.contains(&Key::Left));
        assert!(k.candidate_next.contains(&Key::Right));
    }

    #[test]
    fn page_mapping() {
        let k = Keymap::default();
        assert_eq!(k.map(Key::Char(',')), Some(Key::PageUp));
        assert_eq!(k.map(Key::Up), Some(Key::PageUp));
        assert_eq!(k.map(Key::PageUp), Some(Key::PageUp));
        assert_eq!(k.map(Key::Char('.')), Some(Key::PageDown));
        assert_eq!(k.map(Key::Down), Some(Key::PageDown));
        assert_eq!(k.map(Key::PageDown), Some(Key::PageDown));
        assert_eq!(k.map(Key::Char('a')), None);
        assert_eq!(k.map(Key::Digit(3)), None);
        assert_eq!(k.map(Key::Space), None);
    }

    #[test]
    fn candidate_mapping() {
        let k = Keymap::default();
        assert_eq!(k.map(Key::Left), Some(Key::Left));
        assert_eq!(k.map(Key::Right), Some(Key::Right));
        assert_eq!(k.map(Key::Up), Some(Key::PageUp));
        assert_eq!(k.map(Key::Down), Some(Key::PageDown));
        // 自定义候选键
        let c = Keymap {
            candidate_prev: vec![Key::Char('h')],
            candidate_next: vec![Key::Char('l')],
            ..Default::default()
        };
        assert_eq!(c.map(Key::Char('h')), Some(Key::Left));
        assert_eq!(c.map(Key::Char('l')), Some(Key::Right));
        assert_eq!(c.map(Key::Left), None);
    }

    #[test]
    fn custom_mapping() {
        let k = Keymap { page_prev: vec![Key::Char('[')], page_next: vec![Key::Char(']')], ..Default::default() };
        assert_eq!(k.map(Key::Char('[')), Some(Key::PageUp));
        assert_eq!(k.map(Key::Char(']')), Some(Key::PageDown));
        assert_eq!(k.map(Key::Char(',')), None);
    }

    #[test]
    fn apply_keymap_paging() {
        let k = Keymap::default();
        assert_eq!(apply_keymap(Key::Char(','), &k), Key::PageUp);
        assert_eq!(apply_keymap(Key::Char('.'), &k), Key::PageDown);
        assert_eq!(apply_keymap(Key::Up, &k), Key::PageUp);
        assert_eq!(apply_keymap(Key::Down, &k), Key::PageDown);
        assert_eq!(apply_keymap(Key::PageUp, &k), Key::PageUp);
        assert_eq!(apply_keymap(Key::Left, &k), Key::Left);
        assert_eq!(apply_keymap(Key::Right, &k), Key::Right);
        // 未命中：原样
        assert_eq!(apply_keymap(Key::Char('a'), &k), Key::Char('a'));
        assert_eq!(apply_keymap(Key::Space, &k), Key::Space);
        assert_eq!(apply_keymap(Key::Digit(3), &k), Key::Digit(3));
    }

    #[test]
    fn session_start_keys() {
        assert!(is_session_start_key(Key::Char('a')));
        assert!(is_session_start_key(Key::ShiftChar('H')), "Shift/CapsLock 大写同样开会话（Hello 首字母进序列）");
        // 标点/数字/控制键不得开启会话（放行给应用；`'` 仅会话内作强制分隔符）
        assert!(!is_session_start_key(Key::Char('\'')));
        assert!(!is_session_start_key(Key::Char(',')));
        assert!(!is_session_start_key(Key::Char('.')));
        assert!(!is_session_start_key(Key::Digit(1)));
        assert!(!is_session_start_key(Key::Space));
    }
}
