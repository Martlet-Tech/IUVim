//! 全局热键管理（41-keymap-settings.md §4）：daemon `RegisterHotKey` 注册 keymap 的
//! 全局六功能（主/备两槽），WM_HOTKEY 在工具栏窗口（消息泵线程）收 → 复用 on_click 分派。
//!
//! 与 TSF 完全独立：注册的是 daemon 进程的全局热键（普通软件做法，启动器同款），
//! 命中时系统吞键、不再下发给前台应用/IME——Alt/Ctrl 随便绑（§2 核心架构）。
//!
//! id 编号：`(GlobalAction as u8) << 1 | slot`（slot：0=主 1=备），2 字节内可逆，
//! WM_HOTKEY 的 wParam 即 id，直接解出动作。
//!
//! 注册失败（组合已被其他软件占用）→ 记日志；设置页保存时同步提示（见 settings.rs）。

use iuv_core::{Combo, Keymap};
use iuv_win::{base_key_to_vk, combo_mods};
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    RegisterHotKey, UnregisterHotKey, HOT_KEY_MODIFIERS,
};

pub use iuv_core::GlobalAction;

use crate::log::log_line;

/// 全局动作 → 功能说明（设置页/日志展示）。
pub fn global_action_label(a: GlobalAction) -> &'static str {
    match a {
        GlobalAction::ToggleMode => "中英切换",
        GlobalAction::ToggleWidth => "全角/半角",
        GlobalAction::ToggleScript => "简体/繁体",
        GlobalAction::TogglePunct => "中文标点",
        GlobalAction::OpenSettings => "打开设置",
        GlobalAction::ToggleToolbar => "显示/隐藏工具栏",
    }
}

/// 热键 id：动作高位 + 槽位低位。
fn hotkey_id(a: GlobalAction, secondary: bool) -> i32 {
    ((a as u8 as i32) << 1) | (secondary as i32)
}

/// 从 WM_HOTKEY wParam 解出 (动作, 槽位)。非法 id → None（防御）。
pub fn hotkey_from_id(id: usize) -> Option<(GlobalAction, bool)> {
    let a = match (id >> 1) as u8 {
        0 => GlobalAction::ToggleMode,
        1 => GlobalAction::ToggleWidth,
        2 => GlobalAction::ToggleScript,
        3 => GlobalAction::TogglePunct,
        4 => GlobalAction::OpenSettings,
        5 => GlobalAction::ToggleToolbar,
        _ => return None,
    };
    Some((a, id & 1 == 1))
}

/// 注册全部全局热键（keymap 全局六动作 × 主/备两槽）。目标窗口 = 工具栏窗口
/// （消息泵线程），WM_HOTKEY 由 bar_wnd_proc 收取。
/// 返回 (成功, 失败) 计数——失败（被占用/无 vk）记日志但不阻断。
pub fn register_all(hwnd: HWND, keymap: &Keymap) -> (usize, usize) {
    let mut ok = 0usize;
    let mut fail = 0usize;
    let actions = [
        GlobalAction::ToggleMode,
        GlobalAction::ToggleWidth,
        GlobalAction::ToggleScript,
        GlobalAction::TogglePunct,
        GlobalAction::OpenSettings,
        GlobalAction::ToggleToolbar,
    ];
    for a in actions {
        let slot = keymap.global_slot(a);
        for (i, combo) in slot.iter().enumerate() {
            let secondary = i == 1;
            if register_one(hwnd, a, *combo, secondary) {
                ok += 1;
            } else {
                fail += 1;
            }
        }
    }
    (ok, fail)
}

/// 注册单个热键。组合不可绑（无 vk / 无修饰——全局热键必须有修饰键，防劫持字母）
/// 或 RegisterHotKey 失败 → false。
fn register_one(hwnd: HWND, a: GlobalAction, combo: Combo, secondary: bool) -> bool {
    let Some(vk) = base_key_to_vk(&combo.base) else {
        log_line(&format!(
            "[hotkey] {} {} 无 vk，跳过",
            global_action_label(a),
            combo.name()
        ));
        return false;
    };
    if !combo.has_modifier() {
        log_line(&format!(
            "[hotkey] {} {} 无修饰键，跳过（全局热键必须 ≥1 修饰，防全系统劫持字母/数字）",
            global_action_label(a),
            combo.name()
        ));
        return false;
    }
    let id = hotkey_id(a, secondary);
    // SAFETY: hwnd 为工具栏窗口（消息泵线程存活期间有效）；id/修饰/vk 均为合法值。
    let r = unsafe {
        RegisterHotKey(Some(hwnd), id, HOT_KEY_MODIFIERS(combo_mods(&combo)), vk as u32)
    };
    match r {
        Ok(()) => {
            log_line(&format!(
                "[hotkey] 已注册 {} {} (id={id})",
                global_action_label(a),
                combo.name()
            ));
            true
        }
        Err(e) => {
            log_line(&format!(
                "[hotkey] 注册失败 {} {} (id={id})：{e}（组合被占用/系统限制）",
                global_action_label(a),
                combo.name()
            ));
            false
        }
    }
}

/// 注销全部全局热键（keymap 变化全量重注册前调用）。防御性 Unregister（未注册 id 也调，
/// 返回错误忽略）。
pub fn unregister_all(hwnd: HWND) {
    let actions = [
        GlobalAction::ToggleMode,
        GlobalAction::ToggleWidth,
        GlobalAction::ToggleScript,
        GlobalAction::TogglePunct,
        GlobalAction::OpenSettings,
        GlobalAction::ToggleToolbar,
    ];
    for a in actions {
        for secondary in [false, true] {
            let id = hotkey_id(a, secondary);
            // SAFETY: hwnd 存活期同上；注销不存在的 id 返回错误，忽略。
            let _ = unsafe { UnregisterHotKey(Some(hwnd), id) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hotkey_id_roundtrip() {
        for a in [
            GlobalAction::ToggleMode,
            GlobalAction::ToggleWidth,
            GlobalAction::ToggleScript,
            GlobalAction::TogglePunct,
            GlobalAction::OpenSettings,
            GlobalAction::ToggleToolbar,
        ] {
            for secondary in [false, true] {
                let id = hotkey_id(a, secondary);
                assert_eq!(hotkey_from_id(id as usize), Some((a, secondary)));
            }
        }
        assert_eq!(hotkey_from_id(0xFF), None, "越界 id 防御");
    }

    #[test]
    fn labels_cover_all() {
        for a in [
            GlobalAction::ToggleMode,
            GlobalAction::ToggleWidth,
            GlobalAction::ToggleScript,
            GlobalAction::TogglePunct,
            GlobalAction::OpenSettings,
            GlobalAction::ToggleToolbar,
        ] {
            assert!(!global_action_label(a).is_empty());
        }
    }
}
