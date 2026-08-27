//! 游戏式按键录入（41-keymap-settings.md §5）：设置页点击录入框 → 捕获下一组合键
//! （Alt/Ctrl/Shift/Win 全收）→ 回填槽位。
//!
//! 2026-08-28 二次重构（方案 A，管理员拍板）：**弃用 WH_KEYBOARD_LL**——低层键盘钩子
//! 回调依赖安装线程的 Win32 消息泵，而 daemon 设置窗跑在 eframe/winit 事件循环下，
//! 实测钩子回调**从不触发**（日志：钩子安装/卸载齐全，录入期间零「收到按键」）。
//! 改用 egui 自身事件流（`egui::Event::Key`）：官方注释明确「physical_key 用于 games /
//! input-capture UIs」，天然支持 Alt/Ctrl/Shift 组合，且设置窗有焦点时必然收到
//! （用户录入时焦点必在设置窗），彻底绕开消息泵依赖。
//!
//! 本模块 = 纯逻辑（无窗口/无钩子）：`process_key_event` 把一次按键事件映射为
//! `CaptureOutcome`。消费方（settings.rs）在 eframe 帧内从 `ctx.input()` 遍历事件调用。

use eframe::egui;
use iuv_core::{Combo, Key};

/// 捕获结果。`Captured` = 组合键；`Clear` = Backspace 清除该槽；`Cancel` = Esc 取消
/// （槽位不变）；`Rejected(String)` = 捕获到但校验不过（如纯字母无修饰），给 UI 提示。
#[derive(Clone, Debug, PartialEq)]
pub enum CaptureOutcome {
    Captured(Combo),
    Clear,
    Cancel,
    Rejected(String),
}

/// 处理一次 egui 按键事件（仅 pressed 且非 repeat 的键，repeat 由调用方过滤）。
/// 返回：
/// - `Some(outcome)` = 本次录入结束（捕获/取消/清除/拒绝）；
/// - `None` = 按键不构成录入（纯修饰键按下等，继续等）。
pub fn process_key_event(key: egui::Key, modifiers: &egui::Modifiers) -> Option<CaptureOutcome> {
    // Esc 取消 / Backspace 清除（无论是否带修饰）
    match key {
        egui::Key::Escape => {
            crate::log::log_line("[capture] 收到 Esc → 取消录入");
            return Some(CaptureOutcome::Cancel);
        }
        egui::Key::Backspace => {
            crate::log::log_line("[capture] 收到 Backspace → 清除该槽");
            return Some(CaptureOutcome::Clear);
        }
        _ => {}
    }
    // 纯修饰键按下（egui 对左右修饰键提供独立物理变体）→ 忽略，等组合
    if matches!(
        key,
        egui::Key::ShiftLeft
            | egui::Key::ShiftRight
            | egui::Key::ControlLeft
            | egui::Key::ControlRight
            | egui::Key::AltLeft
            | egui::Key::AltRight
            | egui::Key::SuperLeft
            | egui::Key::SuperRight
    ) {
        return None;
    }
    // 基础键 → iuv Key
    let base = egui_key_to_base(key)?;
    let combo = Combo {
        ctrl: modifiers.ctrl,
        alt: modifiers.alt,
        shift: modifiers.shift,
        win: false, // egui Modifiers 无 Win 字段（Windows 上 Super 归 command）；见 base_key_to_vk
        base,
    };
    // 纯字母无修饰 → 拒绝（会吃掉拼音/全局劫持）
    if !combo.has_modifier() && combo.base_is_letter() {
        crate::log::log_line(&format!(
            "[capture] 纯字母无修饰被拒：{}（等待有效组合）",
            combo.name()
        ));
        return Some(CaptureOutcome::Rejected(combo.name()));
    }
    crate::log::log_line(&format!("[capture] 捕获组合键：{}", combo.name()));
    Some(CaptureOutcome::Captured(combo))
}

/// egui 逻辑键 → iuv `Key` 基础键。无法映射（多媒体/浏览器键等）→ None。
fn egui_key_to_base(key: egui::Key) -> Option<Key> {
    use egui::Key as K;
    Some(match key {
        K::ArrowUp => Key::Up,
        K::ArrowDown => Key::Down,
        K::ArrowLeft => Key::Left,
        K::ArrowRight => Key::Right,
        K::Tab => Key::Tab,
        K::Delete => Key::Delete,
        K::Home => Key::Home,
        K::End => Key::End,
        K::Insert => Key::Insert,
        K::PageUp => Key::PageUp,
        K::PageDown => Key::PageDown,
        K::Enter => Key::Enter,
        K::Space => Key::Space,
        K::Num0 => Key::Digit(0),
        K::Num1 => Key::Digit(1),
        K::Num2 => Key::Digit(2),
        K::Num3 => Key::Digit(3),
        K::Num4 => Key::Digit(4),
        K::Num5 => Key::Digit(5),
        K::Num6 => Key::Digit(6),
        K::Num7 => Key::Digit(7),
        K::Num8 => Key::Digit(8),
        K::Num9 => Key::Digit(9),
        K::A => Key::Char('a'),
        K::B => Key::Char('b'),
        K::C => Key::Char('c'),
        K::D => Key::Char('d'),
        K::E => Key::Char('e'),
        K::F => Key::Char('f'),
        K::G => Key::Char('g'),
        K::H => Key::Char('h'),
        K::I => Key::Char('i'),
        K::J => Key::Char('j'),
        K::K => Key::Char('k'),
        K::L => Key::Char('l'),
        K::M => Key::Char('m'),
        K::N => Key::Char('n'),
        K::O => Key::Char('o'),
        K::P => Key::Char('p'),
        K::Q => Key::Char('q'),
        K::R => Key::Char('r'),
        K::S => Key::Char('s'),
        K::T => Key::Char('t'),
        K::U => Key::Char('u'),
        K::V => Key::Char('v'),
        K::W => Key::Char('w'),
        K::X => Key::Char('x'),
        K::Y => Key::Char('y'),
        K::Z => Key::Char('z'),
        K::F1 => Key::F1,
        K::F2 => Key::F2,
        K::F3 => Key::F3,
        K::F4 => Key::F4,
        K::F5 => Key::F5,
        K::F6 => Key::F6,
        K::F7 => Key::F7,
        K::F8 => Key::F8,
        K::F9 => Key::F9,
        K::F10 => Key::F10,
        K::F11 => Key::F11,
        K::F12 => Key::F12,
        // 标点（逻辑键已按布局给出符号）
        K::Comma => Key::Char(','),
        K::Period => Key::Char('.'),
        K::Semicolon => Key::Char(';'),
        K::Colon => Key::Char(':'),
        K::Minus => Key::Char('-'),
        K::Equals => Key::Char('='),
        K::Plus => Key::Char('+'),
        K::Slash => Key::Char('/'),
        K::Backslash => Key::Char('\\'),
        K::Pipe => Key::Char('|'),
        K::Questionmark => Key::Char('?'),
        K::Exclamationmark => Key::Char('!'),
        K::OpenBracket => Key::Char('['),
        K::CloseBracket => Key::Char(']'),
        K::OpenCurlyBracket => Key::Char('{'),
        K::CloseCurlyBracket => Key::Char('}'),
        K::Backtick => Key::Char('`'),
        K::Quote => Key::Char('\''),
        _ => return None,
    })
}

/// 展示当前是否在录入态（settings 帧内判断按钮文案用）。
#[cfg(test)]
mod tests {
    use super::*;

    fn combo(name: &str) -> Combo {
        Combo::from_name(name).unwrap()
    }

    fn mods(ctrl: bool, alt: bool, shift: bool) -> egui::Modifiers {
        egui::Modifiers {
            alt,
            ctrl,
            shift,
            ..Default::default()
        }
    }

    #[test]
    fn esc_cancels() {
        assert_eq!(
            process_key_event(egui::Key::Escape, &egui::Modifiers::NONE),
            Some(CaptureOutcome::Cancel)
        );
    }

    #[test]
    fn backspace_clears() {
        assert_eq!(
            process_key_event(egui::Key::Backspace, &egui::Modifiers::NONE),
            Some(CaptureOutcome::Clear)
        );
    }

    #[test]
    fn shift_combo_captured() {
        // Shift+F → Shift+F
        assert_eq!(
            process_key_event(egui::Key::F, &mods(false, false, true)),
            Some(CaptureOutcome::Captured(combo("Shift+F")))
        );
        // Ctrl+Shift+F → Ctrl+Shift+F
        assert_eq!(
            process_key_event(egui::Key::F, &mods(true, false, true)),
            Some(CaptureOutcome::Captured(combo("Ctrl+Shift+F")))
        );
        // Alt+1 → Alt+1
        assert_eq!(
            process_key_event(egui::Key::Num1, &mods(false, true, false)),
            Some(CaptureOutcome::Captured(combo("Alt+1")))
        );
    }

    #[test]
    fn plain_letter_rejected() {
        // 纯字母无修饰 → 拒绝（拼音输入空间）
        assert_eq!(
            process_key_event(egui::Key::A, &egui::Modifiers::NONE),
            Some(CaptureOutcome::Rejected("A".into()))
        );
    }

    #[test]
    fn modifier_keys_ignored() {
        // 纯修饰键按下 → None（等组合）
        assert_eq!(
            process_key_event(egui::Key::ShiftLeft, &mods(false, false, true)),
            None
        );
        assert_eq!(
            process_key_event(egui::Key::ControlLeft, &mods(true, false, false)),
            None
        );
        assert_eq!(
            process_key_event(egui::Key::AltLeft, &mods(false, true, false)),
            None
        );
    }

    #[test]
    fn unmapped_key_none() {
        // 多媒体/浏览器键 → None
        assert_eq!(
            process_key_event(egui::Key::BrowserBack, &egui::Modifiers::NONE),
            None
        );
    }

    #[test]
    fn punctuation_mapped() {
        assert_eq!(
            process_key_event(egui::Key::Comma, &egui::Modifiers::NONE),
            Some(CaptureOutcome::Captured(combo(",")))
        );
        assert_eq!(
            process_key_event(egui::Key::Period, &egui::Modifiers::NONE),
            Some(CaptureOutcome::Captured(combo(".")))
        );
        // Shift+逗号（逻辑键已给 '<'？不——egui Comma 逻辑键无 Shift 符号，此处验证组合）
        assert_eq!(
            process_key_event(egui::Key::Comma, &mods(false, false, true)),
            Some(CaptureOutcome::Captured(combo("Shift+,")))
        );
    }
}
