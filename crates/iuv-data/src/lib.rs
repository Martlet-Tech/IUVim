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
pub use opencc::OpenccTable;
pub use ipc::{
    ctl_pipe_name, CtlClient, CtlCmd, CtlResult, CtlServer, PipeClient, PipeServer, Request,
    Response, ToolbarState, CTL_FIELD_MODE, CTL_FIELD_PUNCT, CTL_FIELD_SCRIPT, CTL_FIELD_WIDTH,
};
pub use shm::{ShmReader, ShmWriter};
pub use userdict::UserDict;
