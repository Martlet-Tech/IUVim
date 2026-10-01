//! 按键路由（P2.2 从 text_service.rs 拆出）：`test_key_down`/`handle_key_down`
//! 判定与处理 + 键盘状态辅助函数（`char_code`/`capslock_on` 等，mode.rs 共用）。

use std::rc::Rc;

use iuv_core::{is_session_start_key, Key};
use iuv_win::combo_from_vk;
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, MapVirtualKeyW, MAPVK_VK_TO_CHAR, VK_CAPITAL, VK_SHIFT,
};
use windows::Win32::UI::TextServices::ITfContext;

use crate::composition::Composition;
use crate::log::{self, log_line, perf_record_with, perf_tick};
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
        // 透明模式：全部放行（M10：远端客户端副本未就绪 = 服务端未连接，
        // 语义同原「引擎加载中」）。
        let Some(config) = crate::com::remote_host::backend_config() else {
            return KeyAction::Pass;
        };
        // P4：配置更新 PUSH 驱动——按键路径只剩进程内原子量比较的主题收敛，
        // 零 SHM/IPC/文件读。
        self.daemon_poll_tick();

        let shift = shift_pressed();
        let ctrl = ctrl_pressed();
        let alt = alt_pressed();
        // 小键盘数字（VK_NUMPAD0..9）：NumLock 开 → 归一为主行数字 VK（两者
        // char_code 相同，后续组合键/映射判定统一）；NumLock 关 → 保持原 VK
        // （map_key 不映射 → 放行，End/方向键等导航语义照旧）。
        let vk = match vk {
            0x60..=0x69 if numlock_on() => vk - 0x30,
            _ => vk,
        };
        // M10：会话活性 = last_effect（远端增量语义的基线槽）。
        let session_active = self.last_effect.borrow().is_some();

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
        if let Some(punct) =
            self.chinese_punct_pending(char_code(vk), shift, ctrl, alt, session_active)
        {
            return KeyAction::CommitText(punct);
        }

        // 全角（会话外数字/符号/空格直接上屏全角；字母不在此列，照常进拼音会话）。
        if let Some(text) = self.fullwidth_pending_compute(vk, shift, ctrl, alt, session_active) {
            return KeyAction::CommitText(text);
        }

        // —— 会话内：先查组合键表（41-keymap-settings.md）——
        // 由 (vk, shift, ctrl, alt) 构造 Combo → keymap 命中会话动作 → 归一化消费。
        // Ctrl/Alt 组合 combo_from_vk 直接返回 None（红线：放行给应用/Alt 不进 sink）；
        // 字母基础键跳过（恒走拼音输入，不参与会话快捷键——验证层已禁，此处兜底）。
        // 未命中 → 落回 map_key（字母/数字/标点正常处理）。
        if session_active {
            if let Some(combo) = combo_from_vk(vk, char_code(vk), shift, ctrl, alt) {
                if !combo.base_is_letter() {
                    if let Some(action) = config.keymap.map(&combo) {
                        log_line(&format!(
                            "[key] 组合键命中：{} → {:?}",
                            combo.name(),
                            action
                        ));
                        return KeyAction::SessionKey(action.key());
                    }
                }
            }
        }

        let caps = capslock_on();
        let key = map_key(vk, char_code(vk), shift, caps, ctrl, alt);
        let Some(key) = key else {
            return KeyAction::Pass;
        };
        if !session_active {
            // 开启新会话：仅字母键；CapsLock 生效时字母放行直通（仿微软：Caps = 英文模式，
            // 不建会话；会话内 Caps 字母照常进序列，避免 composition 残留错乱）。
            if !is_session_start_key(key) || caps_passthrough(&key, caps) {
                return KeyAction::Pass;
            }
            return KeyAction::StartSession(key);
        }
        // 会话内按键（未命中组合键表）：字母/数字/标点正常推进会话。
        KeyAction::SessionKey(key)
    }

    /// OnTestKeyDown 判定：本键是否由本输入法消费。
    /// M10：Test 阶段**真正处理**（§4.5.1 去重的第一半）——发请求并缓存裁定；
    /// 请求失败（超时/断线）返回 false 放行，绝不"Test 吃了 Down 却放"。
    pub(crate) fn test_key_down(&self, wparam: WPARAM, _lparam: LPARAM) -> bool {
        let vk = wparam.0 as u16;
        let action = self.route_key(vk);
        if matches!(action, KeyAction::Pass) {
            return false;
        }
        if let Some(key) = Self::action_key(&action) {
            let Some(remote) = crate::com::remote_host::remote() else {
                return false;
            };
            let mods = crate::com::remote_host::wire_mods(
                shift_pressed(),
                ctrl_pressed(),
                alt_pressed(),
            );
            if remote.key_test(key, mods).is_none() {
                return false; // 超时/断线：放行（宁可漏吃不可吞键）
            }
        }
        true
    }

    /// KeyAction 中的会话键（CommitText/Pass 无引擎交互）。
    fn action_key(action: &KeyAction) -> Option<Key> {
        match action {
            KeyAction::StartSession(k) | KeyAction::SessionKey(k) => Some(*k),
            _ => None,
        }
    }

    /// OnKeyDown 完整处理：映射 → 会话推进 → 应用 Effect。
    pub(crate) fn handle_key_down(
        &self,
        pic: &ITfContext,
        wparam: WPARAM,
        _lparam: LPARAM,
    ) -> bool {
        let vk = wparam.0 as u16;
        let t_route = perf_tick();
        let action = self.route_key(vk);
        // 计时区间必须只包 route_key：dispatch 在下方 match 分支里，若被圈进来
        // 这一列就成了「整键总耗时」（实测 30904us ≈ onkey+settext+render+dispatch 之和）。
        perf_record_with("route", t_route, || format!("vk={vk:#x}"));
        let handled = match action {
            KeyAction::Pass => false,
            KeyAction::CommitText(text) => {
                self.commit_punct(pic, &text);
                true
            }
            KeyAction::StartSession(key) => {
                let Some(remote) = crate::com::remote_host::remote().filter(|r| r.ready()) else {
                    return false;
                };
                log_line(&format!("[key] 按键：{}（远端会话外）", key.name()));
                remote.sync_state(&self.runtime_snapshot());
                self.punct_quote_open.set(false); // 拼音输入开始：引号配对复位为开形
                // P4 服务端渲染：会话首键先上报插入点锚点（composition 尚不存在，
                // selection 量取）→ 服务端首帧候选即定位正确；打字期锚点恒定，
                // 后续只在变化时上报（dispatch/follow_layout）。
                if let Some(c) =
                    crate::composition::query_insertion_caret(pic, self.client_id.get())
                {
                    self.caret.set(c);
                    self.caret_reported.set(c);
                    remote.sync_caret(c);
                }
                let mods = crate::com::remote_host::wire_mods(
                    shift_pressed(),
                    ctrl_pressed(),
                    alt_pressed(),
                );
                let Some(outcome) = remote.key_down(key, mods) else {
                    return false; // 超时/断线：放行（§4.5.2）
                };
                let last_effect = self.last_effect.clone();
                *self.composition.borrow_mut() = Some(Composition::new(
                    pic.clone(),
                    self.client_id.get(),
                    Some(Rc::new(move || {
                        // composition 被外部终止（焦点切换等）：远端会话立即收尾，
                        // 下一键以全新会话开始（不再走会话内路径吞键）。
                        last_effect.borrow_mut().take();
                        if let Some(r) = crate::com::remote_host::remote() {
                            r.end_session();
                        }
                    })),
                ));
                self.dispatch_outcome(outcome);
                true
            }
            KeyAction::SessionKey(key) => {
                log_line(&format!("[key] 按键：{}（远端会话内）", key.name()));
                let mods = crate::com::remote_host::wire_mods(
                    shift_pressed(),
                    ctrl_pressed(),
                    alt_pressed(),
                );
                let Some(outcome) =
                    crate::com::remote_host::remote().and_then(|r| r.key_down(key, mods))
                else {
                    return false;
                };
                self.dispatch_outcome(outcome);
                true
            }
        };
        handled
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

/// NumLock 是否生效（切换状态位，决定小键盘数字键的归一，见 route_key）。
fn numlock_on() -> bool {
    // SAFETY: GetKeyState 对 VK_NUMLOCK(0x90) 返回切换状态（最低位 1 = 生效）。
    (unsafe { GetKeyState(0x90) }) & 1 != 0
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
