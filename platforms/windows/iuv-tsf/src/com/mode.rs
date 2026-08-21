//! 模式与实例状态（P2.2 从 text_service.rs 拆出）：中英切换、会话清理、
//! 会话外标点/全角直接上屏判定、运行时四态收尾。均挂 `impl TextService`。

use iuv_core::{chinese_punct, shifted_punct, ImeState, InitialMode, PunctMode};
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

    /// 运行时四态变化后的收尾：live 重渲当前会话（点简繁/全半角/标点立即生效）+ StateSync 上报。
    pub(crate) fn after_runtime_change(&self) {
        // 当前会话重渲：effect() 内部 live 读 runtime，切换后候选/预编辑立即跟随。
        if let Some(sess) = self.session.borrow().as_ref() {
            let effect = sess.effect();
            self.dispatch(&effect);
        }
        // 上报 daemon 看板（§4.1 StateSync）。
        if let Some(client) = self.daemon.borrow().as_ref() {
            let (pid, tid) = self.instance_id();
            client.state_sync(pid, tid, self.runtime_snapshot().to_toolbar().into());
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
                InitialMode::Chinese
            } else {
                InitialMode::English
            };
        }
        self.punct_quote_open.set(false); // 模式切换复位引号配对（下个引号从开形起）
        log_line(&format!(
            "OPENCLOSE 变化：open={open} → {}模式",
            if next { "英文" } else { "中文" }
        ));
        // 同步语言栏"中/英"图标。
        if let Some(lang_bar) = self.lang_bar.borrow().as_ref() {
            langbar::refresh_lang_bar(lang_bar);
        }
        // 工具栏看板同步（中英钮真相源 OnChange → StateSync）。
        if let Some(client) = self.daemon.borrow().as_ref() {
            let (pid, tid) = self.instance_id();
            client.state_sync(pid, tid, self.runtime_snapshot().to_toolbar().into());
        }
        // 关闭输入法：未确认输入按**原文上屏**语义结束（见 flush_session）。
        if !open && (self.session.borrow().is_some() || self.composition.borrow().is_some()) {
            self.flush_session();
            log_line("OPENCLOSE 关闭：活动输入已原文上屏");
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
        let text: Option<String> = self.session.borrow().as_ref().map(|s| s.pending_text());
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
        if self.english_mode.load(std::sync::atomic::Ordering::SeqCst) || session_active || ctrl || alt {
            return None;
        }
        let runtime = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
        if runtime.punct == PunctMode::English {
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
        let comp = Composition::new(pic.clone(), self.client_id.get());
        match comp.set_text(text) {
            Ok(_) => match comp.commit(text) {
                Ok(()) => log_line(&format!("[punct] 中文标点直接上屏 {text}")),
                Err(e) => log_line(&format!("[punct] commit 失败：{e}")),
            },
            Err(e) => log_line(&format!("[punct] set_text 失败：{e}")),
        }
    }
}
