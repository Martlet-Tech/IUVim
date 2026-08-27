//! 游戏式按键录入（41-keymap-settings.md §5）：设置页点击录入框 → 装 `WH_KEYBOARD_LL`
//! 低层键盘钩子 → 捕获下一组合键（Alt/Ctrl/Shift/Win 全收）→ 回填槽位。
//!
//! 与输入法作为 IME 时的按键捕捉机制**完全不同**（管理员要点，2026-08-27）：
//! IME 走 TSF 键 sink（收不到 WM_SYSKEYDOWN → Alt 组合死路）；录入走的是普通软件
//! 的低层钩子——Alt 随便绑。钩子回调吞键（return 1）防止漏进设置窗/焦点应用。
//!
//! 线程模型：钩子须装在**有消息泵的线程**（回调由该线程 pump 触发）。设置窗跑在
//! daemon 主线程（eframe/winit 消息循环），故在主线程安装/卸载。回调经共享状态写
//! 捕获结果 + 置位请求重绘标志（egui ctx.request_repaint 由调用方接线）。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use iuv_core::Combo;
use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, KBDLLHOOKSTRUCT, SetWindowsHookExW, UnhookWindowsHookEx, WH_KEYBOARD_LL,
};

/// 捕获结果：`None` = 等待中 / Esc 取消；`Some(None)` = Backspace 清除该槽；
/// `Some(Some(combo))` = 捕获到组合键。
#[derive(Clone, Debug)]
pub enum CaptureOutcome {
    Captured(Combo),
    Clear,
    Cancel,
}

/// 捕获会话输出（设置页与钩子回调共享）。
#[derive(Default)]
pub struct CaptureState {
    pub outcome: Mutex<Option<CaptureOutcome>>,
    pub request_repaint: AtomicBool,
    pub installed: AtomicBool,
}

/// 钩子句柄槽（SetWindowsHookEx 返回值；卸载后置 None）。
static HOOK: Mutex<Option<HhookSlot>> = Mutex::new(None);
/// 捕获状态全局槽（回调经此写结果；同一时刻至多一个录入会话）。
static STATE: Mutex<Option<Arc<CaptureState>>> = Mutex::new(None);

/// HHOOK 包装（SetWindowsHookEx 返回；卸载在主线程）。HHOOK 含 `*mut c_void`
/// 不自动 Send——仅存于 static Mutex（跨线程锁访问），unsafe impl Send 声明
/// 使用场景（安装/卸载均在主线程，跨线程只读 Option）安全。
struct HhookSlot(windows::Win32::UI::WindowsAndMessaging::HHOOK);
unsafe impl Send for HhookSlot {}

/// 开始捕获：清空结果 + 装钩子（幂等）。返回是否成功装钩。
pub fn begin(state: Arc<CaptureState>) -> bool {
    // 若已有会话：先卸载旧的（防御——正常 UI 流不会并发，但双开设置窗可能）。
    end();
    *state.outcome.lock().unwrap_or_else(|p| p.into_inner()) = None;
    state.request_repaint.store(false, Ordering::SeqCst);
    state.installed.store(false, Ordering::SeqCst);
    {
        let mut s = STATE.lock().unwrap_or_else(|p| p.into_inner());
        *s = Some(state.clone());
    }
    let h = install_hook();
    match h {
        Some(hk) => {
            {
                let mut h = HOOK.lock().unwrap_or_else(|p| p.into_inner());
                *h = Some(hk);
            }
            state.installed.store(true, Ordering::SeqCst);
            true
        }
        None => {
            let mut s = STATE.lock().unwrap_or_else(|p| p.into_inner());
            *s = None;
            false
        }
    }
}

/// 结束捕获：卸钩 + 复位。
pub fn end() {
    if let Some(state) = STATE.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
        state.installed.store(false, Ordering::SeqCst);
    }
    let mut h = HOOK.lock().unwrap_or_else(|p| p.into_inner());
    if let Some(hk) = h.take() {
        // SAFETY: 钩子在主线程安装，卸载也在主线程（end 由调用方主线程调）。
        let _ = unsafe { UnhookWindowsHookEx(hk.0) };
    }
}

fn install_hook() -> Option<HhookSlot> {
    // SAFETY: WH_KEYBOARD_LL 全局低层钩子；lpfn 为静态 extern "system" 回调
    // （hook_proc）；dwthreadid = 0（全局）。主线程 pump 消息时回调被调用。
    // 钩子回调不持有 Rust 借用，仅经原子/互斥写全局状态。
    let h = unsafe { SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0) };
    match h {
        Ok(hk) => Some(HhookSlot(hk)),
        Err(_) => None,
    }
}

/// WH_KEYBOARD_LL 回调：捕获组合键（吞键 return 1）。逻辑：
/// - 忽略释放事件（LLKHF_UP）与纯修饰键按下（等待与其组合的基础键）；
/// - Esc → Cancel（槽位不变）；Backspace → Clear（清除该槽）；
/// - 捕获 vk + 修饰键态 → 构造 Combo；纯字母无修饰 → 拒绝继续等（防吃掉拼音）。
pub unsafe extern "system" fn hook_proc(
    code: i32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if code >= 0 {
        let Some(state) = STATE.lock().unwrap_or_else(|p| p.into_inner()).clone() else {
            return CallNextHookEx(None, code, wparam, lparam);
        };
        if state.installed.load(Ordering::SeqCst) {
            let kb = &*(lparam.0 as *const KBDLLHOOKSTRUCT);
            let is_up = (kb.flags.0 & 128) != 0; // LLKHF_UP
            if !is_up {
                let vk = kb.vkCode;
                let alt = (kb.flags.0 & 32) != 0; // LLKHF_ALTDOWN
                let is_modifier = matches!(vk, 0x10 | 0x11 | 0x12 | 0x5B | 0x5C);
                // Esc 取消
                if vk == 0x1B {
                    *state.outcome.lock().unwrap_or_else(|p| p.into_inner()) =
                        Some(CaptureOutcome::Cancel);
                    state.request_repaint.store(true, Ordering::SeqCst);
                    end();
                    return LRESULT(1);
                }
                // Backspace 清除该槽
                if vk == 0x08 {
                    *state.outcome.lock().unwrap_or_else(|p| p.into_inner()) =
                        Some(CaptureOutcome::Clear);
                    state.request_repaint.store(true, Ordering::SeqCst);
                    end();
                    return LRESULT(1);
                }
                if is_modifier {
                    return LRESULT(1); // 吞掉修饰键按下（等基础键）
                }
                let char_code = {
                    // SAFETY: MapVirtualKeyW 纯查询
                    unsafe {
                        windows::Win32::UI::Input::KeyboardAndMouse::MapVirtualKeyW(
                            vk as u32,
                            windows::Win32::UI::Input::KeyboardAndMouse::MAPVK_VK_TO_CHAR,
                        ) & 0xFFFF
                    }
                };
                let shift = (unsafe {
                    windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(0x10)
                }) < 0;
                let ctrl = (unsafe {
                    windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(0x11)
                }) < 0;
                let win = {
                    let l = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(0x5B) };
                    let r = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetKeyState(0x5C) };
                    l < 0 || r < 0
                };
                let base = iuv_win::vk_to_base_key(vk as u16, char_code);
                if let Some(base) = base {
                    let combo = Combo {
                        ctrl,
                        alt,
                        shift,
                        win,
                        base,
                    };
                    // 纯字母无修饰 → 拒绝（会吃掉拼音/全局劫持）；继续等待
                    if combo.has_modifier() || !combo.base_is_letter() {
                        *state.outcome.lock().unwrap_or_else(|p| p.into_inner()) =
                            Some(CaptureOutcome::Captured(combo));
                        state.request_repaint.store(true, Ordering::SeqCst);
                        end();
                        return LRESULT(1);
                    }
                }
                return LRESULT(1); // 未形成有效组合：吞键继续等
            }
        }
    }
    CallNextHookEx(None, code, wparam, lparam)
}

#[cfg(test)]
mod tests {
    use super::*;
    use iuv_core::Key;

    #[test]
    fn capture_state_defaults() {
        let s = CaptureState::default();
        assert!(s.outcome.lock().unwrap().is_none());
        assert!(!s.installed.load(Ordering::SeqCst));
    }

    #[test]
    fn outcome_variants() {
        let c = Combo::shifted(Key::Left);
        let o = CaptureOutcome::Captured(c);
        match o {
            CaptureOutcome::Captured(c2) => assert_eq!(c2, c),
            _ => panic!("应为 Captured"),
        }
    }
}
