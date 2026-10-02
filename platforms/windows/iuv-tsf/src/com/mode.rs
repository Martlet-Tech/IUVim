//! 模式与实例状态（P2.2 从 text_service.rs 拆出）：中英切换、会话清理、
//! 会话外标点/全角直接上屏判定、运行时四态收尾。均挂 `impl TextService`。

use iuv_core::{chinese_punct, shifted_punct, ImeState, ImeMode, ImePunct};
use windows::Win32::UI::TextServices::ITfContext;

use crate::composition::Composition;
use crate::langbar;
use crate::log::log_line;
use crate::session_bridge::fullwidth_pending;
use crate::ui::CandidateUi;

use super::text_service::TextService;

impl TextService {
    /// 当前运行时四态快照。
    pub(crate) fn runtime_snapshot(&self) -> ImeState {
        *self.runtime.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 运行时四态变化后的收尾：四态同步给服务端（会话运行时）。
    /// 过渡期不重渲当前会话（四态只影响后续键的服务端处理，当前候选重渲需另发
    /// 请求——P4 收敛时统一）。
    pub(crate) fn after_runtime_change(&self) {
        if let Some(r) = crate::com::remote_host::remote() {
            r.sync_state(&self.runtime_snapshot());
        }
    }

    /// 翻转中/英模式（Shift / 语言栏点击共用入口）。
    /// 按 OPENCLOSE compartment 值同步中英模式（OnChange / 初始化共用）。
    ///
    /// open=false（0）= 英文模式；open=true（非 0）= 中文模式。值未变化则不动
    /// （SetValue 会同步重入 OnChange，防抖避免循环）。关闭时清理活动会话。
    /// 32-toolbar §2.4：runtime.mode 镜像 OPENCLOSE（真相源）→ 工具栏中英按钮读它；
    /// 每次变化 StateSync 上报 daemon（§4.1）。
    pub(crate) fn apply_openclose(&self, open: bool) {
        let next = !open;
        if self.english_mode.load(std::sync::atomic::Ordering::SeqCst) == next {
            return;
        }
        self.english_mode
            .store(next, std::sync::atomic::Ordering::SeqCst);
        {
            let mut runtime = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
            runtime.mode = if open {
                ImeMode::Chinese
            } else {
                ImeMode::English
            };
        }
        self.punct_quote_open.set(false); // 模式切换复位引号配对（下个引号从开形起）
        log_line(&format!(
            "OPENCLOSE 变化：open={open} → {}模式",
            if next { "英文" } else { "中文" }
        ));
        // 四态同步给服务端（工具栏中英按钮重绘）：系统侧改动 OPENCLOSE 的全部入口
        // （托盘图标点击 / Shift / Ctrl+Space / 系统热键）都经此处——工具栏按钮路径
        // 下 server 已自应用同值（Ctl 应答），此同步为无害 echo。
        self.after_runtime_change();
        // 同步语言栏"中/英"图标。
        if let Some(lang_bar) = self.lang_bar.borrow().as_ref() {
            langbar::refresh_lang_bar(lang_bar);
        }
        // 关闭输入法：未确认输入按**原文上屏**语义结束（见 flush_session）。
        if !open && (self.session.borrow().is_some() || self.composition.borrow().is_some()) {
            self.flush_session();
            log_line("OPENCLOSE 关闭：活动输入已原文上屏");
        }
    }

    /// M1 桌宠：强制结束打字态（发送 `Typing{active=false}` + 复位 `was_typing`）。
    ///
    /// 供 `flush_session` / `deactivate` / `OnKillThreadFocus` 共用——保证边沿状态机
    /// 自洽：Alt+Tab 切走 → 复位；回焦后首段会话才能重新触发 `Typing(true)` transition
    /// （QA P2-B 修复：切走不复位会让 `was_typing` 卡 true，回焦后首段会话不发
    /// `Typing(true)` → 宠物一直 Idle）。
    pub(crate) fn force_typing_stop(&self) {
        if self.was_typing.get() {
            self.was_typing.set(false);
            self.notify_typing(false);
        }
    }

    /// 未确认输入以**原文上屏**并清理会话（关闭输入法 Ctrl+Space / 焦点切换 Alt+Tab 共用）。
    ///
    /// 用户语义：结束中文输入时，拼音原文提交上屏（`zhu'jin'cheng` 预编辑 →
    /// `zhujincheng`——raw 是用户敲的字母串，撇号只是切分显示层）。
    /// 修复：旧实现只清内存态不终止 TSF composition → 系统终止时带撇号的分节预览
    /// 残留在文档（实测 2026-08-14：Ctrl+Space 后 zhu'jin'cheng 残留上屏）。
    /// 文本为空（异常态）→ cancel 清空；commit/cancel 失败记日志不阻断（残留由系统终止兜底）。
    pub(crate) fn flush_session(&self) {
        self.ui.borrow_mut().hide();
        self.cand_elem.borrow_mut().end();
        // M10：原文 = 服务端 `Session::pending_text()`（picked+raw 真相源，含用户
        // 强制撇号——「composition 去撇号」近似已退役）。
        let text = crate::com::remote_host::remote().and_then(|r| r.pending_raw_text());
        if let Some(comp) = self.composition.borrow().as_ref() {
            match text.as_deref() {
                Some(t) if !t.is_empty() => match comp.commit(t) {
                    Ok(()) => log_line(&format!("会话清理：原文上屏 {t}")),
                    Err(e) => log_line(&format!("会话清理：原文上屏失败：{e}")),
                },
                _ => match comp.cancel() {
                    Ok(()) => log_line("会话清理：cancel 清空预编辑"),
                    Err(e) => log_line(&format!("会话清理：cancel 失败：{e}")),
                },
            }
        }
        *self.session.borrow_mut() = None;
        *self.composition.borrow_mut() = None;
        *self.last_effect.borrow_mut() = None;
        // 远端会话收尾（尽力而为，断线无副作用）。
        if let Some(r) = crate::com::remote_host::remote() {
            r.end_session();
        }
        // M1 桌宠：flush_session 强制结束会话 → 发 Typing(false)（若之前在打字）。
        // 与 dispatch 边沿检测互补：dispatch 走正常 end 路径；flush_session 走强制路径。
        self.force_typing_stop();
    }

    /// 会话外中文标点判定（handle_key_down 与 test_key_down **共用**，保证对称：
    /// Test 吃而 OnKeyDown 放会静默吞键，见 Caps 直通 2026-08-19 教训）。
    /// 命中 → Some(上屏文本)：中文模式 + 无会话 + `runtime.punct` 非英文标点 +
    /// 非 Ctrl/Alt 组合 + 按键字符（含 Shift 推导）命中中文标点映射。
    /// 标点开关读**实例运行时态**（32-toolbar §5.1，非引擎 config）。
    /// 内部处理引号配对状态翻转（`'`/`"` 交替开/关）。
    pub(crate) fn chinese_punct_pending(
        &self,
        char_code: u32,
        shift: bool,
        ctrl: bool,
        alt: bool,
        session_active: bool,
    ) -> Option<String> {
        if self.english_mode.load(std::sync::atomic::Ordering::SeqCst)
            || session_active
            || ctrl
            || alt
        {
            return None;
        }
        let runtime = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
        if runtime.punct == ImePunct::English {
            return None;
        }
        let base = char::from_u32(char_code)?;
        let ascii = shifted_punct(base, shift);
        let quote_open = self.punct_quote_open.get();
        let punct = chinese_punct(ascii, quote_open)?;
        if matches!(ascii, '\'' | '"') {
            self.punct_quote_open.set(!quote_open);
        }
        Some(punct.to_string())
    }

    /// 全角直接上屏判定（会话外；中/英模式统一入口，handle_key_down 与 test_key_down **共用**，
    /// 对称保证 Test 吃 OnKeyDown 也吃，防静默吞键——同 2026-08-19 Caps 直通教训）。
    /// 命中 → Some(全角文本)：`runtime.width == Full` + 非 Ctrl/Alt 组合 + 可全角化 ASCII（见
    /// `session_bridge::fullwidth_pending`：英文模式全转，中文模式数字/符号/空格、字母除外）。
    /// 宽度/标点读**实例运行时态**（32-toolbar §5.1）。
    pub(crate) fn fullwidth_pending_compute(
        &self,
        vk: u16,
        shift: bool,
        ctrl: bool,
        alt: bool,
        session_active: bool,
    ) -> Option<String> {
        if ctrl || alt || session_active {
            return None;
        }
        let base = char::from_u32(super::key_routing::char_code(vk))?;
        let runtime = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
        fullwidth_pending(
            self.english_mode.load(std::sync::atomic::Ordering::SeqCst),
            runtime.width,
            runtime.punct,
            base,
            shift,
            super::key_routing::capslock_on(),
        )
    }

    /// 会话外中文标点直接上屏：临时 composition 一次 set_text+commit（两次 edit session，
    /// 复用既有 Composition 方法；与 flush_session 原文上屏同款路径）。
    pub(crate) fn commit_punct(&self, pic: &ITfContext, text: &str) {
        let comp = Composition::new(pic.clone(), self.client_id.get(), None);
        match comp.set_text(text) {
            Ok(_) => match comp.commit(text) {
                Ok(()) => log_line(&format!("[punct] 中文标点直接上屏 {text}")),
                Err(e) => log_line(&format!("[punct] commit 失败：{e}")),
            },
            Err(e) => log_line(&format!("[punct] set_text 失败：{e}")),
        }
    }
}
