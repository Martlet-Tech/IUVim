//! vk/char → Key 映射 + Effect 应用。契约 01-contract.md §7 与 13 任务书 §3.4。
//! 【Agent D】W1 实现。

use iuv_core::{chinese_punct, fullwidth, shifted_punct, Effect, Key, PunctMode, SessionEnd, WidthMode};

use crate::composition::Composition;
use crate::log::{log_line, perf_record_with, perf_tick};
use crate::ui::{effect_to_snapshot, CandidateUi, CaretRect};

/// 虚拟键 → 归一化 Key。未识别键返回 None（放行给应用）。
///
/// - `vk`：WM_KEYDOWN 的 wParam（VK_*）。
/// - `char_code`：该键在当前键盘布局下的字符值（`MapVirtualKeyW(_, MAPVK_VK_TO_CHAR)`，
///   无 Shift 状态的静态映射），用于字母/OEM 引号判定。
/// - `with_shift` / `with_capslock` / `with_ctrl` / `with_alt`：各修饰键/状态是否生效。
///
/// 字母大小写 = Shift 与 CapsLock 的 XOR（系统惯例：恰好一个生效 → 大写）：
/// 大写 → `Key::ShiftChar`（保形进序列，匹配只认小写、commit 原样上屏）；
/// 小写 → `Key::Char`（含 CapsLock+Shift 反转）。
/// 约定：**Ctrl/Alt 组合键一律放行给应用**（如 Ctrl+S 保存、Alt+字母 菜单）；
/// 会话内快捷键（翻页/移动/调权/隐藏）由 `route_key` 的组合键表（keymap）先行查表
/// 归一化，**不在此映射**——M2 调权 Shift+←/→、隐藏 Shift+Delete 自 41-keymap-settings.md
/// 起移入可配置 keymap（默认值保持原 Shift 组合）。注意：Alt 组合是 `WM_SYSKEYDOWN`，
/// **不经过 ITfKeyEventSink**（TSF 机制限制，输入法收不到）——快捷键设计红线。
/// 映射表集中在此一处，M3+ 加双拼/快捷键只动这里（13 任务书 §5）。
pub fn map_key(
    vk: u16,
    char_code: u32,
    with_shift: bool,
    with_capslock: bool,
    with_ctrl: bool,
    with_alt: bool,
) -> Option<Key> {
    // Ctrl/Alt 组合：放行给应用，绝不消费（13 任务书 §3.3）。
    if with_ctrl || with_alt {
        return None;
    }
    const VK_BACK: u16 = 0x08;
    const VK_SPACE: u16 = 0x20;
    const VK_RETURN: u16 = 0x0D;
    const VK_ESCAPE: u16 = 0x1B;
    // 导航/翻页键常量已移除：这些物理键的会话内语义由 keymap 决定（41-keymap-settings.md
    // §10.6），不再经 map_key 硬编码；combo 构造用 iuv-win keys.rs 的 vk_to_base_key。
    const VK_DELETE: u16 = 0x2E;
    const VK_1: u16 = 0x31;
    const VK_9: u16 = 0x39;
    const VK_A: u16 = 0x41;
    const VK_Z: u16 = 0x5A;
    const VK_OEM_7: u16 = 0xDE; // 引号键（无 Shift = '）
    const VK_OEM_COMMA: u16 = 0xBC; // 逗号键（无 Shift = ,）
    const VK_OEM_PERIOD: u16 = 0xBE; // 句号键（无 Shift = .）
    // 其余 OEM 标点键（会话内作字面尾巴，见 session.rs tail；会话外行为不变——
    // 中文标点/全角判定在 route_key 更前，命中即被吃，未命中原样放行）。
    const VK_OEM_1: u16 = 0xBA; // ; :
    const VK_OEM_PLUS: u16 = 0xBB; // = +
    const VK_OEM_MINUS: u16 = 0xBD; // - _
    const VK_OEM_2: u16 = 0xBF; // / ?
    const VK_OEM_3: u16 = 0xC0; // ` ~
    const VK_OEM_4: u16 = 0xDB; // [ {
    const VK_OEM_5: u16 = 0xDC; // \ |
    const VK_OEM_6: u16 = 0xDD; // ] }

    match vk {
        VK_BACK => Some(Key::Backspace),
        VK_SPACE => Some(Key::Space),
        VK_RETURN => Some(Key::Enter),
        VK_ESCAPE => Some(Key::Esc),
        // 导航/翻页键（PageUp/PageDown/方向键）**不再在此硬编码映射**——它们与会话快捷键
        // 语义冲突：清除 keymap 键位后仍会经此直通 Session 翻页/移动候选（实测：清除
        // page_prev 的 PageUp 后 PageUp 仍翻页）。41-keymap-settings.md §11 修复：
        // 这些物理键的会话内语义**完全由 keymap 决定**——route_key 先查组合键表，命中
        // 归一化（PageUp/Left…）喂 Session；未命中则 map_key 返回 None → Pass 放行给应用
        // （会话外本就放行，行为不变）。候选移动由 keymap candidate_prev/next 归一化
        // 为 Key::Left/Right 后进入 Session，不再经物理方向键直通。
        VK_DELETE => None, // 裸 Delete 放行给应用编辑；Shift+Delete 由组合键表映射 HideCandidate
        VK_1..=VK_9 if !with_shift => Some(Key::Digit((char_code - 0x30) as u8)),
        VK_A..=VK_Z => {
            // 字母：优先用布局字符（无 Shift 态恒小写），退化用 vk 推算。
            let c = if (0x61..=0x7A).contains(&char_code) {
                char_code as u8 as char
            } else {
                (vk + 0x20) as u8 as char
            };
            // Shift 与 CapsLock 恰好一个生效 → 大写（ShiftChar 保形进序列）；否则小写。
            if with_shift ^ with_capslock {
                Some(Key::ShiftChar(c.to_ascii_uppercase()))
            } else {
                Some(Key::Char(c))
            }
        }
        VK_OEM_7 if !with_shift && char_code == 0x27 => Some(Key::Char('\'')),
        // 逗号/句号：无 Shift 时映射为标点键（会话内由 apply_keymap 翻页；会话外放行打标点）。
        VK_OEM_COMMA if !with_shift && char_code == 0x2C => Some(Key::Char(',')),
        VK_OEM_PERIOD if !with_shift && char_code == 0x2E => Some(Key::Char('.')),
        // 其余 OEM 标点键 → 实际字符（char_code 为无 Shift 基准，Shift 形态经
        // shifted_punct 推导）。死键（char_code=0）放行。
        // TODO(用户自定义按键映射)：落地时字面收编集 = 可打印符号 − 用户已定义功能键。
        VK_OEM_1 | VK_OEM_PLUS | VK_OEM_MINUS | VK_OEM_2 | VK_OEM_3 | VK_OEM_4 | VK_OEM_5
        | VK_OEM_6
            if char_code != 0 =>
        {
            Some(Key::Char(shifted_punct(char_code as u8 as char, with_shift)))
        }
        _ => None,
    }
}

/// CapsLock 生效时字母键放行直通（仿微软：Caps = 英文模式，会话外不建会话）。
/// 只作用于会话外首键；会话内 Caps 字母照常进序列（composition 残留反而更乱）。
/// 注意：Caps 生效时 map_key 已把字母映射为大写 ShiftChar（或 Caps+Shift
/// 反转的小写 Char）——两种形态都放行；Shift 单独的大写（无 Caps）不受影响。
pub fn caps_passthrough(key: &Key, capslock: bool) -> bool {
    capslock && matches!(key, Key::Char(c) | Key::ShiftChar(c) if c.is_ascii_alphabetic())
}

/// 进程 exe 名是否命中按键直通白名单（大小写不敏感精确匹配，仿 Weasel PR #1049）。
/// 命中进程的全部按键由 TSF 层放行：不建会话、无候选窗（输入法在该进程完全透明）。
/// 名单为空时直接 false（零开销，不查进程名）。
pub fn is_passthrough_app(exe: &str, list: &[String]) -> bool {
    list.iter().any(|app| app.eq_ignore_ascii_case(exe))
}

/// 全角直接上屏判定（28-initial-state-settings.md 全角行为；纯函数，与 `chinese_punct_pending`
/// 对称但自含——标点表归属检查内置，调用方先跑中文标点再跑全角也可、只跑全角也可）。
///
/// 命中 → Some(全角上屏文本)：`width == Full` 且按键是**可全角化的 ASCII**，按模式分派：
/// - 英文模式：字母（大小写 = Shift 与 Caps 的 XOR）/数字/符号/空格全转全角（`ｍｉｃｒｏｓｏｆｔ１２３`）；
/// - 中文模式：数字/符号/空格转全角；**字母不转**（照常进拼音会话）；中文标点表内符号
///   归标点开关（`punct == Chinese` 时 `，`→`，` 由标点处理，全角不接管）；
/// - `width == Half`、非 ASCII、控制字符 → None（直通给应用）。
pub fn fullwidth_pending(
    english_mode: bool,
    width: WidthMode,
    punct: PunctMode,
    base: char,
    shift: bool,
    caps: bool,
) -> Option<String> {
    if width != WidthMode::Full || !base.is_ascii() {
        return None;
    }
    if english_mode {
        // 英文模式：字母大小写 = Shift⊕Caps（系统惯例）；符号经 Shift 推导。
        let ch = if base.is_ascii_alphabetic() {
            if shift ^ caps {
                base.to_ascii_uppercase()
            } else {
                base.to_ascii_lowercase()
            }
        } else {
            shifted_punct(base, shift)
        };
        return fullwidth(ch).map(|c| c.to_string());
    }
    // 中文模式：字母不转（进拼音会话）；数字/符号/空格转全角。
    if base.is_ascii_alphabetic() {
        return None;
    }
    let ch = shifted_punct(base, shift);
    if punct == PunctMode::Chinese && chinese_punct(ch, true).is_some() {
        return None; // 标点表归属 → 由 chinese_punct_pending 处理，全角不接管
    }
    fullwidth(ch).map(|c| c.to_string())
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
                Ok(()) => log_line(&format!("[commit] commit：{text}")),
                Err(e) => log_line(&format!("[commit] commit 失败：{e}")),
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
            let t_settext = perf_tick();
            match composition.set_text(&effect.composition) {
                Ok(Some(rect)) => {
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
            perf_record_with("settext", t_settext, || {
                format!("len={}", effect.composition.chars().count())
            });
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
                    let t_render = perf_tick();
                    ui.update(&snap);
                    perf_record_with("render", t_render, || "update".to_owned());
                    false
                }
            } else {
                let t_render = perf_tick();
                ui.show(&snap, *caret);
                perf_record_with("render", t_render, || "show".to_owned());
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn map_key_letters_shift_and_caps_xor() {
        // 无修饰：小写 Char（拼音）
        assert_eq!(
            map_key(0x41, 0x61, false, false, false, false),
            Some(Key::Char('a'))
        );
        assert_eq!(
            map_key(0x5A, 0x7A, false, false, false, false),
            Some(Key::Char('z'))
        );
        // Shift+字母 → ShiftChar 大写（保形进序列）
        assert_eq!(
            map_key(0x4B, 0x6B, true, false, false, false),
            Some(Key::ShiftChar('K'))
        );
        assert_eq!(
            map_key(0x41, 0x61, true, false, false, false),
            Some(Key::ShiftChar('A'))
        );
        // CapsLock 生效 → 同样 ShiftChar 大写（直通进序列）
        assert_eq!(
            map_key(0x4B, 0x6B, false, true, false, false),
            Some(Key::ShiftChar('K'))
        );
        // CapsLock + Shift 反转 → 小写 Char（系统惯例）
        assert_eq!(
            map_key(0x4B, 0x6B, true, true, false, false),
            Some(Key::Char('k'))
        );
    }

    #[test]
    fn map_key_capslock_does_not_affect_non_letters() {
        // CapsLock 只影响字母：数字/标点照常（Shift+数字 = 符号仍放行）
        assert_eq!(
            map_key(0x31, 0x31, false, true, false, false),
            Some(Key::Digit(1))
        );
        assert_eq!(
            map_key(0x31, 0x31, true, true, false, false),
            None,
            "Shift+数字放行"
        );
        assert_eq!(
            map_key(0xDE, 0x27, false, true, false, false),
            Some(Key::Char('\''))
        );
    }

    #[test]
    fn map_key_digits_respect_shift() {
        assert_eq!(
            map_key(0x31, 0x31, false, false, false, false),
            Some(Key::Digit(1))
        );
        assert_eq!(
            map_key(0x39, 0x39, false, false, false, false),
            Some(Key::Digit(9))
        );
        // Shift+数字 = 符号，放行给应用
        assert_eq!(map_key(0x31, 0x31, true, false, false, false), None);
    }

    #[test]
    fn map_key_arrows_released_to_keymap() {
        // 41-keymap-settings.md §11：方向键不再由 map_key 硬编码映射——会话内语义
        // 完全由 keymap 决定（route_key 组合键查表归一化为 Left/Right/PageUp 等）；
        // 未命中 → map_key 返回 None → 放行给应用。会话外本就放行。
        assert_eq!(map_key(0x25, 0x25, false, false, false, false), None);
        assert_eq!(map_key(0x27, 0x27, false, false, false, false), None);
        assert_eq!(map_key(0x26, 0x26, false, false, false, false), None);
        assert_eq!(map_key(0x28, 0x28, false, false, false, false), None);
        // Ctrl+左右 = 词跳转，放行给应用
        assert_eq!(map_key(0x25, 0x25, false, false, true, false), None);
    }

    #[test]
    fn map_key_paging_released_to_keymap() {
        // PageUp/PageDown 不再由 map_key 硬编码为翻页（清除 keymap 键位后仍翻页的
        // 根因，41-keymap-settings.md §11）：会话内由 keymap page_prev/page_next 归一化；
        // 未命中 → None → 放行给应用。
        assert_eq!(map_key(0x21, 0, false, false, false, false), None); // PageUp
        assert_eq!(map_key(0x22, 0, false, false, false, false), None); // PageDown
    }

    #[test]
    fn map_key_shift_arrows_released() {
        // Shift+←/→ 由组合键表（route_key 查 keymap）归一化为 Swap 或 放行；
        // map_key 对 Shift+方向键同样返回 None（不再有物理方向键直通路径）。
        assert_eq!(map_key(0x25, 0x25, true, false, false, false), None);
        assert_eq!(map_key(0x27, 0x27, true, false, false, false), None);
        // CapsLock 不影响方向键
        assert_eq!(map_key(0x25, 0x25, true, true, false, false), None);
        assert_eq!(map_key(0x26, 0x26, true, false, false, false), None);
        // 组合仍受 Ctrl/Alt 放行约束（Alt 是系统键收不到；Ctrl 组合放行给应用）
        assert_eq!(map_key(0x25, 0x25, true, false, true, false), None);
        assert_eq!(map_key(0x25, 0x25, true, false, false, true), None);
    }

    #[test]
    fn map_key_apostrophe() {
        assert_eq!(
            map_key(0xDE, 0x27, false, false, false, false),
            Some(Key::Char('\''))
        );
        // Shift+引号 = 双引号，放行
        assert_eq!(map_key(0xDE, 0x27, true, false, false, false), None);
        assert_eq!(map_key(0xDE, 0x22, false, false, false, false), None);
    }

    #[test]
    fn map_key_comma_period() {
        // 无 Shift：逗号/句号映射为标点键（会话内翻页、会话外打标点）
        assert_eq!(
            map_key(0xBC, 0x2C, false, false, false, false),
            Some(Key::Char(','))
        );
        assert_eq!(
            map_key(0xBE, 0x2E, false, false, false, false),
            Some(Key::Char('.'))
        );
        // Shift+逗号/句号 = < > 符号，放行
        assert_eq!(map_key(0xBC, 0x2C, true, false, false, false), None);
        assert_eq!(map_key(0xBE, 0x2E, true, false, false, false), None);
    }

    #[test]
    fn keymap_combo_lookup() {
        // 41-keymap-settings.md：会话快捷键经 Combo 查表归一化（route_key 路径）。
        // 此处直接验证 keymap 的 Combo 语义 + iuv-win combo_from_vk 的拼装。
        use iuv_core::SessionAction;
        use iuv_win::combo_from_vk;
        let cfg = iuv_core::Config::default();
        // 翻页：PageUp / , 主备两槽
        assert_eq!(
            cfg.keymap.map(&combo_from_vk(0x21, 0, false, false, false).unwrap()),
            Some(SessionAction::PagePrev)
        );
        assert_eq!(
            cfg.keymap.map(&combo_from_vk(0xBC, 0x2C, false, false, false).unwrap()),
            Some(SessionAction::PagePrev)
        );
        assert_eq!(
            cfg.keymap.map(&combo_from_vk(0x22, 0, false, false, false).unwrap()),
            Some(SessionAction::PageNext)
        );
        // 调权：Shift+←/→
        assert_eq!(
            cfg.keymap.map(&combo_from_vk(0x25, 0, true, false, false).unwrap()),
            Some(SessionAction::SwapLeft)
        );
        assert_eq!(
            cfg.keymap.map(&combo_from_vk(0x27, 0, true, false, false).unwrap()),
            Some(SessionAction::SwapRight)
        );
        // 隐藏：Shift+Delete
        assert_eq!(
            cfg.keymap.map(&combo_from_vk(0x2E, 0, true, false, false).unwrap()),
            Some(SessionAction::HideCandidate)
        );
        // 字母不入会话查表（拼音输入空间）
        assert_eq!(
            cfg.keymap.map(&combo_from_vk(0x41, 0x61, false, false, false).unwrap()),
            None
        );
        // 未绑定：F5 无映射
        assert_eq!(cfg.keymap.map(&combo_from_vk(0x74, 0, false, false, false).unwrap()), None);
    }

    #[test]
    fn session_start_keys() {
        assert!(iuv_core::is_session_start_key(Key::Char('a')));
        assert!(!iuv_core::is_session_start_key(Key::Char('\'')));
        // 大写（Shift/CapsLock）同样开启会话：Hello 的 H 进序列而非直接上屏
        assert!(iuv_core::is_session_start_key(Key::ShiftChar('A')));
        // 标点/数字/控制键不得开启会话（放行给应用）
        assert!(!iuv_core::is_session_start_key(Key::Char(',')));
        assert!(!iuv_core::is_session_start_key(Key::Char('.')));
        assert!(!iuv_core::is_session_start_key(Key::Digit(1)));
        assert!(!iuv_core::is_session_start_key(Key::Space));
    }

    #[test]
    fn caps_passthrough_letters() {
        // CapsLock 生效：大写 ShiftChar 放行（Caps 英文模式，不建会话）
        assert!(caps_passthrough(&Key::ShiftChar('H'), true));
        // Caps+Shift 反转小写 Char 同样放行
        assert!(caps_passthrough(&Key::Char('h'), true));
        // 无 Caps：Shift 单独的大写照常进序列（M2 大写保形保留）
        assert!(!caps_passthrough(&Key::ShiftChar('H'), false));
        // 非字母不受 Caps 影响（数字/标点/控制键维持原判定）
        assert!(!caps_passthrough(&Key::Digit(1), true));
        assert!(!caps_passthrough(&Key::Char(','), true));
        assert!(!caps_passthrough(&Key::Space, true));
    }

    #[test]
    fn passthrough_app_match() {
        let list = vec!["cyberpunk2077.exe".to_owned(), "dota2.exe".to_owned()];
        // 命中（精确 exe 名）
        assert!(is_passthrough_app("cyberpunk2077.exe", &list));
        assert!(is_passthrough_app("dota2.exe", &list));
        // 大小写不敏感
        assert!(is_passthrough_app("Cyberpunk2077.EXE", &list));
        assert!(is_passthrough_app("DOTA2.exe", &list));
        // 未命中 / 名单为空
        assert!(!is_passthrough_app("notepad.exe", &list));
        assert!(!is_passthrough_app("cyberpunk2077.exe", &[]));
        // 非精确子串不命中
        assert!(!is_passthrough_app("cyberpunk", &list));
        assert!(!is_passthrough_app("xcyberpunk2077.exe", &list));
    }

    #[test]
    fn map_key_control_keys() {
        assert_eq!(
            map_key(0x08, 0, false, false, false, false),
            Some(Key::Backspace)
        );
        assert_eq!(
            map_key(0x20, 0, false, false, false, false),
            Some(Key::Space)
        );
        assert_eq!(
            map_key(0x0D, 0, false, false, false, false),
            Some(Key::Enter)
        );
        assert_eq!(map_key(0x1B, 0, false, false, false, false), Some(Key::Esc));
        // 导航/翻页键由 keymap 路由（41-keymap-settings.md §11），map_key 不再硬编码
        assert_eq!(map_key(0x21, 0, false, false, false, false), None);
        assert_eq!(map_key(0x22, 0, false, false, false, false), None);
        assert_eq!(map_key(0x26, 0, false, false, false, false), None);
        assert_eq!(map_key(0x28, 0, false, false, false, false), None);
    }

    #[test]
    fn map_key_shift_delete_release() {
        // 41-keymap-settings.md：Shift+Delete → HideCandidate 由组合键表（route_key 查
        // keymap）归一化，map_key 不再映射；Shift+Delete 与裸 Delete 均返回 None（放行）。
        assert_eq!(map_key(0x2E, 0, true, false, false, false), None);
        // 裸 Delete 放行给应用（编辑删除）
        assert_eq!(map_key(0x2E, 0, false, false, false, false), None);
        // CapsLock 不影响（非字母键）
        assert_eq!(map_key(0x2E, 0, true, true, false, false), None);
        // 组合受控：Ctrl+Delete / Alt+Delete 放行
        assert_eq!(map_key(0x2E, 0, true, false, true, false), None);
        assert_eq!(map_key(0x2E, 0, true, false, false, true), None);
    }

    #[test]
    fn map_key_unknown_returns_none() {
        assert_eq!(map_key(0x10, 0, false, false, false, false), None); // Shift
        assert_eq!(map_key(0x1B, 0, false, false, false, false), Some(Key::Esc));
        assert_eq!(map_key(0x90, 0, false, false, false, false), None); // NumLock
        assert_eq!(map_key(0x00, 0, false, false, false, false), None);
    }

    #[test]
    fn map_key_modifiers_always_release() {
        // Ctrl/Alt 组合键一律放行给应用（如 Ctrl+S 保存、Alt+F4 关闭），
        // 与是否在映射表内无关（Alt 是系统键，本就收不到）。
        assert_eq!(map_key(0x53, 0x73, false, false, true, false), None); // Ctrl+S
        assert_eq!(map_key(0x53, 0x73, false, false, false, true), None); // Alt+S
        assert_eq!(map_key(0x31, 0x31, false, false, true, false), None); // Ctrl+1
        assert_eq!(map_key(0x20, 0, false, false, true, false), None); // Ctrl+Space
        assert_eq!(map_key(0x0D, 0, false, false, false, true), None); // Alt+Enter
    }

    /// 断言"某事没发生"必须配正向用例：Digit(9) 存在即 Digit 边界可控。
    #[test]
    fn map_key_digit_range_bounds() {
        // VK_0 不在 1..=9 语义内（契约 §3.4），必须放行给应用。
        assert_eq!(map_key(0x30, 0x30, false, false, false, false), None);
        assert_eq!(
            map_key(0x31, 0x31, false, false, false, false),
            Some(Key::Digit(1))
        );
    }

    #[test]
    fn fullwidth_half_mode_release() {
        // 半角：中文/英文模式全放行
        assert_eq!(
            fullwidth_pending(false, WidthMode::Half, PunctMode::Chinese, '1', false, false),
            None
        );
        assert_eq!(
            fullwidth_pending(true, WidthMode::Half, PunctMode::Chinese, 'a', false, false),
            None
        );
        // 非 ASCII / 控制字符不转
        assert_eq!(
            fullwidth_pending(true, WidthMode::Full, PunctMode::Chinese, '中', false, false),
            None
        );
        assert_eq!(
            fullwidth_pending(true, WidthMode::Full, PunctMode::Chinese, '\t', false, false),
            None
        );
    }

    #[test]
    fn fullwidth_chinese_mode_digits_symbols() {
        let f = PunctMode::Chinese;
        // 数字 → 全角
        assert_eq!(
            fullwidth_pending(false, WidthMode::Full, f, '1', false, false),
            Some("１".into())
        );
        assert_eq!(
            fullwidth_pending(false, WidthMode::Full, f, '0', false, false),
            Some("０".into())
        );
        // 非标点表符号 → 全角（含 Shift 推导：-+Shift=_ → ＿）
        assert_eq!(
            fullwidth_pending(false, WidthMode::Full, f, '/', false, false),
            Some("／".into())
        );
        assert_eq!(
            fullwidth_pending(false, WidthMode::Full, f, '-', true, false),
            Some("＿".into())
        );
        // 空格 → U+3000
        assert_eq!(
            fullwidth_pending(false, WidthMode::Full, f, ' ', false, false),
            Some("\u{3000}".into())
        );
    }

    #[test]
    fn fullwidth_chinese_mode_punct_owned_or_release() {
        // 中文标点表内符号：punct=Chinese 时归标点开关，全角不接管
        assert_eq!(
            fullwidth_pending(false, WidthMode::Full, PunctMode::Chinese, ',', false, false),
            None
        );
        assert_eq!(
            fullwidth_pending(false, WidthMode::Full, PunctMode::Chinese, '.', false, false),
            None
        );
        assert_eq!(
            fullwidth_pending(false, WidthMode::Full, PunctMode::Chinese, '[', true, false),
            None,
            "花括号在标点表（『）内，不转全角 ｛"
        );
        // punct=English：标点表不接管 → 全角接管（，→ U+FF0C）
        assert_eq!(
            fullwidth_pending(false, WidthMode::Full, PunctMode::English, ',', false, false),
            Some("，".into())
        );
        // 字母不转（照常进拼音会话）
        assert_eq!(
            fullwidth_pending(false, WidthMode::Full, PunctMode::Chinese, 'a', false, false),
            None
        );
        assert_eq!(
            fullwidth_pending(false, WidthMode::Full, PunctMode::Chinese, 'a', true, false),
            None
        );
    }

    #[test]
    fn fullwidth_english_mode_letters_digits_symbols() {
        let f = PunctMode::Chinese;
        // 字母：大小写 = Shift⊕Caps
        assert_eq!(
            fullwidth_pending(true, WidthMode::Full, f, 'a', false, false),
            Some("ａ".into())
        );
        assert_eq!(
            fullwidth_pending(true, WidthMode::Full, f, 'a', true, false),
            Some("Ａ".into())
        );
        assert_eq!(
            fullwidth_pending(true, WidthMode::Full, f, 'a', false, true),
            Some("Ａ".into())
        );
        assert_eq!(
            fullwidth_pending(true, WidthMode::Full, f, 'a', true, true),
            Some("ａ".into()),
            "Caps+Shift 反转小写"
        );
        // 数字/符号/空格
        assert_eq!(
            fullwidth_pending(true, WidthMode::Full, f, '3', false, false),
            Some("３".into())
        );
        assert_eq!(
            fullwidth_pending(true, WidthMode::Full, f, '.', false, false),
            Some("．".into())
        );
        assert_eq!(
            fullwidth_pending(true, WidthMode::Full, f, ' ', false, false),
            Some("\u{3000}".into())
        );
        // 英文模式不受标点开关影响（. 不归标点表，全角直转）
        assert_eq!(
            fullwidth_pending(true, WidthMode::Full, PunctMode::English, '.', false, false),
            Some("．".into())
        );
    }
}

#[test]
fn map_key_oem_literal_tail_keys() {
    // 冒号 = Shift + OEM_1（US 布局 ; 键）；无 Shift 是分号
    assert_eq!(
        map_key(0xBA, 0x3B, true, false, false, false),
        Some(Key::Char(':'))
    );
    assert_eq!(
        map_key(0xBA, 0x3B, false, false, false, false),
        Some(Key::Char(';'))
    );
    // 路径反斜杠 / 问号
    assert_eq!(
        map_key(0xDC, 0x5C, false, false, false, false),
        Some(Key::Char('\\'))
    );
    assert_eq!(
        map_key(0xBF, 0x2F, true, false, false, false),
        Some(Key::Char('?'))
    );
    // 死键（MapVirtualKeyW 返回 0）放行
    assert_eq!(map_key(0xBA, 0, true, false, false, false), None);
}
