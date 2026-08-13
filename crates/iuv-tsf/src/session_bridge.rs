//! vk/char → Key 映射 + Effect 应用。契约 01-contract.md §7 与 13 任务书 §3.4。
//! 【Agent D】W1 实现。

use iuv_core::{Effect, Key, SessionEnd};

use crate::composition::Composition;
use crate::log::log_line;
use crate::ui::{effect_to_snapshot, CandidateUi, CaretRect};

/// 虚拟键 → 归一化 Key。未识别键返回 None（放行给应用）。
///
/// - `vk`：WM_KEYDOWN 的 wParam（VK_*）。
/// - `char_code`：该键在当前键盘布局下的字符值（`MapVirtualKeyW(_, MAPVK_VK_TO_CHAR)`，
///   无 Shift 状态的静态映射），用于字母/OEM 引号判定。
/// - `with_shift` / `with_ctrl` / `with_alt`：各修饰键是否按下。
///
/// 约定：**Ctrl/Alt 组合键一律放行给应用**（如 Ctrl+S 保存、Alt+字母 菜单），
/// 输入法只消费 Shift 修饰的组合（大小写/符号）。映射表集中在此一处，
/// M3+ 加双拼/快捷键只动这里（13 任务书 §5）。
pub fn map_key(vk: u16, char_code: u32, with_shift: bool, with_ctrl: bool, with_alt: bool) -> Option<Key> {
    // Ctrl/Alt 组合：放行给应用，绝不消费（13 任务书 §3.3）。
    if with_ctrl || with_alt {
        return None;
    }
    const VK_BACK: u16 = 0x08;
    const VK_SPACE: u16 = 0x20;
    const VK_RETURN: u16 = 0x0D;
    const VK_ESCAPE: u16 = 0x1B;
    const VK_PRIOR: u16 = 0x21; // PageUp
    const VK_NEXT: u16 = 0x22; // PageDown
    const VK_UP: u16 = 0x26;
    const VK_DOWN: u16 = 0x28;
    const VK_LEFT: u16 = 0x25;
    const VK_RIGHT: u16 = 0x27;
    const VK_1: u16 = 0x31;
    const VK_9: u16 = 0x39;
    const VK_A: u16 = 0x41;
    const VK_Z: u16 = 0x5A;
    const VK_OEM_7: u16 = 0xDE; // 引号键（无 Shift = '）
    const VK_OEM_COMMA: u16 = 0xBC; // 逗号键（无 Shift = ,）
    const VK_OEM_PERIOD: u16 = 0xBE; // 句号键（无 Shift = .）

    match vk {
        VK_BACK => Some(Key::Backspace),
        VK_SPACE => Some(Key::Space),
        VK_RETURN => Some(Key::Enter),
        VK_ESCAPE => Some(Key::Esc),
        VK_PRIOR => Some(Key::PageUp),
        VK_NEXT => Some(Key::PageDown),
        VK_UP => Some(Key::Up),
        VK_DOWN => Some(Key::Down),
        VK_LEFT => Some(Key::Left),
        VK_RIGHT => Some(Key::Right),
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
        // 逗号/句号：无 Shift 时映射为标点键（会话内由 apply_keymap 翻页；会话外放行打标点）。
        VK_OEM_COMMA if !with_shift && char_code == 0x2C => Some(Key::Char(',')),
        VK_OEM_PERIOD if !with_shift && char_code == 0x2E => Some(Key::Char('.')),
        _ => None,
    }
}

/// 应用键映射（快捷键 → 引擎键）。命中翻页表则重映射为 PageUp/PageDown，否则原样。
/// 会话外开启会话判定（仅字母与 `'`；`,`/`.` 等标点放行给应用）。
/// 两者定义在 iuv-core（config/keymap.rs），此处直接复用。

/// 光标跳变阈值（px）：候选窗可见时，新光标距**上一次光标**位移超过该值
/// 视为"输入点远跳"（点击远处/拖拽窗口跨屏/自动换行），简单版直接清除未完成输入。
/// 完整版（保留 composition、候选框在下一键重新出现）见 14-mod-iuv-tsf-candwin.md §5。
/// 判定基准用增量（与上次 caret 的位移）而非 composition 起点：连续打字时
/// caret 每键只前进约一个字符宽（~15px），长 composition 不会误触发；
/// 点击/换行才产生跳变位移（实测修复 2026-08-13：qingnixiangyong 15 键后
/// composition 起点到末尾 167px > 阈值，旧基准把正常打字误判为远跳而藏候选窗）。
const JUMP_THRESHOLD: f64 = 150.0;

/// 两点距离（像素）。
fn jump_distance(a: CaretRect, b: CaretRect) -> f64 {
    let dx = (a.x - b.x) as f64;
    let dy = (a.y - b.y) as f64;
    (dx * dx + dy * dy).sqrt()
}

/// 应用 Effect：composition 更新 → 候选窗快照 → 会话结束处理。
/// 契约 13 任务书 §3.4：SetText → caret → ui.show/update → end 上屏/取消并 hide。
///
/// 返回 `true` 表示会话已结束（effect.end 为 Some 或远跳清除），调用方应丢弃 Session。
pub fn apply_effect(
    composition: &Composition,
    ui: &mut dyn CandidateUi,
    caret: &mut CaretRect,
    effect: &Effect,
    orientation: iuv_core::Orientation,
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
            // 悬空状态（选中中间级词后）：无 commit 信号——已选词仅在预编辑混合文本中
            // 显示（汉字+尾巴拼音），composition 全程覆盖整个混合文本，set_text 全量更新。
            let prev_caret = *caret; // 跳变检测基准：上一次光标（增量位移）
            match composition.set_text(&effect.composition) {
                Ok(Some(rect)) => {
                    log_line(&format!(
                        "[caret] set_text 返回新光标：x={} y={} w={} h={}",
                        rect.x, rect.y, rect.w, rect.h
                    ));
                    *caret = rect;
                }
                Ok(None) => {
                    log_line(&format!(
                        "[caret] set_text 无光标（GetTextExt 失败/clipped），沿用旧光标：x={} y={} w={} h={}",
                        caret.x, caret.y, caret.w, caret.h
                    ));
                }
                Err(e) => log_line(&format!(
                    "[caret] set_text 失败：{e}，沿用旧光标：x={} y={} w={} h={}",
                    caret.x, caret.y, caret.w, caret.h
                )),
            }
            let mut snap = effect_to_snapshot(effect);
            snap.orientation = orientation;
            if snap.candidates.is_empty() && snap.reading.is_empty() {
                log_line("[candwin] 快照为空，hide");
                ui.hide();
                false
            } else if ui.is_visible() {
                if jump_distance(prev_caret, *caret) > JUMP_THRESHOLD {
                    // 输入点跳变（点击远处/拖拽窗口跨屏/自动换行）：
                    // 仅隐藏候选窗、保留 composition 与 Session；下一键 set_text 后
                    // 自然走 show 分支用新光标重新定位。换行场景输入不丢失。
                    // 基准为增量（与上次 caret 位移）——正常打字每键 ~15px 不触发。
                    log_line(&format!(
                        "[candwin] 光标跳变（prev=({},{}), caret=({},{}), dist={:.0}px），隐藏候选窗待下一键重现",
                        prev_caret.x,
                        prev_caret.y,
                        caret.x,
                        caret.y,
                        jump_distance(prev_caret, *caret)
                    ));
                    ui.hide();
                    false
                } else {
                    log_line(&format!(
                        "[candwin] 窗口已可见，update（不动位置），当前 caret：x={} y={}",
                        caret.x, caret.y
                    ));
                    ui.update(&snap);
                    false
                }
            } else {
                log_line(&format!(
                    "[candwin] 首次 show，caret：x={} y={} w={} h={}",
                    caret.x, caret.y, caret.w, caret.h
                ));
                ui.show(&snap, *caret);
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_key_letters_lowercase() {
        assert_eq!(map_key(0x41, 0x61, false, false, false), Some(Key::Char('a')));
        assert_eq!(map_key(0x5A, 0x7A, false, false, false), Some(Key::Char('z')));
        assert_eq!(map_key(0x4B, 0x6B, true, false, false), Some(Key::Char('k')));
    }

    #[test]
    fn map_key_digits_respect_shift() {
        assert_eq!(map_key(0x31, 0x31, false, false, false), Some(Key::Digit(1)));
        assert_eq!(map_key(0x39, 0x39, false, false, false), Some(Key::Digit(9)));
        // Shift+数字 = 符号，放行给应用
        assert_eq!(map_key(0x31, 0x31, true, false, false), None);
    }

    #[test]
    fn map_key_arrows() {
        assert_eq!(map_key(0x25, 0x25, false, false, false), Some(Key::Left));
        assert_eq!(map_key(0x27, 0x27, false, false, false), Some(Key::Right));
        // Ctrl+左右 = 词跳转，放行给应用
        assert_eq!(map_key(0x25, 0x25, false, true, false), None);
    }

    #[test]
    fn map_key_apostrophe() {
        assert_eq!(map_key(0xDE, 0x27, false, false, false), Some(Key::Char('\'')));
        // Shift+引号 = 双引号，放行
        assert_eq!(map_key(0xDE, 0x27, true, false, false), None);
        assert_eq!(map_key(0xDE, 0x22, false, false, false), None);
    }

    #[test]
    fn map_key_comma_period() {
        // 无 Shift：逗号/句号映射为标点键（会话内翻页、会话外打标点）
        assert_eq!(map_key(0xBC, 0x2C, false, false, false), Some(Key::Char(',')));
        assert_eq!(map_key(0xBE, 0x2E, false, false, false), Some(Key::Char('.')));
        // Shift+逗号/句号 = < > 符号，放行
        assert_eq!(map_key(0xBC, 0x2C, true, false, false), None);
        assert_eq!(map_key(0xBE, 0x2E, true, false, false), None);
    }

    #[test]
    fn apply_keymap_paging() {
        let cfg = iuv_core::Config::default();
        assert_eq!(iuv_core::apply_keymap(Key::Char(','), &cfg.keymap), Key::PageUp);
        assert_eq!(iuv_core::apply_keymap(Key::Char('.'), &cfg.keymap), Key::PageDown);
        assert_eq!(iuv_core::apply_keymap(Key::Up, &cfg.keymap), Key::PageUp);
        assert_eq!(iuv_core::apply_keymap(Key::Down, &cfg.keymap), Key::PageDown);
        assert_eq!(iuv_core::apply_keymap(Key::PageUp, &cfg.keymap), Key::PageUp);
        // 未命中：原样
        assert_eq!(iuv_core::apply_keymap(Key::Char('a'), &cfg.keymap), Key::Char('a'));
        assert_eq!(iuv_core::apply_keymap(Key::Space, &cfg.keymap), Key::Space);
        assert_eq!(iuv_core::apply_keymap(Key::Digit(3), &cfg.keymap), Key::Digit(3));
    }

    #[test]
    fn session_start_keys() {
        assert!(iuv_core::is_session_start_key(Key::Char('a')));
        assert!(!iuv_core::is_session_start_key(Key::Char('\'')));
        // 标点/数字/控制键不得开启会话（放行给应用）
        assert!(!iuv_core::is_session_start_key(Key::Char(',')));
        assert!(!iuv_core::is_session_start_key(Key::Char('.')));
        assert!(!iuv_core::is_session_start_key(Key::Digit(1)));
        assert!(!iuv_core::is_session_start_key(Key::Space));
    }

    #[test]
    fn map_key_control_keys() {
        assert_eq!(map_key(0x08, 0, false, false, false), Some(Key::Backspace));
        assert_eq!(map_key(0x20, 0, false, false, false), Some(Key::Space));
        assert_eq!(map_key(0x0D, 0, false, false, false), Some(Key::Enter));
        assert_eq!(map_key(0x1B, 0, false, false, false), Some(Key::Esc));
        assert_eq!(map_key(0x21, 0, false, false, false), Some(Key::PageUp));
        assert_eq!(map_key(0x22, 0, false, false, false), Some(Key::PageDown));
        assert_eq!(map_key(0x26, 0, false, false, false), Some(Key::Up));
        assert_eq!(map_key(0x28, 0, false, false, false), Some(Key::Down));
    }

    #[test]
    fn map_key_unknown_returns_none() {
        assert_eq!(map_key(0x10, 0, false, false, false), None); // Shift
        assert_eq!(map_key(0x1B, 0, false, false, false), Some(Key::Esc));
        assert_eq!(map_key(0x90, 0, false, false, false), None); // NumLock
        assert_eq!(map_key(0x00, 0, false, false, false), None);
    }

    #[test]
    fn map_key_modifiers_always_release() {
        // Ctrl/Alt 组合键一律放行给应用（如 Ctrl+S 保存、Alt+F4 关闭），
        // 与是否在映射表内无关。
        assert_eq!(map_key(0x53, 0x73, false, true, false), None); // Ctrl+S
        assert_eq!(map_key(0x53, 0x73, false, false, true), None); // Alt+S
        assert_eq!(map_key(0x31, 0x31, false, true, false), None); // Ctrl+1
        assert_eq!(map_key(0x20, 0, false, true, false), None); // Ctrl+Space
        assert_eq!(map_key(0x0D, 0, false, false, true), None); // Alt+Enter
    }

    /// 断言"某事没发生"必须配正向用例：Digit(9) 存在即 Digit 边界可控。
    #[test]
    fn map_key_digit_range_bounds() {
        // VK_0 不在 1..=9 语义内（契约 §3.4），必须放行给应用。
        assert_eq!(map_key(0x30, 0x30, false, false, false), None);
        assert_eq!(map_key(0x31, 0x31, false, false, false), Some(Key::Digit(1)));
    }
}
