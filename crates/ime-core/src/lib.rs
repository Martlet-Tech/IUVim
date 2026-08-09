//! ime-core：引擎（切分/查词/Viterbi/会话状态机/排序管线）。跨平台纯 Rust。
//! W0 冻结件：candidate/config/key；其余由 Agent B 在 W1 实现。

pub mod candidate;
pub mod config;
pub mod engine;
pub mod key;
pub mod lm;
pub mod rerank;
pub mod schema;
pub mod session;
pub mod store;
pub mod viterbi;

pub use candidate::{Candidate, CandidateKind};
pub use config::Config;
pub use engine::Engine;
pub use key::{Effect, Key, PageInfo, SessionEnd};
pub use lm::{LmProvider, UnigramLm};
pub use rerank::{RerankCtx, RerankStage, StaticOrder};
pub use schema::{InputSchema, Quanpin};
pub use session::Session;
pub use store::{NullStore, UserDataStore};
