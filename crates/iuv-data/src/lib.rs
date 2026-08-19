//! iuv-data：词库编译器 + 二进制格式 + Dict 查询层。
//! W0 冻结：dict 完整实现；format/compile 由 Agent A 在 W1 实现。

pub mod compile;
pub mod dict;
pub mod format;
pub mod ipc;
mod mmap;
pub mod opencc;
pub mod shm;
mod userdict;

pub use compile::{compile_files, CompileStats};
pub use dict::{Dict, Entry, INITIAL_BUCKET_SIZE};
pub use format::load;
pub use opencc::{OpenccTable, MAGIC as OCC_MAGIC};
pub use ipc::{PipeClient, PipeServer, Request, Response};
pub use shm::{ShmReader, ShmWriter, SHM_CAPACITY, SHM_HEADER_LEN, SHM_NAME};
pub use userdict::UserDict;
