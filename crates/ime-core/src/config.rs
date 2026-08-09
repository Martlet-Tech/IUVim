//! 引擎配置。W0 完整实现，冻结。

/// 引擎配置。默认：page_size=5, max_candidates=200, max_word_syllables=7。
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct Config {
    pub page_size: usize,
    pub max_candidates: usize,
    /// lattice 词宽上限
    pub max_word_syllables: usize,
}

impl Default for Config {
    fn default() -> Self {
        Config { page_size: 5, max_candidates: 200, max_word_syllables: 7 }
    }
}
