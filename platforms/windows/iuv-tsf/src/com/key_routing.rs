//! 按键路由（P2.2 从 text_service.rs 拆出）：`test_key_down`/`handle_key_down`
//! 判定与处理 + 键盘状态辅助函数（`char_code`/`capslock_on` 等，mode.rs 共用）。

use iuv_core::{apply_keymap, is_session_start_key, Key};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, MapVirtualKeyW, MAPVK_VK_TO_CHAR, VK_CAPITAL, VK_SHIFT,
};
use windows::Win32::UI::TextServices::ITfContext;

use crate::composition::Composition;
use crate::com::engine_host::engine;
use crate::log::{self, log_line};
use crate::session_bridge::{caps_passthrough, is_passthrough_app, map_key};

use super::text_service::TextService;

/// 按键路由判定结果（P2.3：`route_key` 产出，test/handle 共用——消灭约 60 行
/// 对称复制；Test 阶段"是否消费"与 OnKeyDown 阶段"如何处理"由同一判定驱动）。
pub(crate) enum KeyAction {
    /// 放行给应用（不消费）。
    Pass,
    /// 会话外直接上屏文本（全角 / 中文标点）。
    CommitText(String),
    /// 开启新会话并喂第一键。
    StartSession(Key),
    /// 会话内按键（keymap 已应用，交由会话推进）。
    SessionKey(Key),
}

impl TextService {
    /// 按键路由唯一判定点：透明模式/直通名单/英文全角/中文标点/全角直接上屏/
    /// 会话开关，全部决策收敛于此。含 M6 daemon 轮询副作用（config_epoch 热载、
    /// 实例重注册；Test 阶段即消费，见 test_key_down 注释）。
    ///
    /// test_key_down 与 handle_key_down **必须**共用同一判定（对称保证）：
    /// 应用在 OnTestKeyDown 返回 eaten 时即跳过自己的按键处理，若 Test 吃而
    /// OnKeyDown 放，字母会被静默吞掉（实测 2026-08-19：Caps 直通失效）。
    fn route_key(&self, vk: u16) -> KeyAction {
        // 透明模式：全部放行。
        let Some(engine) = engine() else { return KeyAction::Pass };
        // M6：daemon 共享段轮询（低成本：读 u32 版本；用户库版本/配置纪元变化 → 即时生效；
        // 离线→在线翻转重注册）。daemon_poll_tick 唯一触发点在按键路径。
        self.daemon_poll_tick();
        let config = engine.config();

        let shift = shift_pressed();
        let ctrl = ctrl_pressed();
        let alt = alt_pressed();
        let session_active = self.session.borrow().is_some();

        // 按键直通白名单：命中进程全部按键放行（不建会话/无候选窗/不转全角，
        // 输入法在该进程完全透明），名单为空零开销。
        if !config.passthrough_apps.is_empty()
            && is_passthrough_app(&log::module_name(), &config.passthrough_apps)
        {
            return KeyAction::Pass;
        }

        if self.english_mode.load(std::sync::atomic::Ordering::SeqCst) {
            // 英文模式 + 全角：ASCII 直接上屏全角（ｍｉｃｒｏｓｏｆｔ１２３），否则放行。
            return match self.fullwidth_pending_compute(vk, shift, ctrl, alt, session_active) {
                Some(text) => KeyAction::CommitText(text),
                None => KeyAction::Pass,
            };
        }

        // 中文标点（会话外直接上屏全角）：判定与 test_key_down 对称。
        if let Some(punct) = self.chinese_punct_pending(char_code(vk), shift, ctrl, alt, session_active)
        {
            return KeyAction::CommitText(punct);
        }

        // 全角（会话外数字/符号/空格直接上屏全角；字母不在此列，照常进拼音会话）。
        if let Some(text) = self.fullwidth_pending_compute(vk, shift, ctrl, alt, session_active) {
            return KeyAction::CommitText(text);
        }

        let caps = capslock_on();
        let key = map_key(vk, char_code(vk), shift, caps, ctrl, alt);
        let Some(key) = key else { return KeyAction::Pass };
        if self.session.borrow().is_none() {
            // 开启新会话：仅字母键；CapsLock 生效时字母放行直通（仿微软：Caps = 英文模式，
            // 不建会话；会话内 Caps 字母照常进序列，避免 composition 残留错乱）。
            if !is_session_start_key(key) || caps_passthrough(&key, caps) {
                return KeyAction::Pass;
            }
            return KeyAction::StartSession(key);
        }
        // 会话内按键：先应用快捷键映射（翻页键重映射），映射键一律消费。
        KeyAction::SessionKey(apply_keymap(key, &config.keymap))
    }

    /// OnTestKeyDown 判定（无副作用）：本键是否由本输入法消费。
    pub(crate) fn test_key_down(&self, wparam: WPARAM, _lparam: LPARAM) -> bool {
        let vk = wparam.0 as u16;
        !matches!(self.route_key(vk), KeyAction::Pass)
    }

    /// OnKeyDown 完整处理：映射 → 会话推进 → 应用 Effect。
    pub(crate) fn handle_key_down(&self, pic: &ITfContext, wparam: WPARAM, _lparam: LPARAM) -> bool {
        let vk = wparam.0 as u16;
        // M6：远端写后端在 Activate 注册；引擎后台加载未完成则此处补注册（幂等，无副作用）。
        // 必须先于 route_key 的 daemon 轮询（poll 回调可能重载引擎配置/重注册实例）。
        if let Some(engine) = engine() {
            if let Some(client) = self.daemon.borrow().as_ref() {
                if !self.remote_registered.get() {
                    engine.set_user_remote(Some(client.clone()));
                    self.remote_registered.set(true);
                }
            }
        }
        match self.route_key(vk) {
            KeyAction::Pass => false,
            KeyAction::CommitText(text) => {
                self.commit_punct(pic, &text);
                true
            }
            KeyAction::StartSession(key) => {
                let engine = engine().expect("route_key 已校验引擎非透明模式");
                log_line(&format!("[key] 按键：{}（会话外）", key.name()));
                // 注入实例运行时四态（32-toolbar §5.1：per-实例，live 读）。
                let mut session = engine.start_session_with_runtime(self.runtime.clone());
                self.punct_quote_open.set(false); // 拼音输入开始：引号配对复位为开形
                let effect = session.on_key(key);
                *self.session.borrow_mut() = Some(session);
                *self.composition.borrow_mut() =
                    Some(Composition::new(pic.clone(), self.client_id.get()));
                self.dispatch(&effect);
                true
            }
            KeyAction::SessionKey(key) => {
                log_line(&format!(
                    "[key] 按键：{}（会话内）",
                    key.name()
                ));
                let effect = self
                    .session
                    .borrow_mut()
                    .as_mut()
                    .map(|s| s.on_key(key))
                    .expect("会话存在性已由 route_key 判定");
                self.dispatch(&effect);
                true
            }
        }
    }
}

/// 当前 Shift 是否按下（GetKeyState 高位，返回 SHORT）。
fn shift_pressed() -> bool {
    // SAFETY: GetKeyState 查询当前线程键盘状态，返回符号位表示按下。
    (unsafe { GetKeyState(VK_SHIFT.0 as i32) }) < 0
}

/// CapsLock 是否生效（切换状态位，与消息队列无关）。
pub(crate) fn capslock_on() -> bool {
    // SAFETY: GetKeyState 对 VK_CAPITAL 返回切换状态（最低位 1 = 生效）。
    (unsafe { GetKeyState(VK_CAPITAL.0 as i32) }) & 1 != 0
}

/// 当前 Ctrl 是否按下。Ctrl/Alt 组合键一律放行给应用（map_key 内约定）。
fn ctrl_pressed() -> bool {
    // SAFETY: 同上；VK_CONTROL 无.0 常量，用 0x11 字面量。
    (unsafe { GetKeyState(0x11) }) < 0
}

/// 当前 Alt 是否按下。
fn alt_pressed() -> bool {
    // SAFETY: 同上；VK_MENU 无.0 常量，用 0x12 字面量。
    (unsafe { GetKeyState(0x12) }) < 0
}

/// 无副作用的字符映射：MapVirtualKeyW(MAPVK_VK_TO_CHAR) 给出该键的无 Shift 字符值。
pub(crate) fn char_code(vk: u16) -> u32 {
    // SAFETY: MapVirtualKeyW 是纯查询，返回 0 表示无对应字符（死键等）。
    unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_CHAR) & 0xFFFF }
}