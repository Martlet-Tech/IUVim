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
use std::sync::{Arc, Mutex, OnceLock};

use iuv_core::{
    apply_keymap, chinese_punct, is_session_start_key, shifted_punct, Config, Engine, Key,
    RuntimeState, Session,
};
use iuv_data::{CtlCmd, CtlResult, CTL_FIELD_MODE};
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
use crate::ctl::{CtlApplier, CtlEndpoint};
use crate::daemon_client::DaemonClient;
use crate::langbar::{self, LangBarItemButton};
use crate::log::{self, log_line, process_id, thread_id};
use crate::session_bridge::{apply_effect, caps_passthrough, fullwidth_pending, is_passthrough_app, map_key};
use crate::ui::{CandidateUi, CandwinCandidateWindow, CaretRect};
use crate::ui_element::CandidateElementHost;

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
            // M6 日志模块禁用集装配（26-log-modules.md）：引擎配置即共享 config.json，
            // 首装配与 config_epoch 热载（apply_config_hot_reload）两处同步。
            crate::log::set_log_modules_disabled(&engine.config().disabled_log_modules);
            // M2 主动调权用户库装配（18-m2-user-dict.md）：缺失/损坏 → 空库继续，
            // attach 返回 Err 仅记日志（不代表未装配——路径已记录，首次交换时创建文件）。
            let user_path = user_dict_path();
            if let Err(e) = engine.attach_user_dict(user_path.clone()) {
                log_line(&format!("用户词库装配失败（空库继续，路径已记录）：{}", e));
            } else {
                log_line(&format!("用户词库装配成功：{}", user_path.display()));
            }
            // M2.5 简→繁转换器装配（31-script-traditional.md）：iuv.opencc 缺失/损坏 →
            // None 降级简体输出（不阻断）。数据与词库独立装配。
            let occ_path = script_path();
            match iuv_data::OpenccTable::load(&occ_path) {
                Ok(t) => {
                    let conv = iuv_core::ScriptConverter::new(t);
                    engine.attach_script_converter(Some(std::sync::Arc::new(conv)));
                    log_line(&format!(
                        "简繁转换器装配成功：{}（{} 词条）",
                        occ_path.display(),
                        engine.script_converter().map(|c| c.entry_count()).unwrap_or(0)
                    ));
                }
                Err(e) => {
                    engine.attach_script_converter(None);
                    log_line(&format!(
                        "简繁转换器装配失败（繁体模式降级简体输出）：{}",
                        e
                    ));
                }
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

/// %LOCALAPPDATA%\iuv\iuv.opencc（31-script-traditional.md 简繁转换表，与基本库同目录）。
fn script_path() -> PathBuf {
    let mut p = dict_path();
    p.set_file_name("iuv.opencc");
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
    /// 候选窗：CandwinCandidateWindow（M4：ULW 呈现，iuv-ui 绘图）。Rc 共享：同上。
    /// 具体类型（非 `Box<dyn>`）：M6 配置热载需直调 `set_theme`；交互/效果应用同槽。
    ui: Rc<RefCell<CandwinCandidateWindow>>,
    /// 上一次光标矩形（GetTextExt 失败时复用；首次用屏幕中央）。Rc 共享：同上。
    caret: Rc<Cell<CaretRect>>,
    /// Shift 临时英文模式（会话非 active 时 Shift 切换）。
    /// `Arc` 共享：语言栏"中/英"图标与按键路径读同一状态。
    english_mode: Arc<AtomicBool>,
    /// 语言栏"中/英"切换图标（Activate 挂载，Deactivate 卸载）。
    lang_bar: RefCell<Option<ComObject<LangBarItemButton>>>,
    /// TSF 候选 UI 元素宿主（WoW 游戏内候选框实验）。Rc 共享：dispatch 路径同线程访问。
    cand_elem: Rc<RefCell<CandidateElementHost>>,
    /// M6 daemon 客户端（共享段读取 + 管道写；Arc 与引擎 UserRemote 共享）。
    /// Deactivate 不撤——随进程/实例生命周期（TextService Drop 释放）。
    daemon: RefCell<Option<Arc<DaemonClient>>>,
    /// 远端写后端是否已注册到引擎（Activate 时引擎可能仍在后台加载，首键补注册）。
    remote_registered: Cell<bool>,
    /// 引号配对状态（`'`/`"` 交替开/关形）。会话开始/模式切换复位为开。
    punct_quote_open: Cell<bool>,
    /// 实例运行时四态（32-status-toolbar.md §5.1）：per-实例（非进程级 config），
    /// 启动 = `config.initial_state`，运行时操作才改；Session 构造注入（live 读）。
    /// `Arc<Mutex<...>>`：控制通道（SetState）与 OnChange 都写，会话/热键路径读。
    runtime: Arc<Mutex<RuntimeState>>,
    /// 反向控制端点（32-toolbar §4.2/§4.3）：accept 线程 + 隐藏消息窗。Activate 起、
    /// Deactivate/Drop 停（懒建，每个实例一个）。
    ctl: RefCell<Option<CtlEndpoint>>,
    /// 本实例是否已向 daemon 注册（§5.3：passthrough 进程不注册；防重复）。
    registered: Cell<bool>,
}

impl TextService {
    pub(crate) fn new() -> Self {
        INSTANCE_COUNT.fetch_add(1, Ordering::SeqCst);
        // iuv-win 共享呈现层日志钩子注入（一次即可，幂等）。
        static LOGGER_SET: OnceLock<()> = OnceLock::new();
        LOGGER_SET.get_or_init(|| iuv_win::set_logger(Some(crate::log::log_line)));
        let session = Rc::new(RefCell::new(None));
        let composition = Rc::new(RefCell::new(None));
        let caret = Rc::new(Cell::new(CaretRect::default()));
        let cand_elem = Rc::new(RefCell::new(CandidateElementHost::new()));
        // 候选窗交互接线（同线程回调；点击=页内行号→Digit 键上屏；悬停=纯视觉，
        // 窗口内部处理，不驱动会话）。
        // M4 主题：直接读 config.json（引擎可能仍在后台加载，engine() 不可依赖）：
        // `theme` 字段（默认 light）→ theme_light()/theme_dark()。M6 起可经 set_theme 热载。
        let theme = match iuv_core::Config::load().theme {
            iuv_core::ThemeChoice::Light => iuv_ui::theme_light(),
            iuv_core::ThemeChoice::Dark => iuv_ui::theme_dark(),
        };
        let ui_rc = Rc::new(RefCell::new(CandwinCandidateWindow::new(theme)));
        log_line(&format!(
            "候选窗主题：{}（config theme；M6 起可热载）",
            theme.name
        ));
        {
            let s = session.clone();
            let c = composition.clone();
            let u = ui_rc.clone();
            let ca = caret.clone();
            let ce = cand_elem.clone();
            ui_rc.borrow_mut().set_on_click(Some(Box::new(move |row: usize| {
                // Digit 键位上限 1-9（row 0-8）；超限忽略（page_size 配置极端时防御）。
                if row >= 9 {
                    return;
                }
                let effect: Option<iuv_core::Effect> = s
                    .borrow_mut()
                    .as_mut()
                    .map(|sess: &mut Session| sess.on_key(Key::Digit((row + 1) as u8)));
                if let Some(e) = effect {
                    dispatch_effect(&s, &c, &u, &ca, &ce, &e);
                }
            })));
        }
        TextService {
            thread_mgr: RefCell::new(None),
            client_id: Cell::new(0),
            event_cookie: Cell::new(0),
            compartment: RefCell::new(None),
            session,
            composition,
            ui: ui_rc,
            caret,
            cand_elem,
            english_mode: Arc::new(AtomicBool::new(false)),
            lang_bar: RefCell::new(None),
            daemon: RefCell::new(None),
            remote_registered: Cell::new(false),
            punct_quote_open: Cell::new(false),
            // 实例运行时四态：创建时（首次 Activate 前）从 config 初始值取一次
            // （32-toolbar §2.5：设置页默认值 = 新建实例时的初始值；热载不改运行实例）。
            runtime: Arc::new(Mutex::new(RuntimeState::from(
                iuv_core::Config::load().initial_state,
            ))),
            ctl: RefCell::new(None),
            registered: Cell::new(false),
        }
    }

    /// 实例标识（pid:tid）：pid = 进程 id，tid = **OS 线程 id**（`GetCurrentThreadId`，
    /// 非 TSF client id）——前台看板判定 `GetWindowThreadProcessId` 返回 OS 线程 id，
    /// 直接用同一标识匹配实例表（32-toolbar §4.1）。
    fn instance_id(&self) -> (u32, u32) {
        (process_id(), thread_id())
    }

    /// 当前运行时四态快照。
    fn runtime_snapshot(&self) -> RuntimeState {
        *self.runtime.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// 向 daemon 注册实例 + 通知 active（Activate 时；passthrough 进程不注册，iuv 完全透明）。
    /// Register 仅首 Activate 发一次（防重复）；`Active{true}` 每次 Activate 都发（daemon 判
    /// 「iuv 被选中」→ 看板显示；Deactivate 发 false 隐藏）。Register 失败 = daemon 离线
    /// （静默；poll 在线翻转后重注册，§4.4）。
    fn register_instance(&self) {
        let cfg = iuv_core::Config::load();
        let passthrough = !cfg.passthrough_apps.is_empty()
            && is_passthrough_app(&log::module_name(), &cfg.passthrough_apps);
        if passthrough {
            log_line("[toolbar] passthrough 进程：不注册工具栏实例（iuv 完全透明）");
            return;
        }
        let Some(client) = self.daemon.borrow().as_ref().cloned() else {
            return;
        };
        let (pid, tid) = self.instance_id();
        if !self.registered.get() {
            if client.register(pid, tid, self.runtime_snapshot().to_toolbar()) {
                self.registered.set(true);
                log_line(&format!("[toolbar] 实例注册（{pid}:{tid}）"));
            }
        }
        client.set_active(pid, tid, true);
    }

    /// daemon 重启恢复重注册（§4.4：poll 检测离线→在线翻转后调用）：
    /// daemon 重启清空实例表，本进程仍在运行（registered 仍 true）→ 强制重新 Register。
    fn re_register_instance(&self) {
        self.registered.set(false);
        self.register_instance();
    }

    /// 启动反向控制端点（accept 线程 + 隐藏消息窗；§4.2/§4.3）。懒建：Deactivate 停、
    /// Drop 清。失败静默（记日志——工具栏按钮无法到达本实例，其余功能不受影响）。
    fn start_ctl_endpoint(&self) {
        if self.ctl.borrow().is_some() {
            return;
        }
        let Some(hwnd) = CtlEndpoint::create_window() else {
            return;
        };
        // SAFETY: self 为 TextService（COM 对象内层，端点存活期间有效）；端点存于
        // self.ctl 的 RefCell 槽位（地址固定），attach 后 GWLP_USERDATA 指向该固定地址。
        let svc: *const dyn CtlApplier = self as *const TextService as *const dyn CtlApplier;
        let (pid, tid) = self.instance_id();
        *self.ctl.borrow_mut() = Some(CtlEndpoint::new(hwnd, svc));
        let mut slot = self.ctl.borrow_mut();
        slot.as_mut()
            .map(|ep| ep.attach(pid, tid))
            .unwrap_or(false);
    }

    /// 停反向控制端点（Deactivate：Drop 兜底清理，此处显式调以尽快释放窗口/线程）。
    fn stop_ctl_endpoint(&self) {
        let ep = self.ctl.borrow_mut().take();
        drop(ep); // CtlEndpoint::drop 停线程 + 清 GWLP_USERDATA + 销毁窗口
    }

    /// 运行时四态变化后的收尾：live 重渲当前会话（点简繁/全半角/标点立即生效）+ StateSync 上报。
    fn after_runtime_change(&self) {
        // 当前会话重渲：effect() 内部 live 读 runtime，切换后候选/预编辑立即跟随。
        if let Some(sess) = self.session.borrow().as_ref() {
            let effect = sess.effect();
            self.dispatch(&effect);
        }
        // 上报 daemon 看板（§4.1 StateSync）。
        if let Some(client) = self.daemon.borrow().as_ref() {
            let (pid, tid) = self.instance_id();
            client.state_sync(pid, tid, self.runtime_snapshot().to_toolbar());
        }
    }

    /// 应用反向控制命令（CtlCmd::SetState；TSF 线程 wndproc 调用，§4.3）。
    /// mode 走 OPENCLOSE compartment（真相源，OnChange 统一响应）；其余字段直改 runtime。
    fn apply_ctl_cmd(&self, cmd: &CtlCmd) -> CtlResult {
        match cmd {
            CtlCmd::SetState { field, value } => {
                if *field == CTL_FIELD_MODE {
                    // 中英：写 OPENCLOSE compartment（open=1 中文 / 0 英文）；SetValue
                    // 同步重入 OnChange → apply_openclose 更新 runtime.mode + StateSync。
                    let open = *value == 0; // 值 0=中文=打开
                    let mut ok = false;
                    if let Some((comp, tid)) = self
                        .compartment
                        .borrow()
                        .as_ref()
                        .map(|(c, t)| (c.clone(), *t))
                    {
                        ok = langbar::write_openclose(&comp, tid, open).is_ok();
                        if !ok {
                            log_line("[toolbar] 写 OPENCLOSE 失败，本地翻转 mode 兜底");
                        }
                    }
                    if !ok {
                        let mut runtime = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
                        runtime.set_field(CTL_FIELD_MODE, *value);
                        drop(runtime);
                        self.after_runtime_change();
                    }
                } else {
                    let mut runtime = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
                    if !runtime.set_field(*field, *value) {
                        return CtlResult::Err {
                            msg: format!("非法字段 {field}"),
                        };
                    }
                    drop(runtime);
                    self.after_runtime_change();
                }
                CtlResult::Ok {
                    state: self.runtime_snapshot().to_toolbar(),
                }
            }
        }
    }

    /// OnTestKeyDown 判定（无副作用）：本键是否由本输入法消费。
    /// 与 handle_key_down 保持**一致**（放行判定必须同时落在 Test 阶段）：
    /// 应用在 OnTestKeyDown 返回 eaten 时即跳过自己的按键处理，若 Test 吃而
    /// OnKeyDown 放，字母会被静默吞掉（实测 2026-08-19：Caps 直通失效）。
    fn test_key_down(&self, wparam: WPARAM, _lparam: LPARAM) -> bool {
        // 透明模式：全部放行。
        let Some(engine) = engine() else { return false };
        // M6：daemon 共享段轮询（与 handle_key_down 对称）。Test 阶段即消费 config_epoch，
        // 否则「取消英文标点」后首个标点键按旧配置放行（放行→OnKeyDown 不触发→不轮询）
        // → 配置长期陈旧（2026-08-19 实测：取消后仍英文，打字母才触发重载变中文）。
        if let Some(client) = self.daemon.borrow().as_ref() {
            client.poll(&engine, |engine| self.apply_config_hot_reload(engine), || {
                self.re_register_instance()
            });
        }
        let config = engine.config();
        let vk = wparam.0 as u16;
        let shift = shift_pressed();
        let ctrl = ctrl_pressed();
        let alt = alt_pressed();
        let session_active = self.session.borrow().is_some();
        // 按键直通白名单：命中进程全部按键放行（与 handle_key_down 判定一致），名单为空零开销。
        if !config.passthrough_apps.is_empty()
            && is_passthrough_app(&log::module_name(), &config.passthrough_apps)
        {
            return false;
        }
        // 英文模式：全角命中则吃掉（与 handle_key_down 对称），否则全部放行。
        if self.english_mode.load(Ordering::SeqCst) {
            return self
                .fullwidth_pending_compute(vk, shift, ctrl, alt, session_active)
                .is_some();
        }
        // 中文标点（会话外直接上屏）：Test 阶段与 handle_key_down 同判定，防应用双处理。
        if let Some(_) = self.chinese_punct_pending(
            char_code(vk),
            shift,
            ctrl,
            alt,
            session_active,
        ) {
            return true;
        }
        // 全角（会话外数字/符号/空格直接上屏全角）：与 handle_key_down 同判定。
        if self
            .fullwidth_pending_compute(vk, shift, ctrl, alt, session_active)
            .is_some()
        {
            return true;
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
        match &*self.session.borrow() {
            // 会话 active：映射键一律吃掉（含经 keymap 重映射的翻页键）。
            Some(_) => true,
            // 非 active：仅字母键（含 '）吃掉并开启会话；标点/数字等放行给应用。
            // CapsLock 例外：Caps 生效时字母放行直通（Caps = 英文模式，不建会话）——
            // 与 handle_key_down 的 caps_passthrough 对称，Test 阶段即放行。
            None => is_session_start_key(key) && !caps_passthrough(&key, caps),
        }
    }

    /// 翻转中/英模式（Shift / 语言栏点击共用入口）。
    /// 按 OPENCLOSE compartment 值同步中英模式（OnChange / 初始化共用）。
    ///
    /// open=false（0）= 英文模式；open=true（非 0）= 中文模式。值未变化则不动
    /// （SetValue 会同步重入 OnChange，防抖避免循环）。关闭时清理活动会话。
    /// 32-toolbar §2.4：runtime.mode 镜像 OPENCLOSE（真相源）→ 工具栏中英按钮读它；
    /// 每次变化 StateSync 上报 daemon（§4.1）。
    fn apply_openclose(&self, open: bool) {
        let next = !open;
        if self.english_mode.load(Ordering::SeqCst) == next {
            return;
        }
        self.english_mode.store(next, Ordering::SeqCst);
        {
            let mut runtime = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
            runtime.mode = if open {
                iuv_core::InitialMode::Chinese
            } else {
                iuv_core::InitialMode::English
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
            client.state_sync(pid, tid, self.runtime_snapshot().to_toolbar());
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
    fn chinese_punct_pending(
        &self,
        char_code: u32,
        shift: bool,
        ctrl: bool,
        alt: bool,
        session_active: bool,
    ) -> Option<String> {
        if self.english_mode.load(Ordering::SeqCst) || session_active || ctrl || alt {
            return None;
        }
        let runtime = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
        if runtime.punct == iuv_core::PunctMode::English {
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
    fn fullwidth_pending_compute(
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
        let base = char::from_u32(char_code(vk))?;
        let runtime = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
        fullwidth_pending(
            self.english_mode.load(Ordering::SeqCst),
            runtime.width,
            runtime.punct,
            base,
            shift,
            capslock_on(),
        )
    }

    /// 会话外中文标点直接上屏：临时 composition 一次 set_text+commit（两次 edit session，
    /// 复用既有 Composition 方法；与 flush_session 原文上屏同款路径）。
    fn commit_punct(&self, pic: &ITfContext, text: &str) {
        let comp = Composition::new(pic.clone(), self.client_id.get());
        match comp.set_text(text) {
            Ok(_) => match comp.commit(text) {
                Ok(()) => log_line(&format!("[punct] 中文标点直接上屏 {text}")),
                Err(e) => log_line(&format!("[punct] commit 失败：{e}")),
            },
            Err(e) => log_line(&format!("[punct] set_text 失败：{e}")),
        }
    }

    /// OnKeyDown 完整处理：映射 → 会话推进 → 应用 Effect。
    fn handle_key_down(&self, pic: &ITfContext, wparam: WPARAM, _lparam: LPARAM) -> bool {
        let Some(engine) = engine() else { return false };
        // M6：daemon 共享段轮询（低成本：读 u32 版本；用户库版本/配置纪元变化 → 即时生效）。
        // 远端写后端在 Activate 注册；引擎后台加载未完成则此处补注册（幂等，无副作用）。
        if let Some(client) = self.daemon.borrow().as_ref() {
            if !self.remote_registered.get() {
                engine.set_user_remote(Some(client.clone()));
                self.remote_registered.set(true);
            }
            client.poll(&engine, |engine| self.apply_config_hot_reload(engine), || {
                self.re_register_instance()
            });
        }
        let config = engine.config();

        let vk = wparam.0 as u16;
        let shift = shift_pressed();
        let ctrl = ctrl_pressed();
        let alt = alt_pressed();
        let session_active = self.session.borrow().is_some();

        // 按键直通白名单：命中进程全部按键放行（不建会话/无候选窗/不转全角，
        // 输入法在该进程完全透明），名单为空零开销。
        if !config.passthrough_apps.is_empty()
            && is_passthrough_app(&log::module_name(), &config.passthrough_apps)
        {
            return false;
        }

        if self.english_mode.load(Ordering::SeqCst) {
            // 英文模式 + 全角：ASCII 直接上屏全角（ｍｉｃｒｏｓｏｆｔ１２３），否则放行。
            if let Some(text) = self.fullwidth_pending_compute(vk, shift, ctrl, alt, session_active)
            {
                self.commit_punct(pic, &text);
                return true;
            }
            return false;
        }

        // 中文标点（会话外直接上屏全角）：判定与 test_key_down 对称。
        if let Some(punct) = self.chinese_punct_pending(char_code(vk), shift, ctrl, alt, session_active)
        {
            self.commit_punct(pic, &punct);
            return true;
        }

        // 全角（会话外数字/符号/空格直接上屏全角；字母不在此列，照常进拼音会话）。
        if let Some(text) = self.fullwidth_pending_compute(vk, shift, ctrl, alt, session_active) {
            self.commit_punct(pic, &text);
            return true;
        }

        let caps = capslock_on();
        let key = map_key(
            vk,
            char_code(vk),
            shift,
            caps,
            ctrl,
            alt,
        );
        let Some(key) = key else { return false };
        log_line(&format!(
            "[key] 按键：{}（{}）",
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
            // 注入实例运行时四态（32-toolbar §5.1：per-实例，live 读）。
            let mut session = engine.start_session_with_runtime(self.runtime.clone());
            self.punct_quote_open.set(false); // 拼音输入开始：引号配对复位为开形
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
            &self.cand_elem,
            effect,
        )
    }

    /// M6 配置热载（config_epoch 变化触发，DaemonClient::poll 回调）：
    /// 重载 config.json → 引擎配置（page_size/passthrough_apps/主题等读取点随新值生效）
    /// + 候选窗主题即时切换（set_theme，下帧 paint 生效）。
    /// 键位（keymap）热载为 M7 范畴（TSF 键映射装配不热切），keymap 变化仅记日志。
    fn apply_config_hot_reload(&self, engine: &Arc<Engine>) {
        let cfg = iuv_core::Config::load();
        let keymap_changed = cfg.keymap != engine.config().keymap;
        engine.set_config(cfg.clone());
        // 日志模块禁用集热载（26-log-modules.md）：随 config_epoch 生效。
        crate::log::set_log_modules_disabled(&cfg.disabled_log_modules);
        let theme = match cfg.theme {
            iuv_core::ThemeChoice::Light => iuv_ui::theme_light(),
            iuv_core::ThemeChoice::Dark => iuv_ui::theme_dark(),
        };
        self.ui.borrow_mut().set_theme(theme);
        log_line(&format!(
            "[daemon] 配置热载：theme={:?} passthrough_apps={} keymap{}",
            cfg.theme,
            cfg.passthrough_apps.len(),
            if keymap_changed {
                "变化（键位热载 M7）"
            } else {
                "不变"
            }
        ));
    }
}

/// dispatch 的自由函数版：候选窗点击回调（同线程）与 TextService 共用同一路径。
/// 经 Rc 共享槽访问 session/composition/ui/caret/cand_elem；orientation 取自引擎配置。
fn dispatch_effect(
    session: &Rc<RefCell<Option<Session>>>,
    composition: &Rc<RefCell<Option<Composition>>>,
    ui: &Rc<RefCell<CandwinCandidateWindow>>,
    caret: &Rc<Cell<CaretRect>>,
    cand_elem: &Rc<RefCell<CandidateElementHost>>,
    effect: &iuv_core::Effect,
) {
    // TSF 候选 UI 元素同步（与自绘窗平行）：候选非空 → Begin/Update；空 → End。
    // effect.end 的提交/取消路径统一走 ended 分支 End，这里跳过避免多余一次 Update。
    if effect.end.is_none() {
        let snap = crate::ui::effect_to_snapshot(effect);
        cand_elem.borrow_mut().sync(&snap);
    }
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
                    apply_effect(comp, &mut *ui_guard, &mut caret_pos, effect, orientation)
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
    // 自绘候选窗抑制（candidate_owner_apps 名单驱动，2026-08-20 弃矩形启发式）：
    // 命中进程（如 WoW 自绘游戏内候选栏）→ 抑制自绘窗（避免双候选栏）；默认空 = 恒自绘。
    // 名单空时零开销（不查进程名）。候选 UI 元素同步不受影响（游戏桥仍可拉取候选数据）。
    let suppress = engine()
        .map(|e| e.config().candidate_owner_apps)
        .map(|apps| should_suppress_candidate_window(&apps, &log::module_name()))
        .unwrap_or(false);
    ui.borrow_mut().set_suppressed(suppress);
    if ended {
        ui.borrow_mut().hide();
        cand_elem.borrow_mut().end();
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
        // 先停反向控制端点（accept 线程 join + 隐藏窗销毁），再注销——避免 Drop 期间
        // 字段仍存活时 wndproc 并发访问（TSF 线程 Drop 内不泵消息，但防御性先停干净）。
        self.stop_ctl_endpoint();
        // 32-toolbar §4.1：实例 Drop 注销（daemon 从实例表移除，看板失联清理）。
        if let Some(client) = self.daemon.borrow().as_ref() {
            let (pid, tid) = self.instance_id();
            client.unregister(pid, tid);
        }
    }
}

/// 反向控制命令应用目标（32-toolbar §4.3：TSF 线程 wndproc 调用，跨线程经原始指针间接）。
/// 实现挂 `TextService`（内层结构体，非 COM 壳）：字段全部在 TextService 上，指针稳定。
impl CtlApplier for TextService {
    fn apply_cmd(&self, cmd: &CtlCmd) -> CtlResult {
        self.apply_ctl_cmd(cmd)
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

        // TSF 3.0 候选 UI 元素（WoW 游戏内候选框实验）：QI ITfUIElementMgr。
        // 失败仅记日志（元素机制不可用 = 现状行为，输入法不受影响）。
        self.cand_elem.borrow_mut().attach(ptim);

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
                            // 新 TSF 实例初始中英模式（2026-08-19，见 28-initial-state-settings.md；
                            // 32-status-toolbar.md §5.2 修正）：中文默认 = 激活即打开；
                            // 英文默认 = 激活即关闭（Ctrl+Space 可切回）。
                            // **仅 VT_EMPTY（全新线程，compartment 未设置）时写配置默认**——
                            // 若 Activate 在「切走输入法再切回」时重触发且强行写默认，会把用户
                            // 在该窗口改过的中英重置回 config（违反 §2.4 per-实例保留语义）。
                            // 运行时值随实例存活（runtime 字段，本线程此前的设置天然保留）。
                            let default_open = iuv_core::Config::load().initial_state.mode
                                == iuv_core::InitialMode::Chinese;
                            match langbar::read_openclose(&comp) {
                                None => {
                                    if let Err(e) =
                                        langbar::write_openclose(&comp, tid, default_open)
                                    {
                                        log_line(&format!(
                                            "OPENCLOSE 激活写默认 open={default_open} 失败：{e:?}"
                                        ));
                                    }
                                    log_line(&format!(
                                        "OPENCLOSE VT_EMPTY：写默认 open={default_open}"
                                    ));
                                }
                                Some(open) => log_line(&format!(
                                    "OPENCLOSE 已有值 open={open}（保持，运行时值随实例存活）"
                                )),
                            }
                            // 用**实际** compartment 值初始化（含 VT_EMPTY→默认与既有值两种路径），
                            // 不以 config 默认强制——切走再切回保持用户改过的中英。
                            self.apply_openclose(langbar::read_openclose(&comp).unwrap_or(true));
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

        // M6 daemon 客户端装配：user_path = 现有 iuv.user.imedic 路径逻辑。共享段只读
        // 引用 + 管道写；daemon 不在线 → 引擎写路径自动降级本地写盘（绝不挂键）。
        // 引擎可能在后台加载未完成（engine()=None），远端写后端延迟到首键补注册
        // （handle_key_down 的 remote_registered 兜底，set_user_remote 幂等）。
        let user_path = user_dict_path();
        let daemon = Arc::new(DaemonClient::new(user_path.clone()));
        if let Some(engine) = engine() {
            engine.set_user_remote(Some(daemon.clone()));
            self.remote_registered.set(true);
        } else {
            log_line("[daemon] 引擎尚未加载完成：远端写后端延迟到首键注册");
        }
        *self.daemon.borrow_mut() = Some(daemon.clone());

        // 挂载语言栏"中/英"切换图标（失败仅记日志，不影响输入法主体）。
        // 点击归一为写 OPENCLOSE compartment（OnChange 统一响应）；右键弹自定义菜单
        // （设置/关于，经 daemon 客户端发管道命令，2026-08-17 决策：无独立托盘图标）。
        let menu_theme = match iuv_core::Config::load().theme {
            iuv_core::ThemeChoice::Light => iuv_ui::theme_light(),
            iuv_core::ThemeChoice::Dark => iuv_ui::theme_dark(),
        };
        let lang_bar_com = ComObject::new(LangBarItemButton::new(
            self.english_mode.clone(),
            self.compartment
                .borrow()
                .as_ref()
                .map(|(c, _)| (c.clone(), tid)),
            daemon,
            menu_theme,
        ));
        match langbar::add_to_lang_bar(ptim, &lang_bar_com) {
            Ok(()) => {
                *self.lang_bar.borrow_mut() = Some(lang_bar_com);
                log_line("语言栏图标挂载成功");
            }
            Err(e) => log_line(&format!("语言栏图标挂载失败：{e:?}（不影响输入法）")),
        }

        // 32-toolbar：向 daemon 注册实例（四态上报 + Active 通知；passthrough 进程不注册）。
        self.register_instance();

        // 32-toolbar：启动反向控制端点（accept 线程 + 隐藏消息窗；§4.2/§4.3）。
        self.start_ctl_endpoint();

        log_line(&format!("Activate：tid={tid}"));

        // M7 daemon 自启（IME 惰性拉起，搜狗同款）：离线且冷却期满 → CreateProcess
        // 拉起 DLL 同目录 iuv-daemon.exe（后台无控制台，异步不等待；失败静默降级）。
        if let Some(client) = self.daemon.borrow().as_ref() {
            client.ensure_daemon();
        }

        Ok(())
    }

    /// Deactivate 公共清理。
    fn deactivate(&self) {
        // 焦点清理：隐藏候选窗、结束候选元素、丢弃会话与 composition。
        self.ui.borrow_mut().hide();
        self.cand_elem.borrow_mut().clear();
        *self.session.borrow_mut() = None;
        *self.composition.borrow_mut() = None;

        // 32-toolbar：停反向控制端点（accept 线程 + 隐藏窗）+ Active=false 通知
        // （daemon 判「iuv 未被选中」→ 看板隐藏）。registered 保留——同一实例
        // 再激活不重复 Register，但 Active 通知仍发。
        self.stop_ctl_endpoint();
        if let Some(client) = self.daemon.borrow().as_ref() {
            let (pid, tid) = self.instance_id();
            client.set_active(pid, tid, false);
        }

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
    /// 已知遗留（2026-08-16，wow-ime）：全屏游戏（WoW 1.12）Alt+Tab 往返后焦点窗口的
    /// 输入法关联会被系统重置（QQ 拼音实测同样——通病，非本输入法问题）——回来打字
    /// 可能英文直通（托盘图标不刷新误导）；用户侧用 Win+Space 切回即可，不做程序干预。
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
            self.cand_elem.borrow_mut().end();
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

/// 自绘候选窗抑制判定（candidate_owner_apps 名单驱动，2026-08-20 弃矩形启发式）：
/// 名单空 = 恒自绘（false，零开销）；命中进程名 = 抑制自绘窗（true，app 自绘候选栏）。
fn should_suppress_candidate_window(apps: &[String], exe: &str) -> bool {
    !apps.is_empty() && is_passthrough_app(exe, apps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suppress_only_for_listed_apps() {
        // 空名单 = 恒自绘（微信/notepad/WinTerm 等主流应用不误伤）
        assert!(!should_suppress_candidate_window(&[], "weixin.exe"));
        // 命中名单（大小写不敏感精确匹配）= 抑制（WoW 游戏自绘候选栏）
        assert!(should_suppress_candidate_window(&["wow.exe".into()], "WoW.exe"));
        // 未命中名单 = 恒自绘
        assert!(!should_suppress_candidate_window(&["wow.exe".into()], "weixin.exe"));
    }
}
