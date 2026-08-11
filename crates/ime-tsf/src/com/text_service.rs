//! ITfTextInputProcessorEx / ITfKeyEventSink / ITfThreadMgrEventSink 实现。
//! 契约 01-contract.md §7 与 13 任务书 §3.3。
//! 【Agent D】W1 实现。
//!
//! 时序：Activate → AdviseKeyEventSink / AdviseSink；按键经 session_bridge 映射进
//! ime_core::Session，Effect 由 composition + CandidateUi 应用；Deactivate 反向清理。

use std::cell::{Cell, RefCell};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::sync::OnceLock;

use ime_core::{apply_keymap, is_session_start_key, Config, Engine, Session};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, MapVirtualKeyW, MAPVK_VK_TO_CHAR, VK_SHIFT,
};
use windows::Win32::UI::TextServices::{
    ITfContext, ITfDocumentMgr, ITfKeyEventSink, ITfKeyEventSink_Impl, ITfKeystrokeMgr,
    ITfTextInputProcessor_Impl, ITfTextInputProcessorEx, ITfTextInputProcessorEx_Impl,
    ITfThreadMgr, ITfThreadMgrEventSink, ITfThreadMgrEventSink_Impl, ITfSource,
};
use windows_core::{implement, BOOL, Interface, IUnknownImpl, Ref, Result};

use crate::composition::Composition;
use crate::log::log_line;
use crate::session_bridge::{apply_effect, map_key};
use crate::ui::{GdiCandidateWindow, CandidateUi};
use crate::ui::CaretRect;

    /// 进程级引擎单例（契约 §7：`OnceLock<Arc<Engine>>`）。
    /// 词典加载失败 → None = 透明模式（全部按键放行，绝不卡用户）。
    static ENGINE: OnceLock<Option<Arc<Engine>>> = OnceLock::new();

/// 全局活动对象计数（DllCanUnloadNow 用）：实例创建 +1，Drop −1。
static INSTANCE_COUNT: AtomicU32 = AtomicU32::new(0);

pub(crate) fn instance_count() -> u32 {
    INSTANCE_COUNT.load(Ordering::SeqCst)
}

/// 取引擎单例。透明模式下返回 None。
pub(crate) fn engine() -> Option<&'static Arc<Engine>> {
    ENGINE.get_or_init(load_engine).as_ref()
}

fn load_engine() -> Option<Arc<Engine>> {
    let path = dict_path();
    match ime_data::load(&path) {
        Ok(dict) => {
            log_line(&format!(
                "引擎加载成功：{}（词条 {}）",
                path.display(),
                dict.entry_count()
            ));
            Some(Engine::new(dict, Config::load()))
        }
        Err(e) => {
            log_line(&format!("引擎加载失败：{e}（{}），进入透明模式", path.display()));
            None
        }
    }
}

/// %LOCALAPPDATA%\InputIME\input.imedic（用户级数据，契约 §7）。
fn dict_path() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| {
        std::env::var("APPDATA")
            .map(|a| PathBuf::from(a).join("Local").to_string_lossy().into_owned())
            .unwrap_or_else(|_| "C:\\Users\\Default\\AppData\\Local".to_owned())
    });
    PathBuf::from(base).join("InputIME").join(crate::registration::DICT_FILENAME)
}

/// COM 边界兜底：回调内任何 panic 不得穿透到宿主进程，统一捕获降级。
/// 失败时记日志并返回 Ok(默认值)（按键放行 / 空操作）。
fn guard<T>(f: impl FnOnce() -> Result<T>) -> Result<T>
where
    T: Default,
{
    catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|e| {
        log_line(&format!("COM 回调 panic 被捕获：{e:?}，降级处理"));
        Ok(T::default())
    })
}

/// TSF 文本服务实例（每文档激活一个实例，单线程 STA 内使用）。
///
/// 实例状态全部用 RefCell/Cell 包裹：edit session 同步回调会重入同一对象，
/// 不允许出现跨方法 &mut 借用。
#[implement(ITfTextInputProcessorEx, ITfKeyEventSink, ITfThreadMgrEventSink)]
pub(crate) struct TextService {
    /// ITfThreadMgr（Activate 传入，Deactivate 用）。
    thread_mgr: RefCell<Option<ITfThreadMgr>>,
    /// 本实例的 client id（Activate 传入）。
    client_id: Cell<u32>,
    /// ITfThreadMgrEventSink 的 advise cookie（UnadviseSink 用）。
    event_cookie: Cell<u32>,
    /// 活动会话；None = 无会话（字母键将开启新会话）。
    session: RefCell<Option<Session>>,
    /// composition 封装（随会话创建/销毁）。
    composition: RefCell<Option<Composition>>,
    /// 候选窗：GdiCandidateWindow（Agent E 已交付，W2 起生效）。
    ui: RefCell<Box<dyn CandidateUi>>,
    /// 上一次光标矩形（GetTextExt 失败时复用；首次用屏幕中央）。
    caret: Cell<CaretRect>,
    /// 候选窗最近一次定位时的光标锚点（远跳清除检测基准）。
    anchor: Cell<CaretRect>,
    /// Shift 临时英文模式（会话非 active 时 Shift 切换）。
    english_mode: Cell<bool>,
}

impl TextService {
    pub(crate) fn new() -> Self {
        INSTANCE_COUNT.fetch_add(1, Ordering::SeqCst);
        TextService {
            thread_mgr: RefCell::new(None),
            client_id: Cell::new(0),
            event_cookie: Cell::new(0),
            session: RefCell::new(None),
            composition: RefCell::new(None),
            ui: RefCell::new(Box::new(GdiCandidateWindow::new())),
            caret: Cell::new(CaretRect::default()),
            anchor: Cell::new(CaretRect::default()),
            english_mode: Cell::new(false),
        }
    }

    /// OnTestKeyDown 判定（无副作用）：本键是否由本输入法消费。
    fn test_key_down(&self, wparam: WPARAM, _lparam: LPARAM) -> bool {
        // 透明模式：全部放行。
        if engine().is_none() {
            return false;
        }
        // 英文模式：全部放行。
        if self.english_mode.get() {
            return false;
        }
        let vk = wparam.0 as u16;
        let key = map_key(vk, char_code(vk), shift_pressed(), ctrl_pressed(), alt_pressed());
        let Some(key) = key else { return false };
        match &*self.session.borrow() {
            // 会话 active：映射键一律吃掉（含经 keymap 重映射的翻页键）。
            Some(_) => true,
            // 非 active：仅字母键（含 '）吃掉并开启会话；标点/数字等放行给应用。
            None => is_session_start_key(key),
        }
    }

    /// OnKeyDown 完整处理：映射 → 会话推进 → 应用 Effect。
    fn handle_key_down(&self, pic: &ITfContext, wparam: WPARAM, _lparam: LPARAM) -> bool {
        let Some(engine) = engine() else { return false };
        let config = engine.config();

        let vk = wparam.0 as u16;
        // Shift：会话外切换英文模式；会话内忽略（MVP 直接忽略，契约 13 §3.3）。
        if vk == VK_SHIFT.0 {
            if self.session.borrow().is_none() {
                let next = !self.english_mode.get();
                self.english_mode.set(next);
                log_line(&format!("Shift 切换英文模式：{next}"));
            }
            return false;
        }
        if self.english_mode.get() {
            return false;
        }

        let key = map_key(vk, char_code(vk), shift_pressed(), ctrl_pressed(), alt_pressed());
        let Some(key) = key else { return false };

        // 开启新会话：仅字母键。
        if self.session.borrow().is_none() {
            if !is_session_start_key(key) {
                return false;
            }
            let mut session = engine.start_session();
            let effect = session.on_key(key);
            *self.session.borrow_mut() = Some(session);
            *self.composition.borrow_mut() = Some(Composition::new(pic.clone(), self.client_id.get()));
            self.dispatch(&effect);
            return true;
        }

        // 会话内按键：先应用快捷键映射（翻页键重映射），映射键一律消费（test_key_down 已放行非映射键）。
        let key = apply_keymap(key, &config.keymap);
        let effect = self
            .session
            .borrow_mut()
            .as_mut()
            .map(|s| s.on_key(key))
            .expect("会话存在性已在上方校验");
        self.dispatch(&effect);
        true
    }

    /// 应用 Effect（契约 §7）：composition → 候选窗；end 则上屏/取消并清理会话。
    fn dispatch(&self, effect: &ime_core::Effect) {
        let mut caret = self.caret.get();
        let mut anchor = self.anchor.get();
        let ended = {
            let composition = self.composition.borrow();
            match composition.as_ref() {
                Some(comp) => {
                    let mut ui = self.ui.borrow_mut();
                    apply_effect(comp, ui.as_mut(), &mut caret, &mut anchor, effect)
                }
                // composition 缺失（异常路径）：仅更新候选窗并继续。
                None => {
                    log_line("dispatch：composition 缺失，仅更新候选窗");
                    let snap = crate::ui::effect_to_snapshot(effect);
                    let mut ui = self.ui.borrow_mut();
                    if snap.candidates.is_empty() && snap.reading.is_empty() {
                        ui.hide();
                    } else if ui.is_visible() {
                        ui.update(&snap);
                    } else {
                        ui.show(&snap, caret);
                    }
                    effect.end.is_some()
                }
            }
        };
        self.caret.set(caret);
        self.anchor.set(anchor);
        if ended {
            self.ui.borrow_mut().hide();
            *self.session.borrow_mut() = None;
            *self.composition.borrow_mut() = None;
        }
    }
}

impl Drop for TextService {
    fn drop(&mut self) {
        INSTANCE_COUNT.fetch_sub(1, Ordering::SeqCst);
    }
}

// ---- ITfTextInputProcessor / Ex ----

/// TextService_Impl 上才拿得到自有 COM 指针（`to_object()`），
/// 故 activate/deactivate 挂在这里，字段经 Deref 访问 TextService。
impl TextService_Impl {
    /// Activate / ActivateEx 公共初始化。
    fn activate(&self, ptim: &ITfThreadMgr, tid: u32) -> Result<()> {
        *self.thread_mgr.borrow_mut() = Some(ptim.clone());
        self.client_id.set(tid);

        // AdviseKeyEventSink：按键事件走本对象（经 ITfKeystrokeMgr，TSF 标准流程）。
        let key_sink: ITfKeyEventSink = self.to_object().to_interface();
        let keystroke: ITfKeystrokeMgr = ptim.cast()?;
        // SAFETY: 标准 TSF advise；sink 在本对象生命周期内有效，Deactivate 时 Unadvise。
        unsafe { keystroke.AdviseKeyEventSink(tid, &key_sink, true)? };

        // AdviseSink：线程管理器焦点事件（焦点离开时清理会话）。经 ITfSource。
        let thread_sink: ITfThreadMgrEventSink = self.to_object().to_interface();
        let source: ITfSource = ptim.cast()?;
        // SAFETY: 同上；cookie 记录用于 UnadviseSink。
        let cookie = unsafe {
            source.AdviseSink(&<ITfThreadMgrEventSink as Interface>::IID, &thread_sink)?
        };
        self.event_cookie.set(cookie);

        log_line(&format!("Activate：tid={tid}"));
        Ok(())
    }

    /// Deactivate 公共清理。
    fn deactivate(&self) {
        // 焦点清理：隐藏候选窗、丢弃会话与 composition。
        self.ui.borrow_mut().hide();
        *self.session.borrow_mut() = None;
        *self.composition.borrow_mut() = None;

        if let Some(tm) = self.thread_mgr.borrow().as_ref() {
            // SAFETY: 标准 TSF unadvise 调用。
            if let Ok(keystroke) = tm.cast::<ITfKeystrokeMgr>() {
                let _ = unsafe { keystroke.UnadviseKeyEventSink(self.client_id.get()) };
            }
            if let Ok(source) = tm.cast::<ITfSource>() {
                let _ = unsafe { source.UnadviseSink(self.event_cookie.get()) };
            }
        }
        log_line("Deactivate：已清理");
    }
}

impl ITfTextInputProcessor_Impl for TextService_Impl {
    fn Activate(&self, ptim: Ref<ITfThreadMgr>, tid: u32) -> Result<()> {
        guard(|| self.activate(ptim.unwrap(), tid))
    }

    fn Deactivate(&self) -> Result<()> {
        guard(|| {
            self.deactivate();
            Ok(())
        })
    }
}

impl ITfTextInputProcessorEx_Impl for TextService_Impl {
    fn ActivateEx(&self, ptim: Ref<ITfThreadMgr>, tid: u32, _dwflags: u32) -> Result<()> {
        guard(|| self.activate(ptim.unwrap(), tid))
    }
}

// ---- ITfKeyEventSink ----

impl ITfKeyEventSink_Impl for TextService_Impl {
    fn OnSetFocus(&self, _fforeground: BOOL) -> Result<()> {
        Ok(())
    }

    fn OnTestKeyDown(&self, _pic: Ref<ITfContext>, wparam: WPARAM, lparam: LPARAM) -> Result<BOOL> {
        guard(|| Ok(BOOL(i32::from(self.test_key_down(wparam, lparam)))))
    }

    fn OnTestKeyUp(&self, _pic: Ref<ITfContext>, _wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        Ok(BOOL(0))
    }

    fn OnKeyDown(&self, pic: Ref<ITfContext>, wparam: WPARAM, lparam: LPARAM) -> Result<BOOL> {
        guard(|| Ok(BOOL(i32::from(self.handle_key_down(pic.unwrap(), wparam, lparam)))))
    }

    fn OnKeyUp(&self, _pic: Ref<ITfContext>, _wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        Ok(BOOL(0))
    }

    fn OnPreservedKey(&self, _pic: Ref<ITfContext>, _rguid: *const windows_core::GUID) -> Result<BOOL> {
        Ok(BOOL(0))
    }
}

// ---- ITfThreadMgrEventSink ----

impl ITfThreadMgrEventSink_Impl for TextService_Impl {
    fn OnInitDocumentMgr(&self, _pdim: Ref<ITfDocumentMgr>) -> Result<()> {
        Ok(())
    }

    fn OnUninitDocumentMgr(&self, _pdim: Ref<ITfDocumentMgr>) -> Result<()> {
        Ok(())
    }

    /// 焦点切换：清理会话与候选窗（契约 13 §3.3：焦点离开时 hide + 丢弃 Session）。
    fn OnSetFocus(&self, _pdimfocus: Ref<ITfDocumentMgr>, _pdimprevfocus: Ref<ITfDocumentMgr>) -> Result<()> {
        self.ui.borrow_mut().hide();
        *self.session.borrow_mut() = None;
        *self.composition.borrow_mut() = None;
        Ok(())
    }

    fn OnPushContext(&self, _pic: Ref<ITfContext>) -> Result<()> {
        Ok(())
    }

    fn OnPopContext(&self, _pic: Ref<ITfContext>) -> Result<()> {
        Ok(())
    }
}

/// 当前 Shift 是否按下（GetKeyState 高位，返回 SHORT）。
fn shift_pressed() -> bool {
    // SAFETY: GetKeyState 查询当前线程键盘状态，返回符号位表示按下。
    (unsafe { GetKeyState(VK_SHIFT.0 as i32) }) < 0
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
fn char_code(vk: u16) -> u32 {
    // SAFETY: MapVirtualKeyW 是纯查询，返回 0 表示无对应字符（死键等）。
    unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_CHAR) & 0xFFFF }
}
