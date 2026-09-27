//! iuv-server 引擎服务（49 §5 P3a）：transport [`ConnHandler`] 的引擎侧实现。
//!
//! 每连接一个 [`EngineSession`]（对应旧架构「每 TSF 实例一个 Engine Session」）：
//! - `Key` → `Session::on_key` → `Effect` 映射为瘦身 [`KeyOutcome`]（增量 composition）；
//! - 会话结束（`end` / `EndSession`）→ 服务端 Session 丢弃，客户端随后发 `EndSession`；
//! - `ImeState` → 会话运行时四态（客户端是 OPENCLOSE 真相源）。
//!
//! 过渡期边界（P3，P4 收敛时移除）：
//! - `UserMutation` 无独立入口——调权/造词/屏蔽**只**经按键在引擎内生效
//!   （`Session::on_key(SwapLeft/…)` → `engine.swap_weights/…`），此处空实现；
//! - 候选窗仍由客户端自绘（过渡 caps `UIELEMENT`）：`KeyOutcome` 带当前页候选 +
//!   全量候选（游戏内候选栏数据源；每键 `Push::UiElement` 推送已裁撤——实测
//!   单字母 400+ 候选双份载荷顶破客户端截止，见 on_c2s 内注释）；
//! - `FocusChanged`/`CaretMoved`/`SetMaintenance` 不影响服务端会话
//!   （38 号：焦点切换不断会话）。

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use iuv_win::logger::log_line;

use iuv_core::{
    Effect, Engine, ImeState, InitialMode, Key, PunctMode, ScriptMode, SessionEnd, WidthMode,
};
use iuv_proto::{
    Candidate, CandidateKind, Caps, ClientConfig, ClientInfo, ImeMode, ImePunct, ImeScript,
    ImeState as WireImeState, ImeWidth, KeyOutcome, KeyVerdict, PageInfo, Push,
    SessionEnd as WireSessionEnd, C2S, S2C,
};
use iuv_win::transport::{ConnHandler, Reply, Session};

pub mod config_watch;

/// 引擎服务工厂：进程级一个 [`Engine`]，每连接派生会话。
pub struct EngineService {
    engine: Arc<Engine>,
    /// 配置纪元（`config_watch` 监视 config.json 变化时自增）。会话在每个请求
    /// 处理时比对，变化则捎带 `Push::ConfigChanged`（latest-wins，49 §4.6）。
    config_epoch: Arc<AtomicU32>,
}

impl EngineService {
    pub fn new(engine: Arc<Engine>) -> EngineService {
        EngineService {
            engine,
            config_epoch: Arc::new(AtomicU32::new(0)),
        }
    }

    /// 配置纪元句柄（main 装配 `config_watch` 时共享）。
    pub fn config_epoch(&self) -> Arc<AtomicU32> {
        self.config_epoch.clone()
    }
}

impl ConnHandler for EngineService {
    fn on_connect(&self, _client: &ClientInfo, caps: Caps) -> Box<dyn Session> {
        Box::new(EngineSession {
            engine: self.engine.clone(),
            session: None,
            runtime: Arc::new(Mutex::new(ImeState::default())),
            caps,
            baseline: None,
            // 连接时点即基线：不在建连时推当前配置（客户端连接时自行 Config::load），
            // 只推「连接之后发生的变化」。
            seen_epoch: self.config_epoch.load(Ordering::Relaxed),
            config_epoch: self.config_epoch.clone(),
        })
    }
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
}

impl Session for EngineSession {
    fn on_c2s(&mut self, req: C2S, reply: &mut Reply) {
        // 配置变更捎带（49 §4.6 latest-wins）：随本请求的应答/推送一起下行，
        // 客户端收到后自行刷新配置副本。传输层无需服务端主动发送通道。
        let epoch = self.config_epoch.load(Ordering::Relaxed);
        if epoch != self.seen_epoch {
            self.seen_epoch = epoch;
            reply.push(Push::ConfigChanged {
                epoch,
                client_view: ClientConfig {
                    initial_mode: match self.engine.config().initial_state.mode {
                        InitialMode::Chinese => ImeMode::Chinese,
                        InitialMode::English => ImeMode::English,
                    },
                },
            });
        }
        match req {
            C2S::Key { key, full, .. } => {
                let t0 = std::time::Instant::now();
                let effect = self.on_key(core_key(&key));
                let outcome = self.outcome(&effect, full);
                reply.respond(S2C::KeyResult(KeyVerdict::Consumed(outcome)));
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
                reply.respond(S2C::Ok);
            }
            C2S::ImeState(s) => {
                *self.runtime.lock().unwrap_or_else(|e| e.into_inner()) = core_ime_state(&s);
            }
            C2S::Ping { nonce } => reply.respond(S2C::Pong { nonce }),
            C2S::UserMutation(_)
            | C2S::FocusChanged { .. }
            | C2S::CaretMoved { .. }
            | C2S::SetMaintenance { .. }
            | C2S::CtlResult(_) => {}
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
        let with_ui = self.caps.has(Caps::UIELEMENT);
        KeyOutcome {
            eaten: true,
            composition,
            reading,
            end: effect.end.clone().map(wire_session_end),
            candidates: with_ui.then(|| {
                effect
                    .candidates
                    .iter()
                    .map(|c| Candidate {
                        text: c.text.clone(),
                        kind: wire_candidate_kind(c.kind),
                    })
                    .collect()
            }),
            all_candidates: with_ui.then(|| {
                effect
                    .all_candidates
                    .iter()
                    .map(|c| Candidate {
                        text: c.text.clone(),
                        kind: wire_candidate_kind(c.kind),
                    })
                    .collect()
            }),
            page: with_ui.then_some(PageInfo {
                page: effect.page.page as u32,
                page_count: effect.page.page_count as u32,
                page_size: effect.page.page_size as u32,
                total: effect.page.total as u32,
            }),
            selected: with_ui.then_some(effect.selected as u32),
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
