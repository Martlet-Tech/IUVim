//! Windows 按键映射辅助（41-keymap-settings.md）：vk ↔ `iuv_core::Key` 基础键映射
//! + `Combo` 构造/拆解。TSF（route_key 组合键查表）与 daemon（WH_KEYBOARD_LL 录入、
//! `RegisterHotKey` 注册）共用——两进程对同一物理键的语义必须一致。

use iuv_core::{Combo, Key};

// ---- vk 常量（与 windows-rs 0.62 Win32 值一致，避免引整个模块）----
pub const VK_BACK: u16 = 0x08;
pub const VK_TAB: u16 = 0x09;
pub const VK_RETURN: u16 = 0x0D;
pub const VK_ESCAPE: u16 = 0x1B;
pub const VK_SPACE: u16 = 0x20;
pub const VK_PRIOR: u16 = 0x21; // PageUp
pub const VK_NEXT: u16 = 0x22; // PageDown
pub const VK_END: u16 = 0x23;
pub const VK_HOME: u16 = 0x24;
pub const VK_LEFT: u16 = 0x25;
pub const VK_UP: u16 = 0x26;
pub const VK_RIGHT: u16 = 0x27;
pub const VK_DOWN: u16 = 0x28;
pub const VK_INSERT: u16 = 0x2D;
pub const VK_DELETE: u16 = 0x2E;
pub const VK_0: u16 = 0x30;
pub const VK_9: u16 = 0x39;
pub const VK_A: u16 = 0x41;
pub const VK_Z: u16 = 0x5A;
pub const VK_F1: u16 = 0x70;
pub const VK_F12: u16 = 0x7B;

// ---- RegisterHotKey 修饰位（MOD_*）----
pub const MOD_ALT: u32 = 0x0001;
pub const MOD_CONTROL: u32 = 0x0002;
pub const MOD_SHIFT: u32 = 0x0004;
pub const MOD_WIN: u32 = 0x0008;

/// vk → 基础键（不含修饰）。字母归一为小写 `Char`（Shift 语义由 combo 的 shift 位表达）；
/// 数字 → `Digit`；标点 → `Char(char_code)`（未按下 Shift 的字符值）；F1-F12 →
/// `F1..F12`；导航/控制键 → 对应变体。无字符的物理键（功能键以外的杂键）→ None。
pub fn vk_to_base_key(vk: u16, char_code: u32) -> Option<Key> {
    match vk {
        VK_BACK => Some(Key::Backspace),
        VK_TAB => Some(Key::Tab),
        VK_RETURN => Some(Key::Enter),
        VK_ESCAPE => Some(Key::Esc),
        VK_SPACE => Some(Key::Space),
        VK_PRIOR => Some(Key::PageUp),
        VK_NEXT => Some(Key::PageDown),
        VK_END => Some(Key::End),
        VK_HOME => Some(Key::Home),
        VK_LEFT => Some(Key::Left),
        VK_UP => Some(Key::Up),
        VK_RIGHT => Some(Key::Right),
        VK_DOWN => Some(Key::Down),
        VK_INSERT => Some(Key::Insert),
        VK_DELETE => Some(Key::Delete),
        VK_0..=VK_9 => Some(Key::Digit((vk - VK_0) as u8)),
        VK_A..=VK_Z => Some(Key::Char(((vk - VK_A) as u8 + b'a') as char)),
        VK_F1..=VK_F12 => {
            let n = (vk - VK_F1) as u8 + 1;
            Some(match n {
                1 => Key::F1,
                2 => Key::F2,
                3 => Key::F3,
                4 => Key::F4,
                5 => Key::F5,
                6 => Key::F6,
                7 => Key::F7,
                8 => Key::F8,
                9 => Key::F9,
                10 => Key::F10,
                11 => Key::F11,
                _ => Key::F12,
            })
        }
        // 标点/符号键：取无 Shift 的字符值（MapVirtualKey 已给出）。
        // 无字符值（死键/纯功能）→ None。
        _ if char_code != 0 && char_code <= 0x7F => Some(Key::Char(char_code as u8 as char)),
        _ => None,
    }
}

/// 从 (vk, char_code, 修饰键态) 构造 `Combo`。Ctrl/Alt 组合 → None（红线：放行给应用 /
/// Alt 是 WM_SYSKEYDOWN 不进 TSF 键 sink，见 18-m2-user-dict.md 附录）。`win` 恒 false
/// （TSF 会话不跟踪 Win 键；Win 修饰仅 daemon 录入路径使用）。
pub fn combo_from_vk(vk: u16, char_code: u32, shift: bool, ctrl: bool, alt: bool) -> Option<Combo> {
    if ctrl || alt {
        return None;
    }
    let base = vk_to_base_key(vk, char_code)?;
    Some(Combo { ctrl: false, alt: false, shift, win: false, base })
}

/// 基础键 → vk（`RegisterHotKey` 注册用）。`Char` 标点经 `VkKeyScanW`（当前布局）
/// 换算 vk；字母/数字/导航/控制/功能键直接映射。
pub fn base_key_to_vk(base: &Key) -> Option<u16> {
    match base {
        Key::Char(c) => {
            if c.is_ascii_lowercase() {
                Some(VK_A + (*c as u8 - b'a') as u16)
            } else if c.is_ascii_uppercase() {
                Some(VK_A + (*c as u8 - b'A') as u16)
            } else if c.is_ascii_digit() {
                Some(VK_0 + (*c as u8 - b'0') as u16)
            } else {
                // 标点：VkKeyScanW 返回 (vk, shift 态) 的 short；只取低字节 vk。
                // 修饰语义由 Combo 的 shift 位承载（RegisterHotKey 传 MOD_SHIFT），
                // 不依赖 VkKeyScan 的 shift 高位。
                let r = unsafe {
                    windows::Win32::UI::Input::KeyboardAndMouse::VkKeyScanW(*c as u16)
                };
                let vk = (r as u16) & 0xFF;
                if vk == 0 && *c != ' ' {
                    None
                } else if vk == 0xFF {
                    None
                } else {
                    Some(vk)
                }
            }
        }
        Key::ShiftChar(_) => None, // 不参与绑定
        Key::Backspace => Some(VK_BACK),
        Key::Space => Some(VK_SPACE),
        Key::Enter => Some(VK_RETURN),
        Key::Esc => Some(VK_ESCAPE),
        Key::Digit(n) => Some(VK_0 + *n as u16),
        Key::Tab => Some(VK_TAB),
        Key::Delete => Some(VK_DELETE),
        Key::Home => Some(VK_HOME),
        Key::End => Some(VK_END),
        Key::Insert => Some(VK_INSERT),
        Key::PageUp => Some(VK_PRIOR),
        Key::PageDown => Some(VK_NEXT),
        Key::Up => Some(VK_UP),
        Key::Down => Some(VK_DOWN),
        Key::Left => Some(VK_LEFT),
        Key::Right => Some(VK_RIGHT),
        Key::F1 => Some(VK_F1),
        Key::F2 => Some(VK_F1 + 1),
        Key::F3 => Some(VK_F1 + 2),
        Key::F4 => Some(VK_F1 + 3),
        Key::F5 => Some(VK_F1 + 4),
        Key::F6 => Some(VK_F1 + 5),
        Key::F7 => Some(VK_F1 + 6),
        Key::F8 => Some(VK_F1 + 7),
        Key::F9 => Some(VK_F1 + 8),
        Key::F10 => Some(VK_F1 + 9),
        Key::F11 => Some(VK_F1 + 10),
        Key::F12 => Some(VK_F1 + 11),
        Key::SwapLeft | Key::SwapRight | Key::HideCandidate => None,
    }
}

/// `Combo` → `RegisterHotKey` 修饰位（MOD_*）。
pub fn combo_mods(combo: &Combo) -> u32 {
    let mut m = 0;
    if combo.alt {
        m |= MOD_ALT;
    }
    if combo.ctrl {
        m |= MOD_CONTROL;
    }
    if combo.shift {
        m |= MOD_SHIFT;
    }
    if combo.win {
        m |= MOD_WIN;
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vk_to_base_letters() {
        assert_eq!(vk_to_base_key(VK_A, 0x61), Some(Key::Char('a')));
        assert_eq!(vk_to_base_key(VK_Z, 0x7A), Some(Key::Char('z')));
    }

    #[test]
    fn vk_to_base_nav() {
        assert_eq!(vk_to_base_key(VK_LEFT, 0), Some(Key::Left));
        assert_eq!(vk_to_base_key(VK_PRIOR, 0), Some(Key::PageUp));
        assert_eq!(vk_to_base_key(VK_DELETE, 0), Some(Key::Delete));
        assert_eq!(vk_to_base_key(VK_F1, 0), Some(Key::F1));
        assert_eq!(vk_to_base_key(VK_F12, 0), Some(Key::F12));
    }

    #[test]
    fn vk_to_base_digit_punct() {
        assert_eq!(vk_to_base_key(VK_0, 0x30), Some(Key::Digit(0)));
        assert_eq!(vk_to_base_key(VK_9, 0x39), Some(Key::Digit(9)));
        // 逗号键：无 Shift 字符 = ','
        assert_eq!(vk_to_base_key(0xBC, 0x2C), Some(Key::Char(',')));
        // 死键（char_code 0）→ None
        assert_eq!(vk_to_base_key(0xBC, 0), None);
        // 非字符杂键（如右 Shift 0xA1、未知）→ None
        assert_eq!(vk_to_base_key(0xA1, 0), None);
        assert_eq!(vk_to_base_key(0x00, 0), None);
    }

    #[test]
    fn combo_from_vk_flags() {
        // Shift+Left → Combo{shift:true, base:Left}
        assert_eq!(
            combo_from_vk(VK_LEFT, 0, true, false, false),
            Some(Combo { ctrl: false, alt: false, shift: true, win: false, base: Key::Left })
        );
        // Ctrl/Alt 组合 → None（红线）
        assert_eq!(combo_from_vk(VK_LEFT, 0, true, true, false), None);
        assert_eq!(combo_from_vk(VK_LEFT, 0, true, false, true), None);
        // Shift+逗号
        assert_eq!(
            combo_from_vk(0xBC, 0x2C, true, false, false),
            Some(Combo { ctrl: false, alt: false, shift: true, win: false, base: Key::Char(',') })
        );
    }

    #[test]
    fn key_to_vk_roundtrip() {
        assert_eq!(base_key_to_vk(&Key::Left), Some(VK_LEFT));
        assert_eq!(base_key_to_vk(&Key::PageUp), Some(VK_PRIOR));
        assert_eq!(base_key_to_vk(&Key::Digit(5)), Some(VK_0 + 5));
        assert_eq!(base_key_to_vk(&Key::Char('a')), Some(VK_A));
        assert_eq!(base_key_to_vk(&Key::Char('z')), Some(VK_Z));
        assert_eq!(base_key_to_vk(&Key::F5), Some(VK_F1 + 4));
        assert_eq!(base_key_to_vk(&Key::Backspace), Some(VK_BACK));
        // 不可绑定键 → None
        assert_eq!(base_key_to_vk(&Key::SwapLeft), None);
        assert_eq!(base_key_to_vk(&Key::ShiftChar('A')), None);
    }

    #[test]
    fn combo_mods_flags() {
        let c = Combo { ctrl: true, alt: true, shift: false, win: true, base: Key::Char('i') };
        assert_eq!(combo_mods(&c), MOD_CONTROL | MOD_ALT | MOD_WIN);
        let c2 = Combo { ctrl: false, alt: false, shift: true, win: false, base: Key::Left };
        assert_eq!(combo_mods(&c2), MOD_SHIFT);
    }
}
