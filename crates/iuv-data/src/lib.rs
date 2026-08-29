//! iuv-data：词库编译器 + 二进制格式 + Dict 查询层。跨平台纯 Rust
//! （mmap 在无法映射时降级 `fs::read`）。

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
