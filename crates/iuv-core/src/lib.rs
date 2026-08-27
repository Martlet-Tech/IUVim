//! iuv-core：引擎（切分/查词/Viterbi/会话状态机/排序管线）。跨平台纯 Rust。
//! W0 冻结件：candidate/config/key；其余由 Agent B 在 W1 实现。

pub mod api;
pub mod candidate;
pub mod classic;
pub mod config;
pub mod engine;
pub mod key;
pub mod lm;
pub mod punct;
pub mod rime;
pub mod schema;
pub mod script;
pub mod session;
pub(crate) mod userdict;
pub mod viterbi;

pub use api::{EngineCtx, ImeEngine, PendingInput, Span, Translation};
pub use candidate::{Candidate, CandidateKind};
pub use config::keymap::{is_session_start_key, Combo, GlobalAction, Keymap, SessionAction, TwoSlot};
pub use config::{
    Config, ImeState, InitialMode, Orientation, PunctMode, ScriptMode, ThemeChoice,
    WidthMode,
};
pub use engine::Engine;
pub use key::{Effect, Key, PageInfo, SessionEnd};
pub use lm::{LmProvider, UnigramLm};
pub use punct::{chinese_punct, fullwidth, fullwidth_text, shifted_punct};
pub use rime::RimeEngine;
pub use schema::{InputSchema, Quanpin};
pub use script::ScriptConverter;
pub use session::Session;
pub use userdict::{UserMutation, UserRemote};

/// 去掉拼音串中的音节分隔撇号（`ni'hao` → `nihao`）。引擎/会话热路径共用（P1.6 抽取）。
pub(crate) fn strip_apostrophes(s: &str) -> String {
    s.chars().filter(|c| *c != '\'').collect()
}
