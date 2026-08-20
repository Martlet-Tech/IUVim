//! iuv-data：词库编译器 + 二进制格式 + Dict 查询层。
//! W0 冻结：dict 完整实现；format/compile 由 Agent A 在 W1 实现。
//! P3.2：ipc.rs/shm.rs 已移入 iuv-win（纯 Windows）；本 crate 恢复跨平台（mmap 有
//! `fs::read` 真降级）。

pub mod compile;
pub mod dict;
pub mod format;
mod mmap;
pub mod opencc;
mod userdict;

pub use compile::{compile_files, CompileStats};
pub use dict::{Dict, Entry, INITIAL_BUCKET_SIZE};
pub use format::load;
pub use opencc::OpenccTable;
pub use userdict::UserDict;
