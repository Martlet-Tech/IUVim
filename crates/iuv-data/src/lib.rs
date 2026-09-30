//! iuv-data：词库编译器 + 二进制格式 + Dict 查询层 + **共享词汇层**（③-2 镜像归一：
//! Key/四态/ImeState/UserMutation/Ctl 等跨 crate 基础类型的全仓唯一定义，iuv-core/
//! iuv-proto/iuv-win 三方 re-export）。跨平台纯 Rust（mmap 在无法映射时降级 `fs::read`）。

pub mod candidate;
pub mod compile;
pub mod dict;
pub mod format;
pub mod ime;
pub mod key;
mod mmap;
pub mod opencc;
mod userdict;

pub use candidate::CandidateKind;
pub use compile::{compile_files, CompileStats};
pub use dict::{Dict, DictCursor, Entry, INITIAL_BUCKET_SIZE};
pub use format::load;
pub use ime::{CtlCmd, CtlResult, ImeMode, ImePunct, ImeScript, ImeState, ImeWidth};
pub use key::{Key, PageInfo, SessionEnd};
pub use opencc::OpenccTable;
pub use userdict::{UserDict, UserMutation};
