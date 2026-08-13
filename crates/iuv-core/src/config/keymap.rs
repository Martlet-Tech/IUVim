//! 快捷键映射。默认（搜狗/微软同款）：
//! 上翻页 = PageUp / `,` / ↑；下翻页 = PageDown / `.` / ↓；数字 1-9 选中上屏（无映射）。

use crate::Key;

/// 快捷键映射表。
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct Keymap {
    /// 上翻页键
    pub page_prev: Vec<Key>,
    /// 下翻页键
    pub page_next: Vec<Key>,
}

impl Default for Keymap {
    fn default() -> Self {
        Keymap {
            page_prev: vec![Key::PageUp, Key::Char(','), Key::Up],
            page_next: vec![Key::PageDown, Key::Char('.'), Key::Down],
        }
    }
}

impl Keymap {
    /// 命中翻页表则返回重映射后的键（PageUp/PageDown），否则返回 None。
    pub fn page(&self, key: Key) -> Option<Key> {
        if self.page_prev.contains(&key) {
            Some(Key::PageUp)
        } else if self.page_next.contains(&key) {
            Some(Key::PageDown)
        } else {
            None
        }
    }
}

/// 应用快捷键映射：命中翻页表则重映射为 PageUp/PageDown，否则原样返回。
pub fn apply_keymap(key: Key, keymap: &Keymap) -> Key {
    keymap.page(key).unwrap_or(key)
}

/// 会话外是否可用该键开启新会话（仅字母与 `'`；`,`/`.` 等标点放行给应用）。
pub fn is_session_start_key(key: Key) -> bool {
    matches!(key, Key::Char(c) if c.is_ascii_lowercase() || c == '\'')
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
    }

    #[test]
    fn page_mapping() {
        let k = Keymap::default();
        assert_eq!(k.page(Key::Char(',')), Some(Key::PageUp));
        assert_eq!(k.page(Key::Up), Some(Key::PageUp));
        assert_eq!(k.page(Key::PageUp), Some(Key::PageUp));
        assert_eq!(k.page(Key::Char('.')), Some(Key::PageDown));
        assert_eq!(k.page(Key::Down), Some(Key::PageDown));
        assert_eq!(k.page(Key::PageDown), Some(Key::PageDown));
        assert_eq!(k.page(Key::Char('a')), None);
        assert_eq!(k.page(Key::Digit(3)), None);
        assert_eq!(k.page(Key::Space), None);
    }

    #[test]
    fn custom_mapping() {
        let k = Keymap { page_prev: vec![Key::Char('[')], page_next: vec![Key::Char(']')] };
        assert_eq!(k.page(Key::Char('[')), Some(Key::PageUp));
        assert_eq!(k.page(Key::Char(']')), Some(Key::PageDown));
        assert_eq!(k.page(Key::Char(',')), None);
    }

    #[test]
    fn apply_keymap_paging() {
        let k = Keymap::default();
        assert_eq!(apply_keymap(Key::Char(','), &k), Key::PageUp);
        assert_eq!(apply_keymap(Key::Char('.'), &k), Key::PageDown);
        assert_eq!(apply_keymap(Key::Up, &k), Key::PageUp);
        assert_eq!(apply_keymap(Key::Down, &k), Key::PageDown);
        assert_eq!(apply_keymap(Key::PageUp, &k), Key::PageUp);
        // 未命中：原样
        assert_eq!(apply_keymap(Key::Char('a'), &k), Key::Char('a'));
        assert_eq!(apply_keymap(Key::Space, &k), Key::Space);
        assert_eq!(apply_keymap(Key::Digit(3), &k), Key::Digit(3));
    }

    #[test]
    fn session_start_keys() {
        assert!(is_session_start_key(Key::Char('a')));
        assert!(is_session_start_key(Key::Char('\'')));
        // 标点/数字/控制键不得开启会话（放行给应用）
        assert!(!is_session_start_key(Key::Char(',')));
        assert!(!is_session_start_key(Key::Char('.')));
        assert!(!is_session_start_key(Key::Digit(1)));
        assert!(!is_session_start_key(Key::Space));
    }
}
