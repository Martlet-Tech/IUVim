//! iuv-server 引擎服务（49 §5 P3a）：transport [`ConnHandler`] 的引擎侧实现。
//!
//! 每连接一个 [`EngineSession`]（对应旧架构「每 TSF 实例一个 Engine Session」）：
//! - `Key` → `Session::on_key` → `Effect` 映射为瘦身 [`KeyOutcome`]（增量 composition）；
//! - 会话结束（`end` / `EndSession`）→ 服务端 Session 丢弃，客户端随后发 `EndSession`；
//! - `ImeState` → 会话运行时四态（客户端是 OPENCLOSE 真相源）。
//!
//! P4 服务端自渲染候选窗（49 §4.5.3/§2）：候选窗由本进程 [`candwin`] 渲染——
//! - `Effect` → UiSnapshot → 窗口命令（Show/Update/MoveTo/Hide），UI 线程每连接一个；
//! - 光标锚点由客户端经 `C2S::CaretMoved` 上报（屏幕物理坐标；DPI 服务端自算）；
//! - `KeyOutcome` 的候选字段只对**抑制名单命中**的连接填充（游戏桥自绘候选栏
//!   需要 `all_candidates` 数据源），普通应用零候选载荷——每键从几 KB 降到几十字节。
//!
//! 过渡期边界（P3，P4 收敛时移除）：
//! - `UserMutation` 无独立入口——调权/造词/屏蔽**只**经按键在引擎内生效
//!   （`Session::on_key(SwapLeft/…)` → `engine.swap_weights/…`），此处空实现；
//! - `FocusChanged`/`SetMaintenance` 不影响服务端会话（38 号：焦点切换不断会话）。

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iuv_win::logger::log_line;

use iuv_core::{Effect, Engine, ImeState, Key};
use iuv_proto::{Candidate, Caps, ClientConfig, ClientInfo, KeyOutcome, KeyVerdict, Push, ResumeToken, C2S, S2C};
use iuv_win::transport::{ConnHandler, ConnSender, Reply, Session};
use iuv_win::ToolbarSignal;

pub mod candwin;
pub mod config_watch;
pub mod daemon;

use crate::candwin::{CandwinCmd, CandwinHandle};

/// 重绑现场保留期：断连后客户端在此时间内带令牌重连可回绑（49 §4.5.4 方案 C）。
/// 超期回收——服务端不设定时器，`on_connect` 取用时顺带清扫。
const RESUME_TTL: Duration = Duration::from_secs(5 * 60);

/// 断连时保存的会话现场（Drop 里从 `EngineSession` 抽出）。
struct SavedSession {
    session: Option<iuv_core::Session>,
    baseline: Option<String>,
    runtime: Arc<Mutex<ImeState>>,
}

/// 引擎服务工厂：进程级一个 [`Engine`]，每连接派生会话。
pub struct EngineService {
    engine: Arc<Engine>,
    /// 用户库共享段写者（②：客户端 UserMutation → 引擎写盘 → 此处发布；
    /// 本地模式 TSF 实例与（迁移期）daemon 读端消费）。创建失败 = 不发布。
    shm: Arc<Mutex<Option<iuv_win::ShmWriter>>>,
    /// 配置纪元（`config_watch` 监视 config.json 变化时自增）。会话在每个请求
    /// 处理时比对，变化则捎带 `Push::ConfigChanged`（latest-wins，49 §4.6）。
    config_epoch: Arc<AtomicU32>,
    /// 重绑注册表：token → (断连现场, 保存时刻)。断连时由 `EngineSession::drop`
    /// 写入，重连握手带 `Hello.resume` 时取走（49 §4.4）。
    resumes: Arc<Mutex<HashMap<u64, (SavedSession, Instant)>>>,
    /// 连接发送器路由：pid/tid → ConnSender（②控制面：工具栏/热键 Ctl → 客户端）。
    /// 同进程重连覆盖旧条目；陈旧条目 request 返回 Closed，调用方降级。
    senders: Arc<Mutex<HashMap<(u32, u32), ConnSender>>>,
    /// 迁入的 daemon UI（工具栏宿主 + daemon 状态）；main 在启动 transport 前装配。
    ui: Mutex<Option<(Arc<daemon::state::DaemonState>, Arc<daemon::toolbar::ToolbarHost>)>>,
}

impl EngineService {
    pub fn new(engine: Arc<Engine>) -> EngineService {
        let shm = iuv_win::ShmWriter::create_or_open();
        if let Err(e) = &shm {
            log_line(&format!("[shm] 用户库共享段创建失败（不发布）：{e}"));
        }
        EngineService {
            engine,
            config_epoch: Arc::new(AtomicU32::new(0)),
            resumes: Arc::new(Mutex::new(HashMap::new())),
            shm: Arc::new(Mutex::new(shm.ok())),
            senders: Arc::new(Mutex::new(HashMap::new())),
            ui: Mutex::new(None),
        }
    }

    /// 控制面路由表句柄（main 装配 TransportCtlDispatcher 时共享）。
    pub fn senders_handle(&self) -> Arc<Mutex<HashMap<(u32, u32), ConnSender>>> {
        self.senders.clone()
    }

    /// 装配迁入的 daemon UI（main 在 ToolbarHost::spawn 后、transport start 前调用）。
    pub fn attach_ui(
        &self,
        state: Arc<daemon::state::DaemonState>,
        toolbar: Arc<daemon::toolbar::ToolbarHost>,
    ) {
        *self.ui.lock().unwrap_or_else(|e| e.into_inner()) = Some((state, toolbar));
    }


    /// 配置纪元句柄（main 装配 `config_watch` 时共享）。
    pub fn config_epoch(&self) -> Arc<AtomicU32> {
        self.config_epoch.clone()
    }

    /// 取走重绑现场（超期条目顺带清扫——服务端零定时器）。
    fn take_saved(&self, token: u64) -> Option<SavedSession> {
        let mut resumes = self.resumes.lock().unwrap_or_else(|e| e.into_inner());
        let now = Instant::now();
        resumes.retain(|_, (_, at)| now.duration_since(*at) < RESUME_TTL);
        resumes.remove(&token).map(|(s, _)| s)
    }
}

impl ConnHandler for EngineService {
    fn on_connect(
        &self,
        client: &ClientInfo,
        caps: Caps,
        resume: Option<ResumeToken>,
        token: ResumeToken,
        sender: ConnSender,
    ) -> Box<dyn Session> {
        // 控制面路由注册（pid/tid = 客户端握手报备；同进程重连覆盖）。
        self.senders
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert((client.pid, client.tid), sender.clone());
        // 连接时点即基线：不在建连时推当前配置（客户端连接时自行 Config::load），
        // 只推「连接之后发生的变化」。
        let seen_epoch = self.config_epoch.load(Ordering::Relaxed);
        let (session, baseline, runtime) = match resume.and_then(|t| self.take_saved(t.0)) {
            Some(saved) => {
                log_line(&format!(
                    "[resume] 令牌重绑成功：composition 基线长度 {}",
                    saved.baseline.as_deref().unwrap_or("").len()
                ));
                (saved.session, saved.baseline, saved.runtime)
            }
            None => {
                if resume.is_some() {
                    log_line("[resume] 令牌无对应现场（超期/已作废/服务端重启）→ 全新会话");
                }
                (None, None, Arc::new(Mutex::new(ImeState::default())))
            }
        };
        // 候选窗 UI 线程（每连接一个；主题取引擎当前配置，随 ConfigChanged 热载）。
        let theme_choice = self.engine.config().theme;
        Box::new(EngineSession {
            engine: self.engine.clone(),
            session,
            runtime,
            caps,
            baseline,
            config_epoch: self.config_epoch.clone(),
            seen_epoch,
            token,
            resumes: self.resumes.clone(),
            app: client.app.clone(),
            client_pid: client.pid,
            client_tid: client.tid,
            ui: self.ui.lock().unwrap_or_else(|e| e.into_inner()).clone(),
            shm: self.shm.clone(),
            candwin: CandwinHandle::spawn(
                match theme_choice {
                    iuv_core::ThemeChoice::Light => iuv_ui::theme_light(),
                    iuv_core::ThemeChoice::Dark => iuv_ui::theme_dark(),
                },
                sender.clone(),
            ),
            caret: None,
            candwin_visible: false,
            awaiting_caret: None,
        })
    }
}

/// `candidate_owner_apps` 命中判定（客户端游戏桥自绘候选栏 → 本连接不渲染、
/// KeyOutcome 携带候选数据源）。exe 名单大小写不敏感精确匹配（与 TSF 侧同语义）。
fn app_suppressed(app: &str, list: &[String]) -> bool {
    let app = app.to_ascii_lowercase();
    let app = app.strip_suffix(".exe").unwrap_or(&app);
    list.iter().any(|owner| {
        let owner = owner.to_ascii_lowercase();
        let owner = owner.strip_suffix(".exe").unwrap_or(&owner);
        owner == app
    })
}

/// 一条连接的引擎会话。transport 保证 `on_c2s` 单线程独占调用。
pub struct EngineSession {
    engine: Arc<Engine>,
    session: Option<iuv_core::Session>,
    runtime: Arc<Mutex<ImeState>>,
    caps: Caps,
    /// composition 增量基线（None = 无基线，下一应答必须全量，49 §4.5.2）。
    baseline: Option<String>,
    /// 配置纪元（P4 配置热载）：服务端 `config_watch` 变更时自增，会话在请求
    /// 路径比对并捎带推送。进程内原子读，无磁盘/IPC 开销（非轮询）。
    config_epoch: Arc<AtomicU32>,
    seen_epoch: u32,
    /// 本连接的重绑令牌（transport 握手生成并随 SessionAttached 下发）。
    token: ResumeToken,
    /// 重绑注册表（断连时 Drop 写入现场）。
    resumes: Arc<Mutex<HashMap<u64, (SavedSession, Instant)>>>,
    /// 客户端宿主进程名（握手报备；抑制名单匹配用）。
    app: String,
    /// 客户端 pid/tid（握手报备；toolbar 信号/ctl 路由标识）。
    client_pid: u32,
    client_tid: u32,
    /// 迁入的 daemon UI（None = main 未装配）。
    ui: Option<(Arc<daemon::state::DaemonState>, Arc<daemon::toolbar::ToolbarHost>)>,
    /// 用户库共享段写者（与 EngineService 共享；UserMutation 后发布）。
    shm: Arc<Mutex<Option<iuv_win::ShmWriter>>>,
    /// 会话候选窗（服务端自渲染，49 §4.5.3；抑制命中时静默）。
    candwin: CandwinHandle,
    /// 客户端最近上报的光标锚点（屏幕物理坐标；None = 未上报，不渲染）。
    caret: Option<iuv_ui::CaretRect>,
    candwin_visible: bool,
    /// 锚点未上报时的待渲染快照（首会话首键竞态）：会话首键的引擎推进发生在
    /// Test 请求里，早于客户端 StartSession 的 CaretMoved 上报，此时 caret=None
    /// 无法渲染；而 Down 阶段命中 Test 缓存零 IPC，服务端不会再收到本键的 Key。
    /// 快照存此，CaretMoved 首报即补 Show（否则首键候选整帧丢失，下一键才见）。
    awaiting_caret: Option<iuv_ui::UiSnapshot>,
}

/// 断连时保存会话现场（49 §4.4/§4.5.4 方案 C）：连接线程释放
/// `Box<dyn Session>` 走 Drop，把**仍活动的**引擎会话按令牌存入注册表，
/// 客户端带 `Hello.resume` 重连即回绑。EndSession/commit 已清空
/// `self.session` → 无现场可存 = 令牌自然作废（重绑得全新会话）。
impl Drop for EngineSession {
    fn drop(&mut self) {
        if self.session.is_none() {
            return;
        }
        let saved = SavedSession {
            session: self.session.take(),
            baseline: self.baseline.take(),
            runtime: self.runtime.clone(),
        };
        let token = self.token.0;
        self.resumes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(token, (saved, Instant::now()));
        log_line(&format!("[resume] 断连保存现场：token={token:#x}"));
    }
}

impl Session for EngineSession {
    fn on_c2s(&mut self, req: C2S, reply: &mut Reply) {
        // 配置变更捎带（49 §4.6 latest-wins）：随本请求的应答/推送一起下行，
        // 客户端收到后自行刷新配置副本。传输层无需服务端主动发送通道。
        let epoch = self.config_epoch.load(Ordering::Relaxed);
        if epoch != self.seen_epoch {
            self.seen_epoch = epoch;
            let cfg = self.engine.config();
            reply.push(Push::ConfigChanged {
                epoch,
                client_view: ClientConfig {
                    initial_mode: cfg.initial_state.mode,
                },
            });
            // 服务端候选窗主题热载（与客户端 set_theme 同语义）。
            self.candwin.send(CandwinCmd::SetTheme(match cfg.theme {
                iuv_core::ThemeChoice::Light => iuv_ui::theme_light(),
                iuv_core::ThemeChoice::Dark => iuv_ui::theme_dark(),
            }));
        }
        match req {
            C2S::Key { key, full, .. } => {
                let t0 = std::time::Instant::now();
                let effect = self.on_key(key);
                let outcome = self.outcome(&effect, full);
                reply.respond(S2C::KeyResult(KeyVerdict::Consumed(outcome)));
                // P4 服务端自渲染：Effect → 快照 → 候选窗命令（抑制命中的连接静默）。
                self.sync_candwin(&effect);
                // 慢键观测（P0 预算定档数据，49 §4.5.2）：真实词库单键处理分布。
                let elapsed = t0.elapsed();
                if elapsed.as_millis() >= 10 {
                    log_line(&format!(
                        "[perf] on_key 慢键 {elapsed:?} key={key:?} cand={}",
                        effect.candidates.len()
                    ));
                }
                if effect.end.is_some() {
                    // 会话已由本键结束（Commit/Cancel）：服务端丢弃 Session；
                    // 客户端随后发 EndSession（幂等）。
                    self.drop_session();
                }
                // 每键 UiElement 推送已移除（实测：单字母 400+ 候选 × 双份载荷
                // 序列化顶破客户端 20ms 截止）——过渡期全量候选走 KeyOutcome
                // .all_candidates 单份；推送式回归 P4 服务端自渲染。
            }
            C2S::EndSession { .. } => {
                self.drop_session();
                self.hide_candwin();
                reply.respond(S2C::Ok);
            }
            C2S::CandwinHide => {
                // 焦点切换不打断会话（2026-08-21 原则）的远端对应：客户端本地窗
                // 隐藏时同步隐藏服务端窗口，会话保留，回焦后下键经 sync_candwin 重显。
                self.hide_candwin();
                reply.respond(S2C::Ok);
            }
            C2S::ImeState(s) => {
                let core = s;
                *self.runtime.lock().unwrap_or_else(|e| e.into_inner()) = core;
                // ②toolbar 信号迁入：四态变化 → StateChanged。
                if let Some((_, tb)) = &self.ui {
                    tb.handle_signal(&ToolbarSignal::StateChanged {
                        pid: self.client_pid,
                        tid: self.client_tid,
                        state: core,
                    });
                }
                // 客户端 request() 同步等应答（真机实锤 2026-09-27：不回应答 =
                // sync_state 每次白等满 300ms 截止 + 误标 degraded，四态切换卡
                // 300ms）。**每个 C2S 请求必须有应答**——fire-and-forget 变体
                // 一律回 Ok，杜绝整类「等不来的应答」。
                reply.respond(S2C::Ok);
            }
            C2S::Ping { nonce } => reply.respond(S2C::Pong { nonce }),
            C2S::CaretMoved { rect, .. } => {
                // 服务端渲染的光标锚点（客户端只在变化时上报；打字期锚点恒定）。
                let c = iuv_ui::CaretRect { x: rect.left, y: rect.top, w: rect.right - rect.left, h: rect.bottom - rect.top };
                let moved = self.caret != Some(c);
                self.caret = Some(c);
                if moved {
                    if self.candwin_visible {
                        self.candwin.send(CandwinCmd::MoveTo { caret: c });
                    } else if let Some(snap) = self.awaiting_caret.take() {
                        // 首会话首键补位：候选在 Test 请求已产出、因锚点未上报未
                        // 渲染（见 awaiting_caret 字段注释），锚点首报即补 Show。
                        self.candwin.send(CandwinCmd::Show { snap, caret: c });
                        self.candwin_visible = true;
                    }
                }
                reply.respond(S2C::Ok);
            }
            C2S::UserMutation(m) => {
                // M10 ②：客户端用户库变更 → 引擎应用（写盘）→ SHM 发布
                //（SHM 供系统内其他读者观察用户库版本/权重）。
                self.engine.apply_user_mutation(&m);
                if let Some(u) = self.engine.user_dict() {
                    let mut shm = self.shm.lock().unwrap_or_else(|e| e.into_inner());
                    if let Some(w) = shm.as_mut() {
                        match w.write(&u) {
                            Ok(v) => log_line(&format!("[shm] 用户库已发布 version={v}")),
                            Err(e) => log_line(&format!("[shm] 用户库发布失败：{e}")),
                        }
                    }
                }
                reply.respond(S2C::Ok);
            }
            C2S::FocusChanged { focused } => {
                // ②toolbar 信号迁入：焦点变化 → ToolbarSignal（pid/tid = 握手报备）。
                if let Some((_, tb)) = &self.ui {
                    let sig = if focused {
                        let st = *self
                            .runtime
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        ToolbarSignal::FocusGained {
                            pid: self.client_pid,
                            tid: self.client_tid,
                            state: st,
                        }
                    } else {
                        ToolbarSignal::FocusLost {
                            pid: self.client_pid,
                            tid: self.client_tid,
                        }
                    };
                    tb.handle_signal(&sig);
                }
                reply.respond(S2C::Ok);
            }
            C2S::SetMaintenance { .. } | C2S::CtlResult(_) => {
                reply.respond(S2C::Ok);
            }
            C2S::TypingActivity { active } => {
                // ②toolbar 信号迁入：桌宠动画驱动。
                if let Some((_, tb)) = &self.ui {
                    tb.handle_signal(&ToolbarSignal::Typing {
                        pid: self.client_pid,
                        tid: self.client_tid,
                        active,
                    });
                }
                reply.respond(S2C::Ok);
            }
            C2S::OpenSettings => {
                // ②语言栏菜单迁入：窗口已开 → 还原/置前；未开 → 主循环消费标志。
                if let Some((st, _)) = &self.ui {
                    if st.settings_open.load(std::sync::atomic::Ordering::Acquire) {
                        if daemon::settings::focus_existing_window() {
                            log_line("[settings] 已打开 → 还原/置前");
                        }
                    } else {
                        st.open_settings
                            .store(true, std::sync::atomic::Ordering::Release);
                    }
                }
                reply.respond(S2C::Ok);
            }
            C2S::ToggleToolbar => {
                if let Some((_, tb)) = &self.ui {
                    tb.handle_request(&iuv_win::Request::ToggleToolbar);
                }
                reply.respond(S2C::Ok);
            }
            C2S::ToolbarVisibleQuery => {
                let visible = self
                    .ui
                    .as_ref()
                    .map(|(_, tb)| tb.visible())
                    .unwrap_or(false);
                reply.respond(S2C::ToolbarVisible { visible });
            }
            C2S::PendingTextQuery => {
                // flush_session 原文上屏的真相源：picked + raw（核心会话现成方法）。
                // 空串 = 无待上屏内容（客户端走 cancel 清空预编辑）。
                let text = self
                    .session
                    .as_ref()
                    .map(|s| s.pending_text())
                    .unwrap_or_default();
                reply.respond(S2C::PendingText { text });
            }
            // 握手/通用应答/服务端心跳回执不经会话处理；新增变体在语义接入前落这里。
            _ => {}
        }
    }
}

impl EngineSession {
    fn on_key(&mut self, key: Key) -> Effect {
        match &mut self.session {
            None => {
                let mut s = self.engine.start_session_with_runtime(self.runtime.clone());
                let effect = s.on_key(key);
                self.session = Some(s);
                effect
            }
            Some(s) => s.on_key(key),
        }
    }

    fn drop_session(&mut self) {
        self.session = None;
        self.baseline = None;
    }

    /// `Effect` → 瘦身 [`KeyOutcome`]：composition 增量（基线相同 → `None`），
    /// 候选三件套仅 `Caps::UIELEMENT`。
    fn outcome(&mut self, effect: &Effect, full: bool) -> KeyOutcome {
        let changed = full || self.baseline.as_deref() != Some(effect.composition.as_str());
        let composition = changed.then(|| effect.composition.clone());
        let reading = changed.then(|| effect.reading.clone());
        self.baseline = Some(effect.composition.clone());
        // P4 裁撤：候选窗已由服务端自渲染——候选数据源（游戏桥 UI 元素所需）
        // 只对抑制名单命中的连接上线；普通应用发空候选（显式清空客户端旧值，
        // 避免增量合并把上一键候选留在游戏桥里），每键载荷从几 KB 降到几十字节。
        let element_data = self.caps.has(Caps::UIELEMENT) && self.suppressed();
        let to_candidates = |list: &[iuv_core::Candidate]| {
            list.iter()
                .map(|c| Candidate {
                    text: c.text.clone(),
                    kind: c.kind,
                })
                .collect::<Vec<_>>()
        };
        // None = 无 UIELEMENT 能力（客户端无元素宿主）；Some(空) = 有能力但服务端
        // 渲染（显式清空客户端旧值，防增量合并把上一键候选留在游戏桥）。
        let candidates = if self.caps.has(Caps::UIELEMENT) {
            Some(if element_data {
                to_candidates(&effect.candidates)
            } else {
                Vec::new()
            })
        } else {
            None
        };
        let all_candidates = if self.caps.has(Caps::UIELEMENT) {
            Some(if element_data {
                to_candidates(&effect.all_candidates)
            } else {
                Vec::new()
            })
        } else {
            None
        };
        KeyOutcome {
            eaten: true,
            composition,
            reading,
            end: effect.end.clone(),
            candidates,
            all_candidates,
            page: self.caps.has(Caps::UIELEMENT).then_some(effect.page),
            selected: self.caps.has(Caps::UIELEMENT).then_some(effect.selected as u32),
        }
    }

    /// 抑制名单命中？（`candidate_owner_apps` ∩ 客户端宿主进程名；每次判定读
    /// 引擎配置——配置热载即时生效，代价一次 Config 克隆，相对引擎单键可忽略。）
    fn suppressed(&self) -> bool {
        app_suppressed(&self.app, &self.engine.config().candidate_owner_apps)
    }

    /// Effect → 候选窗命令（服务端自渲染；49 §4.5.3）。
    /// 会话结束/取消 → Hide；空快照 → Hide；可见 → Update（原位），不可见 → Show。
    fn sync_candwin(&mut self, effect: &Effect) {
        if effect.end.is_some() {
            self.hide_candwin();
            return;
        }
        if self.suppressed() {
            return; // 抑制命中：客户端游戏桥自绘，服务端窗静默
        }
        let mut snap = iuv_ui::effect_to_snapshot(effect);
        snap.orientation = self.engine.config().candidate_orientation;
        if snap.candidates.is_empty() && snap.reading.is_empty() {
            self.hide_candwin();
            return;
        }
        // 尚未收到客户端锚点（连接后首个会话的首键早于 CaretMoved 到达）：
        // 快照挂起，锚点首报时补 Show（Down 阶段命中 Test 缓存零 IPC，
        // 服务端不会再来本键的 Key——直接丢弃 = 首键候选整帧丢失）。
        let Some(caret) = self.caret else {
            self.awaiting_caret = Some(snap);
            return;
        };
        if self.candwin_visible {
            self.candwin.send(CandwinCmd::Update { snap });
        } else {
            self.candwin.send(CandwinCmd::Show { snap, caret });
            self.candwin_visible = true;
        }
    }

    fn hide_candwin(&mut self) {
        // 挂起快照随会话收尾一并作废（EndSession/CandwinHide/end 都收敛到此），
        // 防陈旧快照在后续 CaretMoved 上意外补 Show。
        self.awaiting_caret = None;
        if self.candwin_visible {
            self.candwin_visible = false;
            self.candwin.send(CandwinCmd::Hide);
        }
    }
}

/// ② 控制面桥：工具栏/全局热键的四态翻转 → ConnSender（pid/tid 路由）→
/// 客户端 `S2C::Ctl` → 应用 → `C2S::CtlResult`。替代 daemon 时代的 CtlClient 旧管道。
pub struct TransportCtlDispatcher {
    pub senders: Arc<Mutex<HashMap<(u32, u32), ConnSender>>>,
}

impl daemon::toolbar::CtlDispatch for TransportCtlDispatcher {
    fn dispatch_ctl(
        &self,
        pid: u32,
        tid: u32,
        cmd: &iuv_win::CtlCmd,
    ) -> Result<iuv_win::CtlResult, String> {
        let sender = {
            let map = self.senders.lock().unwrap_or_else(|e| e.into_inner());
            if map.is_empty() {
                return Err("无活动客户端连接".into());
            }
            map.get(&(pid, tid)).cloned()
        };
        let Some(sender) = sender else {
            return Err("目标实例连接不存在（已断开？）".into());
        };
        // ③-2 归一：CtlCmd/CtlResult 沉底 iuv-data，win/proto 同型直通。
        match sender.request(S2C::Ctl { cmd: *cmd }, Duration::from_secs(3)) {
            Ok(C2S::CtlResult(r)) => Ok(r),
            Ok(_) => Err("Ctl 应答类型错误".into()),
            Err(e) => Err(e.to_string()),
        }
    }
}

