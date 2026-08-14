//! ITfTextInputProcessorEx / ITfKeyEventSink / ITfThreadMgrEventSink 实现。
//! 契约 01-contract.md §7 与 13 任务书 §3.3。
//! 【Agent D】W1 实现。
//!
//! 时序：Activate → AdviseKeyEventSink / AdviseSink；按键经 session_bridge 映射进
//! iuv_core::Session，Effect 由 composition + CandidateUi 应用；Deactivate 反向清理。

use std::cell::{Cell, RefCell};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::sync::OnceLock;

use iuv_core::{apply_keymap, is_session_start_key, Config, Engine, Key, Session};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetKeyState, MapVirtualKeyW, MAPVK_VK_TO_CHAR, VK_CAPITAL, VK_SHIFT,
};
use windows::Win32::UI::TextServices::{
    ITfCompartment, ITfCompartmentEventSink, ITfCompartmentEventSink_Impl, ITfCompartmentMgr,
    ITfContext, ITfDocumentMgr, ITfKeyEventSink, ITfKeyEventSink_Impl, ITfKeystrokeMgr, ITfSource,
    ITfTextInputProcessorEx, ITfTextInputProcessorEx_Impl, ITfTextInputProcessor_Impl,
    ITfThreadMgr, ITfThreadMgrEventSink, ITfThreadMgrEventSink_Impl,
    GUID_COMPARTMENT_KEYBOARD_OPENCLOSE,
};
use windows_core::{implement, ComObject, IUnknownImpl, Interface, Ref, Result, BOOL};

use crate::composition::Composition;
use crate::langbar::{self, LangBarItemButton};
use crate::log::log_line;
use crate::session_bridge::{apply_effect, caps_passthrough, map_key};
use crate::ui::CaretRect;
use crate::ui::{CandidateUi, GdiCandidateWindow, NullCandidateUi};

/// 进程级引擎单例（契约 §7：`OnceLock<Arc<Engine>>`）。
/// 词典加载失败 → None = 透明模式（全部按键放行，绝不卡用户）。
static ENGINE: OnceLock<Option<Arc<Engine>>> = OnceLock::new();
/// 加载是否已启动（防重复 spawn；Activate 与 engine() 兜底并发安全）。
static ENGINE_LOAD_STARTED: AtomicBool = AtomicBool::new(false);

/// 全局活动对象计数（DllCanUnloadNow 用）：实例创建 +1，Drop −1。
static INSTANCE_COUNT: AtomicU32 = AtomicU32::new(0);

pub(crate) fn instance_count() -> u32 {
    INSTANCE_COUNT.load(Ordering::SeqCst)
}

/// 取引擎单例（非阻塞）。透明模式 / 加载未完成时返回 None（按键放行）。
pub(crate) fn engine() -> Option<&'static Arc<Engine>> {
    let loaded = ENGINE.get().and_then(|e| e.as_ref());
    if loaded.is_none() && ENGINE.get().is_none() {
        // 兜底：未走 Activate 就被按键（极端路径），触发后台加载。
        start_engine_load();
    }
    loaded
}

/// 后台异步加载引擎：词库 17MB/65 万词条，首键同步加载会卡顿。
/// Activate（切到输入法）时调用；加载中按键 = 透明放行，绝不阻塞按键路径。
/// 加载失败 → set(None) = 永久透明模式（与现状语义一致，不重试）。
pub(crate) fn start_engine_load() {
    if ENGINE_LOAD_STARTED.swap(true, Ordering::SeqCst) {
        return;
    }
    std::thread::spawn(|| {
        let t0 = std::time::Instant::now();
        log_line("引擎加载开始（后台线程）");
        let engine = load_engine();
        log_line(&format!(
            "引擎加载完成：耗时 {:.0} ms，结果 {:?}",
            t0.elapsed().as_millis(),
            engine.as_ref().map(|_| "就绪").unwrap_or("失败→透明模式")
        ));
        let _ = ENGINE.set(engine);
    });
}

/// 引擎后台加载是否仍在进行（DllCanUnloadNow 用：加载线程运行中访问 DLL 代码，
/// 不可卸载）。set 完成（含失败 set(None)）后恒 false。
pub(crate) fn engine_loading() -> bool {
    ENGINE_LOAD_STARTED.load(Ordering::SeqCst) && ENGINE.get().is_none()
}

fn load_engine() -> Option<Arc<Engine>> {
    let path = dict_path();
    match iuv_data::load(&path) {
        Ok(dict) => {
            log_line(&format!(
                "引擎加载成功：{}（词条 {}）",
                path.display(),
                dict.entry_count()
            ));
            let engine = Engine::new(dict, Config::load());
            // M2 主动调权用户库装配（18-m2-user-dict.md）：缺失/损坏 → 空库继续，
            // attach 返回 Err 仅记日志（不代表未装配——路径已记录，首次交换时创建文件）。
            let user_path = user_dict_path();
            if let Err(e) = engine.attach_user_dict(user_path.clone()) {
                log_line(&format!("用户词库装配失败（空库继续，路径已记录）：{}", e));
            } else {
                log_line(&format!("用户词库装配成功：{}", user_path.display()));
            }
            Some(engine)
        }
        Err(e) => {
            log_line(&format!(
                "引擎加载失败：{e}（{}），进入透明模式",
                path.display()
            ));
            None
        }
    }
}

/// %LOCALAPPDATA%\iuv\iuv.imedic（用户级数据，契约 §7）。
fn dict_path() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| {
        std::env::var("APPDATA")
            .map(|a| {
                PathBuf::from(a)
                    .join("Local")
                    .to_string_lossy()
                    .into_owned()
            })
            .unwrap_or_else(|_| "C:\\Users\\Default\\AppData\\Local".to_owned())
    });
    PathBuf::from(base)
        .join("iuv")
        .join(crate::registration::DICT_FILENAME)
}

/// %LOCALAPPDATA%\iuv\iuv.user.imedic（M2 用户权重覆盖表，与基本库同目录）。
fn user_dict_path() -> PathBuf {
    let mut p = dict_path();
    p.set_file_name("iuv.user.imedic");
    p
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
#[implement(
    ITfTextInputProcessorEx,
    ITfKeyEventSink,
    ITfThreadMgrEventSink,
    ITfCompartmentEventSink
)]
pub(crate) struct TextService {
    /// ITfThreadMgr（Activate 传入，Deactivate 用）。
    thread_mgr: RefCell<Option<ITfThreadMgr>>,
    /// 本实例的 client id（Activate 传入）。
    client_id: Cell<u32>,
    /// ITfThreadMgrEventSink 的 advise cookie（UnadviseSink 用）。
    event_cookie: Cell<u32>,
    /// OPENCLOSE compartment 监听（系统"输入法/非输入法切换"热键驱动，
    /// 经 ITfSource::AdviseSink 挂 ITfCompartmentEventSink）+ cookie。
    /// Step1 仅监听记日志，验证系统热键确实翻转第三方 TIP 的 compartment。
    compartment: RefCell<Option<(ITfCompartment, u32)>>,
    /// 活动会话；None = 无会话（字母键将开启新会话）。
    /// Rc 共享：候选窗点击/hover 回调（同线程）经克隆访问。
    session: Rc<RefCell<Option<Session>>>,
    /// composition 封装（随会话创建/销毁）。Rc 共享：候选窗回调 dispatch 用。
    composition: Rc<RefCell<Option<Composition>>>,
    /// 候选窗：GdiCandidateWindow（Agent E 已交付，W2 起生效）。Rc 共享：同上。
    ui: Rc<RefCell<Box<dyn CandidateUi>>>,
    /// 上一次光标矩形（GetTextExt 失败时复用；首次用屏幕中央）。Rc 共享：同上。
    caret: Rc<Cell<CaretRect>>,
    /// Shift 临时英文模式（会话非 active 时 Shift 切换）。
    /// `Arc` 共享：语言栏"中/英"图标与按键路径读同一状态。
    english_mode: Arc<AtomicBool>,
    /// 语言栏"中/英"切换图标（Activate 挂载，Deactivate 卸载）。
    lang_bar: RefCell<Option<ComObject<LangBarItemButton>>>,
}

impl TextService {
    pub(crate) fn new() -> Self {
        INSTANCE_COUNT.fetch_add(1, Ordering::SeqCst);
        let session = Rc::new(RefCell::new(None));
        let composition = Rc::new(RefCell::new(None));
        let caret = Rc::new(Cell::new(CaretRect::default()));
        let ui_rc = Rc::new(RefCell::new(
            Box::new(NullCandidateUi) as Box<dyn CandidateUi>
        ));
        // 候选窗交互接线（同线程回调；点击=页内行号→Digit 键，悬停=同步 selected）。
        let mut candwin = GdiCandidateWindow::new();
        {
            let s = session.clone();
            let c = composition.clone();
            let u = ui_rc.clone();
            let ca = caret.clone();
            candwin.set_on_click(Some(Box::new(move |row: usize| {
                // Digit 键位上限 1-9（row 0-8）；超限忽略（page_size 配置极端时防御）。
                if row >= 9 {
                    return;
                }
                let effect: Option<iuv_core::Effect> = s
                    .borrow_mut()
                    .as_mut()
                    .map(|sess: &mut Session| sess.on_key(Key::Digit((row + 1) as u8)));
                if let Some(e) = effect {
                    dispatch_effect(&s, &c, &u, &ca, &e);
                }
            })));
            let s = session.clone();
            candwin.set_on_hover(Some(Box::new(move |row: usize| {
                if let Some(sess) = s.borrow_mut().as_mut() {
                    sess.set_selected(row);
                }
            })));
        }
        *ui_rc.borrow_mut() = Box::new(candwin);
        TextService {
            thread_mgr: RefCell::new(None),
            client_id: Cell::new(0),
            event_cookie: Cell::new(0),
            compartment: RefCell::new(None),
            session,
            composition,
            ui: ui_rc,
            caret,
            english_mode: Arc::new(AtomicBool::new(false)),
            lang_bar: RefCell::new(None),
        }
    }

    /// OnTestKeyDown 判定（无副作用）：本键是否由本输入法消费。
    fn test_key_down(&self, wparam: WPARAM, _lparam: LPARAM) -> bool {
        // 透明模式：全部放行。
        if engine().is_none() {
            return false;
        }
        // 英文模式：全部放行。
        if self.english_mode.load(Ordering::SeqCst) {
            return false;
        }
        let vk = wparam.0 as u16;
        let key = map_key(
            vk,
            char_code(vk),
            shift_pressed(),
            capslock_on(),
            ctrl_pressed(),
            alt_pressed(),
        );
        let Some(key) = key else { return false };
        match &*self.session.borrow() {
            // 会话 active：映射键一律吃掉（含经 keymap 重映射的翻页键）。
            Some(_) => true,
            // 非 active：仅字母键（含 '）吃掉并开启会话；标点/数字等放行给应用。
            None => is_session_start_key(key),
        }
    }

    /// 翻转中/英模式（Shift / 语言栏点击共用入口）。
    /// 按 OPENCLOSE compartment 值同步中英模式（OnChange / 初始化共用）。
    ///
    /// open=false（0）= 英文模式；open=true（非 0）= 中文模式。值未变化则不动
    /// （SetValue 会同步重入 OnChange，防抖避免循环）。关闭时清理活动会话。
    fn apply_openclose(&self, open: bool) {
        let next = !open;
        if self.english_mode.load(Ordering::SeqCst) == next {
            return;
        }
        self.english_mode.store(next, Ordering::SeqCst);
        log_line(&format!(
            "OPENCLOSE 变化：open={open} → {}模式",
            if next { "英文" } else { "中文" }
        ));
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

    /// 未确认输入以**原文上屏**并清理会话（关闭输入法 Ctrl+Space / 焦点切换 Alt+Tab 共用）。
    ///
    /// 用户语义：结束中文输入时，拼音原文提交上屏（`zhu'jin'cheng` 预编辑 →
    /// `zhujincheng`——raw 是用户敲的字母串，撇号只是切分显示层）。
    /// 修复：旧实现只清内存态不终止 TSF composition → 系统终止时带撇号的分节预览
    /// 残留在文档（实测 2026-08-14：Ctrl+Space 后 zhu'jin'cheng 残留上屏）。
    /// 文本为空（异常态）→ cancel 清空；commit/cancel 失败记日志不阻断（残留由系统终止兜底）。
    fn flush_session(&self) {
        self.ui.borrow_mut().hide();
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

    /// OnKeyDown 完整处理：映射 → 会话推进 → 应用 Effect。
    fn handle_key_down(&self, pic: &ITfContext, wparam: WPARAM, _lparam: LPARAM) -> bool {
        let Some(engine) = engine() else { return false };
        let config = engine.config();

        let vk = wparam.0 as u16;
        if self.english_mode.load(Ordering::SeqCst) {
            return false;
        }

        let caps = capslock_on();
        let key = map_key(
            vk,
            char_code(vk),
            shift_pressed(),
            caps,
            ctrl_pressed(),
            alt_pressed(),
        );
        let Some(key) = key else { return false };
        log_line(&format!(
            "按键：{}（{}）",
            key.name(),
            if self.session.borrow().is_some() {
                "会话内"
            } else {
                "会话外"
            }
        ));

        // 开启新会话：仅字母键；CapsLock 生效时字母放行直通（仿微软：Caps = 英文模式，
        // 不建会话；会话内 Caps 字母照常进序列，避免 composition 残留错乱）。
        if self.session.borrow().is_none() {
            if !is_session_start_key(key) || caps_passthrough(&key, caps) {
                return false;
            }
            let mut session = engine.start_session();
            let effect = session.on_key(key);
            *self.session.borrow_mut() = Some(session);
            *self.composition.borrow_mut() =
                Some(Composition::new(pic.clone(), self.client_id.get()));
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
    fn dispatch(&self, effect: &iuv_core::Effect) {
        dispatch_effect(
            &self.session,
            &self.composition,
            &self.ui,
            &self.caret,
            effect,
        )
    }
}

/// dispatch 的自由函数版：候选窗点击回调（同线程）与 TextService 共用同一路径。
/// 经 Rc 共享槽访问 session/composition/ui/caret；orientation 取自引擎配置。
fn dispatch_effect(
    session: &Rc<RefCell<Option<Session>>>,
    composition: &Rc<RefCell<Option<Composition>>>,
    ui: &Rc<RefCell<Box<dyn CandidateUi>>>,
    caret: &Rc<Cell<CaretRect>>,
    effect: &iuv_core::Effect,
) {
    let orientation = engine()
        .map(|e| e.config().candidate_orientation)
        .unwrap_or_default();
    let mut caret_pos = caret.get();
    let mut degraded = false;
    let ended = {
        let comp = composition.borrow();
        match comp.as_ref() {
            Some(comp) => {
                // 外部终止（OnCompositionTerminated）降级：丢弃会话，
                // 文档残留文本由用户自行清理，下一键重新开会话（透明放行避免 0x8000FFFF 卡死）。
                if comp.terminated() {
                    log_line("dispatch：composition 被外部终止，降级丢弃会话");
                    degraded = true;
                    true
                } else {
                    let mut ui_guard = ui.borrow_mut();
                    apply_effect(comp, ui_guard.as_mut(), &mut caret_pos, effect, orientation)
                }
            }
            // composition 缺失（异常路径）：仅更新候选窗并继续。
            None => {
                log_line("dispatch：composition 缺失，仅更新候选窗");
                let mut snap = crate::ui::effect_to_snapshot(effect);
                snap.orientation = orientation;
                let mut ui_guard = ui.borrow_mut();
                if snap.candidates.is_empty() && snap.reading.is_empty() {
                    ui_guard.hide();
                } else if ui_guard.is_visible() {
                    ui_guard.update(&snap);
                } else {
                    ui_guard.show(&snap, caret_pos);
                }
                effect.end.is_some()
            }
        }
    };
    caret.set(caret_pos);
    if ended {
        ui.borrow_mut().hide();
        *session.borrow_mut() = None;
        *composition.borrow_mut() = None;
        if degraded {
            log_line("dispatch：降级完成，会话已丢弃");
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
        let cookie =
            unsafe { source.AdviseSink(&<ITfThreadMgrEventSink as Interface>::IID, &thread_sink)? };
        self.event_cookie.set(cookie);

        // 监听系统"输入法/非输入法切换"（GUID_COMPARTMENT_KEYBOARD_OPENCLOSE）：
        // 系统按热键翻转该 compartment，我们经 OnChange 统一响应（中英切换真相源）。
        // ITfCompartment 经 ITfSource 塞 sink（同 Weasel Compartment.cpp 模式）。
        match ptim.cast::<ITfCompartmentMgr>() {
            Ok(mgr) => match unsafe { mgr.GetCompartment(&GUID_COMPARTMENT_KEYBOARD_OPENCLOSE) } {
                Ok(comp) => {
                    let src: ITfSource = comp.cast()?;
                    let comp_sink: ITfCompartmentEventSink = self.to_object().to_interface();
                    // SAFETY: 标准 TSF advise；sink 在本对象生命周期内有效，Deactivate 时 UnadviseSink。
                    match unsafe {
                        src.AdviseSink(&<ITfCompartmentEventSink as Interface>::IID, &comp_sink)
                    } {
                        Ok(cookie) => {
                            *self.compartment.borrow_mut() = Some((comp.clone(), cookie));
                            // 激活即打开（MS IME 同款语义）：初始未设置（VT_EMPTY）或
                            // 关闭（0）时置为打开，保证切入输入法即中文模式；同时保持
                            // 系统状态与我们一致（后续 SetValue 会同步重入 OnChange，防抖无害）。
                            if langbar::read_openclose(&comp) != Some(true) {
                                if let Err(e) = langbar::write_openclose(&comp, tid, true) {
                                    log_line(&format!("OPENCLOSE 激活写 open=1 失败：{e:?}"));
                                }
                            }
                            log_line(&format!(
                                "OPENCLOSE compartment 监听注册 OK（当前 open={}）",
                                match langbar::read_openclose(&comp) {
                                    Some(v) => i32::from(v).to_string(),
                                    None => "未设置".to_owned(),
                                }
                            ));
                            self.apply_openclose(true);
                        }
                        Err(e) => log_line(&format!(
                            "OPENCLOSE compartment AdviseSink 失败：{e:?}（不影响输入法）"
                        )),
                    }
                }
                Err(e) => log_line(&format!(
                    "OPENCLOSE compartment 获取失败：{e:?}（不影响输入法）"
                )),
            },
            Err(_) => log_line("OPENCLOSE compartment 监听注册失败（QI 不到 ITfCompartmentMgr）"),
        }

        // 后台异步加载引擎（词库 17MB/65 万词条）：切到输入法即开始，
        // 首次按键不再同步加载卡顿；加载完成前按键透明放行。
        start_engine_load();

        // 挂载语言栏"中/英"切换图标（失败仅记日志，不影响输入法主体）。
        // 点击归一为写 OPENCLOSE compartment（OnChange 统一响应）。
        let lang_bar_com = ComObject::new(LangBarItemButton::new(
            self.english_mode.clone(),
            self.compartment
                .borrow()
                .as_ref()
                .map(|(c, _)| (c.clone(), tid)),
        ));
        match langbar::add_to_lang_bar(ptim, &lang_bar_com) {
            Ok(()) => {
                *self.lang_bar.borrow_mut() = Some(lang_bar_com);
                log_line("语言栏图标挂载成功");
            }
            Err(e) => log_line(&format!("语言栏图标挂载失败：{e:?}（不影响输入法）")),
        }

        log_line(&format!("Activate：tid={tid}"));
        Ok(())
    }

    /// Deactivate 公共清理。
    fn deactivate(&self) {
        // 焦点清理：隐藏候选窗、丢弃会话与 composition。
        self.ui.borrow_mut().hide();
        *self.session.borrow_mut() = None;
        *self.composition.borrow_mut() = None;

        // 卸载语言栏"中/英"图标（失败仅记日志）。
        if let Some(lang_bar_com) = self.lang_bar.borrow_mut().take() {
            if let Some(tm) = self.thread_mgr.borrow().as_ref() {
                if let Err(e) = langbar::remove_from_lang_bar(tm, &lang_bar_com) {
                    log_line(&format!("语言栏图标卸载失败：{e:?}"));
                }
            }
        }

        // 卸载 OPENCLOSE compartment 监听。
        if let Some((comp, cookie)) = self.compartment.borrow_mut().take() {
            // SAFETY: 标准 TSF unadvise 调用，cookie 为注册返回值。
            if let Ok(src) = comp.cast::<ITfSource>() {
                if let Err(e) = unsafe { src.UnadviseSink(cookie) } {
                    log_line(&format!("OPENCLOSE compartment UnadviseSink 失败：{e:?}"));
                }
            }
        }

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
        guard(|| {
            Ok(BOOL(i32::from(self.handle_key_down(
                pic.unwrap(),
                wparam,
                lparam,
            ))))
        })
    }

    fn OnKeyUp(&self, _pic: Ref<ITfContext>, _wparam: WPARAM, _lparam: LPARAM) -> Result<BOOL> {
        Ok(BOOL(0))
    }

    fn OnPreservedKey(
        &self,
        _pic: Ref<ITfContext>,
        _rguid: *const windows_core::GUID,
    ) -> Result<BOOL> {
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

    /// 焦点切换：未确认输入按**原文上屏**语义结束（同关闭输入法），再清理会话与候选窗。
    /// 修复：旧实现只清内存态 → 系统终止 composition 时预编辑残留（Alt+Tab 遗留问题，
    /// 2026-08-14 与 OPENCLOSE 关闭同根因一并修复）。
    fn OnSetFocus(
        &self,
        _pdimfocus: Ref<ITfDocumentMgr>,
        _pdimprevfocus: Ref<ITfDocumentMgr>,
    ) -> Result<()> {
        if self.session.borrow().is_some() || self.composition.borrow().is_some() {
            self.flush_session();
            log_line("焦点切换：活动输入已原文上屏");
        } else {
            self.ui.borrow_mut().hide();
        }
        Ok(())
    }

    fn OnPushContext(&self, _pic: Ref<ITfContext>) -> Result<()> {
        Ok(())
    }

    fn OnPopContext(&self, _pic: Ref<ITfContext>) -> Result<()> {
        Ok(())
    }
}

// ---- ITfCompartmentEventSink ----

impl ITfCompartmentEventSink_Impl for TextService_Impl {
    /// OPENCLOSE compartment 变化：中英切换唯一响应点（系统热键 / Shift /
    /// 语言栏点击都归一为写该 compartment）。open=false → 英文模式，true → 中文。
    fn OnChange(&self, rguid: *const windows_core::GUID) -> Result<()> {
        guard(|| {
            // SAFETY: rguid 由 TSF 保证非空有效（回调参数约定）。
            if unsafe { *rguid } != GUID_COMPARTMENT_KEYBOARD_OPENCLOSE {
                return Ok(());
            }
            let Some((comp, _)) = self.compartment.borrow().as_ref().map(|c| c.clone()) else {
                log_line("OPENCLOSE 变化但 compartment 缺失，忽略");
                return Ok(());
            };
            // 未设置（VT_EMPTY）视为打开（中文），保持默认行为。
            self.apply_openclose(langbar::read_openclose(&comp).unwrap_or(true));
            Ok(())
        })
    }
}

/// 当前 Shift 是否按下（GetKeyState 高位，返回 SHORT）。
fn shift_pressed() -> bool {
    // SAFETY: GetKeyState 查询当前线程键盘状态，返回符号位表示按下。
    (unsafe { GetKeyState(VK_SHIFT.0 as i32) }) < 0
}

/// CapsLock 是否生效（切换状态位，与消息队列无关）。
fn capslock_on() -> bool {
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
fn char_code(vk: u16) -> u32 {
    // SAFETY: MapVirtualKeyW 是纯查询，返回 0 表示无对应字符（死键等）。
    unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_CHAR) & 0xFFFF }
}
