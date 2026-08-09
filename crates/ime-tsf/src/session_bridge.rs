//! vk/char → Key 映射 + Effect 应用。契约 01-contract.md §7 与 13 任务书 §3.4。
//! 【Agent D】W1 实现。

use ime_core::{Effect, Key, SessionEnd};

use crate::composition::Composition;
use crate::log::log_line;
use crate::ui::{effect_to_snapshot, CandidateUi, CaretRect};

/// 虚拟键 → 归一化 Key。未识别键返回 None（放行给应用）。
///
/// - `vk`：WM_KEYDOWN 的 wParam（VK_*）。
/// - `char_code`：该键在当前键盘布局下的字符值（`MapVirtualKeyW(_, MAPVK_VK_TO_CHAR)`，
///   无 Shift 状态的静态映射），用于字母/OEM 引号判定。
/// - `with_shift`：Shift 是否按下。
///
/// 映射表集中在此一处，M3+ 加双拼/快捷键只动这里（13 任务书 §5）。
pub fn map_key(vk: u16, char_code: u32, with_shift: bool) -> Option<Key> {
    const VK_BACK: u16 = 0x08;
    const VK_SPACE: u16 = 0x20;
    const VK_RETURN: u16 = 0x0D;
    const VK_ESCAPE: u16 = 0x1B;
    const VK_PRIOR: u16 = 0x21; // PageUp
    const VK_NEXT: u16 = 0x22; // PageDown
    const VK_UP: u16 = 0x26;
    const VK_DOWN: u16 = 0x28;
    const VK_1: u16 = 0x31;
    const VK_9: u16 = 0x39;
    const VK_A: u16 = 0x41;
    const VK_Z: u16 = 0x5A;
    const VK_OEM_7: u16 = 0xDE; // 引号键（无 Shift = '）

    match vk {
        VK_BACK => Some(Key::Backspace),
        VK_SPACE => Some(Key::Space),
        VK_RETURN => Some(Key::Enter),
        VK_ESCAPE => Some(Key::Esc),
        VK_PRIOR => Some(Key::PageUp),
        VK_NEXT => Some(Key::PageDown),
        VK_UP => Some(Key::Up),
        VK_DOWN => Some(Key::Down),
        VK_1..=VK_9 if !with_shift => Some(Key::Digit((char_code - 0x30) as u8)),
        VK_A..=VK_Z => {
            // 字母：优先用布局字符（保证小写），退化用 vk 推算。
            let c = if (0x61..=0x7A).contains(&char_code) {
                char_code as u8 as char
            } else {
                (vk + 0x20) as u8 as char
            };
            Some(Key::Char(c))
        }
        VK_OEM_7 if !with_shift && char_code == 0x27 => Some(Key::Char('\'')),
        _ => None,
    }
}

/// 应用 Effect：composition 更新 → 候选窗快照 → 会话结束处理。
/// 契约 13 任务书 §3.4：SetText → caret → ui.show/update → end 上屏/取消并 hide。
///
/// 返回 `true` 表示会话已结束（effect.end 为 Some），调用方应丢弃 Session。
pub fn apply_effect(
    composition: &Composition,
    ui: &mut dyn CandidateUi,
    caret: &mut CaretRect,
    effect: &Effect,
) -> bool {
    match &effect.end {
        Some(SessionEnd::Commit(text)) => {
            match composition.commit(text) {
                Ok(()) => log_line(&format!("commit：{text}")),
                Err(e) => log_line(&format!("commit 失败：{e}")),
            }
            ui.hide();
            true
        }
        Some(SessionEnd::Cancel) => {
            match composition.cancel() {
                Ok(()) => log_line("cancel：清空预编辑"),
                Err(e) => log_line(&format!("cancel 失败：{e}")),
            }
            ui.hide();
            true
        }
        None => {
            match composition.set_text(&effect.composition) {
                Ok(Some(rect)) => *caret = rect,
                Ok(None) => {}
                Err(e) => log_line(&format!("set_text 失败：{e}，沿用上次光标")),
            }
            let snap = effect_to_snapshot(effect);
            if snap.candidates.is_empty() && snap.reading.is_empty() {
                ui.hide();
            } else if ui.is_visible() {
                ui.update(&snap);
            } else {
                ui.show(&snap, *caret);
            }
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_key_letters_lowercase() {
        assert_eq!(map_key(0x41, 0x61, false), Some(Key::Char('a')));
        assert_eq!(map_key(0x5A, 0x7A, false), Some(Key::Char('z')));
        assert_eq!(map_key(0x4B, 0x6B, true), Some(Key::Char('k')));
    }

    #[test]
    fn map_key_digits_respect_shift() {
        assert_eq!(map_key(0x31, 0x31, false), Some(Key::Digit(1)));
        assert_eq!(map_key(0x39, 0x39, false), Some(Key::Digit(9)));
        // Shift+数字 = 符号，放行给应用
        assert_eq!(map_key(0x31, 0x31, true), None);
    }

    #[test]
    fn map_key_apostrophe() {
        assert_eq!(map_key(0xDE, 0x27, false), Some(Key::Char('\'')));
        // Shift+引号 = 双引号，放行
        assert_eq!(map_key(0xDE, 0x27, true), None);
        assert_eq!(map_key(0xDE, 0x22, false), None);
    }

    #[test]
    fn map_key_control_keys() {
        assert_eq!(map_key(0x08, 0, false), Some(Key::Backspace));
        assert_eq!(map_key(0x20, 0, false), Some(Key::Space));
        assert_eq!(map_key(0x0D, 0, false), Some(Key::Enter));
        assert_eq!(map_key(0x1B, 0, false), Some(Key::Esc));
        assert_eq!(map_key(0x21, 0, false), Some(Key::PageUp));
        assert_eq!(map_key(0x22, 0, false), Some(Key::PageDown));
        assert_eq!(map_key(0x26, 0, false), Some(Key::Up));
        assert_eq!(map_key(0x28, 0, false), Some(Key::Down));
    }

    #[test]
    fn map_key_unknown_returns_none() {
        assert_eq!(map_key(0x10, 0, false), None); // Shift
        assert_eq!(map_key(0x1B, 0, false), Some(Key::Esc));
        assert_eq!(map_key(0x90, 0, false), None); // NumLock
        assert_eq!(map_key(0x00, 0, false), None);
    }

    /// 断言"某事没发生"必须配正向用例：Digit(9) 存在即 Digit 边界可控。
    #[test]
    fn map_key_digit_range_bounds() {
        // VK_0 不在 1..=9 语义内（契约 §3.4），必须放行给应用。
        assert_eq!(map_key(0x30, 0x30, false), None);
        assert_eq!(map_key(0x31, 0x31, false), Some(Key::Digit(1)));
    }
}
