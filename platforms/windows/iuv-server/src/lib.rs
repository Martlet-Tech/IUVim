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

use iuv_core::{
    Effect, Engine, ImeState, InitialMode, Key, PunctMode, ScriptMode, SessionEnd, WidthMode,
};
use iuv_proto::{
    Candidate, CandidateKind, Caps, ClientConfig, ClientInfo, ImeMode, ImePunct, ImeScript,
    ImeState as WireImeState, ImeWidth, KeyOutcome, KeyVerdict, PageInfo, Push, ResumeToken,
    SessionEnd as WireSessionEnd, C2S, S2C,
};
use iuv_win::transport::{ConnHandler, Reply, Session};

pub mod candwin;
pub mod config_watch;

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
    /// 配置纪元（`config_watch` 监视 config.json 变化时自增）。会话在每个请求
    /// 处理时比对，变化则捎带 `Push::ConfigChanged`（latest-wins，49 §4.6）。
    config_epoch: Arc<AtomicU32>,
    /// 重绑注册表：token → (断连现场, 保存时刻)。断连时由 `EngineSession::drop`
    /// 写入，重连握手带 `Hello.resume` 时取走（49 §4.4）。
    resumes: Arc<Mutex<HashMap<u64, (SavedSession, Instant)>>>,
}

impl EngineService {
    pub fn new(engine: Arc<Engine>) -> EngineService {
        EngineService {
            engine,
            config_epoch: Arc::new(AtomicU32::new(0)),
            resumes: Arc::new(Mutex::new(HashMap::new())),
        }
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
        _client: &ClientInfo,
        caps: Caps,
        resume: Option<ResumeToken>,
        token: ResumeToken,
    ) -> Box<dyn Session> {
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
            app: _client.app.clone(),
            candwin: CandwinHandle::spawn(match theme_choice {
                iuv_core::ThemeChoice::Light => iuv_ui::theme_light(),
                iuv_core::ThemeChoice::Dark => iuv_ui::theme_dark(),
            }),
            caret: None,
            candwin_visible: false,
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
    /// 会话候选窗（服务端自渲染，49 §4.5.3；抑制命中时静默）。
    candwin: CandwinHandle,
    /// 客户端最近上报的光标锚点（屏幕物理坐标；None = 未上报，不渲染）。
    caret: Option<iuv_ui::CaretRect>,
    candwin_visible: bool,
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
                    initial_mode: match cfg.initial_state.mode {
                        InitialMode::Chinese => ImeMode::Chinese,
                        InitialMode::English => ImeMode::English,
                    },
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
                let effect = self.on_key(core_key(&key));
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
            C2S::ImeState(s) => {
                *self.runtime.lock().unwrap_or_else(|e| e.into_inner()) = core_ime_state(&s);
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
                if moved && self.candwin_visible {
                    self.candwin.send(CandwinCmd::MoveTo { caret: c });
                }
                reply.respond(S2C::Ok);
            }
            C2S::UserMutation(_)
            | C2S::FocusChanged { .. }
            | C2S::SetMaintenance { .. }
            | C2S::CtlResult(_) => {
                reply.respond(S2C::Ok);
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
                    kind: wire_candidate_kind(c.kind),
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
            end: effect.end.clone().map(wire_session_end),
            candidates,
            all_candidates,
            page: self.caps.has(Caps::UIELEMENT).then_some(PageInfo {
                page: effect.page.page as u32,
                page_count: effect.page.page_count as u32,
                page_size: effect.page.page_size as u32,
                total: effect.page.total as u32,
            }),
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
        // 不渲染，等客户端上报后由 MoveTo/下一键 Show 补位。
        let Some(caret) = self.caret else {
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
        if self.candwin_visible {
            self.candwin_visible = false;
            self.candwin.send(CandwinCmd::Hide);
        }
    }
}

/// 线上 `Key` → 核心 `Key`（镜像变体集，P3b 后 engine 直接消费 proto 类型时删除）。
fn core_key(k: &iuv_proto::Key) -> Key {
    match *k {
        iuv_proto::Key::Char(c) => Key::Char(c),
        iuv_proto::Key::ShiftChar(c) => Key::ShiftChar(c),
        iuv_proto::Key::Backspace => Key::Backspace,
        iuv_proto::Key::Space => Key::Space,
        iuv_proto::Key::Enter => Key::Enter,
        iuv_proto::Key::Esc => Key::Esc,
        iuv_proto::Key::Digit(n) => Key::Digit(n),
        iuv_proto::Key::Tab => Key::Tab,
        iuv_proto::Key::Delete => Key::Delete,
        iuv_proto::Key::Home => Key::Home,
        iuv_proto::Key::End => Key::End,
        iuv_proto::Key::Insert => Key::Insert,
        iuv_proto::Key::PageUp => Key::PageUp,
        iuv_proto::Key::PageDown => Key::PageDown,
        iuv_proto::Key::Up => Key::Up,
        iuv_proto::Key::Down => Key::Down,
        iuv_proto::Key::Left => Key::Left,
        iuv_proto::Key::Right => Key::Right,
        iuv_proto::Key::F1 => Key::F1,
        iuv_proto::Key::F2 => Key::F2,
        iuv_proto::Key::F3 => Key::F3,
        iuv_proto::Key::F4 => Key::F4,
        iuv_proto::Key::F5 => Key::F5,
        iuv_proto::Key::F6 => Key::F6,
        iuv_proto::Key::F7 => Key::F7,
        iuv_proto::Key::F8 => Key::F8,
        iuv_proto::Key::F9 => Key::F9,
        iuv_proto::Key::F10 => Key::F10,
        iuv_proto::Key::F11 => Key::F11,
        iuv_proto::Key::F12 => Key::F12,
        iuv_proto::Key::SwapLeft => Key::SwapLeft,
        iuv_proto::Key::SwapRight => Key::SwapRight,
        iuv_proto::Key::HideCandidate => Key::HideCandidate,
    }
}

fn wire_session_end(e: SessionEnd) -> WireSessionEnd {
    match e {
        SessionEnd::Commit(text) => WireSessionEnd::Commit(text),
        SessionEnd::Cancel => WireSessionEnd::Cancel,
    }
}

fn wire_candidate_kind(k: iuv_core::CandidateKind) -> CandidateKind {
    match k {
        iuv_core::CandidateKind::Sentence => CandidateKind::Sentence,
        iuv_core::CandidateKind::Word => CandidateKind::Word,
        iuv_core::CandidateKind::Char => CandidateKind::Char,
    }
}

fn core_ime_state(s: &WireImeState) -> ImeState {
    ImeState {
        mode: match s.mode {
            ImeMode::Chinese => InitialMode::Chinese,
            ImeMode::English => InitialMode::English,
        },
        width: match s.width {
            ImeWidth::Half => WidthMode::Half,
            ImeWidth::Full => WidthMode::Full,
        },
        script: match s.script {
            ImeScript::Simplified => ScriptMode::Simplified,
            ImeScript::Traditional => ScriptMode::Traditional,
        },
        punct: match s.punct {
            ImePunct::Chinese => PunctMode::Chinese,
            ImePunct::English => PunctMode::English,
        },
    }
}
