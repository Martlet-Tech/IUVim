//! ITfTextInputProcessorEx / ITfKeyEventSink / ITfThreadMgrEventSink 实现。
//! 契约 01-contract.md §7 与 13 任务书 §3.3。
//! 【Agent D】W1 实现。
//!
//! 时序：Activate → AdviseKeyEventSink / AdviseSink；按键经 session_bridge 映射进
//! iuv_core::Session，Effect 由 composition + CandidateUi 应用；Deactivate 反向清理。
//!
//! P2.2 拆分：本文件 = COM 壳（实例结构 + 生命周期 + Ctl 端点 + COM trait 实现）；
//! 引擎生命周期 → `engine_host.rs`；按键路由 → `key_routing.rs`；模式/会话外上屏
//! → `mode.rs`；daemon 协作 → `daemon_host.rs`；Effect 应用 → `dispatch.rs`。

use std::cell::{Cell, RefCell};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use iuv_core::{Config, ImeState, InitialMode, Key, PunctMode, ScriptMode, Session, WidthMode};
use iuv_win::{CtlCmd, CtlResult};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};
use windows::Win32::UI::TextServices::{
    CLSID_TF_InputProcessorProfiles, ITfActiveLanguageProfileNotifySink,
    ITfActiveLanguageProfileNotifySink_Impl, ITfCompartment, ITfCompartmentEventSink,
    ITfCompartmentEventSink_Impl, ITfCompartmentMgr, ITfContext, ITfDocumentMgr,
    ITfInputProcessorProfiles, ITfKeyEventSink, ITfKeyEventSink_Impl, ITfKeystrokeMgr, ITfSource,
    ITfTextInputProcessorEx, ITfTextInputProcessorEx_Impl, ITfTextInputProcessor_Impl,
    ITfThreadFocusSink, ITfThreadFocusSink_Impl, ITfThreadMgr, ITfThreadMgrEventSink,
    ITfThreadMgrEventSink_Impl, GUID_COMPARTMENT_KEYBOARD_OPENCLOSE,
};
use windows_core::{implement, ComObject, IUnknownImpl, Interface, Ref, Result, BOOL};

use crate::composition::Composition;
use crate::ctl::{CtlApplier, CtlEndpoint};
use crate::daemon_client::DaemonClient;
use crate::langbar::{self, LangBarItemButton};
use crate::log::{log_line, process_id, thread_id};
use crate::ui::{CandidateUi, CandwinCandidateWindow, CaretRect};
use crate::ui_element::CandidateElementHost;

use super::dispatch::dispatch_effect;
use super::engine_host::{engine, start_engine_load, user_dict_path};

/// 全局活动对象计数（DllCanUnloadNow 用）：实例创建 +1，Drop −1。
static INSTANCE_COUNT: AtomicU32 = AtomicU32::new(0);

pub(crate) fn instance_count() -> u32 {
    INSTANCE_COUNT.load(Ordering::SeqCst)
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
///
/// 字段可见性（P2.2）：`impl TextService` 块分散在 com/ 各子模块，共享字段 pub(crate)。
#[implement(
    ITfTextInputProcessorEx,
    ITfKeyEventSink,
    ITfThreadMgrEventSink,
    ITfCompartmentEventSink,
    ITfThreadFocusSink,
    ITfActiveLanguageProfileNotifySink
)]
pub(crate) struct TextService {
    /// ITfThreadMgr（Activate 传入，Deactivate 用）。
    thread_mgr: RefCell<Option<ITfThreadMgr>>,
    /// 本实例的 client id（Activate 传入）。
    pub(crate) client_id: Cell<u32>,
    /// ITfThreadMgrEventSink 的 advise cookie（UnadviseSink 用）。
    event_cookie: Cell<u32>,
    /// OPENCLOSE compartment 监听（系统"输入法/非输入法切换"热键驱动，
    /// 经 ITfSource::AdviseSink 挂 ITfCompartmentEventSink）+ cookie。
    /// Step1 仅监听记日志，验证系统热键确实翻转第三方 TIP 的 compartment。
    compartment: RefCell<Option<(ITfCompartment, u32)>>,
    /// 线程焦点 sink cookie（ITfThreadFocusSink，维度②应用切出即隐的官方信号）。
    thread_focus_cookie: Cell<u32>,
    /// 输入法 profile 激活通知（ITfInputProcessorProfilesSink，维度①输入法切至/切出
    /// 的官方信号——weasel 语言栏显隐同款）+ 对象与反注册 cookie。
    profiles: RefCell<Option<(ITfInputProcessorProfiles, u32)>>,
    /// 活动会话；None = 无会话（字母键将开启新会话）。
    /// Rc 共享：候选窗点击/hover 回调（同线程）经克隆访问。
    pub(crate) session: Rc<RefCell<Option<Session>>>,
    /// composition 封装（随会话创建/销毁）。Rc 共享：候选窗回调 dispatch 用。
    pub(crate) composition: Rc<RefCell<Option<Composition>>>,
    /// 候选窗：CandwinCandidateWindow（M4：ULW 呈现，iuv-ui 绘图）。Rc 共享：同上。
    /// 具体类型（非 `Box<dyn>`）：M6 配置热载需直调 `set_theme`；交互/效果应用同槽。
    pub(crate) ui: Rc<RefCell<CandwinCandidateWindow>>,
    /// 上一次光标矩形（GetTextExt 失败时复用；首次用屏幕中央）。Rc 共享：同上。
    pub(crate) caret: Rc<Cell<CaretRect>>,
    /// Shift 临时英文模式（会话非 active 时 Shift 切换）。
    /// `Arc` 共享：语言栏"中/英"图标与按键路径读同一状态。
    pub(crate) english_mode: Arc<AtomicBool>,
    /// 语言栏"中/英"切换图标（Activate 挂载，Deactivate 卸载）。
    pub(crate) lang_bar: RefCell<Option<ComObject<LangBarItemButton>>>,
    /// TSF 候选 UI 元素宿主（WoW 游戏内候选框实验）。Rc 共享：dispatch 路径同线程访问。
    pub(crate) cand_elem: Rc<RefCell<CandidateElementHost>>,
    /// M6 daemon 客户端（共享段读取 + 管道写；Arc 与引擎 UserRemote 共享）。
    /// Deactivate 不撤——随进程/实例生命周期（TextService Drop 释放）。
    pub(crate) daemon: RefCell<Option<Arc<DaemonClient>>>,
    /// 远端写后端是否已注册到引擎（Activate 时引擎可能仍在后台加载，首键补注册）。
    pub(crate) remote_registered: Cell<bool>,
    /// 引号配对状态（`'`/`"` 交替开/关形）。会话开始/模式切换复位为开。
    pub(crate) punct_quote_open: Cell<bool>,
    /// 实例运行时四态（32-status-toolbar.md §5.1）：per-实例（非进程级 config），
    /// 启动 = `config.initial_state`，运行时操作才改；Session 构造注入（live 读）。
    /// `Arc<Mutex<...>>`：控制通道（SetState）与 OnChange 都写，会话/热键路径读。
    pub(crate) runtime: Arc<Mutex<ImeState>>,
    /// 反向控制端点（32-toolbar §4.2/§4.3）：accept 线程 + 隐藏消息窗。Activate 起、
    /// Deactivate/Drop 停（懒建，每个实例一个）。
    ctl: RefCell<Option<CtlEndpoint>>,
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
        let theme = match Config::load().theme {
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
            ui_rc
                .borrow_mut()
                .set_on_click(Some(Box::new(move |row: usize| {
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
            thread_focus_cookie: Cell::new(0),
            profiles: RefCell::new(None),
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
            runtime: Arc::new(Mutex::new(Config::load().initial_state)),
            ctl: RefCell::new(None),
        }
    }

    /// 实例标识（pid:tid）：pid = 进程 id，tid = **OS 线程 id**（`GetCurrentThreadId`，
    /// 非 TSF client id）——前台看板判定 `GetWindowThreadProcessId` 返回 OS 线程 id，
    /// 直接用同一标识匹配实例表（32-toolbar §4.1）。
    pub(crate) fn instance_id(&self) -> (u32, u32) {
        (process_id(), thread_id())
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
        slot.as_mut().map(|ep| ep.attach(pid, tid)).unwrap_or(false);
    }

    /// 停反向控制端点（Deactivate：Drop 兜底清理，此处显式调以尽快释放窗口/线程）。
    fn stop_ctl_endpoint(&self) {
        let ep = self.ctl.borrow_mut().take();
        drop(ep); // CtlEndpoint::drop 停线程 + 清 GWLP_USERDATA + 销毁窗口
    }

    /// 应用反向控制命令（CtlCmd；TSF 线程 wndproc 调用，§4.3）。
    /// mode 走 OPENCLOSE compartment（真相源，OnChange 统一响应）；其余字段直改 runtime。
    fn apply_ctl_cmd(&self, cmd: &CtlCmd) -> CtlResult {
        match *cmd {
            CtlCmd::SetMode(english) => {
                // 中英：写 OPENCLOSE compartment（open=中文 / !open=英文）；SetValue
                // 同步重入 OnChange → apply_openclose 更新 runtime.mode + StateSync。
                let open = !english;
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
                    runtime.mode = if english {
                        InitialMode::English
                    } else {
                        InitialMode::Chinese
                    };
                    drop(runtime);
                    self.after_runtime_change();
                }
            }
            CtlCmd::SetWidth(full) => {
                let mut runtime = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
                runtime.width = if full {
                    WidthMode::Full
                } else {
                    WidthMode::Half
                };
                drop(runtime);
                self.after_runtime_change();
            }
            CtlCmd::SetScript(traditional) => {
                let mut runtime = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
                runtime.script = if traditional {
                    ScriptMode::Traditional
                } else {
                    ScriptMode::Simplified
                };
                drop(runtime);
                self.after_runtime_change();
            }
            CtlCmd::SetPunct(english_punct) => {
                let mut runtime = self.runtime.lock().unwrap_or_else(|e| e.into_inner());
                runtime.punct = if english_punct {
                    PunctMode::English
                } else {
                    PunctMode::Chinese
                };
                drop(runtime);
                self.after_runtime_change();
            }
        }
        CtlResult::Ok {
            state: self.runtime_snapshot(),
        }
    }
}

impl Drop for TextService {
    fn drop(&mut self) {
        INSTANCE_COUNT.fetch_sub(1, Ordering::SeqCst);
        // 先停反向控制端点（accept 线程 join + 隐藏窗销毁），再注销——避免 Drop 期间
        // 字段仍存活时 wndproc 并发访问（TSF 线程 Drop 内不泵消息，但防御性先停干净）。
        self.stop_ctl_endpoint();
        // 32-toolbar §4.1：实例 Drop = 失焦上报（daemon 解绑清理；纯信号模型下
        // 「注销」由「失焦」承担）。
        if let Some(client) = self.daemon.borrow().as_ref() {
            let (pid, tid) = self.instance_id();
            client.focus_lost(pid, tid);
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
                            let default_open =
                                Config::load().initial_state.mode == iuv_core::InitialMode::Chinese;
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
        // 维度②：线程焦点 sink（ITfThreadFocusSink）——应用切出 = OnKillThreadFocus
        // 即时隐藏信号（对齐搜狗"焦点离开瞬间消失"）；切入 = OnSetThreadFocus，
        // 重显由随后的 TIP Activate/Register 链路驱动。经 ITfThreadMgr 的 ITfSource。
        // 非致命：失败只记日志降级（activate 后续步骤必须继续，否则图标/注册全灭）。
        let focus_sink: ITfThreadFocusSink = self.to_object().to_interface();
        // SAFETY: 标准 TSF advise；sink 在本对象生命周期内有效，Deactivate 时 Unadvise。
        match unsafe { source.AdviseSink(&<ITfThreadFocusSink as Interface>::IID, &focus_sink) } {
            Ok(fcookie) => self.thread_focus_cookie.set(fcookie),
            Err(e) => log_line(&format!(
                "[focus] ThreadFocusSink advise 失败：{e:?}（维度②信号缺失，主体不受影响）"
            )),
        }

        // 维度①：输入法 profile 激活通知（ITfActiveLanguageProfileNotifySink::OnActivated
        // 带自家 CLSID 与激活标志——weasel 语言栏显隐同款全局信号，与窗口无关）。
        // 全链路非致命 + 分步打点：任一步失败记 HRESULT 后降级，主体功能不受影响。
        let mount_profiles = || -> Result<(ITfInputProcessorProfiles, u32)> {
            let profiles_sink: ITfActiveLanguageProfileNotifySink =
                self.to_object().to_interface();
            // SAFETY: CoCreateInstance 由系统解析注册表创建 TSF 对象（registration.rs 同款）。
            let profiles: ITfInputProcessorProfiles = unsafe {
                CoCreateInstance(&CLSID_TF_InputProcessorProfiles, None, CLSCTX_INPROC_SERVER)
            }?;
            log_line("[profiles] InputProcessorProfiles 创建成功");
            // advise 路径：QI ITfSource 再挂 ActiveLanguageProfileNotifySink。
            // SAFETY: 标准 COM QI；不支持则整体降级（记 HRESULT 观测哪步死）。
            let psrc: ITfSource = profiles.cast()?;
            log_line("[profiles] QI ITfSource 成功");
            // SAFETY: 标准 TSF advise；cookie 记录用于 Deactivate 反注册。
            let pcookie = unsafe {
                psrc.AdviseSink(
                    &<ITfActiveLanguageProfileNotifySink as Interface>::IID,
                    &profiles_sink,
                )?
            };
            Ok((profiles, pcookie))
        };
        match mount_profiles() {
            Ok(pair) => {
                *self.profiles.borrow_mut() = Some(pair);
                log_line(
                    "[focus/profiles] 探测 sink 已挂载（ThreadFocus + ActiveLanguageProfileNotifySink）",
                );
            }
            Err(e) => log_line(&format!(
                "[profiles] ActiveLanguageProfileNotifySink 挂载失败：{e:?}（维度①信号缺失，主体不受影响）"
            )),
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
        let menu_theme = match Config::load().theme {
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

        // 32-toolbar：激活上报（四态经信号通道发 daemon；passthrough 进程不上报）。
        self.signal_focus_gained();

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

        // 32-toolbar：停反向控制端点（accept 线程 + 隐藏窗）+ 失焦上报
        // （daemon 解绑 → 工具条隐藏）。同一实例再 Activate 会重发激活。
        self.stop_ctl_endpoint();
        if let Some(client) = self.daemon.borrow().as_ref() {
            let (pid, tid) = self.instance_id();
            client.focus_lost(pid, tid);
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

        // 卸载线程焦点 sink（维度②）。
        if self.thread_focus_cookie.get() != 0 {
            if let Some(tm) = self.thread_mgr.borrow().as_ref() {
                if let Ok(src) = tm.cast::<ITfSource>() {
                    // SAFETY: 标准 TSF unadvise 调用。
                    let _ = unsafe { src.UnadviseSink(self.thread_focus_cookie.get()) };
                }
            }
            self.thread_focus_cookie.set(0);
        }
        // 卸载输入法 profile 激活通知（维度①）。
        if let Some((profiles, pcookie)) = self.profiles.borrow_mut().take() {
            if let Ok(src) = profiles.cast::<ITfSource>() {
                // SAFETY: 标准 TSF unadvise。
                let _ = unsafe { src.UnadviseSink(pcookie) };
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

// ---- ITfThreadFocusSink / ITfInputProcessorProfilesSink ----
// 显隐治理信号（40-toolbar-show-hide-governance.md 三维模型；weasel 同款实践）。
// 回调点位全部打 log 留验证。

impl ITfThreadFocusSink_Impl for TextService_Impl {
    fn OnSetThreadFocus(&self) -> Result<()> {
        // 维度③：应用切入 → 「激活 + 四态」上报，daemon 绑定并立即重显工具栏
        // （Alt+Tab 回已激活应用必须靠此信号重显——log 实锤的「隐藏后永不重现」根因）。
        log_line("[focus] OnSetThreadFocus（线程焦点获得 → 激活上报）");
        if let Some(client) = self.daemon.borrow().as_ref() {
            let (pid, tid) = self.instance_id();
            client.focus_gained(pid, tid, self.runtime_snapshot());
        }
        Ok(())
    }

    fn OnKillThreadFocus(&self) -> Result<()> {
        // 维度②：应用切出 → 失焦上报，daemon 立即隐藏（对齐搜狗"焦点离开瞬间消失"）。
        log_line("[focus] OnKillThreadFocus（应用切出 → 失焦上报）");
        if let Some(client) = self.daemon.borrow().as_ref() {
            let (pid, tid) = self.instance_id();
            client.focus_lost(pid, tid);
        }
        Ok(())
    }
}

impl ITfActiveLanguageProfileNotifySink_Impl for TextService_Impl {
    fn OnActivated(
        &self,
        clsid: *const windows_core::GUID,
        _guidprofile: *const windows_core::GUID,
        factivated: windows_core::BOOL,
    ) -> Result<()> {
        // 只关心自家 CLSID 的激活通知（weasel 同款过滤）；空指针防御。
        let ours = !clsid.is_null() && unsafe { *clsid } == crate::registration::clsid();
        log_line(&format!(
            "[profiles] OnActivated ours={ours} fActivated={}",
            factivated.as_bool()
        ));
        if !ours {
            return Ok(());
        }
        // 维度①：切走（false）→ 失焦上报立即隐藏；切入（true）→ TIP Activate 链路重显
        // （此处不重复发激活，避免与既有链路双发）。
        if !factivated.as_bool() {
            if let Some(client) = self.daemon.borrow().as_ref() {
                let (pid, tid) = self.instance_id();
                client.focus_lost(pid, tid);
            }
        }
        Ok(())
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

    /// 焦点切换（ITfThreadMgrEventSink）：**不打断会话**（2026-08-21 设计原则——用户未按
    /// Esc/Enter/空格确认候选上屏前，会话不因焦点切换断开；修复 Excel 单元格编辑首键
    /// 落在编辑栏后被误判为 Alt+Tab 级焦点切换而原文上屏的 bug）。
    /// 仅隐藏候选窗/候选元素（避免悬浮到其他应用上方）；session/composition 原样保留，
    /// 返回原应用后继续（Alt+Tab 期间预编辑保留，语义与小狼毫一致）。
    /// 结束输入只由 Esc/Enter/空格（正常上屏）或 Ctrl+Space（apply_openclose 原文上屏）触发。
    fn OnSetFocus(
        &self,
        _pdimfocus: Ref<ITfDocumentMgr>,
        _pdimprevfocus: Ref<ITfDocumentMgr>,
    ) -> Result<()> {
        self.ui.borrow_mut().hide();
        self.cand_elem.borrow_mut().end();
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
