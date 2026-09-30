//! 候选种类（③-2 镜像归一：全仓唯一定义）。core 的完整 `Candidate` 与 proto 的
//! 瘦身线上 `Candidate`（49 §4.5.3）共用本枚举。

use serde::{Deserialize, Serialize};

/// 候选种类。M3+ 可扩：English / Symbol…
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum CandidateKind {
    Sentence,
    Word,
    Char,
}
